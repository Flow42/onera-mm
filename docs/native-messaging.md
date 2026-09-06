# Native Messaging setup

The browser extension does not talk to Onera directly. Chromium starts a
**Native Messaging host** — `onera-nmhost` — and pipes framed JSON to it on
stdin/stdout.

```text
extension service worker
      │  chrome.runtime.sendNativeMessage
      ▼
Native Messaging host (onera-nmhost)
      │  onera-app
      ▼
Onera core
```

## Installing the extension

The extension has no build step. Load it unpacked from whichever copy you have:

| Source       | Directory                                    |
| ------------ | -------------------------------------------- |
| `.deb`       | `/usr/share/onera/extension`                 |
| Release page | the unzipped `onera-extension-<version>.zip` |
| A checkout   | `extension/`                                 |

`chrome://extensions` → enable **Developer mode** → **Load unpacked** → choose
the directory. Reload after editing a file; there is nothing to rebuild.

The id is always `pohiidkpoflhifciokepgpaandghjgmj` because `manifest.json`
pins a `key`. Without that, an unpacked load would get a random id and the host
would refuse every message, since `allowed_origins` names exactly one.

## Installing the host manifest

Chromium finds a host by reading a manifest from a well-known directory. The
The `.deb` installs system manifests for Chromium, Chrome, and Brave. AppImage
releases use the per-user setup command because an AppImage cannot write browser
configuration and its mount path is not stable:

```sh
./onera browser setup --browser brave --host-path "$PWD/onera-nmhost"
```

**Brave (per user):**

```sh
mkdir -p ~/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts
cp packaging/com.onera.host.json \
   ~/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts/
```

Other Chromium browsers use the same layout under their own config directory:

| Browser  | Directory                                                     |
| -------- | ------------------------------------------------------------- |
| Brave    | `~/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts/` |
| Chromium | `~/.config/chromium/NativeMessagingHosts/`                    |
| Chrome   | `~/.config/google-chrome/NativeMessagingHosts/`               |

System-wide equivalents live under `/etc/opt/chrome/native-messaging-hosts/` and
`/etc/chromium/native-messaging-hosts/`.

## The manifest

```json
{
  "name": "com.onera.host",
  "description": "Onera mod manager native messaging host",
  "path": "/usr/lib/onera/onera-nmhost",
  "type": "stdio",
  "allowed_origins": ["chrome-extension://pohiidkpoflhifciokepgpaandghjgmj/"]
}
```

`path` must be **absolute**. For a development build:

```sh
cargo build --release -p onera-nmhost
sed -i "s|/usr/lib/onera/onera-nmhost|$PWD/target/release/onera-nmhost|" \
    ~/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts/com.onera.host.json
```

## Extension identity

The extension manifest contains a public key, so loading the unpacked
`extension/` directory always produces the stable id
`pohiidkpoflhifciokepgpaandghjgmj`. Packaged and CLI-generated host manifests
allow only that origin. Restart the browser after installing a host manifest;
Chromium caches host registration at startup.

`allowed_origins` is the only thing stopping any other extension from driving
Onera, so it must not be left as a wildcard.

## The protocol

Chromium's transport is a 32-bit **native-endian** length prefix followed by
UTF-8 JSON. On top of that Onera defines a versioned envelope:

```jsonc
// request
{ "v": 1, "id": "ext-m3k2-7", "type": "download_and_install",
  "game_domain": "cyberpunk2077", "mod_id": "107", "file_id": null }

// response
{ "v": 1, "id": "ext-m3k2-7", "status": "ok",
  "data": { "queued": true, "request_id": "…",
            "file_id": "100", "file_name": "Test Mod 1.0" } }

// error
{ "v": 1, "id": "ext-m3k2-7", "status": "error",
  "code": "selection_required", "message": "Test Mod offers 3 downloadable files" }
```

`code` is stable and machine-readable; `message` is display-only.

| Code                  | Meaning                                                        |
| --------------------- | -------------------------------------------------------------- |
| `malformed`           | Bad framing, bad JSON, or an identifier that failed validation |
| `unsupported_version` | Extension and host speak different protocol versions           |
| `not_authenticated`   | No API key stored; the user must finish onboarding             |
| `not_found`           | The mod, file or game does not exist                           |
| `selection_required`  | Several plausible files; the user must choose                  |
| `decision_required`   | Conflicts must be resolved in the desktop application          |
| `provider_error`      | Network or API failure                                         |
| `internal`            | Anything else                                                  |

### Commands

