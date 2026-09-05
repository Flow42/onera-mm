# Onera

A Linux-first, game-agnostic mod manager built around the Nexus Mods API v3.

Onera installs mods **transactionally**. Every write to a game directory is
journaled before it happens, every deployed file remembers who provided it and
what was there before, and nothing the user did not put there is ever
overwritten without being asked. A power cut in the middle of an install leaves
a recoverable state, not a broken game.

> **Status: early but end-to-end usable.** The initial product slice,
> desired-state reconciler, clean-game baselines, and reusable mod profiles are
> complete. Onera can capture and verify a local baseline, preview and atomically
> switch multi-mod profiles, retain disabled artifacts for offline reactivation,
> return a game to its recorded clean state, and recover the complete set after a
> failed write. Dependency-aware compatible planning is the next milestone.

## Why another mod manager

Most mod managers answer "what happens when two mods want the same file?" with a
load order and a shrug. Onera answers it with a **file-provider stack**: every
deployed path keeps an ordered record of everything that has ever provided it,
so removing a mod restores whatever it covered — another mod's file, an earlier
version of itself, or the game's own original from a backup.

Three rules follow from that and are enforced everywhere:

| Situation                                    | What Onera does |
| -------------------------------------------- | --------------- |
| A file Onera never installed is in the way   | Always asks     |
| A file Onera installed has since been edited | Always asks     |
| Another mod already owns the file            | Always asks     |

Nothing is overwritten silently, ever.

## Quick start

```sh
# Build the workspace and run every test. No network, no API key needed.
cargo test --workspace

# Frontend and extension tests.
pnpm install && pnpm test

# The CLI.
cargo run -p onera-cli -- --help
cargo run -p onera-cli -- auth login      # reads the key from stdin
cargo run -p onera-cli -- discover
# Preview/apply a retained multi-mod state. Repeat --enable/--disable as needed.
cargo run -p onera-cli -- plan-state --game <game-id> --enable <installation-id>
cargo run -p onera-cli -- apply-state --game <game-id> --enable <installation-id>
```

The full manual smoke test — discover, authenticate, download, install, verify,
remove, restore — is in [`docs/recovery.md`](docs/recovery.md#manual-smoke-test).

## Architecture at a glance

```text
 drivers                    core                        adapters
┌──────────────┐   ┌────────────────────────┐   ┌────────────────────────┐
│ Tauri window │   │ onera-core             │   │ onera-db      (SQLite) │
│ CLI          │──►│  domain, ports, plan   │◄──│ onera-archive (zip/7z) │
│ NM host      │   │ onera-install          │   │ onera-nexus   (API v3) │
│ (extension)  │   │  journal, engine       │   │ onera-download         │
└──────────────┘   │ onera-app  (wiring)    │   │ onera-games   (adapters)│
                   └────────────────────────┘   │ onera-discovery (Steam)│
                                                └────────────────────────┘
```

Tauri, the CLI and the browser extension are **thin adapters**. No filesystem,
installation or conflict logic lives in a frontend component, a Tauri command or
the extension. See [`docs/architecture.md`](docs/architecture.md).

## Documentation

| Document                                                         | What it covers                                                              |
| ---------------------------------------------------------------- | --------------------------------------------------------------------------- |
| [Architecture](docs/architecture.md)                             | Crate layout, ports, and the rules that keep adapters thin                  |
| [Threat model](docs/threat-model.md)                             | What Onera defends against and how                                          |
| [Database schema](docs/database-schema.md)                       | Every table and why it exists                                               |
| [Database maintenance](docs/database-maintenance.md)             | Backup, restore, migrations, and why there is no automatic rollback         |
| [Operation state machine](docs/operation-state-machine.md)       | The journal and its transitions                                             |
| [File-provider stack](docs/file-provider-stack.md)               | How restoration actually works                                              |
| [Game adapter guide](docs/game-adapter-guide.md)                 | Adding a game                                                               |
| [Provider guide](docs/provider-guide.md)                         | Adding a mod source                                                         |
| [Nexus API assumptions](docs/nexus-api-assumptions.md)           | What Onera relies on, and what breaks if it changes                         |
| [Steam baseline assumptions](docs/steam-baseline-assumptions.md) | Which Steam build identity is trustworthy, and which is best effort         |
| [Native Messaging setup](docs/native-messaging.md)               | Wiring the browser extension to the host                                    |
| [Packaging](docs/packaging.md)                                   | AppImage and `.deb`                                                         |
| [Recovery behaviour](docs/recovery.md)                           | What happens after a crash, plus the manual smoke test                      |
| [Frontend contracts](docs/frontend-contracts.md)                 | Baseline, profile and dependency request/response shapes, fixed in advance  |
| [Test strategy](docs/test-strategy.md)                           | What is tested, how, and what is deliberately not                           |
| [Implementation plan](docs/implementation-plan.md)               | Roadmap for product completion, profiles, dependencies, and clean baselines |

## Requirements

- Rust stable (1.85 or newer), Node 22+, pnpm 11+
- `p7zip` (`7zz` or `7z`) for 7-Zip and RAR archives — other formats need nothing
- A running Secret Service implementation (GNOME Keyring, KWallet, …). Onera
  stores the Nexus API key there and **will not fall back to plain text**.
- For the desktop app: `libwebkit2gtk-4.1`, `libgtk-3`

## Known gaps

The remaining roadmap work is deliberately still open:

- Dependency planning has domain types, provider/store ports, profile-facing
  contracts, and a pure solver boundary. Dependency persistence, Nexus ingestion,
  a working solver, compatible bulk updates, overrides, and actionable desktop
  flows remain to be implemented.
- Update checks report newer same-lineage releases, but compatible bulk update
  and dependency-aware update actions belong to the dependency milestone.
- Only Cyberpunk 2077 has a game adapter.
- The RAR path is implemented but untested against real RAR archives.

## Licence

MIT or Apache-2.0, at your option.
