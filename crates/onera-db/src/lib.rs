//! SQLite persistence.
//!
//! One connection pool, one migration set, and implementations of the
//! persistence ports declared in [`onera_core::ports`]. Nothing above this
//! crate knows that SQLite is involved.
//!
//! Queries are written as runtime `sqlx::query` calls rather than the
//! compile-time macros so that building Onera never requires a live database or
//! a checked-in `.sqlx` cache — CI stays a plain `cargo build`.
//!
//! Three settings matter for correctness and are applied to every connection:
//!
//! * `foreign_keys = ON` — SQLite defaults it off, which would silently orphan
//!   provider-stack rows when an installation is deleted.
//! * `journal_mode = WAL` — the UI reads while an install writes.
//! * `busy_timeout` — deployments are serialized per game, but the UI and the
//!   Native Messaging host still contend for the same file.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod backup;
pub mod baseline;
pub mod catalog;
pub mod convert;
pub mod dependency;
pub mod deployment;
pub mod jobs;
pub mod journal;
pub mod profiles;

use onera_core::{CoreError, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

/// Embedded migrations, applied on connect.
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// The schema version this build understands.
pub const SCHEMA_VERSION: i64 = 7;

/// A pooled SQLite database.
#[derive(Debug, Clone)]
pub struct Database {
    pool: SqlitePool,
}

impl Database {
    /// Open (creating if needed) the database at `path` and migrate it.
    ///
    /// # Errors
    /// Fails if the file cannot be opened or a migration fails.
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| CoreError::fs(parent, e))?;
        }
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .map_err(db_err)?
            .create_if_missing(true)
            .foreign_keys(true)
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(Duration::from_secs(15));
        Self::from_options(options).await
    }

    /// Open an in-memory database, for tests.
    ///
    /// # Errors
    /// Fails if migrations fail.
    pub async fn open_in_memory() -> Result<Self> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .map_err(db_err)?
            .foreign_keys(true);
        Self::from_options(options).await
    }

    async fn from_options(options: SqliteConnectOptions) -> Result<Self> {
        // An in-memory database lives only as long as its connection, so the
        // pool is capped at one connection to keep every query on the same one.
        let is_memory = format!("{options:?}").contains("memory");
        let pool = SqlitePoolOptions::new()
            .max_connections(if is_memory { 1 } else { 8 })
            .connect_with(options)
            .await
            .map_err(db_err)?;
        MIGRATOR
            .run(&pool)
            .await
            .map_err(|e| CoreError::Database(format!("migration failed: {e}")))?;
        let db = Self { pool };
        db.check_schema_version().await?;
        Ok(db)
    }

    /// The underlying pool, for adapters that need their own queries.
    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Refuse to run against a database written by a newer Onera.
    ///
    /// Downgrading is the one migration direction that cannot be made safe, so
    /// it is rejected loudly instead of corrupting deployment state.
    async fn check_schema_version(&self) -> Result<()> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT value FROM schema_meta WHERE key = 'schema_version'")
                .fetch_optional(&self.pool)
                .await
                .map_err(db_err)?;
        let found: i64 = row
            .map(|(v,)| v.parse().unwrap_or(0))
            .unwrap_or(SCHEMA_VERSION);
        if found > SCHEMA_VERSION {
            return Err(CoreError::Database(format!(
                "database schema version {found} is newer than this build understands ({SCHEMA_VERSION}); upgrade Onera"
            )));
        }
        Ok(())
    }

    /// Record which adapter version last wrote state.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn set_adapter_version(&self, adapter_id: &str, version: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO adapter_versions (adapter_id, version, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(adapter_id) DO UPDATE SET version = ?2, updated_at = ?3",
        )
        .bind(adapter_id)
        .bind(version)
        .bind(convert::now())
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    /// The recorded version for an adapter, if any.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn adapter_version(&self, adapter_id: &str) -> Result<Option<i64>> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT version FROM adapter_versions WHERE adapter_id = ?1")
                .bind(adapter_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(db_err)?;
        Ok(row.map(|(v,)| v))
    }
}

