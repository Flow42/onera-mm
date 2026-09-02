//! The operation journal.
//!
//! Writes here always precede the corresponding filesystem effect and are
//! confirmed after it. That ordering is the whole reason recovery works: a
//! crash can leave the journal *ahead* of the disk (a step recorded but not
//! performed, which recovery redoes or undoes idempotently) but never behind it
//! (a step performed but unrecorded, which recovery could not see).

use crate::convert::{from_timestamp, hash, now, opt_hash, uuid};
use crate::{db_err, Database};
use async_trait::async_trait;
use onera_core::domain::operation::{Operation, OperationKind, OperationState};
use onera_core::ids::{BackupId, LocalGameId, OperationId};
use onera_core::plan::{InstallPlan, TargetLocation};
use onera_core::ports::{JournalEntry, JournalStatus, OperationJournal};
use onera_core::{CoreError, RelPath, Result};
use sqlx::Row as _;
use std::path::PathBuf;

fn kind_str(k: OperationKind) -> &'static str {
    match k {
        OperationKind::Install => "install",
        OperationKind::Remove => "remove",
        OperationKind::Repair => "repair",
        OperationKind::Reconcile => "reconcile",
        OperationKind::CleanRestore => "clean_restore",
    }
}

fn parse_kind(s: &str) -> Result<OperationKind> {
    Ok(match s {
        "install" => OperationKind::Install,
        "remove" => OperationKind::Remove,
        "repair" => OperationKind::Repair,
        "reconcile" => OperationKind::Reconcile,
        "clean_restore" => OperationKind::CleanRestore,
        other => {
            return Err(CoreError::Database(format!(
                "unknown operation kind {other:?}"
            )))
        }
    })
}

fn parse_state(s: &str) -> Result<OperationState> {
    Ok(match s {
        "planned" => OperationState::Planned,
        "prepared" => OperationState::Prepared,
        "committing" => OperationState::Committing,
        "complete" => OperationState::Complete,
        "rolling_back" => OperationState::RollingBack,
        "rolled_back" => OperationState::RolledBack,
        "failed" => OperationState::Failed,
        other => {
            return Err(CoreError::Database(format!(
                "unknown operation state {other:?}"
            )))
        }
    })
}

fn status_str(s: JournalStatus) -> &'static str {
    match s {
        JournalStatus::Pending => "pending",
        JournalStatus::Staged => "staged",
        JournalStatus::Committed => "committed",
        JournalStatus::Skipped => "skipped",
        JournalStatus::RolledBack => "rolled_back",
    }
}

fn parse_status(s: &str) -> Result<JournalStatus> {
    Ok(match s {
        "pending" => JournalStatus::Pending,
        "staged" => JournalStatus::Staged,
        "committed" => JournalStatus::Committed,
        "skipped" => JournalStatus::Skipped,
        "rolled_back" => JournalStatus::RolledBack,
        other => {
            return Err(CoreError::Database(format!(
                "unknown journal status {other:?}"
            )))
        }
    })
}

#[async_trait]
impl OperationJournal for Database {
    async fn begin(&self, plan: &InstallPlan, kind: OperationKind) -> Result<Operation> {
        let encoded = serde_json::to_string(plan)
            .map_err(|e| CoreError::Database(format!("cannot serialize plan: {e}")))?;
        let timestamp = now();
        sqlx::query(
            "INSERT INTO operations (id, local_game_id, kind, state, plan, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'planned', ?4, ?5, ?5)",
        )
        .bind(plan.operation_id.to_string())
        .bind(plan.local_game_id.to_string())
        .bind(kind_str(kind))
        .bind(encoded)
        .bind(&timestamp)
        .execute(self.pool())
        .await
        .map_err(db_err)?;

        Ok(Operation {
            id: plan.operation_id,
            local_game_id: plan.local_game_id,
            kind,
            state: OperationState::Planned,
            created_at: from_timestamp(&timestamp)?,
            updated_at: from_timestamp(&timestamp)?,
            error: None,
        })
    }

    async fn begin_reconciliation(
        &self,
        plan: &onera_core::domain::reconcile::MutationPlan,
        kind: OperationKind,
    ) -> Result<Operation> {
        let id = OperationId::new();
        let encoded = serde_json::to_string(plan)
            .map_err(|e| CoreError::Database(format!("cannot serialize reconciliation: {e}")))?;
        let timestamp = now();
        sqlx::query(
            "INSERT INTO operations (id, local_game_id, kind, state, plan, created_at, updated_at)
             VALUES (?1, ?2, ?5, 'planned', ?3, ?4, ?4)",
        )
        .bind(id.to_string())
        .bind(plan.desired.local_game_id.to_string())
        .bind(encoded)
        .bind(&timestamp)
        .bind(kind_str(kind))
        .execute(self.pool())
        .await
        .map_err(db_err)?;
        Ok(Operation {
            id,
            local_game_id: plan.desired.local_game_id,
            kind,
            state: OperationState::Planned,
            created_at: from_timestamp(&timestamp)?,
            updated_at: from_timestamp(&timestamp)?,
            error: None,
        })
    }

    async fn set_state(
        &self,
        id: OperationId,
        state: OperationState,
        error: Option<&str>,
    ) -> Result<()> {
        // The legality check and the write happen in one transaction so two
        // concurrent callers cannot both observe the old state and both
        // advance from it.
        let mut tx = self.pool().begin().await.map_err(db_err)?;
        let row: Option<(String,)> = sqlx::query_as("SELECT state FROM operations WHERE id = ?1")
            .bind(id.to_string())
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_err)?;
        let Some((current,)) = row else {
            return Err(CoreError::NotFound {
                kind: "operation",
                id: id.to_string(),
            });
        };
        let current = parse_state(&current)?;
        if current != state && !current.can_transition_to(state) {
            return Err(CoreError::Conflict(format!(
                "operation {id} cannot move from {current} to {state}"
            )));
        }

