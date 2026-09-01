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

| Type                   | Fields                  | Does                                                    |
| ---------------------- | ----------------------- | ------------------------------------------------------- |
| `ping`                 | —                       | Liveness and version                                    |
| `status`               | —                       | Whether authenticated, and which games are registered   |
| `add_mod`              | `game_domain`, `mod_id` | Fetches metadata and queues an Add Mod inbox item       |
| `download`             | `+ file_id`             | Resolves the file and queues a durable desktop download |
| `download_and_install` | `+ file_id`             | Resolves the file and queues an installation preview    |

The host returns only after the request is committed to SQLite. If several files
are plausible, it queues the item as `waiting_for_user` and lets the Add Mod
screen present the choices. Closing the popup or exiting the short-lived host
therefore cannot lose the request. On launch, the desktop routes to the inbox
and marks the request complete only after the requested download or
installation succeeds.

## Validation

Everything on stdin is untrusted, even from a browser: any process running as
the user could be registered under the host name.

- Message size is capped at **1 MiB**, checked _before_ the buffer is allocated.
- Zero-length messages, non-UTF-8 bodies and unknown command types are rejected.
- The request id must be 1–128 characters.
- `game_domain`, `mod_id` and `file_id` must be 1–64 characters of
  `[A-Za-z0-9_-]`. `../../etc/passwd` and `107 OR 1=1` are rejected.
- A malformed frame leaves the stream out of sync, so the host reports the error
  and exits rather than trying to resynchronize.

## What never crosses this boundary

- **The API key**, in either direction.
- **Archive bytes.** The extension sends identifiers; the native application
  downloads. Native Messaging is not a bulk transport, and the browser's
  download manager cannot hash, deduplicate or resume.

## Troubleshooting

| Symptom                                                              | Cause                                                                                     |
| -------------------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| "Onera is not installed, or its browser connector is not registered" | Manifest missing, `path` not absolute, or the browser was not restarted                   |
| "Onera did not respond"                                              | `onera-nmhost` exited at startup — check `stderr`, which Chromium captures in its own log |
| `unsupported_version`                                                | Extension and host are from different builds; update both                                 |
| Nothing happens on click                                             | Extension id is not in `allowed_origins`                                                  |

`onera-nmhost` writes diagnostics to **stderr**, never stdout — stdout belongs to
the protocol. Run it directly to check it starts:

```sh
./target/release/onera-nmhost < /dev/null; echo "exit: $?"
```
