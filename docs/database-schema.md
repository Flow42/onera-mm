# Database schema

SQLite, one file at `$XDG_DATA_HOME/onera/onera.db`, migrated on connect by
`sqlx::migrate!`. Queries are runtime `sqlx::query` calls rather than the
compile-time macros, so building Onera never requires a live database or a
checked-in `.sqlx` cache.

## Conventions

- Identifiers are UUID **text**, so rows can be created offline and reconciled
  later.
- Hashes are `algorithm:hex` in one column (`blake3:9f86d0…`). The algorithm is
  recorded even though BLAKE3 is the only one Onera computes, so provider-supplied
  MD5 digests can be stored without being mistaken for our own.
- Timestamps are RFC 3339 UTC text, which sorts correctly as text.
- Every table is `STRICT`.

Three pragmas are applied to every connection: `foreign_keys = ON` (SQLite
defaults it _off_, which would silently orphan provider-stack rows),
`journal_mode = WAL` (the UI reads while an install writes), and a 15-second
`busy_timeout`.

Onera refuses to open a database whose `schema_version` is newer than the build
understands. Downgrading is the one migration direction that cannot be made safe.

## Tables

### Provider catalogue — cache-like, always re-fetchable

| Table            | Holds                              | Notes                                                                     |
| ---------------- | ---------------------------------- | ------------------------------------------------------------------------- |
| `providers`      | Slug, name, API base               | `nexus` is seeded at startup                                              |
| `accounts`       | Who a stored credential belongs to | **Never** stores the credential itself                                    |
| `games`          | Provider game catalogue            | Unique on `(provider_id, provider_slug)`, so re-fetching updates in place |
| `mods`           | Mod lineages                       | Unique on `(provider_id, game_slug, provider_mod_id)`                     |
| `releases`       | One published version              | `version` stored **verbatim**, never parsed; ordering uses `published_at` |
| `provider_files` | Downloadable artifacts             | `published_hash` is advisory; Onera always computes its own               |

### Local installations

| Table                 | Holds                                  | Notes                                                                                                                                 |
| --------------------- | -------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| `local_game_installs` | A game on this machine                 | Install root, compat prefix and user-data roots are modelled **separately** because on Linux they are genuinely different directories |
| `deploy_roots`        | Resolved deployment directories        | Keyed by an adapter-defined `root_key`                                                                                                |
| `adapter_versions`    | Which adapter version last wrote state | Lets a future adapter detect state it does not understand                                                                             |

### Content storage

| Table                    | Holds                                     | Notes                                                                                                                                                                     |
| ------------------------ | ----------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `archives`               | Downloaded archives                       | `hash` is unique — this is where download deduplication is recorded. `original_filename` preserves what the provider called it; the on-disk path is derived from the hash |
| `archive_provider_files` | Which provider files an archive satisfies | Many-to-many                                                                                                                                                              |
| `archive_entries`        | The immutable extraction manifest         | What was approved cannot drift from what is deployed                                                                                                                      |
| `backups`                | Copies of overwritten files               | Content-addressed, so two mods overwriting the same vanilla file share one blob                                                                                           |

### Deployment — the part that is not a cache

| Table                     | Holds                                                     | Notes                                                                               |
| ------------------------- | --------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| `installations`           | One release deployed into one game                        |                                                                                     |
| `deployed_files`          | One row per `(game, root, relative path)`                 | `current_hash` is what should be on disk                                            |
| `deployed_file_providers` | **The provider stack.** Ordered by `position`, 0 = bottom | A `CHECK` enforces that a row names exactly one of `installation_id` / `backup_id`  |
| `installation_files`      | Which archive entry produced which deployed file          |                                                                                     |
| `created_directories`     | Directories Onera itself created                          | Removal only ever deletes from this list, so a game's own empty directories survive |
| `file_provider_history`   | Append-only audit trail of ownership changes              | Answers "how did this file get like this?"                                          |

### Operations

| Table             | Holds                                              | Notes                                                                                                  |
| ----------------- | -------------------------------------------------- | ------------------------------------------------------------------------------------------------------ |
| `operations`      | One journaled mutation, with the full plan as JSON | `CHECK` constrains `kind` and `state` to the modelled vocabulary                                       |
| `operation_files` | Per-file journal rows                              | Stores the resolved **absolute path**, so recovery works without loading a game adapter                |
| `conflicts`       | Recorded conflicts and their decisions             |                                                                                                        |
| `scoped_rules`    | Remembered decisions                               | Unique on `(mod_id, root_key, path_prefix)` — deliberately narrow; there is no global "always replace" |

### Downloads and browser handoff

| Table            | Holds                             | Notes                                                                                                                       |
| ---------------- | --------------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `download_jobs`  | Persisted download state          | Stores provider game/mod/file identifiers and a stable partial path, never the signed URL: those expire and are credentials |
| `inbox_requests` | Durable Native Messaging requests | Queued until the desktop completes, fails, or the user dismisses the requested action                                       |

Queued, running, and paused downloads resume on startup. Onera re-resolves a
fresh provider URL, sends a byte-range request for the retained partial, and
safely restarts from zero if the server does not support ranges.

## Cascades

`ON DELETE CASCADE` runs from providers down through games, mods, releases and
files, and from installations to their provider-stack rows. Deleting an
installation therefore releases every claim it held in one statement — which is
exactly what `remove_installation` relies on, and what a test asserts.

`backups.id` is referenced with `ON DELETE SET NULL` rather than cascade: losing
a backup record must not delete the stack entry that documents an unmanaged
original ever existed.
