# Implementation plan

## Goal

Move Onera from a strong transactional-core prototype to a usable alpha that can:

1. complete the current desktop and browser-driven install lifecycle;
2. capture and verify a clean game baseline;
3. save and activate game-specific mod profiles;
4. evaluate provider-supplied dependencies and propose a compatible set of mod
   versions; and
5. apply every multi-mod change through one previewable, journaled transaction.

The central architectural change is a **desired-state reconciler**. A profile,
"disable these mods", "update everything compatible", and "return to clean"
are all different ways of describing a desired game state. They must produce the
same kind of dry-run plan and use the same transactional engine.

## Product terminology and safety rules

- Use **profile** for Onera's local, game-specific selection of mods. Nexus also
  has a feature named Collections; using that term for both would make API and UI
  behavior ambiguous.
- A profile is scoped to one `LocalGameId`, not merely a game title. Two copies of
  a game may be on different versions or have different deployment roots.
- An installed mod artifact and an active deployment are different states.
  Disabling a mod removes its active file-provider claims but keeps its archive,
  metadata, layout, and profile membership.
- Exactly one profile is active for a local game. A built-in `Default` profile is
  created when the game is registered.
- Profile membership has an explicit priority. Priority determines provider-stack
  order when enabled mods target the same path; it never bypasses the existing
  conflict preview.
- Provider dependency definitions are advisory input, not executable authority.
  They can block a plan or produce warnings, but cannot write files themselves.
- "No dependencies reported", "dependency information unavailable", and
  "dependencies fetched and unsatisfied" are separate states.
- Ignoring a dependency problem always requires explicit confirmation. The
  override is scoped to a profile member and dependency-definition fingerprint;
  changed provider data invalidates the override.
- No unknown file is deleted to produce a clean state. Onera may delete files it
  deployed and may restore bytes it backed up. Everything else is reported for
  the user or the game store to repair.
- Onera does not parse arbitrary author version strings. Compatibility uses
  provider version identifiers, provider ordering, and materialized candidates.

## Target architecture

```text
desktop / CLI / native-message inbox
                 |
                 v
              onera-app
                 |
       +---------+-------------------+
       | desired-state reconciler    |
       | dependency solver           |
       | baseline verifier           |
       +---------+-------------------+
                 |
        MutationPlan (dry run)
                 |
          explicit approval
                 |
       journaled mutation engine
                 |
      SQLite + backups + archives

Provider ports <--- Nexus dependency adapter
Store ports    <--- Steam build-identity adapter
Game ports     <--- baseline scope/layout rules
```

### New core concepts

Add provider-neutral types under `onera-core/src/domain/`:

- `profile.rs`: `Profile`, `ProfileMember`, `MemberPriority`, `DesiredModState`.
- `dependency.rs`: dependency groups, candidates, DLC requirements, availability,
  health, overrides, and solver results.
- `baseline.rs`: baseline identity, file records, scan findings, and freshness.
- `reconcile.rs`: desired state, mutation steps, risks, approvals, and plan summary.

Keep the solver pure. It should accept a snapshot of installed/available versions,
profile wishes, pins, and dependency groups, then return a deterministic result.
It must not access SQLite, HTTP, or the filesystem.

### Ports

Extend the boundaries rather than putting Nexus or Steam types into the core:

- `ModProvider::dependency_capability()` and `dependencies(...)` return
  provider-neutral dependency sets.
- Add opaque `ProviderVersionId` and `ProviderFileGroupId` identifier types.
- Add a `GameStore` port for build identity and owned-DLC information. Missing
  capabilities return `Unknown`, not an empty set.
- Extend `GameAdapter` with baseline roots and exclusions. User-data roots are
  excluded by default.
- Add `ProfileStore`, `DependencyStore`, and `BaselineStore` ports, implemented
  by `onera-db`.
- Implement `onera-provider` as the registry that resolves a `ProviderId` to a
  provider adapter before adding a second provider.

### Installation model refactor

