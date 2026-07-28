//! XDG base directories.
//!
//! Onera follows the XDG Base Directory specification exactly. Nothing is
//! written to `~/.onera` or next to a game.
//!
//! ```text
//! $XDG_DATA_HOME/onera/archives/blake3/<pp>/<hash>   content-addressed archives
//! $XDG_DATA_HOME/onera/backups/blake3/<pp>/<hash>    overwritten originals
//! $XDG_DATA_HOME/onera/onera.db                      the database
//! $XDG_STATE_HOME/onera/staging/<operation-id>       extraction staging
//! $XDG_STATE_HOME/onera/logs                         rotating logs
//! $XDG_CACHE_HOME/onera/downloads                    in-flight downloads
//! $XDG_CONFIG_HOME/onera/config.toml                 user settings
//! ```
//!
//! Staging lives under state rather than cache because a half-extracted archive
//! belongs to an in-flight operation: a cache cleaner removing it mid-install
//! would break recovery.

use onera_core::{CoreError, Result};
use std::path::PathBuf;

/// Every directory Onera uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Root under `$XDG_DATA_HOME`.
    pub data: PathBuf,
    /// Root under `$XDG_STATE_HOME`.
    pub state: PathBuf,
    /// Root under `$XDG_CACHE_HOME`.
    pub cache: PathBuf,
    /// Root under `$XDG_CONFIG_HOME`.
    pub config: PathBuf,
}

impl Paths {
    /// Resolve the XDG directories for the current user.
    ///
    /// # Errors
    /// Fails if the platform cannot report a home directory.
    pub fn discover() -> Result<Self> {
        let missing = |what: &str| {
            CoreError::InvalidInput(format!("cannot determine the XDG {what} directory"))
        };
        Ok(Self {
            data: dirs::data_dir()
                .ok_or_else(|| missing("data"))?
                .join("onera"),
            state: dirs::state_dir()
                .or_else(|| dirs::data_dir().map(|d| d.join("state")))
                .ok_or_else(|| missing("state"))?
                .join("onera"),
            cache: dirs::cache_dir()
                .ok_or_else(|| missing("cache"))?
                .join("onera"),
            config: dirs::config_dir()
                .ok_or_else(|| missing("config"))?
                .join("onera"),
        })
    }

    /// Point every directory inside `root`, for tests and portable installs.
    #[must_use]
    pub fn rooted_at(root: PathBuf) -> Self {
        Self {
            data: root.join("data"),
            state: root.join("state"),
            cache: root.join("cache"),
            config: root.join("config"),
        }
    }

    /// The SQLite database file.
    #[must_use]
    pub fn database(&self) -> PathBuf {
        self.data.join("onera.db")
    }

    /// Content-addressed archive storage.
    #[must_use]
    pub fn archives(&self) -> PathBuf {
        self.data.join("archives")
    }

    /// Content-addressed backup storage.
    #[must_use]
    pub fn backups(&self) -> PathBuf {
        self.data.join("backups")
    }

    /// Staging root. Each operation gets its own subdirectory.
    #[must_use]
    pub fn staging(&self) -> PathBuf {
        self.state.join("staging")
    }

    /// Staging directory for one operation.
    ///
    /// Unique per operation, so two installs can never share an extraction
    /// directory and a crashed extraction is always identifiable.
    #[must_use]
    pub fn staging_for(&self, operation: onera_core::ids::OperationId) -> PathBuf {
        self.staging().join(operation.to_string())
    }

    /// Log directory.
    #[must_use]
    pub fn logs(&self) -> PathBuf {
        self.state.join("logs")
    }

    /// In-flight download directory.
    #[must_use]
    pub fn downloads(&self) -> PathBuf {
        self.cache.join("downloads")
    }

    /// User configuration file.
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }

    /// Create every directory.
    ///
    /// # Errors
    /// Propagates I/O errors.
    pub async fn ensure(&self) -> Result<()> {
        for dir in [
            self.archives(),
            self.backups(),
            self.staging(),
            self.logs(),
            self.downloads(),
            self.config.clone(),
        ] {
            tokio::fs::create_dir_all(&dir)
                .await
                .map_err(|e| CoreError::fs(&dir, e))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_path_is_under_its_xdg_root() {
        let p = Paths::rooted_at(PathBuf::from("/root"));
        assert!(p.database().starts_with("/root/data"));
        assert!(p.archives().starts_with("/root/data"));
        assert!(p.backups().starts_with("/root/data"));
        assert!(p.staging().starts_with("/root/state"));
        assert!(p.logs().starts_with("/root/state"));
        assert!(p.downloads().starts_with("/root/cache"));
        assert!(p.config_file().starts_with("/root/config"));
    }

    #[test]
    fn staging_is_unique_per_operation() {
        let p = Paths::rooted_at(PathBuf::from("/root"));
        let a = onera_core::ids::OperationId::new();
        let b = onera_core::ids::OperationId::new();
        assert_ne!(p.staging_for(a), p.staging_for(b));
        assert!(p.staging_for(a).starts_with(p.staging()));
    }

    #[tokio::test]
    async fn ensure_creates_everything() {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::rooted_at(dir.path().to_path_buf());
        p.ensure().await.unwrap();
        for path in [
            p.archives(),
            p.backups(),
            p.staging(),
            p.logs(),
            p.downloads(),
        ] {
            assert!(path.is_dir(), "{} was not created", path.display());
        }
    }

    #[test]
    fn discovery_produces_onera_scoped_paths() {
        // The real XDG lookup is environment-dependent; assert only the shape.
        if let Ok(p) = Paths::discover() {
            assert!(p.data.ends_with("onera"));
            assert!(p.cache.ends_with("onera"));
        }
    }
}
