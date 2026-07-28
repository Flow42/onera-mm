//! Guarded writes into a staging directory.
//!
//! [`StagingWriter`] is the only thing in the crate that creates files. It takes
//! a [`RelPath`], so escaping the staging root is impossible by construction,
//! and it re-checks two things that the type system cannot cover:
//!
//! * that no ancestor directory it creates is a symlink, so a hostile archive
//!   cannot lay down `a -> /etc` and then write `a/passwd`;
//! * that the number of bytes read from the archive stays inside the budget,
//!   regardless of what the archive header declared.

use onera_core::hash::FileHash;
use onera_core::paths::RelPath;
use onera_core::{CoreError, Result};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// Creates directories and files under a fixed staging root.
#[derive(Debug)]
pub struct StagingWriter {
    root: PathBuf,
}

impl StagingWriter {
    /// Prepare a staging root. The directory must exist and be empty.
    ///
    /// # Errors
    /// Fails if the directory is missing, is not a directory, or is not empty.
    /// Refusing a non-empty staging directory keeps two operations from ever
    /// sharing one.
    pub fn new(root: &Path) -> Result<Self> {
        let meta = fs::symlink_metadata(root).map_err(|e| CoreError::fs(root, e))?;
        if !meta.is_dir() {
            return Err(CoreError::ArchiveRejected {
                reason: format!("staging path {} is not a directory", root.display()),
            });
        }
        if meta.file_type().is_symlink() {
            return Err(CoreError::ArchiveRejected {
                reason: "staging root must not be a symlink".to_owned(),
            });
        }
        let mut entries = fs::read_dir(root).map_err(|e| CoreError::fs(root, e))?;
        if entries.next().is_some() {
            return Err(CoreError::ArchiveRejected {
                reason: format!("staging directory {} is not empty", root.display()),
            });
        }
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    /// The staging root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Absolute path of a staged entry.
    #[must_use]
    pub fn path_of(&self, rel: &RelPath) -> PathBuf {
        rel.resolve_under(&self.root)
    }

    /// Create a directory and its ancestors, refusing to traverse a symlink.
    ///
    /// # Errors
    /// Fails on I/O errors, or if any component already exists as a symlink or
    /// as a non-directory.
    pub fn create_dir(&self, rel: &RelPath) -> Result<()> {
        let mut current = self.root.clone();
        for component in rel.components() {
            current.push(component);
            match fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(CoreError::ArchiveRejected {
                        reason: format!(
                            "refusing to write through the symlink {}",
                            current.display()
                        ),
                    });
                }
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => {
                    return Err(CoreError::ArchiveRejected {
                        reason: format!(
                            "{} already exists and is not a directory",
                            current.display()
                        ),
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    fs::create_dir(&current).map_err(|e| CoreError::fs(&current, e))?;
                }
                Err(e) => return Err(CoreError::fs(&current, e)),
            }
        }
        Ok(())
    }

    /// Write one file, copying at most `max_bytes` from `source`.
    ///
    /// Returns the number of bytes written.
    ///
    /// # Errors
    /// Returns [`CoreError::ArchiveRejected`] if the entry is larger than the
    /// remaining budget, and propagates I/O errors otherwise.
    pub fn write_file(&self, rel: &RelPath, source: &mut impl Read, max_bytes: u64) -> Result<u64> {
        if let Some(parent) = rel.parent() {
            self.create_dir(&parent)?;
        }
        let path = self.path_of(rel);

        // `create_new` refuses to follow an existing symlink and also catches
        // an archive that lists the same path twice.
        let mut file = fs::File::options()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| CoreError::fs(&path, e))?;

        // Read one byte past the budget so an over-long entry is detected
        // rather than silently truncated.
        let mut limited = source.take(max_bytes.saturating_add(1));
        let mut buf = vec![0_u8; 128 * 1024];
        let mut written = 0_u64;
        loop {
            let read = limited
                .read(&mut buf)
                .map_err(|e| CoreError::fs(&path, e))?;
            if read == 0 {
                break;
            }
            written += read as u64;
            if written > max_bytes {
                let _ = fs::remove_file(&path);
                return Err(CoreError::ArchiveRejected {
                    reason: format!("entry {rel} expands past the remaining size budget"),
                });
            }
            file.write_all(&buf[..read])
                .map_err(|e| CoreError::fs(&path, e))?;
        }
        file.flush().map_err(|e| CoreError::fs(&path, e))?;
        Ok(written)
    }
}

