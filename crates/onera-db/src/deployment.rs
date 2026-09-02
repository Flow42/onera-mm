//! Deployment state: what is on disk, and the provider stack behind each path.
//!
//! The stack is stored as ordered rows in `deployed_file_providers`. Reading it
//! is an `ORDER BY position`; writing it deletes and re-inserts the whole stack
//! inside one transaction, which keeps positions dense and makes a partial
//! write impossible.

use crate::convert::{hash, now, uuid};
use crate::{db_err, Database};
use async_trait::async_trait;
use onera_core::domain::provider_stack::{FileProvider, ProviderStack, StackEntry};
use onera_core::domain::reconcile::{InstallationMapping, MutationPlan};
use onera_core::ids::{
    ArchiveId, BackupId, InstallationId, LocalGameId, ModId, OperationId, ReleaseId,
};
use onera_core::plan::{ConflictChoice, ScopedRule, TargetLocation};
use onera_core::ports::{DeploymentStore, ReconciliationStore};
use onera_core::{CoreError, RelPath, Result};
use sqlx::Row as _;

fn choice_str(c: ConflictChoice) -> &'static str {
    match c {
        ConflictChoice::KeepExisting => "keep_existing",
        ConflictChoice::ReplaceAfterBackup => "replace_after_backup",
        ConflictChoice::AdoptExisting => "adopt_existing",
        ConflictChoice::Abort => "abort",
    }
}

fn parse_choice(s: &str) -> Result<ConflictChoice> {
    Ok(match s {
        "keep_existing" => ConflictChoice::KeepExisting,
        "replace_after_backup" => ConflictChoice::ReplaceAfterBackup,
        "adopt_existing" => ConflictChoice::AdoptExisting,
        "abort" => ConflictChoice::Abort,
        other => {
            return Err(CoreError::Database(format!(
                "unknown conflict choice {other:?}"
            )))
        }
    })
}

impl Database {
    async fn deployed_file_id(
        &self,
        game: LocalGameId,
        target: &TargetLocation,
    ) -> Result<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM deployed_files
             WHERE local_game_id = ?1 AND root_key = ?2 AND rel_path = ?3",
        )
        .bind(game.to_string())
        .bind(&target.root_key)
        .bind(target.path.as_str())
        .fetch_optional(self.pool())
        .await
        .map_err(db_err)?;
        Ok(row.map(|(id,)| id))
    }
}