/// Map a `sqlx` error into a core error.
pub(crate) fn db_err(e: impl std::fmt::Display) -> CoreError {
    CoreError::Database(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_apply_to_a_fresh_database() {
        let db = Database::open_in_memory().await.unwrap();
        let (count,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM sqlite_master WHERE type = 'table'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert!(count > 15, "expected the full schema, found {count} tables");
    }

    /// Write a populated schema-v1 database at `path`, exactly as a user who
    /// installed the first Onera release would have.
    async fn write_populated_v1_database(path: &Path) {
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .unwrap()
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!("../migrations/0001_initial.sql"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE _sqlx_migrations (
                version BIGINT PRIMARY KEY,
                description TEXT NOT NULL,
                installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                success BOOLEAN NOT NULL,
                checksum BLOB NOT NULL,
                execution_time BIGINT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        let migration = MIGRATOR.iter().find(|item| item.version == 1).unwrap();
        sqlx::query(
            "INSERT INTO _sqlx_migrations
                (version, description, success, checksum, execution_time)
             VALUES (?1, ?2, 1, ?3, 0)",
        )
        .bind(migration.version)
        .bind(migration.description.as_ref())
        .bind(migration.checksum.as_ref())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO providers (id, name, api_base, created_at) VALUES ('nexus', 'Nexus', 'https://example.invalid', '2026-01-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(
            "INSERT INTO games VALUES ('game', 'nexus', 'test', 'Test', NULL, '2026-01-01');
             INSERT INTO local_game_installs VALUES ('local', 'game', 'test', 'manual', '/game', NULL, '[]', 1, '2026-01-01');
             INSERT INTO mods VALUES ('mod', 'nexus', '1', 'test', 'Mod', NULL, '2026-01-01');
             INSERT INTO releases VALUES ('release', 'mod', '1', NULL, '{}');
             INSERT INTO archives VALUES ('archive', 'blake3:0000000000000000000000000000000000000000000000000000000000000000', 7, 'mod.zip', 'zip', '/archive', '2026-01-01');
             INSERT INTO installations VALUES ('inactive-installation', 'local', 'mod', 'release', 'archive', 'installed', 'flat', '2025-01-01');
             INSERT INTO installations VALUES ('installation', 'local', 'mod', 'release', 'archive', 'installed', 'flat', '2026-01-01');
             INSERT INTO deployed_files VALUES ('deployed', 'local', 'game', 'mods/a', 'blake3:1111111111111111111111111111111111111111111111111111111111111111', 5, '2026-01-01');
             INSERT INTO installation_files VALUES ('installation', 'deployed', 'payload/a', 'blake3:1111111111111111111111111111111111111111111111111111111111111111', 'create');",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    }

    #[tokio::test]
    async fn version_one_database_migrates_without_losing_rows() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("onera.sqlite3");
        write_populated_v1_database(&path).await;

        let db = Database::open(&path).await.unwrap();
        let (version,): (String,) =
            sqlx::query_as("SELECT value FROM schema_meta WHERE key = 'schema_version'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        let (providers,): (i64,) = sqlx::query_as("SELECT count(*) FROM providers")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let columns: Vec<(String,)> =
            sqlx::query_as("SELECT name FROM pragma_table_info('download_jobs')")
                .fetch_all(db.pool())
                .await
                .unwrap();
        let mapping: (String, String, String, i64) = sqlx::query_as(
            "SELECT root_key, rel_path, source_path, source_size
             FROM installation_mappings WHERE installation_id = 'installation'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        let (active,): (i64,) =
            sqlx::query_as("SELECT active FROM installations WHERE id = 'installation'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        let default_profile: (String, i64, i64) = sqlx::query_as(
            "SELECT name, is_active,
                    (SELECT count(*) FROM profile_members m WHERE m.profile_id = p.id)
             FROM profiles p WHERE local_game_id = 'local'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        let imported: (String, String, i64) =
            sqlx::query_as("SELECT installation_id, desired, priority FROM profile_members")
                .fetch_one(db.pool())
                .await
                .unwrap();

        assert_eq!(version, SCHEMA_VERSION.to_string());
        assert_eq!(providers, 1);
        assert!(columns.iter().any(|(name,)| name == "game_slug"));
        assert!(columns.iter().any(|(name,)| name == "provider_mod_id"));
        assert_eq!(
            mapping,
            ("game".into(), "mods/a".into(), "payload/a".into(), 5)
        );
        assert_eq!(active, 1);
        assert_eq!(default_profile, ("Default".into(), 1, 1));
        assert_eq!(imported, ("installation".into(), "enabled".into(), 10));
    }

    /// A populated profile database from the immediately preceding release
    /// gains dependency tables without rewriting desired state or inventing
    /// provider candidate metadata.
    #[tokio::test]
    async fn populated_version_six_database_migrates_to_dependencies() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("onera-v6.sqlite3");
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .unwrap()
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        for sql in [
            include_str!("../migrations/0001_initial.sql"),
            include_str!("../migrations/0002_product_completion.sql"),
            include_str!("../migrations/0003_desired_state.sql"),
            include_str!("../migrations/0004_active_lineage.sql"),
            include_str!("../migrations/0005_baselines.sql"),
            include_str!("../migrations/0006_profiles.sql"),
        ] {
            sqlx::raw_sql(sql).execute(&pool).await.unwrap();
        }
        sqlx::query(
            "CREATE TABLE _sqlx_migrations (
                version BIGINT PRIMARY KEY,
                description TEXT NOT NULL,
                installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                success BOOLEAN NOT NULL,
                checksum BLOB NOT NULL,
                execution_time BIGINT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        for migration in MIGRATOR.iter().filter(|migration| migration.version <= 6) {
            sqlx::query(
                "INSERT INTO _sqlx_migrations
                    (version, description, success, checksum, execution_time)
                 VALUES (?1, ?2, 1, ?3, 0)",
            )
            .bind(migration.version)
            .bind(migration.description.as_ref())
            .bind(migration.checksum.as_ref())
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::raw_sql(
            "INSERT INTO providers VALUES
                ('nexus', 'Nexus', 'https://example.invalid', '2026-01-01T00:00:00Z');
             INSERT INTO games VALUES
                ('game', 'nexus', 'test', 'Test', NULL, '2026-01-01T00:00:00Z');
             INSERT INTO local_game_installs VALUES
                ('local', 'game', 'test', 'manual', '/game', NULL, '[]', 1,
                 '2026-01-01T00:00:00Z');
             INSERT INTO mods VALUES
                ('mod', 'nexus', '42', 'test', 'Mod', NULL, '2026-01-01T00:00:00Z');
             INSERT INTO releases VALUES ('release', 'mod', 'v1', NULL, '{}');
             INSERT INTO provider_files
                (id, release_id, provider_id, provider_file_id, name, category, is_primary)
             VALUES ('stored-file', 'release', 'nexus', 'file-42', 'mod.zip', 'main', 1);
             INSERT INTO profiles VALUES
                ('profile', 'local', 'Populated', NULL, 1,
                 '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
             INSERT INTO profile_members
                (id, profile_id, mod_id, provider_id, provider_mod_id,
                 provider_file_id, desired, pinned, priority, added_at)
             VALUES ('member', 'profile', 'mod', 'nexus', '42', 'file-42',
                     'enabled', 0, 10, '2026-01-01T00:00:00Z');",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let db = Database::open(&path).await.unwrap();
        let (version,): (String,) =
            sqlx::query_as("SELECT value FROM schema_meta WHERE key = 'schema_version'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        let member: (String, String) = sqlx::query_as(
            "SELECT profile_id, provider_file_id FROM profile_members WHERE id = 'member'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        let position: (Option<i64>,) =
            sqlx::query_as("SELECT provider_position FROM provider_files WHERE id = 'stored-file'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        let (dependency_tables,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM sqlite_master
             WHERE type = 'table' AND name IN ('dependency_snapshots', 'dependency_overrides')",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(version, "7");
        assert_eq!(member, ("profile".into(), "file-42".into()));
        assert_eq!(position.0, None, "legacy position must remain unresolved");
        assert_eq!(dependency_tables, 2);
    }

    /// A user who installed the first Onera release must reach the baseline
    /// schema with their data intact — and with the new tables actually usable,
    /// not merely present.
    #[tokio::test]
    async fn a_populated_v1_database_reaches_the_baseline_schema() {
        use crate::convert::to_timestamp;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("onera.sqlite3");
        write_populated_v1_database(&path).await;

        let db = Database::open(&path).await.unwrap();
        let (version,): (String,) =
            sqlx::query_as("SELECT value FROM schema_meta WHERE key = 'schema_version'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());
        let (baselines_applied,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM _sqlx_migrations WHERE version = 5 AND success")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(
            baselines_applied, 1,
            "0005_baselines.sql must have been applied on top of the v1 fixture"
        );

        for table in [
            "game_baselines",
            "baseline_files",
            "baseline_scan_runs",
            "baseline_scan_findings",
        ] {
            let (present,): (i64,) = sqlx::query_as(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            )
            .bind(table)
            .fetch_one(db.pool())
            .await
            .unwrap();
            assert_eq!(present, 1, "{table} is missing after migrating from v1");
        }

        // The v1 rows are still there, and the game they describe can now carry
        // a baseline through the port rather than through raw SQL.
        let (installations,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM installations WHERE id = 'installation'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(installations, 1, "the v1 installation survived the upgrade");

        // The v1 fixture uses opaque non-UUID ids, so these rows are written
        // through SQL rather than through the typed port.
        sqlx::query(
            "INSERT INTO game_baselines
                (id, local_game_id, source, build_identity, adapter_id, reported_version,
                 status, captured_at, scope_fingerprint, file_count, total_bytes)
             VALUES ('baseline', 'local', 'local_snapshot', NULL, 'test', NULL,
                     'current', ?1, 'b3', 1, 7)",
        )
        .bind(to_timestamp(chrono::Utc::now()))
        .execute(db.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO baseline_files (id, baseline_id, root_key, rel_path, hash, size, mode)
             VALUES ('file', 'baseline', 'game', 'bin/x64/game.exe',
                     'blake3:1111111111111111111111111111111111111111111111111111111111111111',
                     7, 420)",
        )
        .execute(db.pool())
        .await
        .unwrap();
        let (files,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM baseline_files WHERE baseline_id = 'baseline'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(files, 1);

        // Deleting the migrated game takes its baseline with it, which is what
        // proves the foreign key survived the v3/v4 table rebuilds.
        sqlx::query("DELETE FROM local_game_installs WHERE id = 'local'")
            .execute(db.pool())
            .await
            .unwrap();
        let (remaining,): (i64,) = sqlx::query_as("SELECT count(*) FROM game_baselines")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn foreign_keys_are_enforced() {
        let db = Database::open_in_memory().await.unwrap();
        let (on,): (i64,) = sqlx::query_as("PRAGMA foreign_keys")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(
            on, 1,
            "foreign keys must be on or deletes silently orphan rows"
        );
    }

    #[tokio::test]
    async fn a_file_provider_row_must_name_exactly_one_provider() {
        let db = Database::open_in_memory().await.unwrap();
        // Neither installation_id nor backup_id set: the CHECK must reject it.
        let result = sqlx::query(
            "INSERT INTO deployed_file_providers
                (id, deployed_file_id, position, provider_kind, hash, size, recorded_at)
             VALUES ('a', 'b', 0, 'installation', 'blake3:x', 1, 'now')",
        )
        .execute(db.pool())
        .await;
        assert!(
            result.is_err(),
            "the provider-kind CHECK constraint is missing"
        );
    }

    #[tokio::test]
    async fn adapter_versions_round_trip() {
        let db = Database::open_in_memory().await.unwrap();
        assert_eq!(db.adapter_version("cyberpunk2077").await.unwrap(), None);
        db.set_adapter_version("cyberpunk2077", 3).await.unwrap();
        db.set_adapter_version("cyberpunk2077", 4).await.unwrap();
        assert_eq!(db.adapter_version("cyberpunk2077").await.unwrap(), Some(4));
    }

    #[tokio::test]
    async fn refuses_a_future_schema_version() {
        let db = Database::open_in_memory().await.unwrap();
        sqlx::query("UPDATE schema_meta SET value = '999' WHERE key = 'schema_version'")
            .execute(db.pool())
            .await
            .unwrap();
        let err = db.check_schema_version().await.unwrap_err();
        assert!(format!("{err}").contains("newer than this build"), "{err}");
    }
}