The current `installations` row conflates an acquired artifact with an active
deployment. Split those responsibilities without discarding existing data:

- An installation record remains after deactivation and owns the selected
  release, provider file, archive, and resolved layout.
- `installation_mappings` records stable source-to-target mappings and source
  hashes so an artifact can be reactivated without guessing its old layout.
- Active provider-stack rows represent what is currently deployed.
- Replace the destructive `remove_installation` repository operation with:
  `deactivate_installation`, which removes active claims but retains the artifact;
  and `purge_installation`, which permanently removes an unused artifact.
- Enforce at most one active installation per mod and local game.

Generalize the single-mod `InstallPlan` into a `MutationPlan` capable of reaching
one final provider stack per path. The plan can contain activation, deactivation,
upgrade, downgrade, restore, and delete steps. The engine stages every required
byte before entering `Committing` and journals the whole change as one operation.

New operation kinds:

- `reconcile`: profile switches and multi-mod compatible updates;
- `clean_restore`: reconcile to an empty desired mod set, then verify baseline;
- keep `install`, `remove`, and `repair` for focused operations and compatibility.

## Data migrations

Use additive SQL migrations and update `schema_meta` in each migration. Back up
the database before the first migration that changes installation semantics.

### `0002_product_completion.sql`

- Finish read queries for installed mods and update candidates.
- Add indexes needed by game/mod/release list views.
- Implement persistence methods for the existing `download_jobs` table.
- Add `inbox_requests` for browser requests with states `queued`, `claimed`,
  `waiting_for_user`, `complete`, and `failed`.

### `0003_desired_state.sql`

- Add provider file version identity and file-group identity to `provider_files`.
- Add `installation_mappings`.
- Add active/deactivated/artifact state to installations.
- Add a partial unique index for one active installation per game and mod.
- Make operation kinds accept `reconcile` and `clean_restore`.
- Add a persisted desired-state/reconciliation summary to operations.

### `0004_baselines.sql`

- `game_baselines`: game, source, store build identity, adapter/version, status,
  capture time, and scan fingerprint.
- `baseline_files`: root key, normalized relative path, BLAKE3, size, and mode.
- `baseline_scan_runs`: progress, result counts, cancellation, and errors.
- Keep historical baselines when a game build changes; mark them superseded
  rather than overwriting them.

### `0005_profiles.sql`

- `profiles`: local game, name, description, active flag, timestamps.
- `profile_members`: mod, desired provider file/version, optional installation,
  enabled flag, pinned flag, and priority.
- `profile_activation_history`: source/target profile, operation, result, time.
- Scope remembered file-conflict rules to a profile where applicable.

### `0006_dependencies.sql`

- `dependency_snapshots`: source provider version, availability, provider
  revision/fingerprint, raw metadata, and fetch time.
- `dependency_groups`: independent requirements (AND).
- `dependency_candidates`: alternatives within a group (OR), including target
  mod/file/version identifiers and provider ordering.
- `dlc_dependency_candidates`.
- `dependency_overrides`: profile member, definition fingerprint, user reason,
  and timestamp.

All migration tests must cover upgrading a populated v1 fixture, not only opening
a fresh database.

## Milestone 0: finish the current vertical slice

**Purpose:** provide a dependable user journey before adding more state models.

**Status: complete (2026-09-02).** Schema v2 adds durable browser requests and
the context needed to resume downloads. The desktop now uses real Installed,
Updates, Downloads, Add Mod, verification, removal, ownership, and recovery
flows. Startup resumes partial downloads, routes journal recovery first, and
expires abandoned previews with an explicit message. The release configuration
uses a stable extension identity, installs `.deb` host manifests, and ships the
CLI/host companions needed for AppImage per-user setup.

### Backend

- Implement `installed_mods` from installations, releases, mods, and active state.
- Implement `check_updates` with provider refresh, preserving verbatim versions
  and ordering only within a mod lineage.
- Persist download jobs and resume queued/running/paused jobs by re-resolving
  signed URLs.
