//! Nexus error handling.
//!
//! Every response is treated as untrusted: status codes are mapped explicitly,
//! `application/problem+json` bodies are parsed defensively, and nothing from a
//! response body is ever interpolated into an error without being truncated.

use onera_core::CoreError;
use serde::Deserialize;

/// Longest error detail Onera will echo back from the API.
///
/// A hostile or broken server could return megabytes of text; error messages
/// end up in logs and UI labels, so they are bounded here.
const MAX_DETAIL: usize = 500;

/// RFC 9457 problem details, as v3 returns them.
#[derive(Debug, Clone, Deserialize)]
pub struct ProblemDetails {
    /// Short human-readable summary.
    #[serde(default)]
    pub title: Option<String>,
    /// Longer explanation.
    #[serde(default)]
    pub detail: Option<String>,
    /// HTTP status echoed in the body.
    #[serde(default)]
    pub status: Option<u16>,
}

impl ProblemDetails {
    /// A bounded, single-line message safe to display and log.
    #[must_use]
    pub fn message(&self) -> String {
        let raw = match (&self.title, &self.detail) {
            (Some(t), Some(d)) if t != d => format!("{t}: {d}"),
            (Some(t), _) => t.clone(),
            (None, Some(d)) => d.clone(),
            (None, None) => "no detail".to_owned(),
        };
        truncate(&raw.replace(['\n', '\r'], " "))
    }
}

fn truncate(text: &str) -> String {
    if text.chars().count() <= MAX_DETAIL {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(MAX_DETAIL).collect();
    out.push('…');
    out
}

/// Map an HTTP status and optional problem body onto a core error.
///
/// The provider slug is threaded through so the UI can say *which* provider
/// rejected the request when more than one is configured.
#[must_use]
pub fn map_status(
    status: u16,
    body: Option<ProblemDetails>,
    retry_after: Option<u64>,
) -> CoreError {
    let detail = body.map_or_else(|| "no detail".to_owned(), |p| p.message());
    match status {
        401 | 403 => CoreError::Unauthenticated {
            provider: "nexus".to_owned(),
        },
        404 => CoreError::NotFound {
            kind: "nexus resource",
            id: detail,
        },
        429 => CoreError::RateLimited {
            provider: "nexus".to_owned(),
            // Without a Retry-After header, back off for a minute rather than
            // hammering an API that has already asked us to stop.
            retry_after_secs: retry_after.unwrap_or(60),
        },
        400 | 422 => CoreError::InvalidInput(detail),
        500..=599 => CoreError::Provider(format!("nexus server error {status}: {detail}")),
        other => CoreError::Provider(format!("unexpected nexus status {other}: {detail}")),
    }
}

/// Parse a problem-details body, tolerating anything that is not one.
///
/// An API that returns an HTML error page must not turn into a parse panic or
/// an unhelpful "expected value at line 1".
#[must_use]
pub fn parse_problem(body: &str) -> Option<ProblemDetails> {
    serde_json::from_str(body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_authentication_failures() {
        for status in [401, 403] {
            assert!(map_status(status, None, None).is_auth(), "status {status}");
        }
    }

    #[test]
    fn maps_rate_limits_and_honours_retry_after() {
        let err = map_status(429, None, Some(17));
        assert!(
            matches!(
                err,
                CoreError::RateLimited {
                    retry_after_secs: 17,
                    ..
                }
            ),
            "{err:?}"
        );
        // Without a header, a conservative default is used.
        let err = map_status(429, None, None);
        assert!(matches!(
            err,
            CoreError::RateLimited {
                retry_after_secs: 60,
                ..
            }
        ));
        assert!(err.is_retryable());
    }

    #[test]
    fn server_errors_are_retryable_and_client_errors_are_not() {
        assert!(map_status(503, None, None).is_retryable());
        assert!(!map_status(422, None, None).is_retryable());
    }

    #[test]
    fn problem_details_are_parsed_and_combined() {
        let problem = parse_problem(
            r#"{"title":"Not Found","detail":"The mod was not found.","status":404}"#,
        )
        .unwrap();
        assert_eq!(problem.message(), "Not Found: The mod was not found.");
    }

    #[test]
    fn a_non_json_error_body_does_not_break_anything() {
        assert!(parse_problem("<html>502 Bad Gateway</html>").is_none());
        let err = map_status(502, None, None);
        assert!(format!("{err}").contains("502"));
    }

    #[test]
    fn error_details_are_bounded_and_single_line() {
        let problem = ProblemDetails {
            title: Some("x".repeat(10_000)),
            detail: Some("line one\nline two".to_owned()),
            status: None,
        };
        let message = problem.message();
        assert!(
            message.chars().count() <= MAX_DETAIL + 1,
            "{}",
            message.len()
        );
        assert!(!message.contains('\n'));
    }

    #[test]
    fn a_missing_body_still_produces_a_usable_message() {
        assert_eq!(
            ProblemDetails {
                title: None,
                detail: None,
                status: None
            }
            .message(),
            "no detail"
        );
    }
}
