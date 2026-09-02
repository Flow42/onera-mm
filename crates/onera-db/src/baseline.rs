//! Baseline persistence: immutable captures, their files, and scan runs.
//!
//! Three invariants live here rather than in the callers:
//!
//! * **A baseline is written once.** [`Database::put_baseline`] refuses to write
//!   an identifier that already exists, and the schema's triggers refuse an
//!   in-place edit of what was observed.
//! * **A new current baseline supersedes, never replaces.** The previous
//!   `current` row and every one of its file records stay exactly where they
//!   are, so a stale capture remains inspectable after a game update.
//! * **Reads are deterministic.** Baselines come back newest first, files and
//!   findings in a fixed order, so two runs of the same query produce the same
//!   list and a diff of two captures is meaningful.

use crate::convert::{hash, opt_hash, to_timestamp, uuid};
use crate::{db_err, Database};
use async_trait::async_trait;
use onera_core::domain::baseline::{
    BaselineFile, BaselineFinding, BaselineScanRun, BaselineSource, BaselineStatus,
    FileClassification, FindingCounts, GameBaseline, ScanEvidence, ScanPurpose,
    ScanScopeFingerprint, ScanState, StoreBuildIdentity,
};
use onera_core::ids::{BaselineId, BaselineScanRunId, LocalGameId};
use onera_core::ports::BaselineStore;
use onera_core::{CoreError, RelPath, Result};
use sqlx::Row as _;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Column encodings
// ---------------------------------------------------------------------------

const fn source_str(source: BaselineSource) -> &'static str {
    match source {
        BaselineSource::StoreVerifiedCapture => "store_verified_capture",
        BaselineSource::LocalSnapshot => "local_snapshot",
        BaselineSource::StoreManifest => "store_manifest",
    }
}

fn parse_source(value: &str) -> Result<BaselineSource> {
    Ok(match value {
        "store_verified_capture" => BaselineSource::StoreVerifiedCapture,
        "local_snapshot" => BaselineSource::LocalSnapshot,
        "store_manifest" => BaselineSource::StoreManifest,
        other => {
            return Err(CoreError::Database(format!(
                "unknown baseline source {other:?}"
            )))
        }
    })
}

const fn status_str(status: BaselineStatus) -> &'static str {
    match status {
        BaselineStatus::Capturing => "capturing",
        BaselineStatus::Current => "current",
        BaselineStatus::Superseded => "superseded",
        BaselineStatus::Failed => "failed",
    }
}

fn parse_status(value: &str) -> Result<BaselineStatus> {
    Ok(match value {
        "capturing" => BaselineStatus::Capturing,
        "current" => BaselineStatus::Current,
        "superseded" => BaselineStatus::Superseded,
        "failed" => BaselineStatus::Failed,
        other => {
            return Err(CoreError::Database(format!(
                "unknown baseline status {other:?}"
            )))
        }
    })
}

const fn purpose_str(purpose: ScanPurpose) -> &'static str {
    match purpose {
        ScanPurpose::Capture => "capture",
        ScanPurpose::Verify => "verify",
        ScanPurpose::CleanRestore => "clean_restore",
    }
}

fn parse_purpose(value: &str) -> Result<ScanPurpose> {
    Ok(match value {
        "capture" => ScanPurpose::Capture,
        "verify" => ScanPurpose::Verify,
        "clean_restore" => ScanPurpose::CleanRestore,
        other => {
            return Err(CoreError::Database(format!(
                "unknown scan purpose {other:?}"
            )))
        }
    })
}

const fn state_str(state: ScanState) -> &'static str {
    match state {
        ScanState::Running => "running",
        ScanState::Completed => "completed",
        ScanState::Cancelled => "cancelled",
        ScanState::Failed => "failed",
    }
}

fn parse_state(value: &str) -> Result<ScanState> {
    Ok(match value {
        "running" => ScanState::Running,
        "completed" => ScanState::Completed,
        "cancelled" => ScanState::Cancelled,
        "failed" => ScanState::Failed,
        other => return Err(CoreError::Database(format!("unknown scan state {other:?}"))),
    })
}

const fn evidence_str(evidence: ScanEvidence) -> &'static str {
    match evidence {
        ScanEvidence::ContentHashed => "content_hashed",
        ScanEvidence::MetadataOnly => "metadata_only",
    }
}

