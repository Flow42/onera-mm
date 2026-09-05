# Threat model

Onera runs as the user, with write access to their games and read access to a
credential worth money. It processes archives downloaded from a site where
anyone can upload, and it accepts messages from a browser extension. This
document says what it defends against, what it does not, and why.

## Assets

| Asset                             | Why it matters                                                                                    |
| --------------------------------- | ------------------------------------------------------------------------------------------------- |
| The Nexus personal API key        | Grants access to the user's account and download entitlements                                     |
| The user's game installations     | Corruption means a multi-gigabyte redownload; silent corruption means hours of confused debugging |
| Files the user created or edited  | Irreplaceable; not backed up by Steam                                                             |
| The rest of the user's filesystem | Onera runs as them and can write anywhere they can                                                |

## Adversaries

1. **A hostile mod archive.** Anyone can upload to a mod site. The archive is
   the primary attack surface.
2. **A hostile or compromised page in the browser.** A content script runs in a
   page's process; its messages are not authority.
3. **A hostile process on the same machine** that can register itself under the
   Native Messaging host name, or that can read files Onera writes.
4. **A compromised or misbehaving provider API** returning malformed data,
   absurd sizes, download locations pointing somewhere unexpected, or
   _dependency metadata_ naming mods the user never asked for.
5. **A mod author** who controls their own metadata and can name any
   dependency, including one whose identity resembles a mod the user trusts.

## Archive handling

The single most dangerous operation Onera performs is extracting an untrusted
archive. Every rule below is implemented in `onera-archive` and tested in
`crates/onera-archive/tests/malicious_archives.rs`.

