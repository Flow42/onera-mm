//! Conversions between domain types and their stored representation.
//!
//! Kept in one place so the encoding of a hash, an id or a timestamp is defined
//! exactly once and every table agrees.

use chrono::{DateTime, SecondsFormat, Utc};
use onera_core::hash::FileHash;
use onera_core::{CoreError, Result};
use uuid::Uuid;

/// The current time in the stored timestamp format.
#[must_use]
pub fn now() -> String {
    to_timestamp(Utc::now())
}

/// Encode a timestamp. RFC 3339 UTC sorts correctly as plain text.
#[must_use]
pub fn to_timestamp(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// Decode a stored timestamp.
///
/// # Errors
/// Fails if the column does not hold a valid RFC 3339 timestamp.
pub fn from_timestamp(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| CoreError::Database(format!("bad timestamp {s:?}: {e}")))
}

/// Decode a stored UUID.
///
/// # Errors
/// Fails if the column does not hold a valid UUID.
pub fn uuid(s: &str) -> Result<Uuid> {
    Uuid::parse_str(s).map_err(|e| CoreError::Database(format!("bad uuid {s:?}: {e}")))
}

/// Decode a stored hash.
///
/// # Errors
/// Fails if the column does not hold `algorithm:hex`.
pub fn hash(s: &str) -> Result<FileHash> {
    FileHash::from_storage_string(s).map_err(|e| CoreError::Database(format!("bad hash: {e}")))
}

/// Decode an optional stored hash.
///
/// # Errors
/// Fails if a present value is malformed.
pub fn opt_hash(s: Option<String>) -> Result<Option<FileHash>> {
    s.as_deref().map(hash).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_round_trip_and_sort_as_text() {
        let earlier = DateTime::from_timestamp(1_000_000, 0).unwrap();
        let later = DateTime::from_timestamp(2_000_000, 0).unwrap();
        assert_eq!(from_timestamp(&to_timestamp(earlier)).unwrap(), earlier);
        assert!(
            to_timestamp(earlier) < to_timestamp(later),
            "text ordering must match time ordering"
        );
    }

    #[test]
    fn rejects_malformed_columns() {
        assert!(from_timestamp("yesterday").is_err());
        assert!(uuid("not-a-uuid").is_err());
        assert!(hash("blake3:xyz").is_err());
    }

    #[test]
    fn optional_hashes_pass_through_none() {
        assert_eq!(opt_hash(None).unwrap(), None);
        let h = FileHash::blake3_of(b"x");
        assert_eq!(opt_hash(Some(h.to_storage_string())).unwrap(), Some(h));
    }
}
