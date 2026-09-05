# Database backup, restore and migration rollback

Onera's SQLite database is not a cache. It is the only record of which mod owns
which file, what the file looked like before that mod claimed it, and which
backup restores it. Losing it does not lose your mods — the archives and backup
blobs are still on disk — but it loses the _ownership history_ that makes
uninstalling safe, and it turns every managed file into an unknown extra.

This is the one piece of Onera state worth backing up.

## What lives where

Everything is under the XDG directories, so a backup tool that already covers
`~/.local/share` covers most of it.

| Path                               | Holds                                                     | Recreatable?                                 |
| ---------------------------------- | --------------------------------------------------------- | -------------------------------------------- |
| `$XDG_DATA_HOME/onera/onera.db`    | **The database.** Ownership, profiles, journal, baselines | **No**                                       |
| `$XDG_DATA_HOME/onera/backups/`    | Content-addressed copies of overwritten files             | **No** — these are the user's original files |
| `$XDG_DATA_HOME/onera/archives/`   | Downloaded mod archives, content-addressed                | Yes, by re-downloading                       |
| `$XDG_STATE_HOME/onera/staging/`   | Per-operation staging directories                         | Yes — discarded on recovery                  |
| `$XDG_STATE_HOME/onera/logs/`      | Logs                                                      | Yes                                          |
| `$XDG_CACHE_HOME/onera/downloads/` | Partial downloads                                         | Yes                                          |

**Back up `onera.db` and `backups/` together.** A database that references a
backup blob that is not there can describe what a file used to be but cannot put
it back; blobs without the database are unidentifiable content-addressed files.
The other directories are conveniences.

## Taking a backup

Onera uses WAL mode, so copying `onera.db` with `cp` while the application is
running can capture a torn state — the `-wal` and `-shm` files hold committed
data the main file does not yet. Use SQLite's own backup, which is consistent
even against a live database:

```sh
DB="${XDG_DATA_HOME:-$HOME/.local/share}/onera/onera.db"
sqlite3 "$DB" ".backup '/backups/onera-$(date +%F).db'"
```

Then the blobs, which are immutable and content-addressed, so a plain
incremental copy is safe at any time:

```sh
rsync -a "${XDG_DATA_HOME:-$HOME/.local/share}/onera/backups/" /backups/onera-blobs/
```

With Onera closed, a plain file copy is also fine, provided the `-wal` and
`-shm` files travel with it:

```sh
cp "$DB" "$DB-wal" "$DB-shm" /backups/    # only with the application closed
```

## Restoring

1. **Close Onera** — every window, the CLI, and the Native Messaging host.
   Restoring underneath a running process gives it a database that disagrees
   with the one it has open.
2. Put `onera.db` back, and delete any stale `onera.db-wal` and `onera.db-shm`
   beside it — they belong to the database you just replaced, not the one you
   restored.
3. Restore the `backups/` directory.
4. Start Onera and **verify before doing anything else**:

```sh
onera recover        # any operation the restored journal thinks is unfinished
onera verify --game <id> --installation <id>
```

A restored database is, by definition, a database that may be older than the
disk. Onera's ordering rule makes that the safe direction — the journal is
written before the effect it describes, so an older record can only ever
under-claim — but a file installed after the backup was taken is now an unknown
extra that Onera will not touch. `onera verify` names them.

### After restoring an older database

| Situation                                  | What Onera reports                            | What to do                                |
| ------------------------------------------ | --------------------------------------------- | ----------------------------------------- |
| A mod installed after the backup           | Its files are unmanaged extras                | Reinstall it, or remove its files by hand |
| A mod removed after the backup             | Its files are `Missing`                       | Remove the installation record            |
| An operation in flight when the backup ran | Offered for recovery on startup               | `onera recover --rollback`                |
| A baseline captured after the backup       | The game reports no baseline, or an older one | Recapture                                 |

## Migrations

Migrations are embedded in the binary and applied on connect. `SCHEMA_VERSION`
in `crates/onera-db/src/lib.rs` is the version a given build understands, and
`crates/onera-db/migrations/` holds them in order.

| Migration                 | Added                                              |
| ------------------------- | -------------------------------------------------- |
| `0001_initial`            | Catalogue, installations, provider stacks, journal |
| `0002_product_completion` | Download jobs and the durable browser inbox        |
| `0003_desired_state`      | Installation mappings and the reconciler           |
| `0004_active_lineage`     | Active/inactive artifacts                          |
| `0005_baselines`          | Baselines, scan runs and findings                  |
| `0006_profiles`           | Profiles, members and activation attempts          |
| `0007_dependencies`       | Dependency snapshots and overrides                 |

### There is no automatic rollback

Onera applies migrations forward only. There are no `down` scripts, and adding
them would be worse than not having them: a migration that adds a table also
starts filling it, so reversing the schema would silently discard rows the user
cannot get back — the profiles they built, the baselines they captured, the
ownership history that makes uninstalling safe.

**Rolling back a schema means restoring a backup taken before the upgrade.**
That is the supported path, and it is why the backup section above comes first.

### Downgrading to an older Onera

An older build refuses a newer database rather than opening it and misreading
columns it does not know about. `Database::open` compares the applied migration
version against its own `SCHEMA_VERSION` and fails with

> database schema version 8 is newer than this build understands (7); upgrade Onera

To go back:

1. Close Onera.
2. Restore the `onera.db` backup taken **before** the upgrade.
3. Restore the `backups/` directory from the same point, so the ownership
   records and the blobs they name agree.
4. Install the older build and run `onera recover`, then `onera verify`.

Anything done with the newer build is not in the restored database. Treat those
mods as installed-but-unknown, exactly as in the table above.

### Before upgrading

Take a backup first. It costs a second and it is the only way back:

```sh
sqlite3 "$DB" ".backup '/backups/onera-pre-upgrade.db'"
```

## Checking a database

```sh
sqlite3 "$DB" "PRAGMA integrity_check;"      # expect: ok
sqlite3 "$DB" "PRAGMA foreign_key_check;"    # expect: no output
sqlite3 "$DB" "SELECT * FROM _sqlx_migrations ORDER BY version;"
```

`integrity_check` reporting anything but `ok` means the file is damaged: restore
a backup rather than trying to repair it in place. A failing `foreign_key_check`
on a database Onera wrote is a bug — every connection sets `foreign_keys = ON`
— and is worth reporting with the output.

To reclaim space after removing many mods:

```sh
sqlite3 "$DB" "VACUUM;"     # with the application closed
```

## Starting over

If the database is lost and there is no backup, nothing is deployed twice and
nothing is deleted — but Onera no longer knows what it installed:

1. Point Onera at the game again and let it register.
2. Capture a baseline **only if the game directory is genuinely clean**. A
   baseline captured over installed mods records those mods as the clean state,
   which is exactly the lie the capture guard exists to prevent — it refuses
   while Onera knows mods are active, and after losing the database it no longer
   knows. Verify the game through the store first.
3. Reinstall mods through Onera so ownership is recorded again.

Files from the lost installation stay where they are. Onera reports them as
unknown extras and will not remove them without being told to.

## Related

- [`recovery.md`](recovery.md) — what survives an interrupted operation, and why
  the write ordering makes an older database the safe failure.
- [`database-schema.md`](database-schema.md) — what each table holds.
- [`packaging.md`](packaging.md) — where a packaged build puts these directories.
