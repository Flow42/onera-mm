# Baseline, profile and dependency contracts

The request and response shapes for Milestones 2–4, fixed **before** the
backends behind them exist so frontend work can be built and tested against
stable mocks.

Nothing in this document is implemented yet. The domain types it describes are
(`onera_core::domain::{baseline, profile, dependency}`), and every payload here
is the serialization of one of them, so a mock written against this file and the
eventual real command return the same JSON.

## Conventions

Unchanged from the Milestone 0–1 commands:

- Tauri command names are `snake_case`; arguments are passed from the frontend
  as `camelCase` and JSON payload fields are `snake_case`.
- Identifiers cross the boundary as strings. UUIDs are hyphenated; provider and
  store identifiers are opaque and must never be parsed by the frontend.
- Errors are `{ "code": "...", "message": "..." }` with the codes already listed
  in `apps/desktop/src-tauri/src/state.rs`. Two of the existing ones matter
  especially here: `not_found` and `decision_required`.
- One code below does not exist yet: `conflict`, for a precondition that a
  re-plan would fix — deleting the active profile, applying a stale preview.
  `CoreError::Conflict` currently falls through to `internal`, so whoever
  implements the first command that raises it adds
  `E::Conflict(_) => "conflict"` to that mapping. Mocks should use `conflict`
  from the start.
- Enumerations serialize as `snake_case` strings. Enumerations that carry data
  are internally tagged with `"kind"`.
- Long operations emit the existing progress events and take the existing
  cancellation path. Nothing below introduces a second mechanism.

### The rule every one of these shapes exists to enforce

**Unknown is not empty.** A frontend that renders a missing answer as "nothing
required" or "nothing changed" is a bug, not a simplification:

| Field                               | Never render as         |
| ----------------------------------- | ----------------------- |
| `availability.kind = "unavailable"` | "no dependencies"       |
| `availability.kind = "unsupported"` | "no dependencies"       |
| `health = "unknown"`                | a satisfied tick        |
| `freshness.kind = "unknown"`        | "baseline is fresh"     |
| `dlc_ownership = "unknown"`         | owned                   |
| `outcome.kind = "unknown"`          | "compatible", or a plan |

Each of those has its own visual state. The plan-view tests already establish
the precedent: an unrecognized classification fails safe by demanding a
decision.

## Baseline commands

### `baseline_status`

`{ gameId }` → the panel's whole model. Returns `null` for `baseline` when the
game has never been captured; `freshness.kind` is then `"none"`.

```json
{
  "baseline": {
    "id": "3f2b…",
    "local_game_id": "9a1c…",
    "source": "store_verified_capture",
    "build_identity": {
      "store": "steam",
      "app_id": "1091500",
      "build_id": "18234000",
      "branch": null,
      "depots": [{ "depot_id": "1091501", "manifest_id": "77…" }],
      "manifest_path": "/games/steamapps/appmanifest_1091500.acf",
      "observed_at": "2026-09-01T10:00:00Z"
    },
    "adapter_id": "cyberpunk2077",
    "reported_version": "2.21",
    "status": "current",
    "captured_at": "2026-09-01T10:04:12Z",
    "scope_fingerprint": "b3…",
    "file_count": 41233,
    "total_bytes": 71234567890
  },
  "freshness": { "kind": "fresh" },
  "observed_build_identity": { "…": "as above, read now" },
  "active_mod_count": 3,
  "capture_blocked_reason": null
}
```

`source` is one of `store_verified_capture`, `local_snapshot`,
`store_manifest`. A `local_snapshot` must be labelled as such in the UI — it is
what a manual or non-Steam install gets, and it proves only that the files have
not changed since Onera looked, not that they were ever correct.

`freshness.kind` is one of:

| `kind`    | Extra fields           | Panel state                                     |
| --------- | ---------------------- | ----------------------------------------------- |
| `none`    | —                      | offer Capture                                   |
| `fresh`   | —                      | normal                                          |
| `stale`   | `captured`, `observed` | warn; offer Replace stale baseline              |
| `unknown` | `reason`               | show "cannot be verified", offer Capture anyway |