- Persist Native Messaging requests in `inbox_requests`; notify or launch the
  desktop application and route the user to file selection, plan, or approval.
- Check interrupted operations during `AppState::start`; route to Recovery before
  normal navigation when action is required.
- Persist prepared-plan metadata or deliberately expire it with a clear restart
  message and staging cleanup.
- Fix Native Messaging packaging: generate the real allowed extension origin,
  install manifests into supported browser discovery locations for `.deb`, and
  implement an AppImage per-user setup command.

### Desktop

- Add an Add Mod/inbox flow that reaches `fetch_mod` and `prepare_install`.
- Make Installed, Updates, and Downloads real list views with empty, loading,
  retry, and offline states.
- Expose verify, removal, and ownership from installed-mod rows.
- Show queued requests that still need file selection or conflict decisions.

### Exit criteria

- A fresh user can authenticate, register Cyberpunk 2077, request a Nexus mod
  from either the desktop or extension, approve the plan, see it in Installed,
  verify it, remove it, and restore overwritten content.
- Restarting during download or a journaled mutation produces an actionable UI.
- No command used by a primary screen returns a hard-coded empty result.

## Milestone 1: desired-state reconciliation foundation

**Purpose:** make enabling, disabling, switching profiles, compatible updates,
and clean restoration use one safe mechanism.

### Work

- Apply migration `0003_desired_state.sql` and backfill mappings for existing
  installations from their stored plans/archive entries where possible.
- Introduce `DesiredGameState` and `MutationPlan`.
- Build a reconciler that compares current active installations with desired
  installations and produces final per-path provider stacks.
- Generalize staging and commit to cover multiple installations in one operation.
- Preserve explicit conflict decisions and surface newly introduced cross-mod
  conflicts before any writes.
- Add deactivation and reactivation. Reactivation re-extracts the content-addressed
  archive, validates hashes against recorded mappings, then stages it.
- Make rollback restore both filesystem state and active/profile database state.
- Add CLI commands `plan-state`, `apply-state`, `enable`, and `disable` for headless
  testing before UI integration.

### Exit criteria

- A two-mod update or enable/disable set is one journaled operation.
- Failure on any staged write or rename restores the complete previous state.
- Disabled mods remain available for reactivation without a network request.
- Existing single-mod workflows are implemented as one-item reconciliations or
  remain behaviorally identical.

## Milestone 2: initial game baseline and clean-state restoration

**Purpose:** know what "clean" means for this specific installation and game
build before Onera starts changing it.

### Steam identity

Extend Steam discovery to retain:

- the `appmanifest_<appid>.acf` path;
- AppID and BuildID;
- branch/beta key when present; and
- installed depot IDs and manifest IDs when present.

Store these values as a best-effort build identity. Steam's official docs state
that depot manifests contain file paths, sizes, flags, and SHA-1 hashes, but the
consumer Steam client does not expose a documented public API for obtaining the
complete expected manifest. Therefore the first implementation must not depend
on scraping Steam internals or asking for Steam credentials.

Add a future-facing `GameManifestProvider` port so an authoritative manifest can
replace local capture if Steam exposes a supported consumer API later.

### Baseline capture flow

1. Require no active Onera mods. If necessary, preview and apply a reconcile to
   the empty desired state.
2. For Steam games, instruct the user to run Steam's Verify Installed Files and
   explicitly confirm completion.
3. Read and display the detected build/depot identity.
4. Scan only adapter-declared store-managed roots. Exclude saves, logs, caches,
   shader caches, generated configuration, and Proton user-data roots.
5. Reject symlinks and special files from the trusted baseline and report them.
6. Hash every included file with BLAKE3 and persist an immutable baseline.
7. Show a capture summary and make it the current baseline.

The capture is a local observation stamped with Steam build identity—not a claim
that Steam independently attested every byte.

### Baseline status and stale detection

- Compare the current appmanifest build/depot identity at startup and before an
  install. If it changed, mark the baseline stale and prompt for store verification
  and recapture.
