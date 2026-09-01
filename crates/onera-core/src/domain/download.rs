//! Provider-neutral state for persisted, resumable downloads.

use crate::hash::FileHash;
use crate::ids::{ArchiveId, DownloadJobId, ProviderFileId, ProviderId, ProviderModId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Where a download job is in its lifecycle.
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
    #[must_use]
    pub const fn is_resumable(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Paused)
    }
}

/// A persisted download. Signed URLs are deliberately absent because they
/// expire and are credentials; provider identifiers are stored instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadJob {
    /// Identifier.
    pub id: DownloadJobId,
    /// Provider the file comes from.
    pub provider: ProviderId,
    /// Provider game slug needed to resolve a fresh URL.
    pub game_slug: String,
    /// Provider mod identifier needed to resolve a fresh URL.
    pub provider_mod_id: ProviderModId,
    /// Provider file identifier.
    pub provider_file_id: ProviderFileId,
    /// Filename for display.
    pub filename: String,
    /// Expected size, when known.
    pub expected_size: Option<u64>,
    /// Expected content hash, when it is a trusted algorithm Onera computes.
    pub expected_hash: Option<FileHash>,
    /// Stable partial-file path.
    pub temp_path: PathBuf,
    /// Bytes transferred so far.
    pub bytes_downloaded: u64,
    /// Current state.
    pub state: JobState,
    /// Attempts made.
    pub attempts: u32,
    /// Redacted failure message.
    pub error: Option<String>,
    /// Archive created after successful safety inspection.
    pub archive_id: Option<ArchiveId>,
}

impl DownloadJob {
    /// Queue a new job.
    #[must_use]
    pub fn queued(
        provider: ProviderId,
        game_slug: String,
        provider_mod_id: ProviderModId,
        provider_file_id: ProviderFileId,
        filename: String,
        expected_size: Option<u64>,
        temp_path: PathBuf,
    ) -> Self {
        Self {
            id: DownloadJobId::new(),
            provider,
            game_slug,
            provider_mod_id,
            provider_file_id,
            filename,
            expected_size,
            expected_hash: None,
            temp_path,
            bytes_downloaded: 0,
            state: JobState::Queued,
            attempts: 0,
            error: None,
            archive_id: None,
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
            "cyberpunk2077".into(),
            ProviderModId::new("42"),
            ProviderFileId::new("100"),
            "mod.zip".into(),
            Some(1_000),
            PathBuf::from("/tmp/job.part"),
        )
    }

    #[test]
    fn progress_is_bounded_and_unknown_sizes_are_supported() {
        let mut value = job();
        assert_eq!(value.fraction(), Some(0.0));
        value.bytes_downloaded = 1_300;
        assert_eq!(value.fraction(), Some(1.0));
        value.expected_size = None;
        assert_eq!(value.fraction(), None);
    }

    #[test]
    fn only_incomplete_jobs_resume() {
        for state in [JobState::Queued, JobState::Running, JobState::Paused] {
            assert!(state.is_resumable());
        }
        for state in [JobState::Complete, JobState::Failed, JobState::Cancelled] {
            assert!(!state.is_resumable());
        }
    }

    #[test]
    fn signed_urls_cannot_be_serialized_because_the_model_has_no_url() {
        assert!(!serde_json::to_string(&job()).unwrap().contains("http"));
    }
}
