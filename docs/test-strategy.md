# Test strategy

## Numbers

| Suite                        | Command                                                    | Count      |
| ---------------------------- | ---------------------------------------------------------- | ---------- |
| Rust unit and integration    | `cargo test --workspace`                                   | 577        |
| Desktop compiled smoke       | `cargo test` in `apps/desktop/src-tauri`                   | 10         |
| Frontend and extension units | `pnpm test`                                                | 129        |
| Frontend end-to-end          | `pnpm test:e2e`                                            | 50         |
| Live provider compatibility  | `cargo test -p onera-nexus --test live_smoke -- --ignored` | 4 (opt-in) |

**No default test needs network access, an API key, a keyring, a real game or a
built desktop binary.** That is a hard rule: a suite that needs credentials is a
suite that stops being run. The one suite that talks to a live API is
`#[ignore]`d _and_ skips itself when no key is present, so neither
`cargo test --workspace` nor CI ever needs a secret.

The desktop suite is a separate command because the Tauri crate is deliberately
outside the Cargo workspace: it needs webkit2gtk and libsoup, which a headless
job building the core does not. CI runs it in the job that already installs
those.

## Layers

### Property-based

`proptest` covers the one invariant everything else depends on: a successfully
normalized `RelPath` can never escape its root. Three properties, in
`crates/onera-core/src/paths.rs`:

- `normalized_paths_never_escape_root` — for arbitrary input, if normalization
  succeeds the resolved path is lexically inside the root, with no empty, `.` or
  `..` components;
- `normalization_is_idempotent`;
- `traversal_is_always_rejected` — `..` cannot be smuggled in by mixing `/` and
  `\` separators or by padding.

### Unit

The Milestone 2–4 domain types are contract-tested where they live, before any
behaviour exists behind them: profile invariants (one active profile per game,
per-game name uniqueness, priority ordering, and that an enabled member with no
artifact is reported rather than dropped), baseline identity (build identity is
compared and never ordered, an incomparable identity is `Unknown` rather than
`Same`, a metadata-only scan can never report clean, and unknown extras always
need a decision), and dependency states (a fetched empty set, an unavailable
one and an unsupported one are three distinct values; a changed definition
fingerprint invalidates an ignore decision; unknown DLC ownership never counts
as owned). `onera-resolver` is asserted never to report `Compatible` while it
is a scaffold.

Pure logic is tested where it lives: the provider stack (11 tests covering every
push/remove combination including shared identical content, downgrade and buried
removal), the classification rules, the operation state machine (terminal states
have no outgoing transitions; every non-terminal state offers a recovery), the
retry curve, redaction, VDF parsing, and both game adapters' layout resolvers.

Two adapters exist precisely so that "game-agnostic" is tested rather than
asserted. They are deliberately different shapes: Cyberpunk archives always name
their destination directory, so its adapter only ever _strips_ wrapper
directories, while Skyrim archives are as often relative to `Data/` as to the
game root, so its adapter also _adds_ a component. A mapping that is not the
identity function on paths is what exercises the planner, the provider stack and
the baseline scope as game-independent machinery.

### Integration

| File                                        | Covers                                                                                                                                                                                                                                                      |
| ------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `onera-archive/tests/malicious_archives.rs` | Zip slip, Windows traversal, absolute paths, symlinks, hard links, device nodes, compression bombs, entry-count and size limits, deep nesting, dirty staging, cancellation, progress                                                                        |
| `onera-db/tests/persistence.rs`             | Stack round-trips and ordering, cascade deletes, backup sharing, journal transitions, migration from populated v1, durable inbox/download jobs, installed-mod and archive read models                                                                       |
| `onera-install/tests/install_engine.rs`     | 33 tests: clean install, identical sharing, upgrades, downgrades, every conflict class, every decision, rule scoping, fault-injected rename and staging failures, recovery, verification, removal, round trips                                              |
| `onera-install/tests/reconciliation.rs`     | Multi-artifact atomic commit, offline deactivate/reactivate, stale previews, and complete rollback after injected staging/rename failures — including across two mods, so one cannot be left deployed and activated while the other is undone               |
| `onera-nexus/tests/api_contract.rs`         | Auth header, pagination, rate limits, retries, cancellation during backoff, malformed bodies, missing required fields, hostile identifiers                                                                                                                  |
| `onera-download/tests/streaming.rs`         | Streaming, hashing, dedup, hash mismatch, truncated responses, size limits, retries, cancellation, bounded concurrency, byte-range resume, safe non-range restart                                                                                           |
| `onera-discovery/tests/steam_identity.rs`   | Build identity from real-shaped `appmanifest` fixtures: normal manifests, beta branches, multiple depots, missing and malformed optional fields, native/Flatpak/second-drive layouts, manual installs, unknown DLC ownership                                |
| `onera-app/tests/end_to_end.rs`             | The full documented flow, plus conflict handling, malicious archives, secret redaction, and return-to-clean across the whole stack                                                                                                                          |
| `onera-app/tests/baseline.rs`               | Baseline status, capture and verification against a real `appmanifest`: capture leaves the scope untouched, symlinks are reported not followed, a build change is stale, a missing identity is unknown, recapture supersedes                                |
| `onera-app/tests/profiles.rs`               | Profile activation end to end: preview writes nothing, a missing artifact is downloaded during preparation, A→B→A restores byte for byte, a stale preview and an unresolved member are refused, cancellation and crash recovery keep the old profile active |
| `onera-db/tests/profiles.rs`                | Profile CRUD and, for activation, that the switch commits with the deployment it describes, that a refused completion rolls both halves back, and that only non-terminal attempts are offered for recovery                                                  |
| `onera-archive/tests/rar_archives.rs`       | Real RAR 5.0 containers, emitted byte by byte: benign extraction and hashing, magic-byte detection under a wrong name, the executable bit, traversal, symlinks, size limits, a corrupt archive                                                              |
| `onera-install/tests/database_faults.rs`    | Injected SQLite failures around journal transitions and profile activation: a failed begin, a failed transition, a failed entry write, a rollback that cannot record itself, a failed publish, and recovery once the database works again                   |
| `apps/desktop/src-tauri/tests/smoke.rs`     | Every `#[tauri::command]` is registered, the four documented flows have the commands they name, and the desktop's own start-up path runs, restarts and reopens its database                                                                                 |

