# Architecture

Onera is a hexagonal ("ports and adapters") application. The core owns the
domain and the decisions; everything that touches the outside world — a
database, a network, a filesystem, a browser — is an adapter behind a trait.

## Crates

| Crate             | Responsibility                                                   | Depends on       |
| ----------------- | ---------------------------------------------------------------- | ---------------- |
| `onera-core`      | Domain types, ports, path safety, planning, progress, redaction  | nothing in Onera |
| `onera-db`        | SQLite persistence, migrations, repositories                     | `onera-core`     |
| `onera-archive`   | Safe inspection and extraction of zip, tar variants, 7z, rar     | `onera-core`     |
| `onera-install`   | Planner, journaled installer, rollback, verify, remove, recovery | `onera-core`     |
| `onera-discovery` | Steam library parsing, game matching                             | `onera-core`     |
| `onera-games`     | Game adapters (currently Cyberpunk 2077)                         | `onera-core`     |
| `onera-provider`  | Provider registry                                                | `onera-core`     |
| `onera-nexus`     | Nexus Mods API v3 client and personal-API-key auth               | `onera-core`     |
| `onera-download`  | Streaming downloader, content-addressed archive store            | `onera-core`     |
| `onera-app`       | Wiring: which database, which provider, which secret store       | all of the above |
| `onera-cli`       | Command-line driver                                              | `onera-app`      |
| `onera-nmhost`    | Native Messaging driver                                          | `onera-app`      |
| `onera-desktop`   | Tauri driver (outside the Cargo workspace)                       | `onera-app`      |

`onera-install` deliberately does **not** depend on `onera-db`. It talks to
`OperationJournal`, `DeploymentStore` and `BackupStore`, which SQLite happens to
implement. That is what makes "what happens if the database fails halfway
through a commit" a test rather than a hypothesis.

The Tauri crate sits outside the Cargo workspace on purpose: it needs
`webkit2gtk` and `libsoup`, and a CI job that only builds and tests the core
should not need a desktop stack installed.

## Ports

Declared in `onera_core::ports`:

| Port                                                   | Implemented by                  | Why it is a port                                         |
| ------------------------------------------------------ | ------------------------------- | -------------------------------------------------------- |
| `ModProvider`                                          | `onera-nexus`                   | A second provider is a second implementation, not a fork |
| `AuthProvider`                                         | `onera-nexus::ApiKeyAuth`       | Nexus SSO replaces this one type and nothing else        |
| `GameAdapter`                                          | `onera-games`                   | Adding a game must not touch the installer               |
| `ArchiveBackend`                                       | `onera-archive`                 | Lets the installer be tested without real archives       |
| `FileSystem`                                           | `onera-install::RealFileSystem` | Lets rename and write failures be injected               |
| `SecretStore`                                          | `onera-app::KeyringSecretStore` | Lets auth be tested without a D-Bus session              |
| `ArchiveStore`                                         | `onera-download`                | Content addressing is a policy, not a detail             |
| `OperationJournal` / `DeploymentStore` / `BackupStore` | `onera-db`                      | Crash recovery is testable without SQLite                |

Every port is object-safe and stored as `Arc<dyn Trait>`; a test in
`ports.rs` asserts that, because losing object safety would quietly force the
whole application to become generic over its adapters.

## Rules the code enforces

**No provider-specific identifier reaches the installation domain.** Providers
are addressed through `ProviderId`, `ProviderModId` and `ProviderFileId`, which
are opaque newtypes over `String`. Nothing in `onera-install` can ask "which
Nexus mod is this?" because the type does not carry the answer.

**No raw path crosses a boundary.** Anything derived from an archive or from a
message is a `RelPath` — normalized, relative, traversal-free by construction.
The only places that build absolute paths are the ones that own a root.

**Adapters are thin.** A Tauri command parses arguments, calls one `onera-app`
method and shapes the result. A CLI subcommand does the same. The browser
extension sends two strings. If a driver needs a decision, the decision belongs
in `onera-app` or deeper — which is why the CLI and the desktop application
cannot disagree about what an install does.

**Progress and cancellation are core concerns.** Long operations take a
`&dyn ProgressSink` and a `&CancelToken`. The CLI renders events as lines, Tauri
forwards them to the frontend, tests collect them into a vector, and the
`NullProgress` sink discards them. Cancellation is cooperative and is only
checked between journaled steps, so a cancelled operation is always in a state
recovery understands.

## Data flow for an install

```text
extension ─(game domain, mod id)─► NM host ─► durable inbox
                                                │
                                      desktop Add Mod view
                                                │
                          ┌─────────────────────┼─────────────────────┐
                          ▼                     ▼                     ▼
                   onera-nexus           onera-download        onera-archive
                  (metadata, files)      (stream + hash)      (inspect, extract)
                          │                     │                     │
                          └─────────────────────┴──────────┬──────────┘
                                                           ▼
                                                  onera-games adapter
                                                 (map onto deploy roots)
                                                           ▼
                                                  onera-install planner
                                                   (classify, dry run)
                                                           ▼
                                                    ── user approves ──
                                                           ▼
                                                  onera-install engine
                                              (journal → backup → stage →
                                               rename → verify → record)
```

Everything before "user approves" is read-only with respect to the game
directory. That is not a convention; the planner has no write capability.
