//! Secret redaction.
//!
//! Anything that may carry a credential passes through here before it reaches
//! `tracing`, an error message, a Tauri command result or the Native Messaging
//! transport. The rule enforced by tests: an API key must never appear in any
//! string Onera emits.

use std::fmt;

/// Placeholder substituted for any redacted value.
pub const REDACTED: &str = "[redacted]";

/// A string that refuses to print itself.
///
/// `Debug` and `Display` both render [`REDACTED`]. The plaintext is only
/// reachable through [`Secret::expose`], which is deliberately noisy to read at
/// call sites and easy to grep for in review.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wrap a sensitive string.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Reveal the plaintext. Call only where the value leaves the process as a
    /// credential (an HTTP header, a Secret Service item).
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Length of the underlying secret, safe to log.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the secret is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl serde::Serialize for Secret {
    /// Serializes as [`REDACTED`]. A `Secret` can therefore never be persisted
    /// or sent over a transport by accident.
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(REDACTED)
    }
}

/// Replace every occurrence of each known secret in `text`.
///
/// Used as a defence in depth on strings that come back from subprocesses and
/// third-party libraries, which may echo a credential we handed them.
#[must_use]
pub fn scrub(text: &str, secrets: &[&Secret]) -> String {
    let mut out = text.to_owned();
    for secret in secrets {
        if secret.0.len() >= 4 {
            out = out.replace(&secret.0, REDACTED);
        }
    }
    out
}

/// Redact anything that looks like a credential in a URL, keeping the shape of
/// the URL intact for diagnostics.
///
/// Presigned download URLs carry signatures in the query string, so the whole
/// query is dropped rather than filtered by parameter name.
#[must_use]
pub fn redact_url(raw: &str) -> String {
    match url::Url::parse(raw) {
        Ok(mut u) => {
            if u.query().is_some() {
                u.set_query(Some(REDACTED));
            }
            let _ = u.set_password(None);
            if !u.username().is_empty() {
                let _ = u.set_username(REDACTED);
            }
            u.to_string()
        }
        Err(_) => REDACTED.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_never_prints_itself() {
        let s = Secret::new("nx-super-secret-key");
        assert_eq!(format!("{s}"), REDACTED);
        assert_eq!(format!("{s:?}"), REDACTED);
        assert_eq!(
            serde_json::to_string(&s).unwrap(),
            format!("\"{REDACTED}\"")
        );
        assert!(!format!("{s:?}{s}").contains("super-secret"));
    }

    #[test]
    fn scrub_removes_secrets_from_third_party_text() {
        let s = Secret::new("abcd1234");
        let text = "GET /v3/mods failed with apikey=abcd1234";
        assert_eq!(
            scrub(text, &[&s]),
            "GET /v3/mods failed with apikey=[redacted]"
        );
    }

    #[test]
    fn scrub_ignores_trivially_short_secrets() {
        // Substituting a 1-3 char "secret" would mangle unrelated text.
        let s = Secret::new("ab");
        assert_eq!(scrub("a table", &[&s]), "a table");
    }

    #[test]
    fn redact_url_drops_signed_query_strings() {
        let redacted = redact_url("https://cf.nexus.com/file.zip?Signature=deadbeef&Expires=1");
        assert!(redacted.starts_with("https://cf.nexus.com/file.zip?"));
        assert!(!redacted.contains("deadbeef"));
        assert_eq!(redact_url("not a url"), REDACTED);
        assert!(!redact_url("https://user:pw@example.com/x").contains("pw"));
    }
}
