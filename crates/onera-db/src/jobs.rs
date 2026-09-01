//! Persistence for resumable downloads and browser-extension inbox requests.

use crate::convert::{from_timestamp, hash, now, uuid};
use crate::{db_err, Database};
use chrono::{DateTime, Utc};
use onera_core::domain::download::{DownloadJob, JobState};
use onera_core::ids::{
    ArchiveId, DownloadJobId, InboxRequestId, ProviderFileId, ProviderId, ProviderModId,
};
use onera_core::{CoreError, Result};
use serde::{Deserialize, Serialize};
use sqlx::Row as _;
use std::path::PathBuf;

fn job_state(value: JobState) -> &'static str {
    match value {
        JobState::Queued => "queued",
        JobState::Running => "running",
        JobState::Paused => "paused",
        JobState::Complete => "complete",
        JobState::Failed => "failed",
        JobState::Cancelled => "cancelled",
    }
}

fn parse_job_state(value: &str) -> Result<JobState> {
    Ok(match value {
        "queued" => JobState::Queued,
        "running" => JobState::Running,
        "paused" => JobState::Paused,
        "complete" => JobState::Complete,
        "failed" => JobState::Failed,
        "cancelled" => JobState::Cancelled,
        other => {
            return Err(CoreError::Database(format!(
                "unknown download state {other:?}"
            )))
        }
    })
}

/// Action requested by the browser extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxRequestKind {
    /// Cache the mod metadata and let the user choose what to do.
    AddMod,
    /// Download one file into Onera's archive store.
    Download,
    /// Download and continue into an installation preview.
    DownloadAndInstall,
}

impl InboxRequestKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::AddMod => "add_mod",
            Self::Download => "download",
            Self::DownloadAndInstall => "download_and_install",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "add_mod" => Self::AddMod,
            "download" => Self::Download,
            "download_and_install" => Self::DownloadAndInstall,
            other => return Err(CoreError::Database(format!("unknown inbox kind {other:?}"))),
        })
    }
}

/// Lifecycle state of a browser inbox request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxState {
    /// Ready for the desktop application to process.
    Queued,
    /// More input or approval is required from the user.
    WaitingForUser,
    /// The requested action finished.
    Complete,
    /// Processing failed; the redacted error is retained for the UI.
    Failed,
    /// The user deliberately dismissed the request.
    Dismissed,
}

impl InboxState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::WaitingForUser => "waiting_for_user",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Dismissed => "dismissed",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "queued" => Self::Queued,
            "waiting_for_user" => Self::WaitingForUser,
            "complete" => Self::Complete,
            "failed" => Self::Failed,
            "dismissed" => Self::Dismissed,
            other => {
                return Err(CoreError::Database(format!(
                    "unknown inbox state {other:?}"
                )))
            }
        })
    }

    /// Whether the request belongs in the actionable desktop inbox.
    #[must_use]
    pub const fn is_actionable(self) -> bool {
        matches!(self, Self::Queued | Self::WaitingForUser | Self::Failed)
    }
}

/// A durable request received from the browser extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxRequest {
    /// Stable request identifier.
    pub id: InboxRequestId,
    /// Requested action.
    pub kind: InboxRequestKind,
    /// Provider that owns the identifiers.
    pub provider: ProviderId,
    /// Provider game slug.
    pub game_slug: String,
    /// Provider mod identifier.
    pub provider_mod_id: ProviderModId,
    /// Selected provider file, if the browser supplied or Onera resolved one.
    pub provider_file_id: Option<ProviderFileId>,
    /// Current request state.
    pub state: InboxState,
    /// Redacted failure message.
    pub error: Option<String>,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last update time.
    pub updated_at: DateTime<Utc>,
}

impl InboxRequest {
    /// Build a queued Nexus request.
    #[must_use]
    pub fn queued(
        kind: InboxRequestKind,
        game_slug: String,
        provider_mod_id: ProviderModId,
        provider_file_id: Option<ProviderFileId>,
    ) -> Self {
        let at = Utc::now();
        Self {
            id: InboxRequestId::new(),
            kind,
            provider: ProviderId::nexus(),
            game_slug,
            provider_mod_id,
            provider_file_id,
            state: InboxState::Queued,
            error: None,
            created_at: at,
            updated_at: at,
        }
    }
}

