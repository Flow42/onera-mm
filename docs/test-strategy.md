# Test strategy

## Numbers

| Suite                        | Command                  | Count |
| ---------------------------- | ------------------------ | ----- |
| Rust unit and integration    | `cargo test --workspace` | 453   |
| Frontend and extension units | `pnpm test`              | 76    |
| Frontend end-to-end          | `pnpm test:e2e`          | 20    |

**No default test needs network access, an API key, a keyring, a real game or a
built desktop binary.** That is a hard rule: a suite that needs credentials is a
suite that stops being run.

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
retry curve, redaction, VDF parsing, and the Cyberpunk layout resolver.

### Integration

| File                                        | Covers                                                                                                                                                                                                                                                      |
| ------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `onera-archive/tests/malicious_archives.rs` | Zip slip, Windows traversal, absolute paths, symlinks, hard links, device nodes, compression bombs, entry-count and size limits, deep nesting, dirty staging, cancellation, progress                                                                        |
| `onera-db/tests/persistence.rs`             | Stack round-trips and ordering, cascade deletes, backup sharing, journal transitions, migration from populated v1, durable inbox/download jobs, installed-mod and archive read models                                                                       |
| `onera-install/tests/install_engine.rs`     | 33 tests: clean install, identical sharing, upgrades, downgrades, every conflict class, every decision, rule scoping, fault-injected rename and staging failures, recovery, verification, removal, round trips                                              |
| `onera-install/tests/reconciliation.rs`     | Multi-artifact atomic commit, offline deactivate/reactivate, stale previews, and complete rollback after injected staging/rename failures                                                                                                                   |
| `onera-nexus/tests/api_contract.rs`         | Auth header, pagination, rate limits, retries, cancellation during backoff, malformed bodies, missing required fields, hostile identifiers                                                                                                                  |
| `onera-download/tests/streaming.rs`         | Streaming, hashing, dedup, hash mismatch, truncated responses, size limits, retries, cancellation, bounded concurrency, byte-range resume, safe non-range restart                                                                                           |
| `onera-discovery/tests/steam_identity.rs`   | Build identity from real-shaped `appmanifest` fixtures: normal manifests, beta branches, multiple depots, missing and malformed optional fields, native/Flatpak/second-drive layouts, manual installs, unknown DLC ownership                                |
| `onera-app/tests/end_to_end.rs`             | The full documented flow, plus conflict handling, malicious archives, secret redaction, and return-to-clean across the whole stack                                                                                                                          |
| `onera-app/tests/baseline.rs`               | Baseline status, capture and verification against a real `appmanifest`: capture leaves the scope untouched, symlinks are reported not followed, a build change is stale, a missing identity is unknown, recapture supersedes                                |
| `onera-app/tests/profiles.rs`               | Profile activation end to end: preview writes nothing, a missing artifact is downloaded during preparation, A→B→A restores byte for byte, a stale preview and an unresolved member are refused, cancellation and crash recovery keep the old profile active |
| `onera-db/tests/profiles.rs`                | Profile CRUD and, for activation, that the switch commits with the deployment it describes, that a refused completion rolls both halves back, and that only non-terminal attempts are offered for recovery                                                  |

### Fault injection

`onera_install::fs::fault::FaultyFileSystem` wraps the real filesystem and fails
the Nth rename or Nth staging write. It is public rather than `#[cfg(test)]`
because the interesting failures are only reachable from an integration test.

This is not decoration. Writing these tests found two real bugs:

1. The commit loop overwrote the staged journal entry's `backup_id` with `NULL`,
   so an unmanaged original was never pushed onto the provider stack and could
   never be restored.
2. Rollback of a `Planned` operation returned early without cleaning up staged
   temporary files, because staging begins before the state advances.

A third bug — removing a game's own empty directories on uninstall — was found
by the end-to-end test and fixed by tracking created directories per
installation.

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

## Gaps

Named rather than hidden:

- **Interrupted extraction** is not directly fault-injected. Extraction always
  targets a fresh staging directory that is discarded on failure, so the game is
  unaffected either way — but the cleanup path is untested.
- **Database failures mid-operation** are covered by reasoning about write
  ordering, not by an injected SQLite fault.
- **RAR** archives are implemented but untested; no fixture exists.
- The **live Nexus API** is never contacted. A contract drift would be caught by
  the manual smoke test, not by CI.

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
```

The workspace coverage run (`cargo llvm-cov --workspace --all-features
--summary-only`) reports 76.3% line coverage; the desired-state core is at
92.0% and its filesystem executor at 75.3%.

`#![forbid(unsafe_code)]` and `#![warn(missing_docs)]` are set on every library
crate.
