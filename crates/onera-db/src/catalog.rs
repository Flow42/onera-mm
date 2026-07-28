//! Catalogue persistence: providers, games, mods, releases, files and archives.
//!
//! These are the tables that mirror provider metadata. Everything here is
//! cache-like: it can be re-fetched. The deployment tables are not, which is
//! why they live in [`crate::deployment`] and are written transactionally.

use crate::convert::{from_timestamp, now, opt_hash, to_timestamp, uuid};
use crate::{db_err, Database};
use chrono::{DateTime, Utc};
use onera_core::domain::archive::ArchiveFormat;
use onera_core::domain::game::{Game, InstallSource, LocalGameInstall};
use onera_core::domain::release::{FileCategory, Mod, ProviderFile, Release};
use onera_core::hash::FileHash;
use onera_core::ids::{
    ArchiveId, GameId, LocalGameId, ModId, ProviderFileId, ProviderId, ProviderModId, ReleaseId,
};
use onera_core::{CoreError, Result};
use sqlx::Row as _;
use std::path::PathBuf;

fn source_str(s: InstallSource) -> &'static str {
    match s {
        InstallSource::SteamNative => "steam_native",
        InstallSource::SteamFlatpak => "steam_flatpak",
        InstallSource::Manual => "manual",
    }
}

fn parse_source(s: &str) -> Result<InstallSource> {
    Ok(match s {
        "steam_native" => InstallSource::SteamNative,
        "steam_flatpak" => InstallSource::SteamFlatpak,
        "manual" => InstallSource::Manual,
        other => {
            return Err(CoreError::Database(format!(
                "unknown install source {other:?}"
            )))
        }
    })
}

fn category_str(c: FileCategory) -> &'static str {
    match c {
        FileCategory::Main => "main",
        FileCategory::Update => "update",
        FileCategory::Optional => "optional",
        FileCategory::OldVersion => "old_version",
        FileCategory::Miscellaneous => "miscellaneous",
        FileCategory::Unknown => "unknown",
    }
}

fn parse_category(s: &str) -> FileCategory {
    match s {
        "main" => FileCategory::Main,
        "update" => FileCategory::Update,
        "optional" => FileCategory::Optional,
        "old_version" => FileCategory::OldVersion,
        "miscellaneous" => FileCategory::Miscellaneous,
        _ => FileCategory::Unknown,
    }
}