`capture_blocked_reason` is non-null when capture cannot start — the only
current reason is active Onera mods, which the user resolves by reconciling to
the empty desired state first.

### `plan_baseline_capture` / `capture_baseline`

`{ gameId, source, storeVerificationConfirmed }` → a capture preview and then
the capture itself. `storeVerificationConfirmed` is the explicit acknowledgement
that the user ran the store's own file verification; `capture_baseline` returns
`decision_required` without it when `source` is `store_verified_capture`.

The preview reports what will be scanned, so the scope is visible before a long
hash run:

```json
{
  "roots": [{ "key": "game", "kind": "game_install", "path": "/games/…" }],
  "exclusions": [
    {
      "root_key": "game",
      "pattern": { "kind": "prefix", "path": "r6/cache" },
      "reason": "cache",
      "note": "Redscript recompiles this on launch"
    }
  ],
  "estimated_files": 41233,
  "estimated_bytes": 71234567890
}
```

`capture_baseline` returns the `baseline` object above.

### `verify_baseline`

`{ gameId, quick }` → a `BaselineVerification`.

```json
{
  "baseline_id": "3f2b…",
  "scan_run_id": "77a…",
  "state": "completed",
  "evidence": "content_hashed",
  "scope_fingerprint": "b3…",
  "counts": {
    "matching": 41230,
    "modified": 1,
    "missing": 0,
    "extra_managed": 2,
    "extra_unknown": 1,
    "unreadable": 0,
    "special": 0
  },
  "findings": [
    {
      "root_key": "game",
      "path": "r6/scripts/thing.reds",
      "classification": "extra_unknown",
      "expected": null,
      "observed": "blake3:…",
      "detail": null
    }
  ],
  "verified_at": "2026-09-02T09:12:00Z"
}
```

`quick: true` returns `evidence: "metadata_only"`. **A metadata-only result may
never be shown as clean.** Clean requires all four of `state: "completed"`,
`evidence: "content_hashed"`, a `scope_fingerprint` equal to the baseline's, and
no non-matching counts — exactly what `BaselineVerification::is_clean` checks.

`classification` is one of `matching`, `modified`, `missing`, `extra_managed`,
`extra_unknown`, `unreadable`, `special_file`. The last three always require an
individual user decision and are never acted on automatically.

### `plan_return_to_clean` / `apply_return_to_clean`

`{ gameId }` → a `MutationPlan` (the Milestone 1 shape, unchanged) plus the
baseline context:

```json
{
  "plan": { "…": "MutationPlan" },
  "restorable": [{ "root_key": "game", "path": "…", "from": "backup" }],
  "needs_store_repair": [{ "root_key": "game", "path": "…", "classification": "modified" }],
  "unknown_extras": [{ "root_key": "game", "path": "…" }]
}
```

`needs_store_repair` is reported, never repaired: Onera restores bytes it backed
up and deletes files it deployed, and hands everything else back. `unknown_extras`
are never deleted, with or without confirmation, by this command.

## Profile commands

### `profiles`

`{ gameId }` → every profile for the game. Exactly one has `is_active: true`.

```json
[
  {
    "id": "11…",
    "local_game_id": "9a1c…",
    "name": "Default",
    "description": null,
    "is_active": true,
    "created_at": "2026-08-01T12:00:00Z",
    "updated_at": "2026-09-01T18:22:00Z"
  }
]
```

### `profile_members`

`{ profileId }` → the member table, in priority order (lowest first, i.e. the
bottom of the provider stack first).

```json
[
  {
    "id": "22…",
    "profile_id": "11…",
    "mod_id": "33…",
    "selection": {
      "provider": "nexus",
      "provider_mod_id": "107",
      "provider_file_id": "9001",
      "provider_version_id": "v-9001",
      "provider_file_group_id": "g-107"
    },
    "installation_id": "44…",
    "desired": "enabled",
    "pin": {
      "kind": "pinned",
      "pinned_at": "2026-08-20T09:00:00Z",
      "reason": "known-good with my save"
    },
    "priority": 10,
    "added_at": "2026-08-01T12:30:00Z"
  }
]
```

