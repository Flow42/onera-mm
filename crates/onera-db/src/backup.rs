//! Backups of files Onera is about to overwrite.
//!
//! Backup bytes are stored content-addressed alongside archives, so two mods
//! that both overwrite the same vanilla file share one stored copy. The
//! database row is what makes a backup findable; deleting the row does not
//! delete a blob another row still references.

use crate::convert::{now, uuid};
use crate::{db_err, Database};
use async_trait::async_trait;
use onera_core::hash::FileHash;
use onera_core::ids::{BackupId, LocalGameId};
use onera_core::plan::TargetLocation;
use onera_core::ports::BackupStore;
use onera_core::{CoreError, Result};
use std::path::{Path, PathBuf};

/// Backup storage rooted at a directory, content-addressed by BLAKE3.
#[derive(Debug, Clone)]
pub struct FileBackupStore {
    db: Database,
    root: PathBuf,
}

impl FileBackupStore {
    /// Store backups under `root`.
    #[must_use]
    pub fn new(db: Database, root: PathBuf) -> Self {
        Self { db, root }
    }

    /// Path a backup's bytes are stored at, sharded by hash prefix.
    #[must_use]
    pub fn blob_path(&self, hash: &FileHash) -> PathBuf {
        self.root
            .join(hash.algorithm.as_str())
            .join(hash.prefix(2))
            .join(&hash.hex)
    }
}

#[async_trait]
impl BackupStore for FileBackupStore {
    async fn create(
        &self,
        game: LocalGameId,
        target: &TargetLocation,
        source: &Path,
        hash: &FileHash,
        size: u64,
    ) -> Result<BackupId> {
        let blob = self.blob_path(hash);
        if let Some(parent) = blob.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| CoreError::fs(parent, e))?;
        }
        // Content-addressed: if the same bytes were already backed up, the
        // existing blob is reused rather than rewritten.
        if tokio::fs::metadata(&blob).await.is_err() {
            tokio::fs::copy(source, &blob)
                .await
                .map_err(|e| CoreError::fs(source, e))?;
        }

        let id = BackupId::new();
        sqlx::query(
            "INSERT INTO backups
               (id, local_game_id, root_key, rel_path, hash, size, stored_path, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(id.to_string())
        .bind(game.to_string())
        .bind(&target.root_key)
        .bind(target.path.as_str())
        .bind(hash.to_storage_string())
        .bind(size as i64)
        .bind(blob.display().to_string())
        .bind(now())
        .execute(self.db.pool())
        .await
        .map_err(db_err)?;
        Ok(id)
    }

    async fn path_of(&self, id: BackupId) -> Result<Option<PathBuf>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT stored_path FROM backups WHERE id = ?1")
                .bind(id.to_string())
                .fetch_optional(self.db.pool())
                .await
                .map_err(db_err)?;
        Ok(row.map(|(p,)| PathBuf::from(p)))
    }

    async fn path_of_hash(&self, hash: &FileHash) -> Result<Option<PathBuf>> {
        let blob = self.blob_path(hash);
        Ok(tokio::fs::metadata(&blob).await.is_ok().then_some(blob))
    }

    async fn delete(&self, id: BackupId) -> Result<()> {
        let mut tx = self.db.pool().begin().await.map_err(db_err)?;
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT stored_path, hash FROM backups WHERE id = ?1")
                .bind(id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        sqlx::query("DELETE FROM backups WHERE id = ?1")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

        if let Some((path, hash)) = row {
            // Only remove the blob once no other backup row points at it.
            let (remaining,): (i64,) =
                sqlx::query_as("SELECT count(*) FROM backups WHERE hash = ?1")
                    .bind(&hash)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(db_err)?;
            if remaining == 0 {
                let _ = tokio::fs::remove_file(&path).await;
            }
        }
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }
}

/// Look up a backup id from its stored uuid text, for adapters that keep ids as
/// strings.
///
/// # Errors
/// Fails if the text is not a UUID.
pub fn parse_backup_id(text: &str) -> Result<BackupId> {
    Ok(BackupId::from(uuid(text)?))
}