- A quick scan may use size/mtime caching for responsiveness, but a result labeled
  `clean` must come from content hashing.
- Classify findings as `matching`, `modified`, `missing`, `extra_managed`,
  `extra_unknown`, and `unreadable`.

### Return-to-clean flow

1. Preview reconciliation to an empty active mod set.
2. Restore Onera's bottom-of-stack unmanaged backups and delete only files Onera
   introduced.
3. Hash the baseline scope again.
4. If baseline files are missing or modified and Onera has no trusted backup,
   report them as requiring Steam repair; do not synthesize or delete content.
5. Report unknown extras separately and require individual user decisions.

### UI

- Add a Game Integrity/Baseline panel to each registered game.
- During first game setup, recommend baseline capture before the first install.
- Display source, build identity, capture time, freshness, and scan findings.
- Actions: Capture, Verify against baseline, Return to clean, and Replace stale
  baseline.

### Exit criteria

- A verified Steam installation can be captured and later matched byte-for-byte.
- A Steam BuildID/depot change makes the prior baseline visibly stale.
- Returning to clean removes active Onera mods and restores every original Onera
  backed up, while never silently deleting an unknown file.
- Manual/non-Steam games can use a clearly labeled local-snapshot baseline.

## Milestone 3: mod profiles

**Purpose:** let users maintain and safely switch between reusable mod sets.

### Domain and persistence

- Create the Default profile when confirming a game and import currently active
  mods during migration.
- Implement create, rename, duplicate, and delete. The active profile cannot be
  deleted until another profile is activated.
- Implement add, remove, enable, disable, pin/unpin, and reorder members.
- Adding/removing a profile member changes desired state only; the game directory
  changes when the user activates/applies the profile.
- A profile may reference an artifact that is not downloaded yet. Activation
  includes required downloads in its preview.

### Activation

- Resolve desired member versions, then generate a `MutationPlan`.
- Preview downloads, activations, deactivations, upgrades/downgrades, restorations,
  file conflicts, dependency warnings, byte totals, and baseline freshness.
- Apply atomically and mark the new profile active only after verification.
- Keep the old profile active if preparation, commit, or verification fails.

### UI and CLI

- Add a Profiles section scoped to the selected game.
- Provide profile cards plus a member table with enabled, pinned, version,
  dependency health, download state, and priority.
- Support duplicate-as-starting-point and confirmation before deleting a profile.
- CLI: `profiles list/create/delete/show`, `profiles add/remove/enable/disable`,
  `profiles plan-activate`, and `profiles activate`.

### Exit criteria

- Users can create and remove profiles and add/remove/reorder mods in them.
- Switching between two profiles restores shared, covered, and baseline files
  correctly in both directions.
- Restarting during a switch offers rollback and never reports the target profile
  active until the filesystem matches it.

## Milestone 4: dependency-aware planning and compatible updates

**Purpose:** prevent known-bad combinations and offer a safe path to a compatible
profile state.

### Nexus adapter

The checked-in Nexus OpenAPI document exposes experimental file-version
dependency endpoints, including materialized version candidates and DLC groups.
Implement them behind the provider port rather than leaking their schemas into
the core.

- Map Nexus version IDs, file/update-chain IDs, positions, AND dependency groups,
  OR candidates, and DLC alternatives into core types.
- Prefer the non-deprecated materialized batch endpoint for whole-profile checks.
- Paginate until complete, bound response sizes, reuse retry/rate-limit handling,
  and cache snapshots with a TTL.
- Preserve raw JSON and a canonical dependency fingerprint for diagnostics and
  override invalidation.
- If an experimental endpoint fails or disappears, return `Unavailable`; do not
  misrepresent it as a dependency-free mod.

### Solver

Build a deterministic constraint solver in a pure `onera-resolver` crate or an
equivalent pure core module.

Inputs:

