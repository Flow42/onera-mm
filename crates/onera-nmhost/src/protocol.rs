//! The Native Messaging wire protocol.
//!
//! Chromium's Native Messaging transport is a 32-bit native-endian length
//! prefix followed by a UTF-8 JSON document, on stdin/stdout. On top of that,
//! Onera defines a versioned envelope with request ids and structured errors, so
//! that:
//!
//! * an extension built against a newer or older host is told so rather than
//!   silently misbehaving;
//! * responses can be correlated with requests, which matters because a
//!   download or an install produces progress messages interleaved with other
//!   traffic;
//! * every error the extension can receive has a machine-readable code.
//!
//! Everything arriving on stdin is untrusted, even though it comes from a
//! browser: an extension id can be spoofed by anything that can execute as the
//! user, so the host validates length, encoding, version and payload shape
//! before acting.

use chrono::DateTime;
use onera_core::ports::DownloadGrant;
use onera_core::redact::Secret;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

/// Protocol version this host speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// Largest message accepted in either direction.
///
/// Chromium itself caps messages *to* a host at 1 MiB. Onera enforces the same
/// bound on both directions so a malformed length prefix cannot make the host
/// allocate gigabytes.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// Longest page address the host will record.
pub const MAX_PAGE_URL_BYTES: usize = 512;

/// A request from the extension.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Protocol version the extension speaks.
    pub v: u32,
    /// Correlation id, echoed in the response.
    pub id: String,
    /// What to do.
    #[serde(flatten)]
    pub command: Command,
}

/// Commands the extension can send.
///
/// Note what is *not* here: no file paths, no URLs, no credentials. The
/// extension supplies a game domain and a mod id — stable identifiers taken
/// from the page URL — and the host asks the API for everything else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Check the host is alive and version-compatible.
    Ping,
    /// Report the host's status to the extension's popup.
    Status,
    /// Report whether the desktop application is running.
    AppState,
    /// Start the desktop application if it is not already running.
    LaunchApp,
    /// Report what Onera already knows about one mod.
    ModState {
        /// Provider game slug from the page URL.
        game_domain: String,
        /// Mod id from the page URL.
        mod_id: String,
    },
    /// Record a mod for later without downloading.
    AddMod {
        /// Provider game slug from the page URL.
        game_domain: String,
        /// Mod id from the page URL.
        mod_id: String,
    },
    /// Download a mod's file into the archive store.
    Download {
        /// Provider game slug.
        game_domain: String,
        /// Mod id.
        mod_id: String,
        /// A specific file, when the user already chose one.
        file_id: Option<String>,
        /// The page this was requested from, recorded so the desktop can
        /// reopen it later. Validated as a provider URL before it is stored.
        #[serde(default)]
        page_url: Option<String>,
        /// A download the provider's website authorised, when the request came
        /// from its own "Mod manager download" rather than from Onera's button.
        #[serde(default)]
        grant: Option<WireGrant>,
    },
    /// Download and then install, previewing conflicts first.
    DownloadAndInstall {
        /// Provider game slug.
        game_domain: String,
        /// Mod id.
        mod_id: String,
        /// A specific file, when the user already chose one.
        file_id: Option<String>,
        /// The page this was requested from.
        #[serde(default)]
        page_url: Option<String>,
        /// A download the provider's website authorised.
        #[serde(default)]
        grant: Option<WireGrant>,
    },
}

/// A permission the provider's website minted for one file, as it crosses the
/// transport.
///
/// This is the single exception to "the extension sends identifiers and nothing
/// else", and it is a narrow one: the value is useless for anything but
/// downloading the file it names, it stops working within minutes, and Onera
/// cannot obtain one by itself — only a browser where the user pressed the
/// button can. It is turned into a [`DownloadGrant`](onera_core::ports::DownloadGrant)
/// after validation and never logged.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireGrant {
    /// The nonce, exactly as the site issued it.
    pub key: String,
    /// Unix seconds after which the provider stops honouring it.
    pub expires: i64,
}