impl Database {
    /// Insert or update a persisted download job.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn put_download_job(&self, job: &DownloadJob) -> Result<()> {
        sqlx::query(
            "INSERT INTO download_jobs
               (id, provider_id, provider_file_id, filename, expected_size, expected_hash,
                bytes_downloaded, temp_path, state, attempts, error, archive_id,
                created_at, updated_at, game_slug, provider_mod_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13, ?14, ?15)
             ON CONFLICT(id) DO UPDATE SET
               expected_size = ?5, expected_hash = ?6, bytes_downloaded = ?7,
               temp_path = ?8, state = ?9, attempts = ?10, error = ?11,
               archive_id = ?12, updated_at = ?13, game_slug = ?14,
               provider_mod_id = ?15",
        )
        .bind(job.id.to_string())
        .bind(job.provider.as_str())
        .bind(job.provider_file_id.as_str())
        .bind(&job.filename)
        .bind(job.expected_size.map(|v| v as i64))
        .bind(job.expected_hash.as_ref().map(|h| h.to_storage_string()))
        .bind(job.bytes_downloaded as i64)
        .bind(job.temp_path.display().to_string())
        .bind(job_state(job.state))
        .bind(i64::from(job.attempts))
        .bind(&job.error)
        .bind(job.archive_id.map(|id| id.to_string()))
        .bind(now())
        .bind(&job.game_slug)
        .bind(job.provider_mod_id.as_str())
        .execute(self.pool())
        .await
        .map_err(db_err)?;
        Ok(())
    }

    /// Every download job, newest first.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn download_jobs(&self) -> Result<Vec<DownloadJob>> {
        let rows = sqlx::query(
            "SELECT id, provider_id, game_slug, provider_mod_id, provider_file_id,
                    filename, expected_size, expected_hash, bytes_downloaded,
                    temp_path, state, attempts, error, archive_id
             FROM download_jobs ORDER BY created_at DESC",
        )
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;
        rows.into_iter().map(row_to_download).collect()
    }

    /// Download jobs that should continue after a restart.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn resumable_download_jobs(&self) -> Result<Vec<DownloadJob>> {
        let rows = sqlx::query(
            "SELECT id, provider_id, game_slug, provider_mod_id, provider_file_id,
                    filename, expected_size, expected_hash, bytes_downloaded,
                    temp_path, state, attempts, error, archive_id
             FROM download_jobs
             WHERE state IN ('queued', 'running', 'paused') ORDER BY created_at",
        )
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;
        rows.into_iter().map(row_to_download).collect()
    }

    /// Most recent completed job for a provider file.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn completed_download(
        &self,
        provider: &ProviderId,
        file: &ProviderFileId,
    ) -> Result<Option<DownloadJob>> {
        let row = sqlx::query(
            "SELECT id, provider_id, game_slug, provider_mod_id, provider_file_id,
                    filename, expected_size, expected_hash, bytes_downloaded,
                    temp_path, state, attempts, error, archive_id
             FROM download_jobs
             WHERE provider_id = ?1 AND provider_file_id = ?2 AND state = 'complete'
             ORDER BY updated_at DESC LIMIT 1",
        )
        .bind(provider.as_str())
        .bind(file.as_str())
        .fetch_optional(self.pool())
        .await
        .map_err(db_err)?;
        row.map(row_to_download).transpose()
    }

    /// Persist a browser request.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn put_inbox_request(&self, request: &InboxRequest) -> Result<()> {
        sqlx::query(
            "INSERT INTO inbox_requests
               (id, request_kind, provider_id, game_slug, provider_mod_id,
                provider_file_id, state, error, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET provider_file_id = ?6, state = ?7,
               error = ?8, updated_at = ?10",
        )
        .bind(request.id.to_string())
        .bind(request.kind.as_str())
        .bind(request.provider.as_str())
        .bind(&request.game_slug)
        .bind(request.provider_mod_id.as_str())
        .bind(
            request
                .provider_file_id
                .as_ref()
                .map(ProviderFileId::as_str),
        )
        .bind(request.state.as_str())
        .bind(&request.error)
        .bind(request.created_at.to_rfc3339())
        .bind(request.updated_at.to_rfc3339())
        .execute(self.pool())
        .await
        .map_err(db_err)?;
        Ok(())
    }

    /// Actionable browser requests, oldest first.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn inbox_requests(&self) -> Result<Vec<InboxRequest>> {
        let rows = sqlx::query(
            "SELECT id, request_kind, provider_id, game_slug, provider_mod_id,
                    provider_file_id, state, error, created_at, updated_at
             FROM inbox_requests
             WHERE state IN ('queued', 'waiting_for_user', 'failed')
             ORDER BY created_at",
        )
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;
        rows.into_iter().map(row_to_inbox).collect()
    }

    /// Change an inbox request's state.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn set_inbox_state(
        &self,
        id: InboxRequestId,
        state: InboxState,
        error: Option<&str>,
    ) -> Result<()> {
        let changed = sqlx::query(
            "UPDATE inbox_requests SET state = ?2, error = ?3, updated_at = ?4 WHERE id = ?1",
        )
        .bind(id.to_string())
        .bind(state.as_str())
        .bind(error)
        .bind(now())
        .execute(self.pool())
        .await
        .map_err(db_err)?
        .rows_affected();
        if changed == 0 {
            return Err(CoreError::NotFound {
                kind: "inbox request",
                id: id.to_string(),
            });
        }
        Ok(())
    }
}

