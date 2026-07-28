//! Retry policy: exponential backoff with full jitter, plus rate-limit awareness.
//!
//! Backoff uses *full* jitter (`sleep = random(0, base * 2^attempt)`) rather
//! than a fixed multiplier. With several concurrent downloads all retrying after
//! the same 429, a deterministic backoff makes every client retry at the same
//! instant and re-trigger the limit; full jitter spreads them out.
//!
//! A `Retry-After` header always wins over the computed backoff: the server has
//! told us exactly how long to wait, and guessing shorter is how an application
//! gets its key throttled.

use std::time::Duration;

/// How retries are paced.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetryPolicy {
    /// Maximum number of attempts, including the first.
    pub max_attempts: u32,
    /// Base delay for the exponential curve.
    pub base_delay: Duration,
    /// Ceiling for any single wait.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(30),
        }
    }
}

impl RetryPolicy {
    /// A policy that never retries, for tests and for interactive calls where
    /// the user is waiting.
    #[must_use]
    pub fn none() -> Self {
        Self {
            max_attempts: 1,
            ..Self::default()
        }
    }

    /// How long to wait before `attempt` (0-indexed), given a server hint.
    ///
    /// `random` is supplied by the caller so the curve is testable without a
    /// flaky test.
    #[must_use]
    pub fn delay_for(&self, attempt: u32, retry_after: Option<Duration>, random: f64) -> Duration {
        if let Some(hint) = retry_after {
            return hint.min(self.max_delay.max(hint));
        }
        let exponent = attempt.min(16);
        let ceiling = self
            .base_delay
            .saturating_mul(1_u32 << exponent)
            .min(self.max_delay);
        // Full jitter: anywhere in [0, ceiling].
        ceiling.mul_f64(random.clamp(0.0, 1.0))
    }

    /// Whether another attempt is allowed after `attempt` (0-indexed) failed.
    #[must_use]
    pub fn should_retry(&self, attempt: u32, error: &onera_core::CoreError) -> bool {
        attempt + 1 < self.max_attempts && error.is_retryable()
    }
}

/// What the API's rate-limit headers say.
///
/// Nexus reports hourly and daily budgets. Onera reads them so it can slow down
/// *before* being refused rather than after.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RateLimit {
    /// Requests left in the hourly budget.
    pub hourly_remaining: Option<i64>,
    /// Requests left in the daily budget.
    pub daily_remaining: Option<i64>,
}

impl RateLimit {
    /// Read the limit headers from a response.
    #[must_use]
    pub fn from_headers(headers: &reqwest::header::HeaderMap) -> Self {
        let read =
            |name: &str| -> Option<i64> { headers.get(name)?.to_str().ok()?.trim().parse().ok() };
        Self {
            hourly_remaining: read("x-rl-hourly-remaining"),
            daily_remaining: read("x-rl-daily-remaining"),
        }
    }

    /// Whether the budget is nearly gone and callers should back off.
    #[must_use]
    pub fn is_nearly_exhausted(&self) -> bool {
        let low = |v: Option<i64>| v.is_some_and(|n| n <= 10);
        low(self.hourly_remaining) || low(self.daily_remaining)
    }

    /// Whether the budget is gone entirely.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        let gone = |v: Option<i64>| v.is_some_and(|n| n <= 0);
        gone(self.hourly_remaining) || gone(self.daily_remaining)
    }
}

/// Parse a `Retry-After` header, which may be either seconds or an HTTP date.
#[must_use]
pub fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds.min(3_600)));
    }
    let when = chrono::DateTime::parse_from_rfc2822(value.trim()).ok()?;
    let delta = when.timestamp() - chrono::Utc::now().timestamp();
    Some(Duration::from_secs(delta.clamp(0, 3_600) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderMap;

    #[test]
    fn backoff_grows_exponentially_and_is_capped() {
        let policy = RetryPolicy::default();
        // With random = 1.0 the delay is the full ceiling.
        assert_eq!(policy.delay_for(0, None, 1.0), Duration::from_millis(250));
        assert_eq!(policy.delay_for(1, None, 1.0), Duration::from_millis(500));
        assert_eq!(policy.delay_for(2, None, 1.0), Duration::from_millis(1_000));
        assert_eq!(
            policy.delay_for(20, None, 1.0),
            policy.max_delay,
            "must be capped"
        );
    }

    #[test]
    fn jitter_spreads_retries_across_the_whole_window() {
        let policy = RetryPolicy::default();
        let full = policy.delay_for(3, None, 1.0);
        assert_eq!(policy.delay_for(3, None, 0.0), Duration::ZERO);
        assert_eq!(policy.delay_for(3, None, 0.5), full / 2);
    }

    #[test]
    fn a_server_hint_overrides_the_computed_backoff() {
        let policy = RetryPolicy::default();
        let hint = Duration::from_secs(120);
        assert_eq!(
            policy.delay_for(0, Some(hint), 1.0),
            hint,
            "we must wait as long as the server asked, even past max_delay"
        );
    }

    #[test]
    fn only_retryable_errors_are_retried() {
        let policy = RetryPolicy::default();
        let retryable = onera_core::CoreError::Provider("timeout".into());
        let permanent = onera_core::CoreError::InvalidInput("bad request".into());
        assert!(policy.should_retry(0, &retryable));
        assert!(!policy.should_retry(0, &permanent));
        assert!(
            !policy.should_retry(4, &retryable),
            "attempt budget must be respected"
        );
        assert!(!RetryPolicy::none().should_retry(0, &retryable));
    }

    #[test]
    fn rate_limit_headers_are_read() {
        let mut headers = HeaderMap::new();
        headers.insert("x-rl-hourly-remaining", "95".parse().unwrap());
        headers.insert("x-rl-daily-remaining", "2400".parse().unwrap());
        let limit = RateLimit::from_headers(&headers);
        assert_eq!(limit.hourly_remaining, Some(95));
        assert!(!limit.is_nearly_exhausted());

        headers.insert("x-rl-hourly-remaining", "3".parse().unwrap());
        assert!(RateLimit::from_headers(&headers).is_nearly_exhausted());
        headers.insert("x-rl-hourly-remaining", "0".parse().unwrap());
        assert!(RateLimit::from_headers(&headers).is_exhausted());
    }

    #[test]
    fn missing_or_malformed_rate_limit_headers_are_tolerated() {
        let mut headers = HeaderMap::new();
        assert_eq!(RateLimit::from_headers(&headers), RateLimit::default());
        headers.insert("x-rl-hourly-remaining", "not a number".parse().unwrap());
        assert_eq!(RateLimit::from_headers(&headers).hourly_remaining, None);
        assert!(!RateLimit::from_headers(&headers).is_exhausted());
    }

    #[test]
    fn retry_after_accepts_seconds_and_dates() {
        let mut headers = HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "30".parse().unwrap());
        assert_eq!(parse_retry_after(&headers), Some(Duration::from_secs(30)));

        // An absurd value is clamped rather than blocking for a day.
        headers.insert(reqwest::header::RETRY_AFTER, "99999999".parse().unwrap());
        assert_eq!(
            parse_retry_after(&headers),
            Some(Duration::from_secs(3_600))
        );

        headers.insert(reqwest::header::RETRY_AFTER, "gibberish".parse().unwrap());
        assert_eq!(parse_retry_after(&headers), None);
    }
}