impl std::fmt::Debug for WireGrant {
    /// Prints the expiry and never the key: a nonce in a log is a nonce
    /// somebody else can spend before it lapses.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WireGrant")
            .field("key", &onera_core::redact::REDACTED)
            .field("expires", &self.expires)
            .finish()
    }
}

impl WireGrant {
    /// Convert into the core's grant, or explain why it cannot be one.
    ///
    /// # Errors
    /// Returns a message for a nonce with the wrong shape or an expiry that is
    /// not a time.
    pub fn to_grant(&self) -> Result<DownloadGrant, String> {
        if self.key.is_empty() || self.key.len() > 128 {
            return Err("grant.key must be between 1 and 128 characters".to_owned());
        }
        if !self
            .key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err("grant.key contains characters that are not allowed".to_owned());
        }
        let expires_at = DateTime::from_timestamp(self.expires, 0)
            .ok_or_else(|| "grant.expires is not a time".to_owned())?;
        Ok(DownloadGrant {
            key: Secret::new(self.key.clone()),
            expires_at,
        })
    }
}

/// A response from the host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Protocol version this host speaks.
    pub v: u32,
    /// Correlation id from the request, or `"unknown"` if it could not be read.
    pub id: String,
    /// The outcome.
    #[serde(flatten)]
    pub result: ResponseBody,
}

/// What happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResponseBody {
    /// The command succeeded.
    Ok {
        /// Command-specific data.
        data: serde_json::Value,
    },
    /// The command failed.
    Error {
        /// Machine-readable code.
        code: ErrorCode,
        /// Human-readable message, already redacted.
        message: String,
    },
}

/// Machine-readable error codes.
///
/// The extension switches on these rather than on message text, so wording can
/// change without breaking it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The message was not valid framed JSON, or exceeded the size limit.
    Malformed,
    /// The extension speaks a version this host does not.
    UnsupportedVersion,
    /// No API key is stored; the user must complete onboarding.
    NotAuthenticated,
    /// The mod, file or game could not be found.
    NotFound,
    /// The user must choose between several plausible files.
    SelectionRequired,
    /// The user must resolve conflicts in the desktop application.
    DecisionRequired,
    /// The provider refused or the network failed.
    ProviderError,
    /// Anything else.
    Internal,
}

/// Errors from reading or writing a framed message.
#[derive(Debug, thiserror::Error)]
pub enum FramingError {
    /// The stream ended cleanly. Normal at shutdown.
    #[error("the stream ended")]
    Eof,
    /// The declared length exceeded [`MAX_MESSAGE_BYTES`].
    #[error("message of {0} bytes exceeds the {MAX_MESSAGE_BYTES} byte limit")]
    TooLarge(usize),
    /// The body was not valid UTF-8 JSON.
    #[error("malformed message: {0}")]
    Malformed(String),
    /// The underlying stream failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Read one length-prefixed JSON message.
///
/// # Errors
/// Returns [`FramingError::Eof`] at a clean end of stream, and
/// [`FramingError::TooLarge`] *before* allocating when the prefix is absurd.
pub fn read_message<R: Read>(reader: &mut R) -> Result<Request, FramingError> {
    let mut length_bytes = [0_u8; 4];
    match reader.read_exact(&mut length_bytes) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(FramingError::Eof),
        Err(e) => return Err(e.into()),
    }

    // Chromium writes the length in the platform's native byte order.
    let length = u32::from_ne_bytes(length_bytes) as usize;
    // Checked before any allocation: this is the whole point of the limit.
    if length > MAX_MESSAGE_BYTES {
        return Err(FramingError::TooLarge(length));
    }
    if length == 0 {
        return Err(FramingError::Malformed("zero-length message".to_owned()));
    }

    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body)?;
    let text = String::from_utf8(body)
        .map_err(|_| FramingError::Malformed("message is not valid UTF-8".to_owned()))?;
    serde_json::from_str(&text).map_err(|e| FramingError::Malformed(e.to_string()))
}

