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

```sh
dpkg-deb --contents onera_0.1.0_amd64.deb
dpkg -I onera_0.1.0_amd64.deb              # check declared dependencies
./Onera_0.1.0_amd64.AppImage --appimage-extract-and-run --version
./onera browser manifest --host-path "$PWD/onera-nmhost"
```

Then run the manual smoke test in [`recovery.md`](recovery.md#manual-smoke-test)
against the installed binary.