/// Hash a staged file and build its manifest record.
///
/// # Errors
/// Propagates I/O errors from re-reading the staged file.
pub fn hash_and_record(
    writer: &StagingWriter,
    rel: RelPath,
    size: u64,
    executable: bool,
) -> Result<onera_core::domain::archive::ManifestFile> {
    let path = writer.path_of(&rel);
    let mut file = fs::File::open(&path).map_err(|e| CoreError::fs(&path, e))?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0_u8; 256 * 1024];
    loop {
        let read = file.read(&mut buf).map_err(|e| CoreError::fs(&path, e))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(onera_core::domain::archive::ManifestFile {
        path: rel,
        size,
        hash: FileHash::blake3(*hasher.finalize().as_bytes()),
        executable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn rel(p: &str) -> RelPath {
        RelPath::normalize(p).unwrap()
    }

    #[test]
    fn refuses_a_non_empty_staging_directory() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("stale"), b"x").unwrap();
        assert!(StagingWriter::new(dir.path()).is_err());
    }

    #[test]
    fn writes_files_and_creates_parents() {
        let dir = tempfile::tempdir().unwrap();
        let w = StagingWriter::new(dir.path()).unwrap();
        let n = w
            .write_file(&rel("a/b/c.txt"), &mut &b"hello"[..], 1024)
            .unwrap();
        assert_eq!(n, 5);
        assert_eq!(fs::read(dir.path().join("a/b/c.txt")).unwrap(), b"hello");
    }

    #[test]
    fn refuses_to_write_through_a_symlinked_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let w = StagingWriter::new(dir.path()).unwrap();
        // Simulate an archive having already created `escape -> /outside`.
        symlink(outside.path(), dir.path().join("escape")).unwrap();

        let err = w
            .write_file(&rel("escape/pwned.txt"), &mut &b"x"[..], 1024)
            .unwrap_err();
        assert!(matches!(err, CoreError::ArchiveRejected { .. }), "{err:?}");
        assert!(
            !outside.path().join("pwned.txt").exists(),
            "wrote outside staging"
        );
    }

    #[test]
    fn refuses_to_overwrite_a_duplicate_entry() {
        let dir = tempfile::tempdir().unwrap();
        let w = StagingWriter::new(dir.path()).unwrap();
        w.write_file(&rel("dup.txt"), &mut &b"first"[..], 1024)
            .unwrap();
        assert!(w
            .write_file(&rel("dup.txt"), &mut &b"second"[..], 1024)
            .is_err());
        assert_eq!(fs::read(dir.path().join("dup.txt")).unwrap(), b"first");
    }

    #[test]
    fn stops_an_entry_that_lies_about_its_size() {
        let dir = tempfile::tempdir().unwrap();
        let w = StagingWriter::new(dir.path()).unwrap();
        let payload = vec![0_u8; 1000];
        let err = w
            .write_file(&rel("bomb"), &mut &payload[..], 100)
            .unwrap_err();
        assert!(matches!(err, CoreError::ArchiveRejected { .. }), "{err:?}");
        assert!(
            !dir.path().join("bomb").exists(),
            "partial file must be removed"
        );
    }

    #[test]
    fn a_file_exactly_at_the_budget_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let w = StagingWriter::new(dir.path()).unwrap();
        assert_eq!(
            w.write_file(&rel("exact"), &mut &[0_u8; 100][..], 100)
                .unwrap(),
            100
        );
    }

    #[test]
    fn hashes_what_was_written() {
        let dir = tempfile::tempdir().unwrap();
        let w = StagingWriter::new(dir.path()).unwrap();
        let n = w.write_file(&rel("f"), &mut &b"onera"[..], 64).unwrap();
        let record = hash_and_record(&w, rel("f"), n, false).unwrap();
        assert_eq!(record.hash, FileHash::blake3_of(b"onera"));
        assert_eq!(record.size, 5);
    }

    #[test]
    fn refuses_a_directory_that_collides_with_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let w = StagingWriter::new(dir.path()).unwrap();
        w.write_file(&rel("thing"), &mut &b"x"[..], 64).unwrap();
        assert!(w.create_dir(&rel("thing/child")).is_err());
    }
}