`installation_id: null` means the artifact is not downloaded. That member is a
download in the activation preview, not an omission.

`pin.kind` is `unpinned` or `pinned`. `priority` is a signed integer, not a list
index: inserting between two members does not renumber the profile.

### Mutating commands

All of these change desired state only and return the updated object. None of
them touches the game directory.

| Command                  | Arguments                                            |
| ------------------------ | ---------------------------------------------------- |
| `create_profile`         | `{ gameId, name, description?, copyFromProfileId? }` |
| `rename_profile`         | `{ profileId, name }`                                |
| `delete_profile`         | `{ profileId }`                                      |
| `add_profile_member`     | `{ profileId, modId, providerFileId? }`              |
| `remove_profile_member`  | `{ memberId }`                                       |
| `set_member_state`       | `{ memberId, desired }` — `enabled` \| `disabled`    |
| `set_member_pin`         | `{ memberId, pinned, reason? }`                      |
| `reorder_profile_member` | `{ memberId, priority }`                             |

`create_profile` with `copyFromProfileId` is duplicate-as-starting-point.
`delete_profile` on the active profile returns `conflict`; another profile must
be activated first, so a game is never left without one.

### `plan_profile_activation` / `activate_profile`

`{ profileId }` → one preview covering everything the switch entails.

```json
{
  "from_profile_id": "11…",
  "to_profile_id": "12…",
  "plan": { "…": "MutationPlan" },
  "downloads": [{ "member_id": "22…", "name": "…", "bytes": 41234567 }],
  "dependency": { "…": "the resolve_dependencies payload below" },
  "baseline_freshness": { "kind": "fresh" },
  "bytes_to_write": 91234567,
  "ready": false,
  "blockers": [
    { "kind": "cross_mod_conflict", "target": "game:archive/pc/mod/a.archive" },
    { "kind": "dependency_unsatisfied", "member_id": "22…" }
  ]
}
```

`ready` is false whenever `blockers` is non-empty; `activate_profile` on a plan
that is not ready returns `decision_required`. Cross-mod conflicts are resolved
with the existing `decide` command and are **separate from dependency
problems** — accepting a dependency risk never picks a winner for a path
conflict.

`activate_profile` returns the activation record. `state` is one of `preparing`,
`applying`, `applied`, `rolled_back`, `failed`; the target profile is reported
active only in `applied`, which is reached after filesystem verification.

## Dependency commands

### `resolve_dependencies`

`{ profileId }`, or `{ profileId, previewMembers }` when checking a change the
user has not committed yet → a `ResolutionResult`.

```json
{
  "outcome": {
    "kind": "update_set",
    "select": [
      {
        "provider": "nexus",
        "provider_mod_id": "107",
        "provider_file_id": "9002",
        "provider_version_id": "v-9002",
        "provider_file_group_id": "g-107",
        "profile_member_id": "22…"
      }
    ],
    "install": []
  },
  "health": [
    {
      "profile_member_id": "22…",
      "health": "unsatisfied",
      "unsatisfied": [
        {
          "source": {
            "provider": "nexus",
            "game_slug": "cyberpunk2077",
            "provider_mod_id": "107",
            "provider_file_id": "9001",
            "provider_version_id": "v-9001"
          },
          "group_id": "55…",
          "label": "Cyber Engine Tweaks",
          "explanation": "no available candidate targets this game"
        }
      ]
    }
  ],
  "evidence": {
    "fresh": 12,
    "cached": 3,
    "stale": 1,
    "unavailable": 0,
    "unsupported": 0,
    "unknown_dlc": 0
  }
}
```

`outcome.kind` is one of `compatible`, `install_missing`, `update_set`,
`disable_set`, `unsatisfied`, `unknown`. Only the middle three carry a plan the
user can accept; offer an action **only** for an outcome that was actually
solved.