- enabled profile members and their selected/pinned versions;
- installed and remotely available candidate versions;
- AND/OR dependency groups and known DLC ownership;
- ignored-definition fingerprints; and
- candidate availability/status.

Hard constraints:

- one selected version per enabled mod/file group;
- every non-ignored dependency group has at least one selected candidate;
- pinned members cannot change;
- candidates must target the same game and a visible/downloadable release;
- known missing DLC cannot be treated as satisfied.

Preference order:

1. keep the current compatible version;
2. avoid disabling a mod;
3. minimize the number of changed mods and downloads;
4. prefer newer provider positions within the same file group; and
5. use stable provider IDs as the final tie-breaker.

Never compare free-form version strings.

Results:

- `Compatible`: no action required;
- `InstallMissing`: add dependency candidates;
- `UpdateSet`: a compatible set of upgrades/downgrades;
- `DisableSet`: minimal members that can be disabled to make the remainder valid;
- `Unsatisfied`: no solution under pins/availability;
- `Unknown`: provider data or DLC ownership is unavailable.

Handle cycles as a graph, not recursion without guards. A cycle is valid when all
members can be selected simultaneously; otherwise it contributes to an
unsatisfied explanation.

### User flows

Run a dependency check when:

- adding or enabling a profile member;
- preparing an install or profile activation;
- checking or applying updates;
- changing a pin; and
- provider dependency data has changed.

When requirements are not met, offer only plans that were actually solved:

- Install missing requirements.
- Update/downgrade all affected mods to the proposed compatible versions.
- Disable the proposed conflicting members in this profile.
- Change pins and solve again.
- Ignore named requirements at the user's risk.
- Cancel without changing desired or active state.

The confirmation view must explain which source mod requires what, which selected
candidate satisfies it, why a mod would be downgraded/disabled, and whether the
information is stale or unavailable.

"Update all compatible" means solve the entire enabled profile, not update each
mod independently. It produces one reconciliation preview and one journaled
operation after all downloads are staged.

File conflicts remain distinct from declared dependency incompatibilities. The
same preview may contain both, but ignoring a dependency never silently chooses
a winner for a path conflict.

### Exit criteria

- A missing known dependency blocks apply and produces an actionable prompt.
- The solver finds compatible multi-mod upgrades/downgrades where they exist.
- Pins are honored, disable suggestions are minimal and explained, and ignored
  constraints remain visibly risky.
- Changed dependency metadata invalidates the relevant ignore decision.
- Offline operation uses labeled cached data and never calls stale data current.

## Milestone 5: hardening and release readiness

- Add at least one more game adapter to prove profile/baseline behavior is not
  Cyberpunk-specific.
- Exercise a real RAR fixture.
- Add injected SQLite failures around journal transitions and profile activation.
- Fault-inject interrupted baseline scans and multi-mod reconciliations.
- Add a recorded live-Nexus compatibility smoke test that is opt-in and requires
  no secret in normal CI.
- Run a compiled Tauri smoke suite for install, profile switch, dependency prompt,
  and recovery; retain bridge-stub Playwright tests for fast UI coverage.
- Add package-install tests for `.deb` Native Messaging registration and AppImage
  setup.
- Add database backup/restore and migration rollback documentation.
- Update the threat model for provider metadata poisoning, dependency confusion,
  stale baseline identity, and profile-switch rollback.

## Test matrix

### Dependency solver

- satisfied and missing single dependencies;
- independent AND groups and OR alternatives;
- cycles, self-dependencies, duplicate candidates, and empty groups;
- incompatible pins and no-solution explanations;
- deterministic selection and minimal-change preference;
- compatible upgrade requiring a dependency downgrade;
- minimal disable set;
- missing/unknown DLC ownership;
- stale cache and provider-unavailable distinction;
- ignore override scoping and fingerprint invalidation.

Use property tests for determinism, termination on cyclic graphs, and the rule
that every reported `Compatible`/`UpdateSet` result satisfies every hard
constraint.

### Profiles and reconciliation

