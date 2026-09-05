# Packaging

Onera ships as an AppImage and a `.deb`, both built by Tauri's bundler.

## What a package contains

| Component                  | Installed by `.deb` at                                                |
| -------------------------- | --------------------------------------------------------------------- |
| `onera-desktop`            | `/usr/bin/onera-desktop`                                              |
| `onera-nmhost`             | `/usr/lib/onera/onera-nmhost`                                         |
| Native Messaging manifests | Chromium, Chrome, and Brave system discovery directories under `/etc` |
| Reference host manifest    | `/usr/share/onera/native-messaging/com.onera.host.json`               |
| Desktop entry and icons    | `/usr/share/applications`, `/usr/share/icons`                         |

The CLI (`onera`) is built separately and is not currently part of the desktop
bundle.

## Build prerequisites

```sh
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
                 librsvg2-dev patchelf p7zip-full
```

## Building

```sh
cargo build --release -p onera-nmhost -p onera-cli   # the host must exist first
pnpm install
pnpm --filter onera-desktop build                    # frontend
pnpm tauri build                                     # bundles deb + AppImage
```

Artifacts land in `apps/desktop/src-tauri/target/release/bundle/`.

The `beforeBuildCommand` in `tauri.conf.json` rebuilds the frontend, so the
explicit `pnpm --filter onera-desktop build` is only needed when building the
frontend on its own.

## Runtime dependencies

The `.deb` declares:

| Package                             | Why                                    |
| ----------------------------------- | -------------------------------------- |
| `libwebkit2gtk-4.1-0`, `libgtk-3-0` | The Tauri webview                      |
| `p7zip-full`                        | `7z`/`7zz`, for 7-Zip and RAR archives |

7-Zip is a **runtime** dependency only for those two formats. Zip and the tar
variants are handled by pure-Rust code, so a user who never downloads a `.7z`
mod does not need it. When it is missing, Onera reports
`CoreError::Unsupported` naming the package rather than failing obscurely.

A Secret Service implementation (GNOME Keyring, KWallet, KeePassXC with Secret
Service enabled) is required but is not declared as a dependency, because which
one a user has depends on their desktop.

## AppImage notes

An AppImage cannot install a Native Messaging manifest into the user's browser
config, and its temporary mount path changes each run. Releases therefore ship
the CLI and Native Messaging host alongside the AppImage. Put both executables
in a stable location, then run the per-user setup command:

```sh
chmod +x onera onera-nmhost
./onera browser setup --browser brave --host-path "$PWD/onera-nmhost"
```

Use `--browser chromium` or `--browser chrome` for those browsers. The command
creates the correct per-user directory and writes an absolute host path. It can
also print a manifest without writing it:

```sh
./onera browser manifest --host-path "$PWD/onera-nmhost"
```

AppImages also mount at a different path on every run, so an absolute `path` in
the manifest must point at an extracted location rather than inside the mount.

## Reproducibility

The release profile sets `lto = "thin"`, `codegen-units = 1` and strips debug
info. `Cargo.lock` and `pnpm-lock.yaml` are committed, so a given commit builds
the same dependency graph.

## Verifying a package

Two layers, because they catch different things.

**The declarations** are checked on every commit, by unit tests in `onera-cli`:
that the manifest this repository ships points at the path the `.deb` file map
installs the host binary to, that every browser directory named above is
registered from that one manifest, that the runtime dependencies are still
declared, and that per-user setup writes each browser its own directory with an
absolute host path. A mismatch there produces a package that installs cleanly
and silently never works — the browser reports only "host not found".

**The artifact** is checked at release time, because it needs a full build:

```sh
packaging/verify-package.sh onera_0.1.0_amd64.deb Onera_0.1.0_amd64.AppImage
```

It unpacks the `.deb` into a temporary directory — no root, nothing installed —
and confirms the bundler actually acted on those declarations: the host binary
is present and executable at the path the manifest names, every browser
directory holds a byte-identical copy of the manifest, and the dependencies are
declared. Given an AppImage it also runs `onera browser setup` against a scratch
`XDG_CONFIG_HOME` and checks the manifest it writes records an absolute path,
which is what makes the registration survive the AppImage remounting elsewhere.

For a manual look:

```sh
dpkg-deb --contents onera_0.1.0_amd64.deb
dpkg -I onera_0.1.0_amd64.deb              # check declared dependencies
./Onera_0.1.0_amd64.AppImage --appimage-extract-and-run --version
./onera browser manifest --host-path "$PWD/onera-nmhost"
```

Then run the manual smoke test in [`recovery.md`](recovery.md#manual-smoke-test)
against the installed binary.

## Upgrading and downgrading

The database is migrated forward on first launch of a new build, and there is no
automatic rollback. Take a backup before upgrading; see
[`database-maintenance.md`](database-maintenance.md).