fn parse_evidence(value: &str) -> Result<ScanEvidence> {
    Ok(match value {
        "content_hashed" => ScanEvidence::ContentHashed,
        "metadata_only" => ScanEvidence::MetadataOnly,
        other => {
            return Err(CoreError::Database(format!(
                "unknown scan evidence {other:?}"
            )))
        }
    })
}

const fn classification_str(classification: FileClassification) -> &'static str {
    match classification {
        FileClassification::Matching => "matching",
        FileClassification::Modified => "modified",
        FileClassification::Missing => "missing",
        FileClassification::ExtraManaged => "extra_managed",
        FileClassification::ExtraUnknown => "extra_unknown",
        FileClassification::Unreadable => "unreadable",
        FileClassification::SpecialFile => "special_file",
    }
}

fn parse_classification(value: &str) -> Result<FileClassification> {
    Ok(match value {
        "matching" => FileClassification::Matching,
        "modified" => FileClassification::Modified,
        "missing" => FileClassification::Missing,
        "extra_managed" => FileClassification::ExtraManaged,
        "extra_unknown" => FileClassification::ExtraUnknown,
        "unreadable" => FileClassification::Unreadable,
        "special_file" => FileClassification::SpecialFile,
        other => {
            return Err(CoreError::Database(format!(
                "unknown file classification {other:?}"
            )))
        }
    })
}

fn rel_path(value: &str) -> Result<RelPath> {
    RelPath::normalize(value).map_err(|error| CoreError::Database(format!("bad path: {error}")))
}

/// A `u64` count, stored as SQLite's signed 64-bit integer.
#[allow(clippy::cast_possible_wrap)]
const fn as_i64(value: u64) -> i64 {
    value as i64
}

#[allow(clippy::cast_sign_loss)]
const fn as_u64(value: i64) -> u64 {
    if value < 0 {
        0
    } else {
        value as u64
    }
}

// ---------------------------------------------------------------------------
// Row decoding
// ---------------------------------------------------------------------------

fn baseline_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<GameBaseline> {
    let identity: Option<String> = row.try_get("build_identity").map_err(db_err)?;
    let build_identity = identity
        .as_deref()
        .map(|json| {
            serde_json::from_str::<StoreBuildIdentity>(json)
                .map_err(|error| CoreError::Database(format!("bad build identity: {error}")))
        })
        .transpose()?;
    Ok(GameBaseline {
        id: BaselineId::from(uuid(&row.try_get::<String, _>("id").map_err(db_err)?)?),
        local_game_id: LocalGameId::from(uuid(
            &row.try_get::<String, _>("local_game_id").map_err(db_err)?,
        )?),
        source: parse_source(&row.try_get::<String, _>("source").map_err(db_err)?)?,
        build_identity,
        adapter_id: row.try_get("adapter_id").map_err(db_err)?,
        reported_version: row.try_get("reported_version").map_err(db_err)?,
        status: parse_status(&row.try_get::<String, _>("status").map_err(db_err)?)?,
        captured_at: crate::convert::from_timestamp(
            &row.try_get::<String, _>("captured_at").map_err(db_err)?,
        )?,
        scope_fingerprint: ScanScopeFingerprint::from(
            row.try_get::<String, _>("scope_fingerprint")
                .map_err(db_err)?,
        ),
        file_count: as_u64(row.try_get::<i64, _>("file_count").map_err(db_err)?),
        total_bytes: as_u64(row.try_get::<i64, _>("total_bytes").map_err(db_err)?),
    })
}