impl Database {
    /// Register a provider, or update its display name and base URL.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn upsert_provider(&self, id: &ProviderId, name: &str, api_base: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO providers (id, name, api_base, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET name = ?2, api_base = ?3",
        )
        .bind(id.as_str())
        .bind(name)
        .bind(api_base)
        .bind(now())
        .execute(self.pool())
        .await
        .map_err(db_err)?;
        Ok(())
    }

    /// Cache a provider's game, returning the stored identity.
    ///
    /// Matching is on `(provider, provider_slug)`, so re-fetching the catalogue
    /// updates names in place instead of duplicating games.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn upsert_game(&self, game: &Game) -> Result<GameId> {
        let existing: Option<(String,)> =
            sqlx::query_as("SELECT id FROM games WHERE provider_id = ?1 AND provider_slug = ?2")
                .bind(game.provider.as_str())
                .bind(&game.provider_slug)
                .fetch_optional(self.pool())
                .await
                .map_err(db_err)?;

        if let Some((id,)) = existing {
            sqlx::query(
                "UPDATE games SET name = ?2, steam_app_id = ?3, cached_at = ?4 WHERE id = ?1",
            )
            .bind(&id)
            .bind(&game.name)
            .bind(game.steam_app_id.map(i64::from))
            .bind(now())
            .execute(self.pool())
            .await
            .map_err(db_err)?;
            return Ok(GameId::from(uuid(&id)?));
        }

        sqlx::query(
            "INSERT INTO games (id, provider_id, provider_slug, name, steam_app_id, cached_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(game.id.to_string())
        .bind(game.provider.as_str())
        .bind(&game.provider_slug)
        .bind(&game.name)
        .bind(game.steam_app_id.map(i64::from))
        .bind(now())
        .execute(self.pool())
        .await
        .map_err(db_err)?;
        Ok(game.id)
    }

    /// All cached games for a provider.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn games(&self, provider: &ProviderId) -> Result<Vec<Game>> {
        let rows = sqlx::query(
            "SELECT id, provider_slug, name, steam_app_id FROM games
             WHERE provider_id = ?1 ORDER BY name",
        )
        .bind(provider.as_str())
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;
        rows.into_iter()
            .map(|row| {
                let id: String = row.try_get("id").map_err(db_err)?;
                let steam: Option<i64> = row.try_get("steam_app_id").map_err(db_err)?;
                Ok(Game {
                    id: GameId::from(uuid(&id)?),
                    provider: provider.clone(),
                    provider_slug: row.try_get("provider_slug").map_err(db_err)?,
                    name: row.try_get("name").map_err(db_err)?,
                    steam_app_id: steam.and_then(|v| u32::try_from(v).ok()),
                })
            })
            .collect()
    }

    /// When the game catalogue for a provider was last refreshed.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn games_cached_at(&self, provider: &ProviderId) -> Result<Option<DateTime<Utc>>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT max(cached_at) FROM games WHERE provider_id = ?1")
                .bind(provider.as_str())
                .fetch_optional(self.pool())
                .await
                .map_err(db_err)?;
        row.map(|(t,)| from_timestamp(&t)).transpose()
    }

    /// Record a detected or manually added local game installation.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn upsert_local_install(&self, install: &LocalGameInstall) -> Result<LocalGameId> {
        let roots = serde_json::to_string(&install.user_data_roots)
            .map_err(|e| CoreError::Database(e.to_string()))?;
        let existing: Option<(String,)> =
            sqlx::query_as("SELECT id FROM local_game_installs WHERE install_root = ?1")
                .bind(install.install_root.display().to_string())
                .fetch_optional(self.pool())
                .await
                .map_err(db_err)?;

        if let Some((id,)) = existing {
            sqlx::query(
                "UPDATE local_game_installs
                 SET game_id = ?2, adapter_id = ?3, source = ?4, compat_prefix = ?5,
                     user_data_roots = ?6, confirmed = ?7
                 WHERE id = ?1",
            )
            .bind(&id)
            .bind(install.game_id.to_string())
            .bind(&install.adapter_id)
            .bind(source_str(install.source))
            .bind(
                install
                    .compat_prefix
                    .as_ref()
                    .map(|p| p.display().to_string()),
            )
            .bind(&roots)
            .bind(i64::from(install.confirmed))
            .execute(self.pool())
            .await
            .map_err(db_err)?;
            return Ok(LocalGameId::from(uuid(&id)?));
        }

        sqlx::query(
            "INSERT INTO local_game_installs
               (id, game_id, adapter_id, source, install_root, compat_prefix,
                user_data_roots, confirmed, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .bind(install.id.to_string())
        .bind(install.game_id.to_string())
        .bind(&install.adapter_id)
        .bind(source_str(install.source))
        .bind(install.install_root.display().to_string())
        .bind(
            install
                .compat_prefix
                .as_ref()
                .map(|p| p.display().to_string()),
        )
        .bind(&roots)
        .bind(i64::from(install.confirmed))
        .bind(now())
        .execute(self.pool())
        .await
        .map_err(db_err)?;
        Ok(install.id)
    }

    /// Every known local installation.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn local_installs(&self) -> Result<Vec<LocalGameInstall>> {
        let rows = sqlx::query(
            "SELECT id, game_id, adapter_id, source, install_root, compat_prefix,
                    user_data_roots, confirmed
             FROM local_game_installs ORDER BY install_root",
        )
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;
        rows.into_iter()
            .map(|row| {
                let id: String = row.try_get("id").map_err(db_err)?;
                let game_id: String = row.try_get("game_id").map_err(db_err)?;
                let source: String = row.try_get("source").map_err(db_err)?;
                let roots: String = row.try_get("user_data_roots").map_err(db_err)?;
                let compat: Option<String> = row.try_get("compat_prefix").map_err(db_err)?;
                let install_root: String = row.try_get("install_root").map_err(db_err)?;
                let confirmed: i64 = row.try_get("confirmed").map_err(db_err)?;
                Ok(LocalGameInstall {
                    id: LocalGameId::from(uuid(&id)?),
                    game_id: GameId::from(uuid(&game_id)?),
                    adapter_id: row.try_get("adapter_id").map_err(db_err)?,
                    source: parse_source(&source)?,
                    install_root: PathBuf::from(install_root),
                    compat_prefix: compat.map(PathBuf::from),
                    user_data_roots: serde_json::from_str(&roots)
                        .map_err(|e| CoreError::Database(e.to_string()))?,
                    confirmed: confirmed != 0,
                })
            })
            .collect()
    }

    /// Mark a detected installation as confirmed by the user.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn confirm_local_install(&self, id: LocalGameId) -> Result<()> {
        sqlx::query("UPDATE local_game_installs SET confirmed = 1 WHERE id = ?1")
            .bind(id.to_string())
            .execute(self.pool())
            .await
            .map_err(db_err)?;
        Ok(())
    }

    /// Cache a mod's metadata.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn upsert_mod(&self, m: &Mod) -> Result<ModId> {
        let existing: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM mods WHERE provider_id = ?1 AND game_slug = ?2 AND provider_mod_id = ?3",
        )
        .bind(m.provider.as_str())
        .bind(&m.game_slug)
        .bind(m.provider_mod_id.as_str())
        .fetch_optional(self.pool())
        .await
        .map_err(db_err)?;

        if let Some((id,)) = existing {
            sqlx::query("UPDATE mods SET name = ?2, author = ?3, updated_at = ?4 WHERE id = ?1")
                .bind(&id)
                .bind(&m.name)
                .bind(&m.author)
                .bind(now())
                .execute(self.pool())
                .await
                .map_err(db_err)?;
            return Ok(ModId::from(uuid(&id)?));
        }

        sqlx::query(
            "INSERT INTO mods (id, provider_id, provider_mod_id, game_slug, name, author, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(m.id.to_string())
        .bind(m.provider.as_str())
        .bind(m.provider_mod_id.as_str())
        .bind(&m.game_slug)
        .bind(&m.name)
        .bind(&m.author)
        .bind(now())
        .execute(self.pool())
        .await
        .map_err(db_err)?;
        Ok(m.id)
    }

    /// Find a cached mod by its provider identity.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn find_mod(
        &self,
        provider: &ProviderId,
        game_slug: &str,
        provider_mod_id: &ProviderModId,
    ) -> Result<Option<Mod>> {
        let row = sqlx::query(
            "SELECT id, name, author FROM mods
             WHERE provider_id = ?1 AND game_slug = ?2 AND provider_mod_id = ?3",
        )
        .bind(provider.as_str())
        .bind(game_slug)
        .bind(provider_mod_id.as_str())
        .fetch_optional(self.pool())
        .await
        .map_err(db_err)?;

        row.map(|row| {
            let id: String = row.try_get("id").map_err(db_err)?;
            Ok(Mod {
                id: ModId::from(uuid(&id)?),
                provider: provider.clone(),
                provider_mod_id: provider_mod_id.clone(),
                game_slug: game_slug.to_owned(),
                name: row.try_get("name").map_err(db_err)?,
                author: row.try_get("author").map_err(db_err)?,
            })
        })
        .transpose()
    }

    /// Cache a release. The version string is stored verbatim.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn upsert_release(&self, r: &Release) -> Result<ReleaseId> {
        let published = r.published_at.map(to_timestamp);
        let existing: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM releases WHERE mod_id = ?1 AND version = ?2
             AND coalesce(published_at, '') = coalesce(?3, '')",
        )
        .bind(r.mod_id.to_string())
        .bind(&r.version)
        .bind(&published)
        .fetch_optional(self.pool())
        .await
        .map_err(db_err)?;
        if let Some((id,)) = existing {
            return Ok(ReleaseId::from(uuid(&id)?));
        }

        sqlx::query(
            "INSERT INTO releases (id, mod_id, version, published_at, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(r.id.to_string())
        .bind(r.mod_id.to_string())
        .bind(&r.version)
        .bind(&published)
        .bind(r.metadata.to_string())
        .execute(self.pool())
        .await
        .map_err(db_err)?;
        Ok(r.id)
    }

    /// Cache a downloadable provider file.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn upsert_provider_file(&self, f: &ProviderFile) -> Result<()> {
        sqlx::query(
            "INSERT INTO provider_files
               (id, release_id, provider_id, provider_file_id, name, size_bytes,
                category, published_hash, uploaded_at, is_primary)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(provider_id, provider_file_id) DO UPDATE SET
               release_id = ?2, name = ?5, size_bytes = ?6, category = ?7,
               published_hash = ?8, uploaded_at = ?9, is_primary = ?10",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(f.release_id.to_string())
        .bind(f.provider.as_str())
        .bind(f.provider_file_id.as_str())
        .bind(&f.name)
        .bind(f.size_bytes.map(|s| s as i64))
        .bind(category_str(f.category))
        .bind(f.published_hash.as_ref().map(FileHash::to_storage_string))
        .bind(f.uploaded_at.map(to_timestamp))
        .bind(i64::from(f.is_primary))
        .execute(self.pool())
        .await
        .map_err(db_err)?;
        Ok(())
    }

    /// Files cached for a release.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn provider_files(&self, release: ReleaseId) -> Result<Vec<ProviderFile>> {
        let rows = sqlx::query(
            "SELECT provider_id, provider_file_id, name, size_bytes, category,
                    published_hash, uploaded_at, is_primary
             FROM provider_files WHERE release_id = ?1 ORDER BY name",
        )
        .bind(release.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;

        rows.into_iter()
            .map(|row| {
                let provider: String = row.try_get("provider_id").map_err(db_err)?;
                let file_id: String = row.try_get("provider_file_id").map_err(db_err)?;
                let category: String = row.try_get("category").map_err(db_err)?;
                let size: Option<i64> = row.try_get("size_bytes").map_err(db_err)?;
                let uploaded: Option<String> = row.try_get("uploaded_at").map_err(db_err)?;
                let primary: i64 = row.try_get("is_primary").map_err(db_err)?;
                Ok(ProviderFile {
                    provider: ProviderId::new(provider),
                    provider_file_id: ProviderFileId::new(file_id),
                    release_id: release,
                    name: row.try_get("name").map_err(db_err)?,
                    size_bytes: size.map(|s| s as u64),
                    category: parse_category(&category),
                    published_hash: opt_hash(row.try_get("published_hash").map_err(db_err)?)?,
                    uploaded_at: uploaded.as_deref().map(from_timestamp).transpose()?,
                    is_primary: primary != 0,
                })
            })
            .collect()
    }

    /// Record an archive in content-addressed storage.
    ///
    /// Returns the existing id when the same bytes are already stored, which is
    /// how download deduplication is recorded.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn upsert_archive(
        &self,
        hash: &FileHash,
        size: u64,
        original_filename: &str,
        format: ArchiveFormat,
        stored_path: &std::path::Path,
    ) -> Result<ArchiveId> {
        let existing: Option<(String,)> = sqlx::query_as("SELECT id FROM archives WHERE hash = ?1")
            .bind(hash.to_storage_string())
            .fetch_optional(self.pool())
            .await
            .map_err(db_err)?;
        if let Some((id,)) = existing {
            return Ok(ArchiveId::from(uuid(&id)?));
        }

        let id = ArchiveId::new();
        sqlx::query(
            "INSERT INTO archives
               (id, hash, size, original_filename, format, stored_path, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(id.to_string())
        .bind(hash.to_storage_string())
        .bind(size as i64)
        .bind(original_filename)
        .bind(format!("{format:?}").to_lowercase())
        .bind(stored_path.display().to_string())
        .bind(now())
        .execute(self.pool())
        .await
        .map_err(db_err)?;
        Ok(id)
    }

    /// Record the manifest of an extracted archive.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn record_archive_entries(
        &self,
        archive: ArchiveId,
        manifest: &onera_core::domain::archive::ArchiveManifest,
    ) -> Result<()> {
        let mut tx = self.pool().begin().await.map_err(db_err)?;
        for file in &manifest.files {
            sqlx::query(
                "INSERT INTO archive_entries (id, archive_id, path, kind, size, hash, executable)
                 VALUES (?1, ?2, ?3, 'file', ?4, ?5, ?6)
                 ON CONFLICT(archive_id, path) DO UPDATE SET
                   size = ?4, hash = ?5, executable = ?6",
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(archive.to_string())
            .bind(file.path.as_str())
            .bind(file.size as i64)
            .bind(file.hash.to_storage_string())
            .bind(i64::from(file.executable))
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }
}