- CRUD, name uniqueness per game, active-profile deletion guard;
- add/remove/enable/disable/pin/reorder;
- activate from empty, switch A -> B -> A, and duplicate profile;
- shared identical files and explicit priority;
- cross-mod conflict decisions;
- changed-on-disk files blocking deactivation;
- activation with missing cached archive;
- rollback after staged-write, rename, database, and verification failures;
- restart during every non-terminal state;
- active profile changes only after successful verification.

### Baselines

- native Steam, Flatpak Steam, and manual installs;
- BuildID/depot identity parsing and stale detection;
- exclusions for user data, caches, symlinks, and special files;
- clean, modified, missing, managed extra, and unknown extra classifications;
- exact restoration from unmanaged backups;
- no deletion of unknown files;
- cancellation and restart of large scans;
- game update followed by recapture while preserving baseline history.

### End to end

Cover this scenario through `onera-app`, CLI, and desktop UI:

1. register a game and capture its baseline;
2. create profiles A and B;
3. add a mod with a missing dependency to A;
4. accept the compatible install set;
5. switch to B, disabling one conflict;
6. update all of B to a solved compatible set;
7. interrupt and roll back a switch;
8. return to clean and verify the baseline;
9. change the mocked Steam BuildID and confirm the baseline becomes stale.

## Delivery order and estimates

Approximate engineering effort for one experienced developer, excluding product
design review and live-provider delays:

| Milestone                              | Depends on           |  Estimate |
| -------------------------------------- | -------------------- | --------: |
| 0. Current vertical slice              | current code         | 3-5 weeks |
| 1. Desired-state reconciliation        | 0                    | 4-6 weeks |
| 2. Baseline and clean state            | 1                    | 3-5 weeks |
| 3. Profiles                            | 1; integrates with 2 | 3-5 weeks |
| 4. Dependencies and compatible updates | 1 and 3              | 5-8 weeks |
| 5. Hardening                           | all                  | 3-5 weeks |

Milestones 2 and the profile CRUD portion of 3 can proceed in parallel after
Milestone 1. Total expected effort is roughly 21-34 engineer-weeks. Keep each
milestone releasable behind capability flags rather than maintaining one long
feature branch.

## Recommended first implementation slice

The first PR sequence should be small enough to review independently:

1. Add v1-to-v2 migration fixtures and installed-mod read queries.
2. Persist download jobs and browser inbox requests.
3. Complete the desktop Add Mod -> Installed lifecycle.
4. Add provider version/file-group identifiers without dependency behavior.
5. Add installation mappings and deactivation without purge.
6. Introduce one-item `MutationPlan`, then generalize it to multi-mod state.
7. Implement Steam build-identity parsing and local baseline capture.
8. Add Default profile CRUD and activation on top of the reconciler.
9. Add Nexus dependency ingestion and cache.
10. Land the pure solver, then expose compatible update/disable/ignore choices.

Do not begin with the solver UI. The solver only becomes useful after the app can
persist provider version identity, represent disabled artifacts, and atomically
apply a multi-mod desired state.

## External API assumptions to validate during implementation

- The checked-in `nexus_openapi.yaml` marks dependency endpoints experimental.
  Add contract fixtures for the exact version deployed when work begins and make
  capability loss non-fatal.
- Steam documents depot manifests as containing file metadata and SHA-1 hashes,
  and identifies each depot manifest and application build separately. It does
  not document a consumer API that Onera can rely on to download the complete
  expected manifest. Treat local baseline capture plus BuildID/depot identity as
  the supported first release.
- Revisit these assumptions at the start of Milestones 2 and 4, and record the
  validated behavior in `docs/nexus-api-assumptions.md` and a new
  `docs/steam-baseline-assumptions.md`.

References:

- `nexus_openapi.yaml`
- <https://partner.steamgames.com/doc/store/application/builds>
- <https://partner.steamgames.com/doc/store/application/depots>
- <https://partner.steamgames.com/doc/sdk/uploading>
