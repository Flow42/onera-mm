//! The real filesystem adapter, plus the fault-injecting one used in tests.
//!
//! Every write the installer performs goes through [`FileSystem`]. That is what
//! makes "what happens if the rename fails halfway through a 400-file install"
//! a unit test rather than a thought experiment.

use async_trait::async_trait;
use onera_core::hash::{hash_file_blake3, FileHash};
use onera_core::ports::FileSystem;
use onera_core::{CoreError, Result};
use std::path::{Path, PathBuf};

/// Suffix given to target-adjacent temporary files.
///
/// Adjacency is required, not cosmetic: `rename(2)` is only atomic within a
/// filesystem, and a game directory is routinely on a different mount from
/// `$XDG_DATA_HOME`.
pub const TEMP_SUFFIX: &str = ".onera-tmp";

/// Filesystem access backed by `tokio::fs`.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealFileSystem;

#[async_trait]
impl FileSystem for RealFileSystem {
    async fn exists(&self, path: &Path) -> Result<bool> {
        Ok(tokio::fs::symlink_metadata(path).await.is_ok())
    }

    async fn stat_hash(&self, path: &Path) -> Result<Option<(FileHash, u64)>> {
        let meta = match tokio::fs::symlink_metadata(path).await {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(CoreError::fs(path, e)),
        };
        // A symlink where a managed file should be is not "the file, modified";
        // it is a different kind of object and the installer must not treat it
        // as ordinary content.
        if meta.file_type().is_symlink() {
            return Err(CoreError::Conflict(format!(
                "{} is a symbolic link; Onera does not manage links",
                path.display()
            )));
        }
        if !meta.is_file() {
            return Ok(None);
        }
        let hash = hash_file_blake3(path)
            .await
            .map_err(|e| CoreError::fs(path, e))?;
        Ok(Some((hash, meta.len())))
    }

    async fn create_dir_all(&self, path: &Path) -> Result<()> {
        tokio::fs::create_dir_all(path)
            .await
            .map_err(|e| CoreError::fs(path, e))
    }

    async fn copy_file(&self, from: &Path, to: &Path) -> Result<u64> {
        if let Some(parent) = to.parent() {
            self.create_dir_all(parent).await?;
        }
        tokio::fs::copy(from, to)
            .await
            .map_err(|e| CoreError::fs(from, e))
    }

    async fn write_temp_adjacent(&self, final_path: &Path, from: &Path) -> Result<PathBuf> {
        let parent = final_path.parent().ok_or_else(|| {
            CoreError::InvalidInput(format!("{} has no parent directory", final_path.display()))
        })?;
        self.create_dir_all(parent).await?;

        let name = final_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_owned());
        let temp = parent.join(format!("{name}{TEMP_SUFFIX}-{}", uuid::Uuid::new_v4()));
        tokio::fs::copy(from, &temp)
            .await
            .map_err(|e| CoreError::fs(from, e))?;

        // Durability: the bytes must be on the platter before the rename, or a
        // crash can leave a renamed-but-empty file.
        let file = tokio::fs::File::open(&temp)
            .await
            .map_err(|e| CoreError::fs(&temp, e))?;
        file.sync_all().await.map_err(|e| CoreError::fs(&temp, e))?;
        Ok(temp)
    }

    async fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        tokio::fs::rename(from, to)
            .await
            .map_err(|e| CoreError::fs(from, e))
    }

    async fn remove_file(&self, path: &Path) -> Result<()> {
        match tokio::fs::remove_file(path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(CoreError::fs(path, e)),
        }
    }

    async fn remove_dir_if_empty(&self, path: &Path) -> Result<bool> {
        match tokio::fs::remove_dir(path).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            // `DirectoryNotEmpty` is the expected, uninteresting outcome: the
            // directory still holds files Onera does not own.
            Err(_) => Ok(false),
        }
    }

    async fn sync_dir(&self, path: &Path) -> Result<()> {
        let dir = tokio::fs::File::open(path)
            .await
            .map_err(|e| CoreError::fs(path, e))?;
        dir.sync_all().await.map_err(|e| CoreError::fs(path, e))
    }
}

pub mod fault {
    //! A filesystem wrapper that fails on demand.
    //!
    //! Public rather than `#[cfg(test)]` because the interesting failure cases
    //! — a rename that dies halfway through a multi-file deployment — can only
    //! be reached from an integration test, and because the application-level
    //! end-to-end tests inject the same faults.

    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Which operation to fail.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[non_exhaustive]
    pub enum FailAt {
        /// Never fail.
        Never,
        /// Fail the Nth rename (0-indexed).
        Rename(usize),
        /// Fail the Nth temporary write (0-indexed).
        TempWrite(usize),
    }

