//! Persisted download jobs.
//!
//! A job survives a restart. Onera records what was being fetched and how far it
//! got, so closing the application mid-download is recoverable rather than
//! wasted bandwidth — and so a queue of twenty mods can be resumed.

use onera_core::hash::FileHash;
use onera_core::ids::{DownloadJobId, ProviderFileId, ProviderId};
use serde::{Deserialize, Serialize};

/// Where a job is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Waiting for a concurrency slot.
    Queued,
    /// Bytes are arriving.
    Running,
    /// Interrupted, resumable.
    Paused,
    /// Finished and promoted into storage.
    Complete,
    /// Failed after exhausting its attempts.
    Failed,
    /// Cancelled by the user.
    Cancelled,
}

impl JobState {
    /// Whether the job still has work to do.
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Paused)
    }

    /// Whether a restart should pick this job back up.
    ///
    /// A job that was `Running` when the process died is resumable: its bytes
    /// were going to a `.part` file that is still there.
    #[must_use]
    pub const fn is_resumable(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Paused)
    }
}

/// A persisted download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadJob {
    /// Identifier.
    pub id: DownloadJobId,
    /// Provider the file comes from.
    pub provider: ProviderId,
    /// Provider's file identifier. The signed URL is *not* stored: it expires,
    /// and it is a credential.
    pub provider_file_id: ProviderFileId,
    /// Filename for display.
    pub filename: String,
    /// Expected size, when known.
    pub expected_size: Option<u64>,
    /// Expected hash, when the provider published one.
    pub expected_hash: Option<FileHash>,
    /// Bytes transferred so far.
    pub bytes_downloaded: u64,
    /// Current state.
    pub state: JobState,
    /// Attempts made.
    pub attempts: u32,
    /// Redacted failure message.
    pub error: Option<String>,
}

impl DownloadJob {
    /// Queue a new job.
    #[must_use]
    pub fn queued(
        provider: ProviderId,
        provider_file_id: ProviderFileId,
        filename: String,
        expected_size: Option<u64>,
    ) -> Self {
        Self {
            id: DownloadJobId::new(),
            provider,
            provider_file_id,
            filename,
            expected_size,
            expected_hash: None,
            bytes_downloaded: 0,
            state: JobState::Queued,
            attempts: 0,
            error: None,
        }
    }

    /// Fraction complete, when the size is known.
    #[must_use]
    pub fn fraction(&self) -> Option<f64> {
        let total = self.expected_size?;
        if total == 0 {
            return Some(1.0);
        }
        Some((self.bytes_downloaded as f64 / total as f64).clamp(0.0, 1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> DownloadJob {
        DownloadJob::queued(
            ProviderId::nexus(),
            ProviderFileId::new("100"),
            "mod.zip".into(),
            Some(1_000),
        )
    }

    #[test]
    fn a_new_job_is_queued_and_empty() {
        let j = job();
        assert_eq!(j.state, JobState::Queued);
        assert_eq!(j.bytes_downloaded, 0);
        assert_eq!(j.fraction(), Some(0.0));
    }

    #[test]
    fn progress_is_reported_as_a_fraction() {
        let mut j = job();
        j.bytes_downloaded = 250;
        assert_eq!(j.fraction(), Some(0.25));
        // More bytes than declared is clamped rather than reported as 130%.
        j.bytes_downloaded = 1_300;
        assert_eq!(j.fraction(), Some(1.0));
    }

    #[test]
    fn an_unknown_size_has_no_fraction() {
        let mut j = job();
        j.expected_size = None;
        assert_eq!(j.fraction(), None);
    }

    #[test]
    fn interrupted_jobs_are_resumable_and_finished_ones_are_not() {
        for state in [JobState::Queued, JobState::Running, JobState::Paused] {
            assert!(
                state.is_resumable(),
                "{state:?} should resume after a restart"
            );
            assert!(state.is_active());
        }
        for state in [JobState::Complete, JobState::Failed, JobState::Cancelled] {
            assert!(!state.is_resumable(), "{state:?} must not be restarted");
            assert!(!state.is_active());
        }
    }

    #[test]
    fn a_job_never_carries_a_signed_url() {
        // The type has no URL field at all: a resumed job re-resolves the
        // download, because signed locations expire and are credentials.
        let encoded = serde_json::to_string(&job()).unwrap();
        assert!(!encoded.contains("http"), "{encoded}");
    }
}