#[async_trait]
impl DeploymentStore for Database {
    async fn stack(&self, game: LocalGameId, target: &TargetLocation) -> Result<ProviderStack> {
        let Some(file_id) = self.deployed_file_id(game, target).await? else {
            return Ok(ProviderStack::new());
        };
        let rows = sqlx::query(
            "SELECT provider_kind, installation_id, backup_id, hash, size
             FROM deployed_file_providers
             WHERE deployed_file_id = ?1 ORDER BY position",
        )
        .bind(&file_id)
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;

        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let kind: String = row.try_get("provider_kind").map_err(db_err)?;
            let provider = match kind.as_str() {
                "installation" => {
                    let id: String = row.try_get("installation_id").map_err(db_err)?;
                    FileProvider::Installation {
                        installation_id: InstallationId::from(uuid(&id)?),
                    }
                }
                "unmanaged" => {
                    let id: String = row.try_get("backup_id").map_err(db_err)?;
                    FileProvider::UnmanagedBackup {
                        backup_id: BackupId::from(uuid(&id)?),
                    }
                }
                other => {
                    return Err(CoreError::Database(format!(
                        "unknown provider kind {other:?}"
                    )))
                }
            };
            let size: i64 = row.try_get("size").map_err(db_err)?;
            entries.push(StackEntry {
                provider,
                hash: hash(&row.try_get::<String, _>("hash").map_err(db_err)?)?,
                size: size.max(0) as u64,
            });
        }
        Ok(ProviderStack::from_entries(entries))
    }

    async fn put_stack(
        &self,
        game: LocalGameId,
        target: &TargetLocation,
        stack: &ProviderStack,
    ) -> Result<()> {
        let mut tx = self.pool().begin().await.map_err(db_err)?;
        let existing: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM deployed_files
             WHERE local_game_id = ?1 AND root_key = ?2 AND rel_path = ?3",
        )
        .bind(game.to_string())
        .bind(&target.root_key)
        .bind(target.path.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;

        // An empty stack means nothing provides this path any more; the row and
        // its history are removed together.
        let Some(top) = stack.top() else {
            if let Some((id,)) = existing {
                sqlx::query("DELETE FROM deployed_files WHERE id = ?1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
            }
            tx.commit().await.map_err(db_err)?;
            return Ok(());
        };

        let file_id = match existing {
            Some((id,)) => {
                sqlx::query(
                    "UPDATE deployed_files SET current_hash = ?2, size = ?3, updated_at = ?4
                     WHERE id = ?1",
                )
                .bind(&id)
                .bind(top.hash.to_storage_string())
                .bind(top.size as i64)
                .bind(now())
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
                id
            }
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                sqlx::query(
                    "INSERT INTO deployed_files
                       (id, local_game_id, root_key, rel_path, current_hash, size, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )
                .bind(&id)
                .bind(game.to_string())
                .bind(&target.root_key)
                .bind(target.path.as_str())
                .bind(top.hash.to_storage_string())
                .bind(top.size as i64)
                .bind(now())
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
                id
            }
        };

        sqlx::query("DELETE FROM deployed_file_providers WHERE deployed_file_id = ?1")
            .bind(&file_id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

        for (position, entry) in stack.entries().iter().enumerate() {
            let (kind, installation, backup) = match entry.provider {
                FileProvider::Installation { installation_id } => {
                    ("installation", Some(installation_id.to_string()), None)
                }
                FileProvider::UnmanagedBackup { backup_id } => {
                    ("unmanaged", None, Some(backup_id.to_string()))
                }
            };
            sqlx::query(
                "INSERT INTO deployed_file_providers
                   (id, deployed_file_id, position, provider_kind,
                    installation_id, backup_id, hash, size, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(&file_id)
            .bind(position as i64)
            .bind(kind)
            .bind(installation)
            .bind(backup)
            .bind(entry.hash.to_storage_string())
            .bind(entry.size as i64)
            .bind(now())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }

        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn targets_of(&self, installation: InstallationId) -> Result<Vec<TargetLocation>> {
        let rows = sqlx::query(
            "SELECT DISTINCT f.root_key, f.rel_path
             FROM deployed_files f
             JOIN deployed_file_providers p ON p.deployed_file_id = f.id
             WHERE p.installation_id = ?1
             ORDER BY f.root_key, f.rel_path",
        )
        .bind(installation.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;
        rows.into_iter().map(row_to_target).collect()
    }

    async fn all_targets(&self, game: LocalGameId) -> Result<Vec<TargetLocation>> {
        let rows = sqlx::query(
            "SELECT root_key, rel_path FROM deployed_files
             WHERE local_game_id = ?1 ORDER BY root_key, rel_path",
        )
        .bind(game.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;
        rows.into_iter().map(row_to_target).collect()
    }

    async fn record_installation(
        &self,
        installation: InstallationId,
        game: LocalGameId,
        mod_id: ModId,
        release: ReleaseId,
        archive: ArchiveId,
    ) -> Result<()> {
        let mut tx = self.pool().begin().await.map_err(db_err)?;
        // Installing a new artifact in a lineage is an upgrade/downgrade. Keep
        // the previous artifact, but release the unique active slot first.
        sqlx::query(
            "UPDATE installations
             SET active = 0, state = 'artifact', deactivated_at = ?4
             WHERE local_game_id = ?1 AND mod_id = ?2 AND id != ?3 AND active = 1",
        )
        .bind(game.to_string())
        .bind(mod_id.to_string())
        .bind(installation.to_string())
        .bind(now())
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        sqlx::query(
            "INSERT INTO installations
               (id, local_game_id, mod_id, release_id, archive_id, state, installed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'installed', ?6)
             ON CONFLICT(id) DO UPDATE SET
               state = 'installed', active = 1, deactivated_at = NULL, installed_at = ?6",
        )
        .bind(installation.to_string())
        .bind(game.to_string())
        .bind(mod_id.to_string())
        .bind(release.to_string())
        .bind(archive.to_string())
        .bind(now())
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn remove_installation(&self, installation: InstallationId) -> Result<()> {
        sqlx::query("DELETE FROM installations WHERE id = ?1")
            .bind(installation.to_string())
            .execute(self.pool())
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn deactivate_installation(&self, installation: InstallationId) -> Result<()> {
        sqlx::query(
            "UPDATE installations
             SET active = 0, state = 'artifact', deactivated_at = ?2
             WHERE id = ?1",
        )
        .bind(installation.to_string())
        .bind(now())
        .execute(self.pool())
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn activate_installation(&self, installation: InstallationId) -> Result<()> {
        sqlx::query(
            "UPDATE installations
             SET active = 1, state = 'installed', deactivated_at = NULL
             WHERE id = ?1",
        )
        .bind(installation.to_string())
        .execute(self.pool())
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn put_mapping(&self, mapping: &InstallationMapping) -> Result<()> {
        sqlx::query(
            "INSERT INTO installation_mappings
               (id, installation_id, root_key, rel_path, source_path, source_hash, source_size, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(installation_id, root_key, rel_path) DO UPDATE SET
               source_path = excluded.source_path,
               source_hash = excluded.source_hash,
               source_size = excluded.source_size",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(mapping.installation_id.to_string())
        .bind(&mapping.target.root_key)
        .bind(mapping.target.path.as_str())
        .bind(mapping.source.as_str())
        .bind(mapping.source_hash.to_storage_string())
        .bind(mapping.source_size as i64)
        .bind(now())
        .execute(self.pool())
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn mappings_of(&self, installation: InstallationId) -> Result<Vec<InstallationMapping>> {
        let rows = sqlx::query(
            "SELECT root_key, rel_path, source_path, source_hash, source_size
             FROM installation_mappings WHERE installation_id = ?1
             ORDER BY root_key, rel_path",
        )
        .bind(installation.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;
        rows.into_iter()
            .map(|row| {
                let source_path: String = row.try_get("source_path").map_err(db_err)?;
                let rel_path: String = row.try_get("rel_path").map_err(db_err)?;
                let source_hash: String = row.try_get("source_hash").map_err(db_err)?;
                let source_size: i64 = row.try_get("source_size").map_err(db_err)?;
                Ok(InstallationMapping {
                    installation_id: installation,
                    source: RelPath::normalize(&source_path)?,
                    target: TargetLocation {
                        root_key: row.try_get("root_key").map_err(db_err)?,
                        path: RelPath::normalize(&rel_path)?,
                    },
                    source_hash: hash(&source_hash)?,
                    source_size: source_size.max(0) as u64,
                })
            })
            .collect()
    }

    async fn record_created_dirs(
        &self,
        game: LocalGameId,
        installation: InstallationId,
        dirs: &[TargetLocation],
    ) -> Result<()> {
        let mut tx = self.pool().begin().await.map_err(db_err)?;
        for dir in dirs {
            sqlx::query(
                "INSERT INTO created_directories
                   (id, local_game_id, installation_id, root_key, rel_path, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(installation_id, root_key, rel_path) DO NOTHING",
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(game.to_string())
            .bind(installation.to_string())
            .bind(&dir.root_key)
            .bind(dir.path.as_str())
            .bind(now())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    async fn created_dirs(&self, installation: InstallationId) -> Result<Vec<TargetLocation>> {
        // Ordered by length so callers can remove the deepest directories first.
        let rows = sqlx::query(
            "SELECT root_key, rel_path FROM created_directories
             WHERE installation_id = ?1 ORDER BY length(rel_path), rel_path",
        )
        .bind(installation.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;
        rows.into_iter().map(row_to_target).collect()
    }

    async fn installations_of_mod(
        &self,
        game: LocalGameId,
        mod_id: ModId,
    ) -> Result<Vec<InstallationId>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT id FROM installations
                 WHERE local_game_id = ?1 AND mod_id = ?2 AND active = 1",
        )
        .bind(game.to_string())
        .bind(mod_id.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;
        rows.into_iter()
            .map(|(id,)| Ok(InstallationId::from(uuid(&id)?)))
            .collect()
    }

    async fn record_history(
        &self,
        game: LocalGameId,
        target: &TargetLocation,
        operation: OperationId,
        event: &str,
        entry: Option<&StackEntry>,
    ) -> Result<()> {
        let Some(file_id) = self.deployed_file_id(game, target).await? else {
            // History is an audit trail of a tracked path; if the path is not
            // tracked there is nothing to attach the entry to.
            return Ok(());
        };
        sqlx::query(
            "INSERT INTO file_provider_history
               (id, deployed_file_id, operation_id, event, installation_id, hash, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(file_id)
        .bind(operation.to_string())
        .bind(event)
        .bind(
            entry
                .and_then(|e| e.provider.installation_id())
                .map(|i| i.to_string()),
        )
        .bind(entry.map(|e| e.hash.to_storage_string()))
        .bind(now())
        .execute(self.pool())
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn rules_for(&self, mod_id: ModId) -> Result<Vec<ScopedRule>> {
        let rows =
            sqlx::query("SELECT root_key, path_prefix, choice FROM scoped_rules WHERE mod_id = ?1")
                .bind(mod_id.to_string())
                .fetch_all(self.pool())
                .await
                .map_err(db_err)?;
        rows.into_iter()
            .map(|row| {
                let choice: String = row.try_get("choice").map_err(db_err)?;
                Ok(ScopedRule {
                    mod_id,
                    root_key: row.try_get("root_key").map_err(db_err)?,
                    path_prefix: row.try_get("path_prefix").map_err(db_err)?,
                    choice: parse_choice(&choice)?,
                })
            })
            .collect()
    }

    async fn put_rule(&self, rule: &ScopedRule) -> Result<()> {
        sqlx::query(
            "INSERT INTO scoped_rules (id, mod_id, root_key, path_prefix, choice, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(mod_id, root_key, path_prefix) DO UPDATE SET choice = ?5",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(rule.mod_id.to_string())
        .bind(&rule.root_key)
        .bind(&rule.path_prefix)
        .bind(choice_str(rule.choice))
        .bind(now())
        .execute(self.pool())
        .await
        .map_err(db_err)?;
        Ok(())
    }
}

fn row_to_target(row: sqlx::sqlite::SqliteRow) -> Result<TargetLocation> {
    let rel: String = row.try_get("rel_path").map_err(db_err)?;
    Ok(TargetLocation {
        root_key: row.try_get("root_key").map_err(db_err)?,
        path: RelPath::normalize(&rel)?,
    })
}

#[async_trait]
impl ReconciliationStore for Database {
    async fn complete_reconciliation(
        &self,
        operation: OperationId,
        plan: &MutationPlan,
    ) -> Result<()> {
        let mut tx = self.pool().begin().await.map_err(db_err)?;
        for (target, stack) in &plan.final_stacks {
            let existing: Option<(String,)> = sqlx::query_as(
                "SELECT id FROM deployed_files WHERE local_game_id = ?1 AND root_key = ?2 AND rel_path = ?3",
            )
            .bind(plan.desired.local_game_id.to_string())
            .bind(&target.root_key)
            .bind(target.path.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_err)?;
            let Some(top) = stack.top() else {
                if let Some((id,)) = existing {
                    sqlx::query("DELETE FROM deployed_files WHERE id = ?1")
                        .bind(id)
                        .execute(&mut *tx)
                        .await
                        .map_err(db_err)?;
                }
                continue;
            };
            let file_id = if let Some((id,)) = existing {
                sqlx::query("UPDATE deployed_files SET current_hash = ?2, size = ?3, updated_at = ?4 WHERE id = ?1")
                    .bind(&id).bind(top.hash.to_storage_string()).bind(top.size as i64).bind(now())
                    .execute(&mut *tx).await.map_err(db_err)?;
                id
            } else {
                let id = uuid::Uuid::new_v4().to_string();
                sqlx::query("INSERT INTO deployed_files (id, local_game_id, root_key, rel_path, current_hash, size, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)")
                    .bind(&id).bind(plan.desired.local_game_id.to_string()).bind(&target.root_key)
                    .bind(target.path.as_str()).bind(top.hash.to_storage_string()).bind(top.size as i64).bind(now())
                    .execute(&mut *tx).await.map_err(db_err)?;
                id
            };
            sqlx::query("DELETE FROM deployed_file_providers WHERE deployed_file_id = ?1")
                .bind(&file_id)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
            for (position, entry) in stack.entries().iter().enumerate() {
                let (kind, installation, backup) = match entry.provider {
                    FileProvider::Installation { installation_id } => {
                        ("installation", Some(installation_id.to_string()), None)
                    }
                    FileProvider::UnmanagedBackup { backup_id } => {
                        ("unmanaged", None, Some(backup_id.to_string()))
                    }
                };
                sqlx::query("INSERT INTO deployed_file_providers (id, deployed_file_id, position, provider_kind, installation_id, backup_id, hash, size, recorded_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)")
                    .bind(uuid::Uuid::new_v4().to_string()).bind(&file_id).bind(position as i64).bind(kind)
                    .bind(installation).bind(backup).bind(entry.hash.to_storage_string()).bind(entry.size as i64).bind(now())
                    .execute(&mut *tx).await.map_err(db_err)?;
            }
        }
        let wanted: std::collections::BTreeSet<_> = plan
            .desired
            .installations
            .iter()
            .map(ToString::to_string)
            .collect();
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT id FROM installations WHERE local_game_id = ?1")
                .bind(plan.desired.local_game_id.to_string())
                .fetch_all(&mut *tx)
                .await
                .map_err(db_err)?;
        // Release every unique active slot before selecting the desired rows.
        sqlx::query("UPDATE installations SET active = 0, state = 'artifact', deactivated_at = ?2 WHERE local_game_id = ?1")
            .bind(plan.desired.local_game_id.to_string()).bind(now())
            .execute(&mut *tx).await.map_err(db_err)?;
        let existing: std::collections::BTreeSet<_> = rows.into_iter().map(|(id,)| id).collect();
        for id in &wanted {
            if !existing.contains(id) {
                return Err(CoreError::NotFound {
                    kind: "retained installation",
                    id: id.clone(),
                });
            }
            sqlx::query("UPDATE installations SET active = 1, state = 'installed', deactivated_at = NULL WHERE id = ?1")
                .bind(id)
                .execute(&mut *tx).await.map_err(db_err)?;
        }
        let result = sqlx::query("UPDATE operations SET state = 'complete', updated_at = ?2 WHERE id = ?1 AND state = 'committing'")
            .bind(operation.to_string()).bind(now()).execute(&mut *tx).await.map_err(db_err)?;
        if result.rows_affected() != 1 {
            return Err(CoreError::Conflict(format!(
                "operation {operation} is not committing"
            )));
        }
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }
}
