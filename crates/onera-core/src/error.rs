//! Core error type.
//!
//! Adapters map their own failures into these variants so the application layer
//! and the UI can react without knowing which provider, archive backend or
//! database is in use. Every variant is safe to display: secrets are redacted
//! at construction time by [`crate::redact`].

use crate::paths::RelPathError;

/// Result alias for core operations.
pub type Result<T> = std::result::Result<T, CoreError>;

/// A domain-level failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CoreError {
    /// An untrusted path could not be normalized.
    #[error("invalid path: {0}")]
    InvalidPath(#[from] RelPathError),

    /// An archive was rejected before or during extraction.
    #[error("archive rejected: {reason}")]
    ArchiveRejected {
        /// Human-readable reason, safe to show to the user.
        reason: String,
    },

    /// An archive backend failed for a non-security reason.
    #[error("archive backend error: {0}")]
    Archive(String),

    /// The layout of an archive could not be mapped unambiguously.
    #[error("archive layout is ambiguous: {0}")]
    AmbiguousLayout(String),

    /// A game installation did not validate.
    #[error("game installation invalid: {0}")]
    InvalidGameInstall(String),

    /// A provider (network) call failed.
    #[error("provider error: {0}")]
    Provider(String),

    /// The provider rejected our credentials.
    #[error("not authenticated with provider {provider}")]
    Unauthenticated {
        /// Provider slug.
        provider: String,
    },

    /// The provider asked us to slow down.
    #[error("rate limited by provider {provider}; retry after {retry_after_secs}s")]
    RateLimited {
        /// Provider slug.
        provider: String,
        /// Seconds to wait before retrying.
        retry_after_secs: u64,
    },

    /// Secret storage was unavailable or refused the operation.
    ///
    /// Onera never falls back to plaintext storage; this is always fatal for
    /// the affected credential.
    #[error("secret storage unavailable: {0}")]
    SecretStore(String),

    /// A database operation failed.
    #[error("database error: {0}")]
    Database(String),

    /// A filesystem operation failed.
    #[error("filesystem error at {path}: {source}")]
    Filesystem {
        /// The path involved, as displayed to the user.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A hash check failed. Always fatal for the containing operation.
    #[error("integrity check failed for {path}: expected {expected}, got {actual}")]
    IntegrityMismatch {
        /// Path that failed verification.
        path: String,
        /// Hash we recorded.
        expected: String,
        /// Hash we computed.
        actual: String,
    },

    /// The operation needs a decision that has not been supplied.
    #[error("operation requires a user decision: {0}")]
    DecisionRequired(String),

    /// The operation was cancelled by the user or by shutdown.
    #[error("operation cancelled")]
    Cancelled,

    /// A precondition on the recorded state did not hold; the caller should
    /// re-plan rather than retry.
    #[error("state conflict: {0}")]
    Conflict(String),

    /// Something the caller asked for does not exist.
    #[error("{kind} not found: {id}")]
    NotFound {
        /// What kind of entity was missing.
        kind: &'static str,
        /// The identifier that was looked up.
        id: String,
    },

    /// A feature is not supported by the selected adapter.
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// Input failed validation.
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

impl CoreError {
    /// Whether retrying the exact same call could plausibly succeed.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::RateLimited { .. } | Self::Provider(_))
    }

    /// Whether the failure is caused by the user's credentials.
    #[must_use]
    pub fn is_auth(&self) -> bool {
        matches!(self, Self::Unauthenticated { .. })
    }

    /// Build a filesystem error from a path and an I/O error.
    pub fn fs(path: impl AsRef<std::path::Path>, source: std::io::Error) -> Self {
        Self::Filesystem {
            path: path.as_ref().display().to_string(),
            source,
        }
    }
}