`health` is one of `satisfied`, `unsatisfied`, `ignored`, `not_applicable`,
`unknown`. `unknown` and `unsatisfied` block apply.

`evidence` drives the disclosure banner. Any non-zero `stale`, `unavailable` or
`unknown_dlc` means the answer rests on incomplete data and must say so; offline
operation shows labelled cached data and never calls it current.

### `dependency_snapshot`

`{ modId, providerFileId }` → the raw requirement list for the detail view.

```json
{
  "id": "66…",
  "source": { "…": "as in health.unsatisfied[].source" },
  "availability": { "kind": "cached", "fetched_at": "2026-09-01T08:00:00Z", "stale": true },
  "groups": [
    {
      "id": "55…",
      "provider_group_key": "req-1",
      "label": "Cyber Engine Tweaks",
      "kind": "required",
      "candidates": [
        {
          "provider": "nexus",
          "game_slug": "cyberpunk2077",
          "provider_mod_id": "107",
          "provider_file_id": "9001",
          "provider_version_id": "v-9001",
          "provider_file_group_id": "g-107",
          "position": 12,
          "status": "available",
          "display_name": "CET 1.35.0"
        }
      ]
    }
  ],
  "dlc": [{ "id": "56…", "label": "Phantom Liberty", "alternatives": ["1091501"] }],
  "provider_revision": null,
  "fingerprint": "b3…",
  "fetched_at": "2026-09-01T08:00:00Z",
  "raw": null
}
```

`availability.kind` is `fetched`, `cached`, `unsupported` or `unavailable`.
**Only `fetched` and `cached` make an empty `groups` array mean "this mod
requires nothing."** Groups are ANDed; the candidates inside one are ORed. An
empty `candidates` array is a requirement nothing can satisfy, not a satisfied
one.

`kind` on a group is `required`, `recommended` or `incompatible`; `recommended`
is advisory and never blocks. `position` is the provider's own ordering within
`provider_file_group_id` — the only ordering that exists. There is no version
string to compare, and the frontend must not invent one.

`raw` is diagnostic and is not rendered.

### `set_dependency_override` / `clear_dependency_override`

`{ memberId, groupId, fingerprint, reason }` → records that the user accepted a
named risk. `reason` is required: ignoring a requirement is always an explicit,
attributable decision.

The override is scoped to the member _and_ the `fingerprint` shown when it was
accepted. When the provider's definition changes, the fingerprint changes, the
override stops applying, and the requirement resurfaces. The frontend must send
back the fingerprint it displayed rather than a fresh one, so accepting a risk
cannot silently cover a requirement the user never saw.

## CLI equivalents

Same application methods, same shapes. `--json` prints the payloads above
verbatim, which is how these contracts stay honest without a compiled desktop
binary.

```sh
onera baseline status   --game <local-game-id>
onera baseline capture  --game <local-game-id> --verified
onera baseline verify   --game <local-game-id> [--quick]
onera baseline clean    --game <local-game-id> [--apply]

onera profiles list     --game <local-game-id>
onera profiles create   --game <local-game-id> --name <name> [--from <profile-id>]
onera profiles delete   --profile <profile-id>
onera profiles show     --profile <profile-id>
onera profiles add      --profile <profile-id> --mod <mod-id> [--file <provider-file-id>]
onera profiles remove   --member <member-id>
onera profiles enable   --member <member-id>
onera profiles disable  --member <member-id>
onera profiles pin      --member <member-id> [--reason <text>]
onera profiles reorder  --member <member-id> --priority <int>
onera profiles plan-activate --profile <profile-id>
onera profiles activate      --profile <profile-id>

onera deps check    --profile <profile-id>
onera deps show     --mod <mod-id> --file <provider-file-id>
onera deps ignore   --member <member-id> --group <group-id> --fingerprint <hex> --reason <text>
```

The existing `plan-state`, `apply-state`, `enable` and `disable` commands are
unchanged and remain the lowest-level way to drive a desired state.
