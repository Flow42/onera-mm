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
   absurd sizes, or download locations pointing somewhere unexpected.

## Archive handling

The single most dangerous operation Onera performs is extracting an untrusted
archive. Every rule below is implemented in `onera-archive` and tested in
`crates/onera-archive/tests/malicious_archives.rs`.

| Attack                                                    | Defence                                                                                                                                            |
| --------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| Zip slip (`../../etc/cron.d/x`)                           | `RelPath::normalize` rejects any `..` component. **Fatal**: the whole archive is refused, because a traversal entry is never an accident           |
| Windows-style traversal (`..\..\x`)                       | Backslash is treated as a separator during normalization                                                                                           |
| Absolute paths (`/etc/passwd`)                            | Rejected before normalization                                                                                                                      |
| Drive and UNC prefixes (`C:\`, `\\host\share`)            | Rejected explicitly                                                                                                                                |
| Symlink escape (`link -> /etc`, then write `link/passwd`) | Links are **never extracted**. `StagingWriter` additionally re-checks every ancestor with `symlink_metadata` before creating it                    |
| Hard links, devices, FIFOs, sockets                       | Never extracted; recorded as rejected entries and shown to the user                                                                                |
| Decompression bomb                                        | Per-entry ratio heuristic on declared sizes, plus a _running byte budget_ measured against what is actually written — a lying header does not help |
| Entry-count exhaustion                                    | `max_entries`                                                                                                                                      |
| Path-length and depth exhaustion                          | `max_path_len`, `max_depth` (hard ceiling of 32)                                                                                                   |
| Duplicate entries overwriting each other                  | Files are opened `create_new`; a repeated path fails                                                                                               |
| Trailing-dot/space aliasing (`file.` vs `file`)           | Components that trim to nothing are rejected                                                                                                       |
| Extraction into the game directory                        | Structurally impossible: `extract` takes a staging root, and `StagingWriter` refuses a non-empty directory                                         |
| A misnamed archive handed to the wrong parser             | Format is detected from magic bytes, never the extension                                                                                           |
| An external tool doing something unexpected               | After `7zz` runs, the staging tree is **re-walked and re-validated**: any symlink, special file or oversized file fails the operation              |

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
