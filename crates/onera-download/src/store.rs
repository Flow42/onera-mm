//! Content-addressed archive storage.
//!
//! Layout, per the design:
//!
//! ```text
//! $XDG_DATA_HOME/onera/archives/blake3/<first two hex chars>/<full hash>
//! ```
//!
//! The two-character shard keeps directory sizes reasonable on filesystems that
//! degrade with tens of thousands of entries. The original filename is *not*
//! part of the path — it lives in the database — so two downloads of the same
//! bytes under different names share one stored file.

use async_trait::async_trait;
use onera_core::hash::FileHash;
use onera_core::ports::ArchiveStore;
use onera_core::{CoreError, Result};
use std::path::{Path, PathBuf};

/// Number of leading hex characters used as the shard directory.
pub const SHARD_LEN: usize = 2;

/// Archive storage rooted at a directory.
#[derive(Debug, Clone)]
pub struct ContentAddressedStore {
    root: PathBuf,
}

impl ContentAddressedStore {
    /// Store archives under `root`, which is created on first write.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The storage root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[async_trait]
impl ArchiveStore for ContentAddressedStore {
    fn path_for(&self, hash: &FileHash) -> PathBuf {
        self.root
            .join(hash.algorithm.as_str())
            .join(hash.prefix(SHARD_LEN))
            .join(&hash.hex)
    }

    async fn contains(&self, hash: &FileHash) -> Result<bool> {
        Ok(tokio::fs::metadata(self.path_for(hash)).await.is_ok())
    }

    async fn promote(&self, temp: &Path, hash: &FileHash) -> Result<PathBuf> {
        let final_path = self.path_for(hash);
        if let Some(parent) = final_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| CoreError::fs(parent, e))?;
        }

        // Already stored: the identical bytes are there, so the new copy is
        // redundant. This is where duplicate downloads collapse.
        if tokio::fs::metadata(&final_path).await.is_ok() {
            let _ = tokio::fs::remove_file(temp).await;
            return Ok(final_path);
        }

        match tokio::fs::rename(temp, &final_path).await {
            Ok(()) => Ok(final_path),
            // A cross-device rename cannot be atomic; fall back to a copy into
            // the same directory followed by a rename, which can.
            Err(e) if e.raw_os_error() == Some(18) => {
                let staging = final_path.with_extension("incoming");
                tokio::fs::copy(temp, &staging)
                    .await
                    .map_err(|e| CoreError::fs(temp, e))?;
                tokio::fs::rename(&staging, &final_path)
                    .await
                    .map_err(|e| CoreError::fs(&staging, e))?;
                let _ = tokio::fs::remove_file(temp).await;
                Ok(final_path)
            }
            Err(e) => Err(CoreError::fs(temp, e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash_of(data: &[u8]) -> FileHash {
        FileHash::blake3_of(data)
    }

    #[test]
    fn paths_follow_the_documented_layout() {
        let store = ContentAddressedStore::new(PathBuf::from("/data/onera/archives"));
        let hash = hash_of(b"payload");
        let path = store.path_for(&hash);
        let expected = PathBuf::from("/data/onera/archives")
            .join("blake3")
            .join(&hash.hex[..2])
            .join(&hash.hex);
        assert_eq!(path, expected);
    }

    #[tokio::test]
    async fn promotion_moves_the_file_into_place() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContentAddressedStore::new(dir.path().join("archives"));
        let temp = dir.path().join("incoming.part");
        tokio::fs::write(&temp, b"payload").await.unwrap();

        let hash = hash_of(b"payload");
        assert!(!store.contains(&hash).await.unwrap());
        let stored = store.promote(&temp, &hash).await.unwrap();

        assert_eq!(stored, store.path_for(&hash));
        assert_eq!(tokio::fs::read(&stored).await.unwrap(), b"payload");
        assert!(store.contains(&hash).await.unwrap());
        assert!(!temp.exists(), "the temporary file should be consumed");
    }

    #[tokio::test]
    async fn promoting_bytes_that_are_already_stored_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContentAddressedStore::new(dir.path().join("archives"));
        let hash = hash_of(b"payload");

        for name in ["first.part", "second.part"] {
            let temp = dir.path().join(name);
            tokio::fs::write(&temp, b"payload").await.unwrap();
            assert_eq!(
                store.promote(&temp, &hash).await.unwrap(),
                store.path_for(&hash)
            );
            assert!(!temp.exists());
        }

        // One shard directory, one file.
        let shard = store.path_for(&hash).parent().unwrap().to_path_buf();
        let count = std::fs::read_dir(shard).unwrap().count();
        assert_eq!(count, 1, "identical downloads must share one stored file");
    }

    #[tokio::test]
    async fn different_bytes_land_in_different_places() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContentAddressedStore::new(dir.path().join("archives"));
        assert_ne!(
            store.path_for(&hash_of(b"a")),
            store.path_for(&hash_of(b"b"))
        );
    }
}
