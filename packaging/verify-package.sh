#!/usr/bin/env bash
#
# Verify a built Onera package actually installs a working Native Messaging
# registration.
#
# The unit tests in `onera-cli` check that the *declarations* agree with each
# other — that the manifest names the path the `.deb` file map installs the host
# to. They cannot check that the bundler acted on those declarations. This does,
# against a real artifact, without needing root: the `.deb` is unpacked into a
# temporary directory rather than installed.
#
# Usage:
#   packaging/verify-package.sh <onera_*.deb> [Onera_*.AppImage]
#
# Exits non-zero on the first problem, naming it.

set -euo pipefail

DEB=${1:-}
APPIMAGE=${2:-}

if [[ -z $DEB ]]; then
    echo "usage: $0 <onera_*.deb> [Onera_*.AppImage]" >&2
    exit 2
fi

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

pass() { echo "  ok: $*"; }

command -v dpkg-deb >/dev/null || fail "dpkg-deb is required to verify a .deb"

[[ -f $DEB ]] || fail "$DEB does not exist"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

echo "Verifying $DEB"

# ---------------------------------------------------------------------------
# Contents
# ---------------------------------------------------------------------------

dpkg-deb --extract "$DEB" "$WORK/root"
dpkg-deb --info "$DEB" >"$WORK/control"

HOST_PATH=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['path'])" \
    packaging/com.onera.host.json)

# The manifest the package ships points at an absolute path. That exact path
# must exist in the package, or Native Messaging fails at runtime with nothing
# more informative than "host not found".
[[ -f "$WORK/root/$HOST_PATH" ]] || fail "the package does not install the host at $HOST_PATH"
pass "the Native Messaging host is installed at $HOST_PATH"

[[ -x "$WORK/root/$HOST_PATH" ]] || fail "$HOST_PATH is installed without the executable bit"
pass "the host is executable"

[[ -f "$WORK/root/usr/bin/onera-desktop" ]] || fail "the desktop binary is missing"
pass "the desktop binary is installed"

# ---------------------------------------------------------------------------
# Browser registration
# ---------------------------------------------------------------------------

for directory in \
    /etc/chromium/native-messaging-hosts \
    /etc/opt/chrome/native-messaging-hosts \
    /etc/brave/native-messaging-hosts \
    /usr/share/onera/native-messaging; do

    installed="$WORK/root$directory/com.onera.host.json"
    [[ -f $installed ]] || fail "no host manifest installed in $directory"

    # Every copy must be byte-identical to the one in the repository, so a
    # stale bundled copy cannot register a different extension.
    cmp -s "$installed" packaging/com.onera.host.json \
        || fail "$directory/com.onera.host.json differs from packaging/com.onera.host.json"

    # And it must resolve to the binary this same package installed.
    manifest_path=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['path'])" "$installed")
    [[ -f "$WORK/root$manifest_path" ]] \
        || fail "$directory manifest points at $manifest_path, which this package does not install"
done
pass "every browser directory registers the packaged host"

# ---------------------------------------------------------------------------
# The browser extension
# ---------------------------------------------------------------------------

# The extension has no build step, so the package ships the source directory
# and the user loads it unpacked. Every file listed in the bundler's `files`
# map has to arrive, in the layout `manifest.json` expects: it refers to
# `src/content.js`, so a flattened install would not load.
EXTENSION_ROOT=$WORK/root/usr/share/onera/extension
[[ -f $EXTENSION_ROOT/manifest.json ]] || fail "the package installs no extension manifest"

while read -r relative; do
    [[ -f "$EXTENSION_ROOT/$relative" ]] \
        || fail "the packaged extension is missing $relative"
done < <(cd extension && find . -type f -printf '%P\n')
pass "the browser extension is installed complete at /usr/share/onera/extension"

# A packaged extension whose id is not the one the host manifest allows would
# install cleanly and then refuse every message.
packaged_key=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1])).get('key',''))" \
    "$EXTENSION_ROOT/manifest.json")
repository_key=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1])).get('key',''))" \
    extension/manifest.json)
[[ -n $packaged_key ]] || fail "the packaged extension manifest has no key, so its id is random"
[[ $packaged_key == "$repository_key" ]] \
    || fail "the packaged extension key differs from the repository's, so its id will differ"
pass "the packaged extension keeps the id the host manifest allows"

# ---------------------------------------------------------------------------
# Declared dependencies
# ---------------------------------------------------------------------------

for package in libwebkit2gtk-4.1-0 libgtk-3-0 p7zip-full; do
    grep -q "$package" "$WORK/control" || fail "the package does not depend on $package"
done
pass "runtime dependencies are declared"

# ---------------------------------------------------------------------------
# AppImage
# ---------------------------------------------------------------------------

if [[ -n $APPIMAGE ]]; then
    echo "Verifying $APPIMAGE"
    [[ -f $APPIMAGE ]] || fail "$APPIMAGE does not exist"
    [[ -x $APPIMAGE ]] || fail "$APPIMAGE is not executable"

    # An AppImage cannot write into /etc, so the per-user setup command is the
    # only registration path it has. Run it against a scratch HOME and check
    # what it wrote, rather than touching the real configuration.
    if [[ -x ./target/release/onera ]]; then
        HOST=$WORK/onera-nmhost
        touch "$HOST"
        chmod +x "$HOST"

        DESKTOP=$WORK/onera-desktop
        touch "$DESKTOP"
        chmod +x "$DESKTOP"

        XDG_CONFIG_HOME="$WORK/config" ./target/release/onera browser setup \
            --browser brave --host-path "$HOST" --desktop-path "$DESKTOP" >/dev/null

        written="$WORK/config/BraveSoftware/Brave-Browser/NativeMessagingHosts/com.onera.host.json"
        [[ -f $written ]] || fail "browser setup wrote no manifest for Brave"

        # The mount point changes on every run, so a relative path here would
        # break on the next launch.
        written_path=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['path'])" "$written")
        [[ $written_path == /* ]] || fail "browser setup recorded the relative path $written_path"
        [[ $written_path == "$HOST" ]] || fail "browser setup recorded $written_path, not $HOST"
        pass "per-user setup registers an absolute host path"

        # Without this record an AppImage user's extension can see that Onera
        # is not running and still be unable to start it: the window is inside
        # a bundle whose name and mount point the launcher cannot guess.
        recorded=$WORK/config/onera/desktop-path
        [[ -f $recorded ]] || fail "browser setup recorded no desktop path"
        [[ "$(cat "$recorded")" == "$DESKTOP" ]] \
            || fail "browser setup recorded $(cat "$recorded"), not $DESKTOP"
        # The record names something Onera will execute.
        [[ "$(stat -c '%a' "$recorded")" == "600" ]] \
            || fail "the desktop-path record is not owner-only"
        pass "per-user setup records where the desktop application lives"
    else
        echo "  skipped: build ./target/release/onera to verify per-user setup"
    fi
fi

echo
echo "Package verification passed."