| Type                   | Fields                           | Does                                                                 |
| ---------------------- | -------------------------------- | -------------------------------------------------------------------- |
| `ping`                 | —                                | Liveness and version                                                 |
| `status`               | —                                | Authentication, registered games, and whether the window runs        |
| `app_state`            | —                                | Whether the desktop window is running, and whether it can be started |
| `launch_app`           | —                                | Starts the desktop window if it is not already running               |
| `mod_state`            | `game_domain`, `mod_id`          | What Onera already has for one mod                                   |
| `add_mod`              | `game_domain`, `mod_id`          | Fetches metadata and queues an Add Mod inbox item                    |
| `download`             | `+ file_id`, `page_url`, `grant` | Resolves the file and queues a durable desktop download              |
| `download_and_install` | `+ file_id`, `page_url`, `grant` | Resolves the file and queues an installation                         |

### Downloads the website authorised

Nexus issues a download location straight to a **premium** account. Every other
account gets one by pressing **Mod manager download** on the mod page, which
mints a `nxm://` link carrying a nonce (`key`) and an expiry (`expires`); the
same API endpoint accepts those and refuses without them. Onera cannot produce a
nonce — only a browser where the user pressed the button can — so this is the
one value that travels _inwards_ over this transport besides identifiers:

```jsonc
{
  "v": 1,
  "id": "ext-m3k2-8",
  "type": "download_and_install",
  "game_domain": "cyberpunk2077",
  "mod_id": "4198",
  // The id the *site* uses for the file, which is not the one the API keys on.
  // The host matches a request against either.
  "file_id": "154093",
  "grant": { "key": "Ab3-_cd9", "expires": 1757200000 },
}
```

The extension catches that link in the page's own world (`nxm-intercept.js`),
parses it as untrusted input (`nxm.js`) and sends the fields above; it never
forwards the raw address. The grant is not a credential: it authorises one file
for a few minutes and nothing else. It is stored with the queued request —
because the host process that received it exits long before the desktop spends
it — and it is redacted in every log and never serialized to the window.

A grant that lapsed before the desktop reached it fails the request with a
message saying to press the button again, rather than with whatever the provider
says to a stale nonce.

### Is the window running, and starting it

`status` and `app_state` both answer the liveness question, because the host
replying proves only that Onera is _installed_ — Chromium starts `onera-nmhost`
on demand, and it exits when the port closes.

The desktop writes a heartbeat to `$XDG_RUNTIME_DIR/onera/desktop.json` every
five seconds. Three facts must agree before the host calls it running: the
record parses, its pid still resolves to a live process, and its heartbeat is
under twenty seconds old. The pid alone would be fooled by a recycled id and the
timestamp alone by a process suspended mid-write; together they are wrong only
inside a window narrower than one poll, and being wrong costs a redundant launch
rather than a lost request.

`launch_app` resolves the window's location from four sources, in order:

1. `ONERA_DESKTOP_BIN`. A host inherits the _browser's_ environment, so this
   only works if the browser itself was started with it set.
2. `$XDG_CONFIG_HOME/onera/desktop-path`, written by
   `onera browser setup --desktop-path`.
3. A binary named `onera-desktop` beside the running host.
4. `/usr/bin/onera-desktop` and the other fixed install locations.

Only the `.deb` is covered by 3 and 4. An AppImage is a single file whose name
carries a version, and a development tree builds the host into `target/release`
while the window goes to `apps/desktop/src-tauri/target/release` — so both need
the recorded path, which is why `--desktop-path` exists.

Reading an executable's path out of a file is a real risk, and the record is
guarded accordingly: it is written `0600` and refused on read if it is a
symlink, if its mode lets another user write it, or if what it names is not an
executable file. The bar is set by what already exists — a Native Messaging
manifest is a file naming an executable that the _browser_ runs on a page's
say-so — so a file naming an executable Onera runs on the user's own say-so is
not new authority, provided nobody else can write it.

A launch returns as soon as the process is spawned — a window takes seconds to
appear — so the extension polls `app_state` for the result rather than being
told.

### What the extension knows about a mod

`mod_state` is asked once per mod page and answers from the local catalogue
alone when Onera has never seen the mod, which is the common case and costs no
API request. The provider is asked only when the mod is already downloaded or
installed — exactly when "is there something newer?" is worth a request. A
provider failure downgrades the answer to the local facts rather than discarding
them, so an offline user is still told what they have.