| Attack                                                    | Defence                                                                                                                                                                             |
| --------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Zip slip (`../../etc/cron.d/x`)                           | `RelPath::normalize` rejects any `..` component. **Fatal**: the whole archive is refused, because a traversal entry is never an accident                                            |
| Windows-style traversal (`..\..\x`)                       | Backslash is treated as a separator during normalization                                                                                                                            |
| Absolute paths (`/etc/passwd`)                            | Rejected before normalization                                                                                                                                                       |
| Drive and UNC prefixes (`C:\`, `\\host\share`)            | Rejected explicitly                                                                                                                                                                 |
| Symlink escape (`link -> /etc`, then write `link/passwd`) | Links are **never extracted**. `StagingWriter` additionally re-checks every ancestor with `symlink_metadata` before creating it                                                     |
| Hard links, devices, FIFOs, sockets                       | Never extracted; recorded as rejected entries and shown to the user                                                                                                                 |
| Decompression bomb                                        | Per-entry ratio heuristic on declared sizes, plus a _running byte budget_ measured against what is actually written — a lying header does not help                                  |
| Entry-count exhaustion                                    | `max_entries`                                                                                                                                                                       |
| Path-length and depth exhaustion                          | `max_path_len`, `max_depth` (hard ceiling of 32)                                                                                                                                    |
| Duplicate entries overwriting each other                  | Files are opened `create_new`; a repeated path fails                                                                                                                                |
| Trailing-dot/space aliasing (`file.` vs `file`)           | Components that trim to nothing are rejected                                                                                                                                        |
| Extraction into the game directory                        | Structurally impossible: `extract` takes a staging root, and `StagingWriter` refuses a non-empty directory                                                                          |
| A misnamed archive handed to the wrong parser             | Format is detected from magic bytes, never the extension                                                                                                                            |
| An external tool doing something unexpected               | `7zz` is told not to restore links (`-snl-`), and after it runs the staging tree is **re-walked and re-validated**: any symlink, special file or oversized file fails the operation |

### The backslash trade-off

Backslashes are treated as path separators. On Linux `\` is a legal filename
character, so a genuine file named `weird\name.txt` is split into two
components. This is deliberate: archives produced on Windows routinely use `\`,
and letting `..\..\x` through because "Linux doesn't use backslashes" would be a
traversal vulnerability. Splitting a rare filename is the cheaper failure.

### Why traversal is fatal but links are not

A symlink in a tarball is ordinary — tar a source tree and you get some. Onera
drops those entries and tells the user. A `..` component is not ordinary; no
legitimate mod packaging process produces one. An archive containing one is not
trustworthy for its other entries either, so the whole thing is refused.

## Filesystem safety

- **Nothing is overwritten silently.** Unmanaged files, externally modified
  files and other mods' files all stop the plan for a decision.
- **Writes are atomic.** Content goes to a target-_adjacent_ temporary file
  (same filesystem, so `rename(2)` is atomic), is hashed, and only then renamed.
- **Backups precede overwrites.** A backup exists before the temporary file is
  even written, so a crash between the two still leaves the original.
- **Deployed files are re-read after the rename** and their hash re-checked. A
  mismatch fails the operation and triggers a rollback.
- **Only directories Onera created are removed.** A game's own empty directory —
  `archive/pc/mod` in a stock Cyberpunk install — survives uninstalling every
  mod, because created directories are recorded per installation.
- **A symlink where a managed file belongs is a hard error**, not "a modified
  file". Onera does not manage links and will not follow one it did not expect.

## Credential handling

- The API key lives **only** in the Secret Service. There is no file-backed
  fallback: if the keyring is unavailable, storing fails and the user is told.
  `crates/onera-app/tests/end_to_end.rs` asserts that no file Onera writes
  contains the key.
- `Secret` cannot print itself. Its `Debug`, `Display` and `Serialize`
  implementations all render `[redacted]`, so a credential cannot reach a log or
  a transport by accident.
- Signed download URLs are treated as secrets: `redact_url` drops the entire
  query string before a URL is logged.
- Third-party error text is scrubbed with `redact::scrub` before display, because
  an HTTP library may echo a header it was handed.
- The extension **never receives** the key, in either direction. It has no
  storage permission for one and no code path that could ask.

## Browser boundary

- The extension reads **only the URL**, and only `https://www.nexusmods.com/`.
  It extracts a game domain and a mod id and nothing else — no scraping, so a
  Nexus redesign cannot break it and a hostile page cannot feed it fabricated
  file metadata.
- The service worker **re-derives** identity from the URL rather than trusting
  what a content script sent it.
- The Native Messaging host validates protocol version, message size (1 MiB
  cap, checked _before_ allocating), request-id length, and every identifier
  against `[A-Za-z0-9_-]{1,64}`.
- Archives never travel over Native Messaging. The browser's download manager is
  never used — it cannot hash, deduplicate or resume, and it would put an
  untrusted file somewhere Onera did not choose.
- The extension checks that a reply's request id matches what it sent, so
  desynchronized replies cannot attribute one mod's result to another.

## Network boundary

- HTTPS only, in both the API client and the downloader. A non-HTTPS download
  location is refused outright.
- Redirects are bounded (5) and cannot downgrade to plain HTTP.
- `Content-Length` is validated against bytes received; a truncated transfer is
  never promoted into storage.
- Downloads have a size ceiling and a stall timeout.
- Every response is treated as untrusted: unknown enum values deserialize to
  `Unknown` rather than failing, absent optional fields are absent, and error
  bodies are parsed defensively and truncated to 500 characters before display.
- Path segments built from provider or extension input are percent-encoded, so
  a mod id of `../../admin` cannot reach a different endpoint.

## Provider metadata

Dependency definitions are the first provider data Onera _acts_ on rather than
merely displays, so they get their own boundary.

- **Metadata is advisory input, never executable authority.** A dependency
  definition can block a plan or raise a warning. It cannot install anything,
  choose a download location, or write a file. Everything a solved plan does
  still goes through the same preview, the same conflict rules and the same
  transaction as a manual install.
- **Every candidate is re-checked against the game and its selectability.** A
  candidate for a different game, or one the author has hidden or removed, is
  rejected rather than selected — a provider cannot use a dependency edge to
  route a user to content outside the game they are modding.
- **"No dependencies", "unavailable" and "unsatisfied" are three states.** A
  provider that fails, times out, or drops the endpoint produces `Unavailable`
  with a reason. It never collapses to an empty requirement list, because an
  empty list is a _permission to proceed_ and a failed fetch is not.
- **Stale cached metadata is labelled stale** and never presented as current.
  Offline operation is allowed; pretending the cache is fresh is not.
- **Definitions are fingerprinted.** An "ignore this requirement" decision is
  scoped to the exact definition the user was shown. Changing the metadata
  invalidates the decision rather than silently inheriting the accepted risk,
  so an author cannot get a broader dependency accepted by editing it after the
  fact.
- **Bounds are Onera's, not the provider's.** Batch sizes, page counts and row
  ceilings are capped locally, so a server that keeps answering "one more page"
  produces an honest `Unavailable` rather than an unbounded amount of work.

### Dependency confusion

The classic supply-chain shape — a hostile package taking the name of one the
user meant — is constrained here rather than eliminated:

| Attack                                            | Defence                                                                                                    |
| ------------------------------------------------- | ---------------------------------------------------------------------------------------------------------- |
| A dependency naming a mod from a different game   | Candidates are rejected unless they target the same game slug                                              |
| A dependency resolving to a hidden or deleted mod | Non-selectable candidates are never chosen                                                                 |
| Silent version drift under an accepted decision   | Pins are honoured exactly; a solved set that requires changing a pinned version is reported, not applied   |
| A solved set applied without the user seeing it   | Every solver outcome is a _proposal_. Installing, updating, downgrading and disabling all require approval |
| Metadata edited after a risk was accepted         | The ignore decision carries the definition fingerprint and stops applying when it changes                  |
| A requirement quietly dropped by an API change    | Capability loss is reported as unavailable, and an unsatisfied blocking requirement refuses to apply       |

What this does **not** defend against is a user approving a hostile mod that a
legitimate mod genuinely depends on. Onera can show what is being installed and
where it came from; it cannot judge whether an author's stated dependency is a
good idea. That is the same boundary as "a malicious mod's content", below.

## Baseline identity

A baseline is a claim about what "clean" means for one installation. Trusting a
stale one would make every later verification meaningless.

- **A baseline is bound to the build it was captured from.** Steam's BuildID and
  depot manifest identity are recorded with it, and a changed build marks the
  baseline stale rather than letting it keep answering.
- **Unknown identity is not fresh identity.** A manual or non-Steam install has
  no build identity to compare, and reports `Unknown` — a distinct state from
  "current". The panel is required never to render unknown freshness as
  freshness.
- **Comparison, never ordering.** Build identities are compared for equality.
  Onera does not decide that one build is "newer" and therefore fine.
- **The declared scope is fingerprinted into the baseline.** Narrowing an
  adapter's exclusions later invalidates existing baselines instead of quietly
  making "clean" easier to reach.
- **A partial scan can never be clean.** An interrupted or metadata-only scan
  reports what it saw and refuses a clean verdict, because only a complete
  content-hashed walk proves absence. Files an interrupted walk never reached
  are _unknown_, not missing.
- **A capture is refused while Onera knows mods are active**, so a baseline
  cannot record a modded directory as the clean state. This guard depends on the
  database: after losing it, Onera no longer knows, which is why
  [`database-maintenance.md`](database-maintenance.md) says to verify through
  the store before recapturing.
- **Restoration never deletes what it does not recognize.** Returning to clean
  restores baseline files and names the unknown extras it is leaving alone.

## Profile switching

A profile switch is the largest single change Onera makes: it can deploy and
withdraw dozens of files across several mods at once.

- **One transaction, one journal entry per file.** A switch is an ordinary
  reconciliation and uses the same engine, the same staging, the same atomic
  renames and the same rollback as a one-mod install.
- **The active profile is published with the deployment, not after it.** The
  profile only becomes active in the same database transaction that publishes
  the deployment it describes, and only after every file has been renamed into
  place and re-hashed. A crash between the two is impossible by construction.
- **A failed switch leaves the previous profile active.** Rollback restores the
  files while SQLite still describes the previous state, so the pair never
  disagrees. Tested by injecting both filesystem and database faults; see
  `crates/onera-install/tests/database_faults.rs`.
- **A rollback that cannot be recorded is not reported as done.** The operation
  stays non-terminal and is offered for recovery on the next launch, rather than
  being retried automatically into a state nothing has verified.
- **Changed-on-disk files block a switch** rather than being overwritten. A file
  edited after the preview stops the plan for a decision, the same as any other
  external modification.
- **Activation attempts left by a dead process are finalized on startup**, and
  none of them can make a profile active — only the completion transaction can.

## Out of scope

- **A malicious mod's _content_.** Onera will faithfully install a script mod
  that does something hostile when the game runs it. Deciding whether a mod's
  code is trustworthy is the user's judgement, not a mod manager's.
- **A compromised Nexus Mods.** If the API serves a hostile archive under a
  legitimate mod's identity, Onera will install it — subject to every archive
  rule above.
- **Sandboxing the game.** Onera does not confine what a game does once running.
- **Multi-user or privilege boundaries.** Onera runs as one user and assumes
  their session is theirs.
- **A compromised local account.** Anything that can run as the user can read
  the keyring through the same D-Bus API Onera uses.
