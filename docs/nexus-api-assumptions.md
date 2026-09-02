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

| Purpose                     | Endpoint                                                    | Stability per the spec |
| --------------------------- | ----------------------------------------------------------- | ---------------------- |
| Mod metadata                | `GET /games/{game_domain}/mods/{game_scoped_id}`            | Experimental           |
| A mod's file slots          | `GET /mods/{id}/files`                                      | Experimental           |
| Versions of a file slot     | `GET /mod-files/{id}/versions`                              | Experimental           |
| Declared dependencies       | `GET /mod-file-versions/{id}/dependencies`                  | Experimental           |
| Resolved candidates (batch) | `POST /mod-file-versions/dependencies/ranges/materialized/batch` | Experimental      |
| Candidate identities        | `POST /mod-file-versions/batch`                             | Experimental           |

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

## Dependencies

Three endpoints, because no one of them can answer the question honestly on its
own. `crates/onera-nexus/src/dependencies.rs` is the only place that knows this.

| Endpoint | What it is authoritative about |
| --- | --- |
| `GET /mod-file-versions/{id}/dependencies` | whether a version declares anything at all |
| `POST …/dependencies/ranges/materialized/batch` | which concrete versions currently satisfy a declaration |
| `POST /mod-file-versions/batch` | display identity (name, version) of a candidate |

**The deprecated twin is not used.** `POST
/mod-file-versions/dependencies/materialized/batch` takes the same request and
returns the same rows but is marked deprecated; Onera calls
`/mod-file-versions/dependencies/ranges/materialized/batch`.

### Why the raw endpoint is asked at all

**Assumption:** the specification says a source version with no resolvable
candidates *contributes no rows* to the batch response. A missing row therefore
means "declared nothing", "declared something unresolvable", or "the resolver
had nothing to say" — three states Onera must keep apart, because collapsing
them turns an unsatisfiable profile into an apparently compatible one.

So the raw declaration is fetched per source first, and it decides:

| Nexus said | Onera reports |
| --- | --- |
| both definition arrays empty | `Fetched`, no groups; `declares_no_dependencies()` is true |
| definitions, but no candidate rows for one | a group with **no candidates** — visible and unsatisfiable |
| the declaration call failed | `Unavailable` with the provider's reason |
| the batch call failed for the chunk | `Unavailable` for every source in that chunk |

An empty `groups` list is never used to mean "we do not know". Only sources that
declared at least one version-range definition are sent to the batch endpoint; a
DLC-only or dependency-free version costs one request, not two.

**If the raw endpoint disappears:** every source becomes `Unavailable`. The
dependency check reports that it could not run — it never reports a clean bill
of health it did not receive.

### Request and response bounds

`DependencyLimits` in `crates/onera-nexus/src/client.rs` holds them; the defaults
are the documented request caps.

| Bound | Default | Why |
| --- | --- | --- |
| Source ids per batch request | 5000 | the specification's `maxItems` |
| Version ids per detail request | 2000 | the specification's `maxItems` |
| Candidate rows per page | 1000 | the specification's default `page_size` |
| Pages per chunk | 64 | a server that always says "one more page" must not loop forever |
| Rows per chunk | 100 000 | a server must not be able to make Onera allocate without limit |
| Response body | 16 MiB | enforced while streaming, before any parser sees the bytes |

Exceeding a bound produces `Unavailable` for the affected sources rather than a
truncated candidate list, because a truncated list silently turns a satisfiable
requirement into an unsatisfiable one.

Pagination stops on an empty page, on `meta.total_count` being reached, or on a
short page — whichever comes first. `meta.total_count` is only meaningful on a
non-empty page, per the specification, and missing metadata stops pagination
rather than continuing hopefully.

### Identifiers

Four Nexus identifiers stay distinct, because collapsing any two of them selects
the wrong artifact:

| Nexus | Onera |
| --- | --- |
| `version_id` (mod file version) | `provider_version_id`, and `provider_file_id` — the same id, kept in separate fields so a provider where they differ needs no core change |
| `mod_file_id` (update group/chain) | `provider_file_group_id` |
| `source_version_id` | `DependencySource::provider_version_id` |
| `position` (decimal string) | `DependencyCandidate::position` |

**Assumption:** `position` is a decimal string (`"3"`, `"3.5"`) so a version can
be inserted between two others, and higher means newer *within one chain*. Onera
scales it by a million into the domain's `i64`, which preserves ordering; a
position that is not a plain decimal — an exponent, a NaN, whitespace, 4 KB of
digits — becomes `None`, and an unordered candidate is honestly unordered rather
than guessed at. Nothing anywhere parses the version *string*.

**Assumption:** the batch rows identify a candidate's mod by a composite id and
say nothing about its game. The raw definitions name both, keyed by the same
`mod_file_id`, so that is where a candidate's game slug and mod-page id come
from. A candidate whose game cannot be established keeps an empty slug and
`CandidateStatus::Unknown`, so `is_selectable_for` rejects it for every game.

### Candidate status and strength

`mod_status` and the file `category` both have to agree before a candidate is
selectable:

| Nexus | Onera |
| --- | --- |
| `published` + a live category | `Available` |
| `published` + `removed` category | `Removed` |
| `published` + `archived` category | `Hidden` |
| `hidden`, `not_published`, `under_moderation` | `Hidden` |
| `removed`, `removed_by_staff` | `Removed` |
| anything this build does not recognise | `Unknown` — never selectable |

**Assumption: Nexus states no requirement strength.** Every declared dependency
maps to `RequirementKind::Required`. Onera does not invent a `Recommended` or
`Incompatible` edge it was never told about.

DLC definitions map straight across: one definition is one `DlcRequirement`, and
the `dlc_targets` inside it are OR-alternatives.

### Caching

The adapter does not cache. It preserves the provider's raw declaration JSON, the
materialized rows, and `fetched_at` on every snapshot, which is what the
application and database layers need to apply a TTL and to label stale data. TTL
policy lives there, not here.

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
API key. A live-API test would be opt-in behind an environment variable and is
not part of the default suite.

`crates/onera-nexus/tests/api_contract.rs` covers pagination, rate limiting,
retries, cancellation, malformed bodies, missing required fields, and hostile
identifiers.

`crates/onera-nexus/tests/dependency_contract.rs` pins down the exact behaviour
described above: multi-page batches, several sources answered in request order,
AND groups with OR candidates, DLC alternatives, a dependency-free version
against a version with zero materialized rows, an unresolvable group, hidden and
removed and unknown candidate status, the page and row ceilings, source-id
chunking, a throttled `POST` retried with the same body, cancellation, a lost
credential, a withdrawn endpoint, and malformed responses — each asserting that
the result is honest unavailability rather than an empty requirement list.