```jsonc
{
  "v": 1,
  "id": "ext-1",
  "status": "ok",
  "data": {
    "name": "Test Mod",
    "game_registered": true,
    "downloaded": true,
    "installed": true,
    "installed_version": "1.0.0",
    "installed_at": "2026-03-04T10:00:00Z",
    "update_available": true,
    "latest_version": "1.1.0",
    "latest_published_at": "2026-08-09T00:00:00Z",
  },
}
```

The extension turns that into the buttons a page carries: an installed mod is
never offered a plain download, an out-of-date one leads with its update, and a
mod already in the archive store offers to install rather than fetch again.

### Running a request unattended

A `download` or `download_and_install` sent from a page button carries
`auto_run`, and the desktop's watcher picks it up within a couple of seconds
without the user clicking anything else. How far it runs is bounded:

| Request                | Runs to                                                    |
| ---------------------- | ---------------------------------------------------------- |
| `add_mod`              | metadata cached; nothing transferred                       |
| `download`             | the archive in Onera's store                               |
| `download_and_install` | a plan, applied **only if nothing in it needs a decision** |

That last row is the rule the whole design rests on. A plan whose files all land
on untouched paths asks nothing, so applying it is exactly what the user
requested. A plan that would overwrite something unmanaged, something edited
since Onera wrote it, or another mod's file stops and waits, whichever side the
request came from.

A request is only auto-runnable when it names a file _and_ — for an install —
resolves to exactly one registered game. Two registered copies of the same game
is a question for the user, not a coin toss.

The host returns only after the request is committed to SQLite. If several files
are plausible, it queues the item as `waiting_for_user` and lets the Add Mod
screen present the choices. Closing the popup or exiting the short-lived host
therefore cannot lose the request. On launch, the desktop routes to the inbox
and marks the request complete only after the requested download or
installation succeeds. A request the watcher has picked up carries a lease
timestamp, so a crash mid-download leaves a request that is retried on the next
launch rather than one that is either lost or run twice.

## Validation

Everything on stdin is untrusted, even from a browser: any process running as
the user could be registered under the host name.

- Message size is capped at **1 MiB**, checked _before_ the buffer is allocated.
- Zero-length messages, non-UTF-8 bodies and unknown command types are rejected.
- The request id must be 1–128 characters.
- `game_domain`, `mod_id` and `file_id` must be 1–64 characters of
  `[A-Za-z0-9_-]`. `../../etc/passwd` and `107 OR 1=1` are rejected.
- `grant.key`, when present, must be 1–128 characters of `[A-Za-z0-9_-]`, and
  `grant.expires` must be a representable Unix timestamp. A key is a nonce and
  nothing else: one carrying `&`, `/` or `..` would be an attempt to reshape the
  request it is spent on, and is refused.
- `page_url` must be an `https://www.nexusmods.com/<game>/mods/<id>` address of
  at most 512 characters, with no query string, fragment or credential. It is
  the one field an extension supplies that Onera later hands back to a browser,
  so `javascript:`, `file:`, and `www.nexusmods.com.evil.test` are all refused
  rather than stored.
- A malformed frame leaves the stream out of sync, so the host reports the error
  and exits rather than trying to resynchronize.

## What never crosses this boundary

- **The API key**, in either direction.
- **Archive bytes.** The extension sends identifiers; the native application
  downloads. Native Messaging is not a bulk transport, and the browser's
  download manager cannot hash, deduplicate or resume.
- **Anything the page chose.** The extension re-derives a mod's identity from
  the URL rather than trusting what a content script sends, and rebuilds
  `page_url` from that identity — so a tab's query string never reaches Onera.
  A captured `nxm://` link is held to the same rule: the host is sent parsed,
  validated identifiers and a nonce, never the address the page produced.

## Troubleshooting

| Symptom                                                              | Cause                                                                                     |
| -------------------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| "Onera is not installed, or its browser connector is not registered" | Manifest missing, `path` not absolute, or the browser was not restarted                   |
| "Onera did not respond"                                              | `onera-nmhost` exited at startup — check `stderr`, which Chromium captures in its own log |
| `unsupported_version`                                                | Extension and host are from different builds; update both                                 |
| Nothing happens on click                                             | Extension id is not in `allowed_origins`                                                  |
| The popup says Onera is not running, and Open Onera does nothing     | No desktop binary was found; re-run `browser setup --desktop-path <path to the window>`   |
| A queued request never runs                                          | The window is closed, or the request needs a file or a game the user has not chosen yet   |

`onera-nmhost` writes diagnostics to **stderr**, never stdout — stdout belongs to
the protocol. Run it directly to check it starts:

```sh
./target/release/onera-nmhost < /dev/null; echo "exit: $?"
```
