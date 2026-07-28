# Nexus Mods API assumptions

Onera targets **API v3** at `https://api.nexusmods.com/v3`. This document records
what it relies on and what breaks if Nexus changes it, so a future maintainer
does not have to reverse-engineer the client to find out.

## Authentication

The v3 specification declares two security schemes:

```yaml
ApiKeyAuth: { type: apiKey, in: header, name: apikey }
BearerJwtAuth: { type: http, scheme: bearer, bearerFormat: JWT }
```

Onera implements the first. `AuthProvider` exists precisely so the second — or a
full SSO flow — can be added without touching `NexusClient` or anything in the
core: the client only ever asks for a `Credential`.

**Assumption:** the header is literally `apikey`, lower case.
**If it changes:** one constant in `crates/onera-nexus/src/client.rs`.

## Endpoints Onera uses from v3

| Purpose                 | Endpoint                                         | Stability per the spec |
| ----------------------- | ------------------------------------------------ | ---------------------- |
| Mod metadata            | `GET /games/{game_domain}/mods/{game_scoped_id}` | Experimental           |
| A mod's file slots      | `GET /mods/{id}/files`                           | Experimental           |
| Versions of a file slot | `GET /mod-files/{id}/versions`                   | Experimental           |

Several of these are marked **Experimental** by Nexus, meaning they may change
significantly or be removed. Onera's wire types are correspondingly defensive:
every non-required field is `Option`, and `ModFileCategory` has a
`#[serde(other)]` fallback so a new category cannot break a mod listing.

## Endpoints v3 does not cover

The v3 specification Onera was written against covers mods, mod files and mod
file versions. It does **not** cover three things Onera needs:

| Need                                           | What Onera does                                                       | Where                         |
| ---------------------------------------------- | --------------------------------------------------------------------- | ----------------------------- |
| Validate a credential and identify the account | `GET /v1/users/validate.json`                                         | `auth.rs`, one function       |
| The supported-game catalogue                   | `GET /v1/games.json`                                                  | `client.rs::games`            |
| Resolve a file into a download location        | `GET /v1/games/{domain}/mods/{id}/files/{file_id}/download_link.json` | `client.rs::resolve_download` |

These are the documented v1 endpoints. They are confined to three clearly marked
places; migrating them when v3 grows equivalents is a change to three functions
and their tests.

**If v1 is withdrawn:** authentication, game discovery and downloading stop
working. Mod metadata continues to work. This is the largest single external
risk in the project, which is why it is written down here rather than buried.

## Data model mapping

| Nexus concept                                    | Onera concept              |
| ------------------------------------------------ | -------------------------- |
| Mod                                              | `Mod` (a lineage)          |
| Mod file (the persistent slot on a mod page)     | _not modelled separately_  |
| Mod file version (the thing actually downloaded) | `Release` + `ProviderFile` |

A `Release` is created per mod-file-version, taking its `version` string and its
`uploaded_at`. A `ProviderFile` points at the same version and carries what the
downloader needs.

## Version strings

**Assumption: version strings are meaningless to compare.** Mod authors use
`1.0`, `v2.3-beta`, `2024.05.01`, `Final`, and `hotfix 3` — sometimes within one
mod. Onera stores the string byte for byte and never parses it. Ordering uses
`uploaded_at`, and `Release::is_newer_than` panics if handed releases of two
different mods.

If a release has no timestamp, Onera reports that it cannot order it rather than
guessing.

## Rate limiting

**Assumption:** Nexus reports budgets in `x-rl-hourly-remaining` and
`x-rl-daily-remaining`, and signals throttling with HTTP 429 plus optionally
`Retry-After` (seconds or an HTTP date).

Onera reads the budget headers on every response so it can slow down _before_
being refused. On 429 it honours `Retry-After` exactly — the server has said how
long to wait, and guessing shorter is how an application gets its key throttled.
Without a header it waits 60 seconds.

Backoff is exponential with **full jitter** (`sleep = random(0, base · 2ⁿ)`),
because with several concurrent downloads a deterministic backoff makes every
client retry at the same instant and re-trigger the limit.

Missing or malformed rate-limit headers are tolerated; they simply disable the
early-slowdown heuristic.

## Downloads

**Assumption:** download locations are HTTPS URLs carrying a signature in the
query string, valid for a short time.

Consequences, all implemented:

- the URL is treated as a **secret** — `redact_url` drops the whole query string
  before anything is logged;
- a persisted download job stores the _provider file id_, never the URL, and
  re-resolves on resume;
- a non-HTTPS location is refused outright;
- redirects are bounded and cannot downgrade to plain HTTP.

An empty download-location list is reported with a message about free accounts
needing to start the download from the website, because that is the usual cause.

## What Onera will not do

**It does not scrape.** Everything shown comes from the API. The browser
extension extracts a game domain and a mod id from the URL and nothing else — no
titles, no file tables, no download buttons. A Nexus redesign cannot break it.

## Testing

Default tests use mocked HTTP fixtures (`wiremock`) and need no network and no
API key: `crates/onera-nexus/tests/api_contract.rs` covers pagination, rate
limiting, retries, cancellation, malformed bodies, missing required fields, and
hostile identifiers. A live-API test would be opt-in behind an environment
variable and is not part of the default suite.