fn scan_run_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<BaselineScanRun> {
    let baseline_id: Option<String> = row.try_get("baseline_id").map_err(db_err)?;
    let finished_at: Option<String> = row.try_get("finished_at").map_err(db_err)?;
    Ok(BaselineScanRun {
        id: BaselineScanRunId::from(uuid(&row.try_get::<String, _>("id").map_err(db_err)?)?),
        local_game_id: LocalGameId::from(uuid(
            &row.try_get::<String, _>("local_game_id").map_err(db_err)?,
        )?),
        baseline_id: baseline_id
            .as_deref()
            .map(|id| uuid(id).map(BaselineId::from))
            .transpose()?,
        purpose: parse_purpose(&row.try_get::<String, _>("purpose").map_err(db_err)?)?,
        state: parse_state(&row.try_get::<String, _>("state").map_err(db_err)?)?,
        evidence: parse_evidence(&row.try_get::<String, _>("evidence").map_err(db_err)?)?,
        started_at: crate::convert::from_timestamp(
            &row.try_get::<String, _>("started_at").map_err(db_err)?,
        )?,
        finished_at: finished_at
            .as_deref()
            .map(crate::convert::from_timestamp)
            .transpose()?,
        files_scanned: as_u64(row.try_get::<i64, _>("files_scanned").map_err(db_err)?),
        bytes_hashed: as_u64(row.try_get::<i64, _>("bytes_hashed").map_err(db_err)?),
        counts: FindingCounts {
            matching: as_u64(row.try_get::<i64, _>("count_matching").map_err(db_err)?),
            modified: as_u64(row.try_get::<i64, _>("count_modified").map_err(db_err)?),
            missing: as_u64(row.try_get::<i64, _>("count_missing").map_err(db_err)?),
            extra_managed: as_u64(
                row.try_get::<i64, _>("count_extra_managed")
                    .map_err(db_err)?,
            ),
            extra_unknown: as_u64(
                row.try_get::<i64, _>("count_extra_unknown")
                    .map_err(db_err)?,
            ),
            unreadable: as_u64(row.try_get::<i64, _>("count_unreadable").map_err(db_err)?),
            special: as_u64(row.try_get::<i64, _>("count_special").map_err(db_err)?),
        },
        error: row.try_get("error").map_err(db_err)?,
    })
}

const BASELINE_COLUMNS: &str = "id, local_game_id, source, build_identity, adapter_id,
     reported_version, status, captured_at, scope_fingerprint, file_count, total_bytes";

const SCAN_RUN_COLUMNS: &str = "id, local_game_id, baseline_id, purpose, state, evidence,
     started_at, finished_at, files_scanned, bytes_hashed, count_matching, count_modified,
     count_missing, count_extra_managed, count_extra_unknown, count_unreadable, count_special,
     error";

// ---------------------------------------------------------------------------
// The port
// ---------------------------------------------------------------------------

#[async_trait]
impl BaselineStore for Database {
    async fn current_baseline(&self, game: LocalGameId) -> Result<Option<GameBaseline>> {
        let row = sqlx::query(&format!(
            "SELECT {BASELINE_COLUMNS} FROM game_baselines
             WHERE local_game_id = ?1 AND status = 'current'"
        ))
        .bind(game.to_string())
        .fetch_optional(self.pool())
        .await
        .map_err(db_err)?;
        row.as_ref().map(baseline_from_row).transpose()
    }