/// Write one length-prefixed JSON message.
///
/// # Errors
/// Fails if the encoded response exceeds the size limit or the stream errors.
pub fn write_message<W: Write>(writer: &mut W, response: &Response) -> Result<(), FramingError> {
    let encoded =
        serde_json::to_vec(response).map_err(|e| FramingError::Malformed(e.to_string()))?;
    if encoded.len() > MAX_MESSAGE_BYTES {
        return Err(FramingError::TooLarge(encoded.len()));
    }
    writer.write_all(&(encoded.len() as u32).to_ne_bytes())?;
    writer.write_all(&encoded)?;
    writer.flush()?;
    Ok(())
}

/// Validate a request's version and identifiers.
///
/// # Errors
/// Returns a ready-to-send [`Response`] describing what is wrong.
pub fn validate(request: &Request) -> Result<(), Response> {
    if request.v != PROTOCOL_VERSION {
        return Err(error_response(
            &request.id,
            ErrorCode::UnsupportedVersion,
            format!(
                "this Onera host speaks protocol version {PROTOCOL_VERSION}, the extension sent {}",
                request.v
            ),
        ));
    }
    if request.id.is_empty() || request.id.len() > 128 {
        return Err(error_response(
            "unknown",
            ErrorCode::Malformed,
            "the request id must be between 1 and 128 characters".to_owned(),
        ));
    }

    let check = |field: &str, value: &str| -> Result<(), Response> {
        if value.is_empty() || value.len() > 64 {
            return Err(error_response(
                &request.id,
                ErrorCode::Malformed,
                format!("{field} must be between 1 and 64 characters"),
            ));
        }
        // Page identifiers are slugs and numbers. Anything else is either a
        // broken extension or an attempt to reach a different endpoint.
        if !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(error_response(
                &request.id,
                ErrorCode::Malformed,
                format!("{field} contains characters that are not allowed"),
            ));
        }
        Ok(())
    };

    match &request.command {
        Command::Ping | Command::Status | Command::AppState | Command::LaunchApp => Ok(()),
        Command::AddMod {
            game_domain,
            mod_id,
        }
        | Command::ModState {
            game_domain,
            mod_id,
        } => {
            check("game_domain", game_domain)?;
            check("mod_id", mod_id)
        }
        Command::Download {
            game_domain,
            mod_id,
            file_id,
            page_url,
            grant,
        }
        | Command::DownloadAndInstall {
            game_domain,
            mod_id,
            file_id,
            page_url,
            grant,
        } => {
            check("game_domain", game_domain)?;
            check("mod_id", mod_id)?;
            if let Some(id) = file_id {
                check("file_id", id)?;
            }
            if let Some(grant) = grant {
                grant.to_grant().map_err(|why| {
                    error_response(&request.id, ErrorCode::Malformed, why)
                })?;
            }
            match page_url {
                Some(url) => check_page_url(&request.id, url),
                None => Ok(()),
            }
        }
    }
}

/// Accept only a provider mod page as a recorded address.
///
/// The URL is the one field an extension supplies that Onera later hands back
/// to a browser, so it is checked against the same host and shape the content
/// script runs on. Anything else is dropped rather than stored: a `javascript:`
/// or `file:` address surviving into the desktop's "open on Nexus" button would
/// turn a stored string into code someone else chose.
fn check_page_url(request_id: &str, url: &str) -> Result<(), Response> {
    let reject = |why: &str| {
        Err(error_response(
            request_id,
            ErrorCode::Malformed,
            format!("page_url {why}"),
        ))
    };
    if url.len() > MAX_PAGE_URL_BYTES {
        return reject("is longer than this host accepts");
    }
    let Some(rest) = url.strip_prefix("https://www.nexusmods.com/") else {
        return reject("is not a Nexus Mods page address");
    };
    // Same shape the content script matches: <game>/mods/<id>, and nothing that
    // could carry a credential or a second target.
    let mut parts = rest.split('/');
    let (Some(game), Some("mods"), Some(mod_id)) = (parts.next(), parts.next(), parts.next())
    else {
        return reject("is not a mod page address");
    };
    if game.is_empty() || mod_id.is_empty() {
        return reject("names no mod");
    }
    if url.contains(['@', '?', '#', '\\']) {
        return reject("carries a query, fragment or credential");
    }
    if !rest
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.'))
    {
        return reject("contains characters that are not allowed");
    }
    Ok(())
}