fn row_to_download(row: sqlx::sqlite::SqliteRow) -> Result<DownloadJob> {
    let id: String = row.try_get("id").map_err(db_err)?;
    let expected_size: Option<i64> = row.try_get("expected_size").map_err(db_err)?;
    let expected_hash: Option<String> = row.try_get("expected_hash").map_err(db_err)?;
    let bytes: i64 = row.try_get("bytes_downloaded").map_err(db_err)?;
    let attempts: i64 = row.try_get("attempts").map_err(db_err)?;
    let archive: Option<String> = row.try_get("archive_id").map_err(db_err)?;
    let state: String = row.try_get("state").map_err(db_err)?;
    Ok(DownloadJob {
        id: DownloadJobId::from(uuid(&id)?),
        provider: ProviderId::new(row.try_get::<String, _>("provider_id").map_err(db_err)?),
        game_slug: row.try_get("game_slug").map_err(db_err)?,
        provider_mod_id: ProviderModId::new(
            row.try_get::<String, _>("provider_mod_id")
                .map_err(db_err)?,
        ),
        provider_file_id: ProviderFileId::new(
            row.try_get::<String, _>("provider_file_id")
                .map_err(db_err)?,
        ),
        filename: row.try_get("filename").map_err(db_err)?,
        expected_size: expected_size.and_then(|v| u64::try_from(v).ok()),
        expected_hash: expected_hash.map(|value| hash(&value)).transpose()?,
        temp_path: PathBuf::from(row.try_get::<String, _>("temp_path").map_err(db_err)?),
        bytes_downloaded: bytes.max(0) as u64,
        state: parse_job_state(&state)?,
        attempts: attempts.max(0) as u32,
        error: row.try_get("error").map_err(db_err)?,
        archive_id: archive
            .map(|value| uuid(&value).map(ArchiveId::from))
            .transpose()?,
    })
}

fn row_to_inbox(row: sqlx::sqlite::SqliteRow) -> Result<InboxRequest> {
    let id: String = row.try_get("id").map_err(db_err)?;
    let kind: String = row.try_get("request_kind").map_err(db_err)?;
    let state: String = row.try_get("state").map_err(db_err)?;
    let created: String = row.try_get("created_at").map_err(db_err)?;
    let updated: String = row.try_get("updated_at").map_err(db_err)?;
    Ok(InboxRequest {
        id: InboxRequestId::from(uuid(&id)?),
        kind: InboxRequestKind::parse(&kind)?,
        provider: ProviderId::new(row.try_get::<String, _>("provider_id").map_err(db_err)?),
        game_slug: row.try_get("game_slug").map_err(db_err)?,
        provider_mod_id: ProviderModId::new(
            row.try_get::<String, _>("provider_mod_id")
                .map_err(db_err)?,
        ),
        provider_file_id: row
            .try_get::<Option<String>, _>("provider_file_id")
            .map_err(db_err)?
            .map(ProviderFileId::new),
        state: InboxState::parse(&state)?,
        error: row.try_get("error").map_err(db_err)?,
        created_at: from_timestamp(&created)?,
        updated_at: from_timestamp(&updated)?,
    })
}