    async fn baselines(&self, game: LocalGameId) -> Result<Vec<GameBaseline>> {
        let rows = sqlx::query(&format!(
            "SELECT {BASELINE_COLUMNS} FROM game_baselines
             WHERE local_game_id = ?1 ORDER BY captured_at DESC, id DESC"
        ))
        .bind(game.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;
        rows.iter().map(baseline_from_row).collect()
    }

    async fn put_baseline(&self, baseline: &GameBaseline, files: &[BaselineFile]) -> Result<()> {
        let mut tx = self.pool().begin().await.map_err(db_err)?;

        let existing: Option<(String,)> =
            sqlx::query_as("SELECT id FROM game_baselines WHERE id = ?1")
                .bind(baseline.id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        if existing.is_some() {
            return Err(CoreError::Conflict(format!(
                "baseline {} already exists and is immutable",
                baseline.id
            )));
        }

        // A new current baseline supersedes the previous one. The old row and
        // every file it recorded are kept: this is a lifecycle change, not a
        // deletion, so a superseded capture stays inspectable.
        if baseline.status == BaselineStatus::Current {
            sqlx::query(
                "UPDATE game_baselines SET status = 'superseded'
                 WHERE local_game_id = ?1 AND status = 'current'",
            )
            .bind(baseline.local_game_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }

        sqlx::query(
            "INSERT INTO game_baselines
                (id, local_game_id, source, build_identity, adapter_id, reported_version,
                 status, captured_at, scope_fingerprint, file_count, total_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(baseline.id.to_string())
        .bind(baseline.local_game_id.to_string())
        .bind(source_str(baseline.source))
        .bind(
            baseline
                .build_identity
                .as_ref()
                .map(|identity| {
                    serde_json::to_string(identity).map_err(|error| {
                        CoreError::Database(format!("cannot encode build identity: {error}"))
                    })
                })
                .transpose()?,
        )
        .bind(&baseline.adapter_id)
        .bind(baseline.reported_version.as_deref())
        .bind(status_str(baseline.status))
        .bind(to_timestamp(baseline.captured_at))
        .bind(baseline.scope_fingerprint.as_str())
        .bind(as_i64(baseline.file_count))
        .bind(as_i64(baseline.total_bytes))
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        for file in files {
            sqlx::query(
                "INSERT INTO baseline_files
                    (id, baseline_id, root_key, rel_path, hash, size, mode)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .bind(Uuid::new_v4().to_string())
            .bind(baseline.id.to_string())
            .bind(&file.root_key)
            .bind(file.path.as_str())
            .bind(file.hash.to_storage_string())
            .bind(as_i64(file.size))
            .bind(file.mode.map(i64::from))
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                CoreError::Database(format!(
                    "cannot record baseline file {}:{}: {error}",
                    file.root_key, file.path
                ))
            })?;
        }

        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn baseline_files(&self, baseline: BaselineId) -> Result<Vec<BaselineFile>> {
        let rows = sqlx::query(
            "SELECT root_key, rel_path, hash, size, mode FROM baseline_files
             WHERE baseline_id = ?1 ORDER BY root_key, rel_path",
        )
        .bind(baseline.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;

        let mut files = Vec::with_capacity(rows.len());
        for row in rows {
            let mode: Option<i64> = row.try_get("mode").map_err(db_err)?;
            files.push(BaselineFile {
                root_key: row.try_get("root_key").map_err(db_err)?,
                path: rel_path(&row.try_get::<String, _>("rel_path").map_err(db_err)?)?,
                hash: hash(&row.try_get::<String, _>("hash").map_err(db_err)?)?,
                size: as_u64(row.try_get::<i64, _>("size").map_err(db_err)?),
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                mode: mode.map(|mode| mode as u32),
            });
        }
        Ok(files)
    }

    async fn supersede_baseline(&self, baseline: BaselineId) -> Result<()> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT status FROM game_baselines WHERE id = ?1")
                .bind(baseline.to_string())
                .fetch_optional(self.pool())
                .await
                .map_err(db_err)?;
        let Some((status,)) = row else {
            return Err(CoreError::NotFound {
                kind: "baseline",
                id: baseline.to_string(),
            });
        };
        // A failed capture was never authoritative, so there is nothing to
        // supersede; saying so is better than silently promoting it.
        if !matches!(status.as_str(), "capturing" | "current" | "superseded") {
            return Err(CoreError::Conflict(format!(
                "baseline {baseline} is {status} and cannot be superseded"
            )));
        }
        sqlx::query("UPDATE game_baselines SET status = 'superseded' WHERE id = ?1")
            .bind(baseline.to_string())
            .execute(self.pool())
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn put_scan_run(&self, run: &BaselineScanRun) -> Result<()> {
        sqlx::query(
            "INSERT INTO baseline_scan_runs
                (id, local_game_id, baseline_id, purpose, state, evidence, started_at,
                 finished_at, files_scanned, bytes_hashed, count_matching, count_modified,
                 count_missing, count_extra_managed, count_extra_unknown, count_unreadable,
                 count_special, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                     ?17, ?18)
             ON CONFLICT(id) DO UPDATE SET
                baseline_id = excluded.baseline_id,
                state = excluded.state,
                evidence = excluded.evidence,
                finished_at = excluded.finished_at,
                files_scanned = excluded.files_scanned,
                bytes_hashed = excluded.bytes_hashed,
                count_matching = excluded.count_matching,
                count_modified = excluded.count_modified,
                count_missing = excluded.count_missing,
                count_extra_managed = excluded.count_extra_managed,
                count_extra_unknown = excluded.count_extra_unknown,
                count_unreadable = excluded.count_unreadable,
                count_special = excluded.count_special,
                error = excluded.error",
        )
        .bind(run.id.to_string())
        .bind(run.local_game_id.to_string())
        .bind(run.baseline_id.map(|id| id.to_string()))
        .bind(purpose_str(run.purpose))
        .bind(state_str(run.state))
        .bind(evidence_str(run.evidence))
        .bind(to_timestamp(run.started_at))
        .bind(run.finished_at.map(to_timestamp))
        .bind(as_i64(run.files_scanned))
        .bind(as_i64(run.bytes_hashed))
        .bind(as_i64(run.counts.matching))
        .bind(as_i64(run.counts.modified))
        .bind(as_i64(run.counts.missing))
        .bind(as_i64(run.counts.extra_managed))
        .bind(as_i64(run.counts.extra_unknown))
        .bind(as_i64(run.counts.unreadable))
        .bind(as_i64(run.counts.special))
        .bind(run.error.as_deref())
        .execute(self.pool())
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn scan_run(&self, id: BaselineScanRunId) -> Result<Option<BaselineScanRun>> {
        let row = sqlx::query(&format!(
            "SELECT {SCAN_RUN_COLUMNS} FROM baseline_scan_runs WHERE id = ?1"
        ))
        .bind(id.to_string())
        .fetch_optional(self.pool())
        .await
        .map_err(db_err)?;
        row.as_ref().map(scan_run_from_row).transpose()
    }

    async fn put_findings(
        &self,
        run: BaselineScanRunId,
        findings: &[BaselineFinding],
    ) -> Result<()> {
        let mut tx = self.pool().begin().await.map_err(db_err)?;
        let exists: Option<(String,)> =
            sqlx::query_as("SELECT id FROM baseline_scan_runs WHERE id = ?1")
                .bind(run.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        if exists.is_none() {
            return Err(CoreError::NotFound {
                kind: "baseline scan run",
                id: run.to_string(),
            });
        }

        // A scan that is re-run — resumed after a cancellation, say — replaces
        // its own findings wholesale. Appending would produce a result that is
        // partly from a walk that never finished.
        sqlx::query("DELETE FROM baseline_scan_findings WHERE scan_run_id = ?1")
            .bind(run.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

        for (seq, finding) in findings.iter().enumerate() {
            sqlx::query(
                "INSERT INTO baseline_scan_findings
                    (id, scan_run_id, seq, root_key, rel_path, classification,
                     expected_hash, observed_hash, detail)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )
            .bind(Uuid::new_v4().to_string())
            .bind(run.to_string())
            .bind(i64::try_from(seq).unwrap_or(i64::MAX))
            .bind(&finding.root_key)
            .bind(finding.path.as_str())
            .bind(classification_str(finding.classification))
            .bind(finding.expected.as_ref().map(FileHashExt::storage))
            .bind(finding.observed.as_ref().map(FileHashExt::storage))
            .bind(finding.detail.as_deref())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }

        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn findings(&self, run: BaselineScanRunId) -> Result<Vec<BaselineFinding>> {
        let rows = sqlx::query(
            "SELECT root_key, rel_path, classification, expected_hash, observed_hash, detail
             FROM baseline_scan_findings WHERE scan_run_id = ?1 ORDER BY seq",
        )
        .bind(run.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;

        let mut findings = Vec::with_capacity(rows.len());
        for row in rows {
            findings.push(BaselineFinding {
                root_key: row.try_get("root_key").map_err(db_err)?,
                path: rel_path(&row.try_get::<String, _>("rel_path").map_err(db_err)?)?,
                classification: parse_classification(
                    &row.try_get::<String, _>("classification").map_err(db_err)?,
                )?,
                expected: opt_hash(row.try_get("expected_hash").map_err(db_err)?)?,
                observed: opt_hash(row.try_get("observed_hash").map_err(db_err)?)?,
                detail: row.try_get("detail").map_err(db_err)?,
            });
        }
        Ok(findings)
    }
}

/// Small helper so an `Option<&FileHash>` can be bound without a closure.
trait FileHashExt {
    fn storage(&self) -> String;
}

impl FileHashExt for onera_core::hash::FileHash {
    fn storage(&self) -> String {
        self.to_storage_string()
    }
}