    /// Wraps [`RealFileSystem`] and injects one failure.
    #[derive(Debug, Clone)]
    pub struct FaultyFileSystem {
        inner: RealFileSystem,
        fail_at: FailAt,
        renames: Arc<AtomicUsize>,
        temps: Arc<AtomicUsize>,
    }

    impl FaultyFileSystem {
        /// Wrap the real filesystem and fail at the given point.
        #[must_use]
        pub fn new(fail_at: FailAt) -> Self {
            Self {
                inner: RealFileSystem,
                fail_at,
                renames: Arc::new(AtomicUsize::new(0)),
                temps: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl FileSystem for FaultyFileSystem {
        async fn exists(&self, path: &Path) -> Result<bool> {
            self.inner.exists(path).await
        }
        async fn stat_hash(&self, path: &Path) -> Result<Option<(FileHash, u64)>> {
            self.inner.stat_hash(path).await
        }
        async fn create_dir_all(&self, path: &Path) -> Result<()> {
            self.inner.create_dir_all(path).await
        }
        async fn copy_file(&self, from: &Path, to: &Path) -> Result<u64> {
            self.inner.copy_file(from, to).await
        }
        async fn write_temp_adjacent(&self, final_path: &Path, from: &Path) -> Result<PathBuf> {
            let n = self.temps.fetch_add(1, Ordering::SeqCst);
            if self.fail_at == FailAt::TempWrite(n) {
                return Err(CoreError::fs(
                    final_path,
                    std::io::Error::other("injected staging failure"),
                ));
            }
            self.inner.write_temp_adjacent(final_path, from).await
        }
        async fn rename(&self, from: &Path, to: &Path) -> Result<()> {
            let n = self.renames.fetch_add(1, Ordering::SeqCst);
            if self.fail_at == FailAt::Rename(n) {
                return Err(CoreError::fs(
                    to,
                    std::io::Error::other("injected rename failure"),
                ));
            }
            self.inner.rename(from, to).await
        }
        async fn remove_file(&self, path: &Path) -> Result<()> {
            self.inner.remove_file(path).await
        }
        async fn remove_dir_if_empty(&self, path: &Path) -> Result<bool> {
            self.inner.remove_dir_if_empty(path).await
        }
        async fn sync_dir(&self, path: &Path) -> Result<()> {
            self.inner.sync_dir(path).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stat_hash_reports_missing_files_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            RealFileSystem
                .stat_hash(&dir.path().join("nope"))
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn stat_hash_hashes_real_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        tokio::fs::write(&path, b"onera").await.unwrap();
        let (hash, size) = RealFileSystem.stat_hash(&path).await.unwrap().unwrap();
        assert_eq!(hash, FileHash::blake3_of(b"onera"));
        assert_eq!(size, 5);
    }

    #[tokio::test]
    async fn a_symlink_where_a_file_belongs_is_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink("/etc/passwd", &link).unwrap();
        let err = RealFileSystem.stat_hash(&link).await.unwrap_err();
        assert!(matches!(err, CoreError::Conflict(_)), "{err:?}");
    }

    #[tokio::test]
    async fn temp_files_are_written_next_to_their_target() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        tokio::fs::write(&source, b"payload").await.unwrap();
        let target = dir.path().join("deep/nested/target.bin");

        let temp = RealFileSystem
            .write_temp_adjacent(&target, &source)
            .await
            .unwrap();
        assert_eq!(
            temp.parent(),
            target.parent(),
            "rename would not be atomic across mounts"
        );
        assert!(temp
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains(TEMP_SUFFIX));
        assert_eq!(tokio::fs::read(&temp).await.unwrap(), b"payload");
    }

    #[tokio::test]
    async fn removing_a_missing_file_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        RealFileSystem
            .remove_file(&dir.path().join("ghost"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn only_empty_directories_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let full = dir.path().join("full");
        let empty = dir.path().join("empty");
        tokio::fs::create_dir_all(&full).await.unwrap();
        tokio::fs::create_dir_all(&empty).await.unwrap();
        tokio::fs::write(full.join("keep"), b"x").await.unwrap();

        assert!(!RealFileSystem.remove_dir_if_empty(&full).await.unwrap());
        assert!(full.exists(), "a directory with user files must survive");
        assert!(RealFileSystem.remove_dir_if_empty(&empty).await.unwrap());
        assert!(!empty.exists());
    }
}