        sqlx::query("UPDATE operations SET state = ?2, error = ?3, updated_at = ?4 WHERE id = ?1")
            .bind(id.to_string())
            .bind(state.to_string())
            .bind(error)
            .bind(now())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn get(&self, id: OperationId) -> Result<Option<Operation>> {
        let row = sqlx::query(
            "SELECT id, local_game_id, kind, state, error, created_at, updated_at
             FROM operations WHERE id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(self.pool())
        .await
        .map_err(db_err)?;
        row.map(row_to_operation).transpose()
    }

    async fn plan(&self, id: OperationId) -> Result<Option<InstallPlan>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT plan FROM operations WHERE id = ?1")
            .bind(id.to_string())
            .fetch_optional(self.pool())
            .await
            .map_err(db_err)?;
        row.map(|(json,)| {
            serde_json::from_str(&json)
                .map_err(|e| CoreError::Database(format!("stored plan is unreadable: {e}")))
        })
        .transpose()
    }

    async fn put_entry(&self, id: OperationId, entry: &JournalEntry) -> Result<()> {
        sqlx::query(
            "INSERT INTO operation_files
               (id, operation_id, seq, root_key, rel_path, abs_path, source_hash,
                previous_hash, backup_id, temp_path, status, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?12, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(operation_id, seq) DO UPDATE SET
               abs_path = ?12, source_hash = ?6, previous_hash = ?7, backup_id = ?8,
               temp_path = ?9, status = ?10, updated_at = ?11",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(id.to_string())
        .bind(i64::from(entry.seq))
        .bind(&entry.target.root_key)
        .bind(entry.target.path.as_str())
        .bind(entry.source_hash.to_storage_string())
        .bind(entry.previous_hash.as_ref().map(FileHashExt::storage))
        .bind(entry.backup_id.map(|b| b.to_string()))
        .bind(entry.temp_path.as_ref().map(|p| p.display().to_string()))
        .bind(status_str(entry.status))
        .bind(now())
        .bind(entry.absolute_path.display().to_string())
        .execute(self.pool())
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn entries(&self, id: OperationId) -> Result<Vec<JournalEntry>> {
        let rows = sqlx::query(
            "SELECT seq, root_key, rel_path, abs_path, source_hash, previous_hash,
                    backup_id, temp_path, status
             FROM operation_files WHERE operation_id = ?1 ORDER BY seq",
        )
        .bind(id.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;

        rows.into_iter()
            .map(|row| {
                let rel: String = row.try_get("rel_path").map_err(db_err)?;
                let backup: Option<String> = row.try_get("backup_id").map_err(db_err)?;
                let temp: Option<String> = row.try_get("temp_path").map_err(db_err)?;
                let status: String = row.try_get("status").map_err(db_err)?;
                let seq: i64 = row.try_get("seq").map_err(db_err)?;
                Ok(JournalEntry {
                    seq: u32::try_from(seq).unwrap_or(0),
                    target: TargetLocation {
                        root_key: row.try_get("root_key").map_err(db_err)?,
                        path: RelPath::normalize(&rel)?,
                    },
                    absolute_path: PathBuf::from(
                        row.try_get::<String, _>("abs_path").map_err(db_err)?,
                    ),
                    source_hash: hash(&row.try_get::<String, _>("source_hash").map_err(db_err)?)?,
                    previous_hash: opt_hash(row.try_get("previous_hash").map_err(db_err)?)?,
                    backup_id: backup.as_deref().map(uuid).transpose()?.map(BackupId::from),
                    temp_path: temp.map(PathBuf::from),
                    status: parse_status(&status)?,
                })
            })
            .collect()
    }

    async fn interrupted(&self) -> Result<Vec<Operation>> {
        let rows = sqlx::query(
            "SELECT id, local_game_id, kind, state, error, created_at, updated_at
             FROM operations
             WHERE state NOT IN ('complete', 'rolled_back', 'failed')
             ORDER BY created_at",
        )
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;
        rows.into_iter().map(row_to_operation).collect()
    }
}

/// Small helper so `Option<FileHash>` can be bound without a temporary.
trait FileHashExt {
    fn storage(&self) -> String;
}

impl FileHashExt for onera_core::FileHash {
    fn storage(&self) -> String {
        self.to_storage_string()
    }
}

fn row_to_operation(row: sqlx::sqlite::SqliteRow) -> Result<Operation> {
    let id: String = row.try_get("id").map_err(db_err)?;
    let game: String = row.try_get("local_game_id").map_err(db_err)?;
    let kind: String = row.try_get("kind").map_err(db_err)?;
    let state: String = row.try_get("state").map_err(db_err)?;
    Ok(Operation {
        id: OperationId::from(uuid(&id)?),
        local_game_id: LocalGameId::from(uuid(&game)?),
        kind: parse_kind(&kind)?,
        state: parse_state(&state)?,
        error: row.try_get("error").map_err(db_err)?,
        created_at: from_timestamp(&row.try_get::<String, _>("created_at").map_err(db_err)?)?,
        updated_at: from_timestamp(&row.try_get::<String, _>("updated_at").map_err(db_err)?)?,
    })
}