/// Build a success response.
#[must_use]
pub fn ok_response(id: &str, data: serde_json::Value) -> Response {
    Response {
        v: PROTOCOL_VERSION,
        id: id.to_owned(),
        result: ResponseBody::Ok { data },
    }
}

/// Build an error response.
#[must_use]
pub fn error_response(id: &str, code: ErrorCode, message: String) -> Response {
    Response {
        v: PROTOCOL_VERSION,
        id: id.to_owned(),
        result: ResponseBody::Error { code, message },
    }
}

/// Map a core error onto a wire error code.
#[must_use]
pub fn code_for(error: &onera_core::CoreError) -> ErrorCode {
    use onera_core::CoreError as E;
    match error {
        E::Unauthenticated { .. } => ErrorCode::NotAuthenticated,
        E::NotFound { .. } => ErrorCode::NotFound,
        E::DecisionRequired(_) | E::AmbiguousLayout(_) => ErrorCode::DecisionRequired,
        E::Provider(_) | E::RateLimited { .. } => ErrorCode::ProviderError,
        E::InvalidInput(_) => ErrorCode::Malformed,
        _ => ErrorCode::Internal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn frame(json: &str) -> Vec<u8> {
        let mut out = (json.len() as u32).to_ne_bytes().to_vec();
        out.extend_from_slice(json.as_bytes());
        out
    }

    fn request(command: Command) -> Request {
        Request {
            v: PROTOCOL_VERSION,
            id: "req-1".into(),
            command,
        }
    }

    #[test]
    fn a_well_formed_message_round_trips() {
        let bytes = frame(r#"{"v":1,"id":"req-1","type":"ping"}"#);
        let parsed = read_message(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(parsed, request(Command::Ping));

        let mut out = Vec::new();
        write_message(
            &mut out,
            &ok_response("req-1", serde_json::json!({ "pong": true })),
        )
        .unwrap();
        let echoed = read_back(&out);
        assert!(echoed.contains(r#""id":"req-1""#), "{echoed}");
        assert!(echoed.contains(r#""status":"ok""#), "{echoed}");
    }

    fn read_back(bytes: &[u8]) -> String {
        let length = u32::from_ne_bytes(bytes[..4].try_into().unwrap()) as usize;
        assert_eq!(
            length,
            bytes.len() - 4,
            "the prefix must match the body length"
        );
        String::from_utf8(bytes[4..].to_vec()).unwrap()
    }

    #[test]
    fn every_command_parses_from_its_documented_shape() {
        let cases = [
            (r#"{"v":1,"id":"a","type":"status"}"#, Command::Status),
            (
                r#"{"v":1,"id":"a","type":"add_mod","game_domain":"cyberpunk2077","mod_id":"107"}"#,
                Command::AddMod {
                    game_domain: "cyberpunk2077".into(),
                    mod_id: "107".into(),
                },
            ),
            (
                r#"{"v":1,"id":"a","type":"download","game_domain":"cyberpunk2077","mod_id":"107","file_id":null}"#,
                Command::Download {
                    game_domain: "cyberpunk2077".into(),
                    mod_id: "107".into(),
                    file_id: None,
                    page_url: None,
                    grant: None,
                },
            ),
            (
                r#"{"v":1,"id":"a","type":"download_and_install","game_domain":"cyberpunk2077","mod_id":"107","file_id":"100"}"#,
                Command::DownloadAndInstall {
                    game_domain: "cyberpunk2077".into(),
                    mod_id: "107".into(),
                    file_id: Some("100".into()),
                    page_url: None,
                    grant: None,
                },
            ),
        ];
        for (json, expected) in cases {
            let parsed = read_message(&mut Cursor::new(frame(json))).unwrap();
            assert_eq!(parsed.command, expected, "{json}");
        }
    }

    #[test]
    fn the_new_commands_parse_from_their_documented_shape() {
        let cases = vec![
            (r#"{"v":1,"id":"a","type":"app_state"}"#, Command::AppState),
            (
                r#"{"v":1,"id":"a","type":"launch_app"}"#,
                Command::LaunchApp,
            ),
            (
                r#"{"v":1,"id":"a","type":"mod_state","game_domain":"cyberpunk2077","mod_id":"107"}"#,
                Command::ModState {
                    game_domain: "cyberpunk2077".into(),
                    mod_id: "107".into(),
                },
            ),
            (
                r#"{"v":1,"id":"a","type":"download","game_domain":"cyberpunk2077","mod_id":"107","file_id":"100","page_url":"https://www.nexusmods.com/cyberpunk2077/mods/107"}"#,
                Command::Download {
                    game_domain: "cyberpunk2077".into(),
                    mod_id: "107".into(),
                    file_id: Some("100".into()),
                    page_url: Some("https://www.nexusmods.com/cyberpunk2077/mods/107".into()),
                    grant: None,
                },
            ),
        ];
        for (json, expected) in cases {
            let parsed = read_message(&mut Cursor::new(frame(json))).unwrap();
            assert_eq!(parsed.command, expected, "{json}");
        }
    }

    #[test]
    fn a_grant_from_the_website_parses_and_is_checked() {
        let json = r#"{"v":1,"id":"a","type":"download","game_domain":"cyberpunk2077",
            "mod_id":"4198","file_id":"154093","grant":{"key":"Ab3-_cd9","expires":1757200000}}"#;
        let parsed = read_message(&mut Cursor::new(frame(json))).unwrap();
        let Command::Download { grant, .. } = &parsed.command else {
            panic!("expected a download");
        };
        let grant = grant.as_ref().expect("the grant survived the transport");
        assert!(validate(&parsed).is_ok());
        let converted = grant.to_grant().unwrap();
        assert_eq!(converted.key.expose(), "Ab3-_cd9");
        assert_eq!(converted.expires_at.timestamp(), 1_757_200_000);
        // The nonce is as loggable as an API key, which is to say not at all.
        assert!(!format!("{grant:?}").contains("Ab3-_cd9"), "{grant:?}");
    }

    #[test]
    fn a_grant_that_is_not_one_is_refused_at_the_boundary() {
        for grant in [
            WireGrant {
                key: String::new(),
                expires: 1_757_200_000,
            },
            WireGrant {
                key: "a".repeat(129),
                expires: 1_757_200_000,
            },
            // A key is a nonce, not a place to hide a query string or a path.
            WireGrant {
                key: "abc&redirect=http://evil.test".into(),
                expires: 1_757_200_000,
            },
            WireGrant {
                key: "abc/../../x".into(),
                expires: 1_757_200_000,
            },
            WireGrant {
                key: "Ab3".into(),
                expires: i64::MAX,
            },
        ] {
            let request = request(Command::Download {
                game_domain: "cyberpunk2077".into(),
                mod_id: "4198".into(),
                file_id: Some("154093".into()),
                page_url: None,
                grant: Some(grant.clone()),
            });
            assert!(grant.to_grant().is_err(), "{grant:?}");
            assert!(validate(&request).is_err(), "{grant:?}");
        }
    }

    #[test]
    fn only_a_provider_mod_page_is_accepted_as_a_page_url() {
        let accepted = [
            "https://www.nexusmods.com/cyberpunk2077/mods/107",
            "https://www.nexusmods.com/skyrimspecialedition/mods/3863/",
        ];
        for url in accepted {
            let request = request(Command::Download {
                game_domain: "cyberpunk2077".into(),
                mod_id: "107".into(),
                file_id: None,
                page_url: Some(url.to_owned()),
                grant: None,
            });
            assert!(validate(&request).is_ok(), "{url} should be accepted");
        }

        let rejected = [
            // Not the provider, however much the path looks like it.
            "https://www.nexusmods.com.evil.test/cyberpunk2077/mods/107",
            "http://www.nexusmods.com/cyberpunk2077/mods/107",
            "https://nexusmods.com/cyberpunk2077/mods/107",
            // Not a mod page.
            "https://www.nexusmods.com/cyberpunk2077/users/1",
            "https://www.nexusmods.com/cyberpunk2077/mods/",
            // Anything that could carry a second target or a credential.
            "https://user:pw@www.nexusmods.com/cyberpunk2077/mods/107",
            "https://www.nexusmods.com/cyberpunk2077/mods/107?key=secret",
            "https://www.nexusmods.com/cyberpunk2077/mods/107#x",
            "javascript:alert(1)",
            "file:///etc/passwd",
        ];
        for url in rejected {
            let request = request(Command::Download {
                game_domain: "cyberpunk2077".into(),
                mod_id: "107".into(),
                file_id: None,
                page_url: Some(url.to_owned()),
                grant: None,
            });
            assert!(validate(&request).is_err(), "{url} should be rejected");
        }
    }

    #[test]
    fn an_oversized_page_url_is_rejected() {
        let long = format!(
            "https://www.nexusmods.com/cyberpunk2077/mods/{}",
            "1".repeat(MAX_PAGE_URL_BYTES)
        );
        let request = request(Command::Download {
            game_domain: "cyberpunk2077".into(),
            mod_id: "107".into(),
            file_id: None,
            page_url: Some(long),
            grant: None,
        });
        assert!(validate(&request).is_err());
    }

    #[test]
    fn an_oversized_length_prefix_is_refused_without_allocating() {
        let mut bytes = (u32::MAX).to_ne_bytes().to_vec();
        bytes.extend_from_slice(b"{}");
        let err = read_message(&mut Cursor::new(bytes)).unwrap_err();
        assert!(matches!(err, FramingError::TooLarge(_)), "{err:?}");
    }

    #[test]
    fn a_message_at_the_limit_is_accepted_and_one_past_it_is_not() {
        let padding = "x".repeat(MAX_MESSAGE_BYTES - 40);
        let json = format!(r#"{{"v":1,"id":"{padding}","type":"ping"}}"#);
        assert!(json.len() <= MAX_MESSAGE_BYTES);
        assert!(read_message(&mut Cursor::new(frame(&json))).is_ok());

        let too_big = format!(
            r#"{{"v":1,"id":"{}","type":"ping"}}"#,
            "x".repeat(MAX_MESSAGE_BYTES)
        );
        assert!(matches!(
            read_message(&mut Cursor::new(frame(&too_big))),
            Err(FramingError::TooLarge(_))
        ));
    }

    #[test]
    fn malformed_bodies_are_rejected() {
        for body in [
            "not json",
            "{}",
            r#"{"v":1}"#,
            r#"{"v":1,"id":"a","type":"nope"}"#,
        ] {
            let result = read_message(&mut Cursor::new(frame(body)));
            assert!(result.is_err(), "{body:?} should not parse");
        }
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        let mut bytes = 4_u32.to_ne_bytes().to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe, 0xfd, 0xfc]);
        let err = read_message(&mut Cursor::new(bytes)).unwrap_err();
        assert!(format!("{err}").contains("UTF-8"), "{err}");
    }

    #[test]
    fn a_truncated_stream_reports_eof_rather_than_an_error() {
        assert!(matches!(
            read_message(&mut Cursor::new(Vec::new())),
            Err(FramingError::Eof)
        ));
        // A prefix with no body is a real error, not a clean shutdown.
        let bytes = 100_u32.to_ne_bytes().to_vec();
        assert!(matches!(
            read_message(&mut Cursor::new(bytes)),
            Err(FramingError::Io(_))
        ));
    }

    #[test]
    fn a_zero_length_message_is_rejected() {
        let bytes = 0_u32.to_ne_bytes().to_vec();
        assert!(matches!(
            read_message(&mut Cursor::new(bytes)),
            Err(FramingError::Malformed(_))
        ));
    }

    #[test]
    fn a_version_mismatch_is_reported_explicitly() {
        let request = Request {
            v: 99,
            ..request(Command::Ping)
        };
        let response = validate(&request).unwrap_err();
        assert!(matches!(
            response.result,
            ResponseBody::Error {
                code: ErrorCode::UnsupportedVersion,
                ..
            }
        ));
    }

    #[test]
    fn hostile_identifiers_are_rejected_by_validation() {
        let hostile = [
            "../../etc/passwd",
            "107/../../admin",
            "107 OR 1=1",
            "cyberpunk2077?x=1",
            "",
            &"x".repeat(65),
        ];
        for value in hostile {
            let request = request(Command::AddMod {
                game_domain: "cyberpunk2077".into(),
                mod_id: value.to_owned(),
            });
            let response = validate(&request).unwrap_err();
            assert!(
                matches!(
                    response.result,
                    ResponseBody::Error {
                        code: ErrorCode::Malformed,
                        ..
                    }
                ),
                "{value:?} was accepted"
            );
        }
    }

    #[test]
    fn ordinary_identifiers_are_accepted() {
        let request = request(Command::DownloadAndInstall {
            game_domain: "cyberpunk2077".into(),
            mod_id: "107".into(),
            file_id: Some("file_100-a".into()),
            page_url: Some("https://www.nexusmods.com/cyberpunk2077/mods/107".into()),
            grant: None,
        });
        assert!(validate(&request).is_ok());
    }

    #[test]
    fn an_absurd_request_id_is_rejected() {
        for id in [String::new(), "x".repeat(129)] {
            let request = Request {
                id,
                ..request(Command::Ping)
            };
            assert!(validate(&request).is_err());
        }
    }

    #[test]
    fn core_errors_map_onto_stable_codes() {
        use onera_core::CoreError as E;
        let cases = [
            (
                E::Unauthenticated {
                    provider: "nexus".into(),
                },
                ErrorCode::NotAuthenticated,
            ),
            (
                E::NotFound {
                    kind: "mod",
                    id: "1".into(),
                },
                ErrorCode::NotFound,
            ),
            (
                E::DecisionRequired("conflicts".into()),
                ErrorCode::DecisionRequired,
            ),
            (
                E::AmbiguousLayout("two readings".into()),
                ErrorCode::DecisionRequired,
            ),
            (
                E::RateLimited {
                    provider: "nexus".into(),
                    retry_after_secs: 1,
                },
                ErrorCode::ProviderError,
            ),
            (E::Cancelled, ErrorCode::Internal),
        ];
        for (error, expected) in cases {
            assert_eq!(code_for(&error), expected, "{error}");
        }
    }

    #[test]
    fn responses_never_carry_a_secret_shaped_field() {
        // The response type has no field an implementation could put a key in.
        let encoded = serde_json::to_string(&ok_response(
            "a",
            serde_json::json!({ "authenticated": true }),
        ))
        .unwrap();
        assert!(!encoded.contains("apikey"), "{encoded}");
        assert!(!encoded.contains("token"), "{encoded}");
    }
}