### Fault injection

Two injectors, deliberately the same shape so a test can reach for either:

- `onera_install::fs::fault::FaultyFileSystem` wraps the real filesystem and
  fails the Nth rename or Nth staging write.
- `onera_db::fault::FaultyDatabase` wraps the real database and fails the Nth
  call to one persistence operation — a journal transition, a journal entry, the
  reconciliation-publishing transaction, a profile activation. `EveryAfter`
  fails that call and all later ones, modelling a database that has become
  unusable rather than one statement losing a race; it is the only way to reach
  a failure _inside_ the rollback path, which is only entered once something
  else has already failed.

Both are public rather than `#[cfg(test)]` because the interesting failures are
only reachable from an integration test in another crate.

Scans are interrupted through the ordinary `CancelToken`, driven from a progress
sink that trips partway through a walk — cancelling before a scan starts only
tests the guard at the top.

This is not decoration. Writing these tests found real bugs:

1. The commit loop overwrote the staged journal entry's `backup_id` with `NULL`,
   so an unmanaged original was never pushed onto the provider stack and could
   never be restored.
2. Rollback of a `Planned` operation returned early without cleaning up staged
   temporary files, because staging begins before the state advances.
3. A temporary file whose journal entry failed to write was never cleaned up by
   anything: rollback walks the journal, so an unrecorded temp file sat in the
   game directory permanently. Found by injecting a `put_entry` failure.

A fourth bug — removing a game's own empty directories on uninstall — was found
by the end-to-end test and fixed by tracking created directories per
installation. Three more came from the RAR fixture; see below.

### What the RAR fixture found

RAR was implemented and untested, because there is no free encoder and `7zz`
decodes RAR but never produces it. `rar_archives.rs` emits the RAR 5.0 container
directly — it is simple enough for stored entries — and reads it back with the
same external tool a user's machine would use. Three real bugs, all invisible to
every other test:

1. **Every RAR entry was rejected as a symbolic link.** `7z l -slt` prints a
   `Symbolic Link` key for _every_ RAR entry and leaves it blank for ordinary
   ones. Reading a blank value as a link meant a user previewing a RAR mod saw
   an archive with no content and a page of rejections.
2. **The switch that suppresses link restoration was misspelled.** `-snld-` is
   accepted by 7-Zip and does nothing; the real switch is `-snl-`. 7-Zip was
   therefore still creating symlinks on disk, and only the staging re-walk
   caught them — which failed the whole archive instead of dropping the link, so
   a RAR containing an ordinary symlink was uninstallable.
3. **The executable bit was always lost.** 7-Zip does not restore unix
   permissions, and the manifest read executability from disk. The zip and tar
   backends take it from what the archive declared; the external-tool path now
   does too.

### Frontend

Vitest covers the two boundaries where untrusted data arrives: the extension's
URL parsing and native-messaging envelope, and the desktop bridge's error
normalization. The view-models (`plan-view`, `progress`) are pure and tested
exhaustively, including that an _unknown_ classification from a newer backend
fails safe by requiring a decision.

Playwright drives the real SvelteKit build against a stubbed Tauri bridge, so
onboarding, masking of the key field, recovery-first startup, the durable
browser inbox, installed-mod read model, persisted downloads, and the whole Game
Integrity panel are covered without a compiled desktop binary. The integrity
specs exist to pin the claims the panel must never make: unknown freshness is
not freshness, a local snapshot says so, a quick check is never clean, a capture
cannot start over active mods or without the store-verification confirmation,
and returning to clean names what it refuses to touch.

## Coverage of the required cases

| Required                           | Where                                                                            |
| ---------------------------------- | -------------------------------------------------------------------------------- |
| Path normalization and traversal   | `paths.rs` proptests, `validate.rs`, `malicious_archives.rs`                     |
| Malicious archives, archive bombs  | `malicious_archives.rs`, `limits.rs`                                             |
| Layout detection                   | `cyberpunk2077.rs` (plain, wrapped, nested, ambiguous, unrecognizable)           |
| Clean installs                     | `install_engine.rs`, `end_to_end.rs`                                             |
| Identical shared files             | `provider_stack.rs`, `install_engine.rs`                                         |
| Same-mod upgrades and downgrades   | `install_engine.rs`                                                              |
| Unmanaged and cross-mod conflicts  | `install_engine.rs`, `end_to_end.rs`                                             |
| External file modifications        | `install_engine.rs`, `end_to_end.rs`                                             |
| Restoring previous providers       | `install_engine.rs`, `provider_stack.rs`                                         |
| Restoring unmanaged backups        | `install_engine.rs`, `end_to_end.rs`                                             |
| Interrupted writes and renames     | `install_engine.rs` (fault injection)                                            |
| Multi-mod atomic reconciliation    | `reconciliation.rs`, `domain/reconcile.rs`                                       |
| Profile activation and rollback    | `onera-app/tests/profiles.rs`, `reconciliation.rs`, `onera-db/tests/profiles.rs` |
| Restart recovery                   | `install_engine.rs`                                                              |
| Removal and reinstall round trips  | `install_engine.rs`                                                              |
| Nexus pagination and rate limiting | `api_contract.rs`                                                                |
| Malformed API responses            | `api_contract.rs`, `models.rs`                                                   |
| Secret redaction                   | `redact.rs`, `end_to_end.rs`                                                     |
| Native Messaging validation        | `onera-nmhost/src/protocol.rs`                                                   |
| Steam build identity and layouts   | `onera-discovery/tests/steam_identity.rs`                                        |
| Baseline capture and verification  | `onera-app/tests/baseline.rs`, `onera-install/tests/baseline.rs`                 |
| Return to clean                    | `end_to_end.rs`, `tests/e2e/integrity.spec.ts`                                   |
| Browser-extension flows            | `tests/js/`, `tests/e2e/`                                                        |
| RAR archives                       | `rar_archives.rs` (a real RAR 5.0 container, built in the test)                  |
| A second game adapter              | `skyrimse.rs`, `apps/desktop/src-tauri/tests/smoke.rs`                           |
| Database faults mid-operation      | `onera-install/tests/database_faults.rs`                                         |
| Interrupted baseline scans         | `onera-app/tests/baseline.rs`, `onera-install/tests/baseline.rs`                 |
| Package installation               | `onera-cli` unit tests, `packaging/verify-package.sh`                            |
| Live provider drift                | `onera-nexus/tests/live_smoke.rs` (opt-in)                                       |

## Packaging

The `.deb` and the AppImage register the Native Messaging host by two different
routes, and both are checked:

- **Declarations**, in `onera-cli`'s unit tests: the manifest the `.deb` ships
  points at the path the same package installs the host binary to, every
  supported browser directory is registered from the one checked-in manifest,
  the runtime dependencies are still declared, and the per-user setup path
  writes each browser its own directory with an absolute host path. A mismatch
  here produces a package that installs cleanly and silently never works,
  because the browser reports only "host not found".
- **Artifacts**, in `packaging/verify-package.sh`: run against a real built
  `.deb` (and optionally the AppImage), it unpacks the package without root and
  checks that the bundler acted on those declarations. Not part of CI, because
  it needs a full Tauri build; it is a release step.

## Gaps

Named rather than hidden:

- **Interrupted extraction** is not directly fault-injected. Extraction always
  targets a fresh staging directory that is discarded on failure, so the game is
  unaffected either way — but the cleanup path is untested.
- **No windowed desktop test.** The compiled smoke suite checks that every
  command is registered and that start-up works, and Playwright drives the real
  views against a stubbed bridge. Nothing drives a real window against a real
  backend: that needs `tauri-driver`, a display server and a webkit WebDriver
  binary in CI. The seam between them is covered by the frontend contract
  payload tests rather than by an integrated run.
- **The live Nexus suite is opt-in**, so drift is caught when someone runs it or
  during the manual smoke test, not by CI. Making it a CI job would require a
  credential, which is the rule this strategy will not break. Run it before a
  release.
- **`.deb` and AppImage artifacts** are verified by a script that is run at
  release time, not on every commit.

## Quality gates

CI runs, and all must pass:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check          # advisories, licences, bans
pnpm exec eslint .
pnpm exec prettier --check .
pnpm --filter onera-desktop exec svelte-check
pnpm test
pnpm test:e2e

cd apps/desktop/src-tauri && cargo test   # the compiled smoke suite
```

Before a release, additionally:

```sh
cargo test -p onera-nexus --test live_smoke -- --ignored --test-threads=1
packaging/verify-package.sh <onera_*.deb> <Onera_*.AppImage>
```

plus the manual smoke test in [`recovery.md`](recovery.md#manual-smoke-test).

The workspace coverage run (`cargo llvm-cov --workspace --all-features
--summary-only`) reports 76.3% line coverage; the desired-state core is at
92.0% and its filesystem executor at 75.3%.

`#![forbid(unsafe_code)]` and `#![warn(missing_docs)]` are set on every library
crate.
