//! Archive inspection results.
//!
//! Inspection is separate from extraction on purpose: Onera enumerates and
//! validates every entry, decides whether the archive is acceptable, and only
//! then extracts. The [`ArchiveManifest`] produced *after* extraction is
//! immutable and is what every later stage — layout resolution, planning,
//! deployment, verification — reads from.

use crate::hash::FileHash;
use crate::ids::ArchiveId;
use crate::paths::RelPath;
use serde::{Deserialize, Serialize};

/// Container formats Onera can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveFormat {
    /// PKZIP, including the common `.zip` mod distribution format.
    Zip,
    /// Uncompressed tar.
    Tar,
    /// gzip-compressed tar.
    TarGz,
    /// bzip2-compressed tar.
    TarBz2,
    /// xz-compressed tar.
    TarXz,
    /// zstd-compressed tar.
    TarZstd,
    /// 7-Zip, handled by an external `7zz`/`7z` process.
    SevenZ,
    /// RAR, handled by the external process when the build supports it.
    Rar,
}

impl ArchiveFormat {
    /// Whether this format is handled by spawning an external process.
    #[must_use]
    pub const fn needs_external_tool(self) -> bool {
        matches!(self, Self::SevenZ | Self::Rar)
    }
}

/// What kind of thing an archive entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A symbolic link. Never extracted; see `docs/threat-model.md`.
    Symlink,
    /// A hard link. Never extracted.
    Hardlink,
    /// A device node, FIFO, socket or anything else. Never extracted.
    Special,
}

impl EntryKind {
    /// Whether Onera will write this entry to disk.
    #[must_use]
    pub const fn is_extractable(self) -> bool {
        matches!(self, Self::File | Self::Directory)
    }
}

/// One entry as seen during inspection, before anything is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveEntry {
    /// Normalized path inside the archive.
    pub path: RelPath,
    /// What the entry is.
    pub kind: EntryKind,
    /// Uncompressed size in bytes as declared by the archive.
    ///
    /// Declared, not measured: a malicious archive can lie, so extraction
    /// enforces its own running byte budget as well.
    pub declared_size: u64,
    /// Compressed size in bytes, when the format reports one.
    pub compressed_size: Option<u64>,
    /// Link target for symlinks and hardlinks, kept for diagnostics only.
    pub link_target: Option<String>,
}

impl ArchiveEntry {
    /// Ratio of declared uncompressed size to compressed size.
    ///
    /// A high ratio is the classic signature of a decompression bomb.
    #[must_use]
    pub fn compression_ratio(&self) -> Option<f64> {
        let compressed = self.compressed_size?;
        if compressed == 0 {
            return (self.declared_size > 0).then_some(f64::INFINITY);
        }
        Some(self.declared_size as f64 / compressed as f64)
    }
}

/// The result of inspecting an archive without extracting it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveInspection {
    /// Detected container format.
    pub format: ArchiveFormat,
    /// Every entry, in archive order, with normalized paths.
    pub entries: Vec<ArchiveEntry>,
    /// Entries that were rejected, with the reason, for display to the user.
    pub rejected: Vec<RejectedEntry>,
}

impl ArchiveInspection {
    /// Total declared uncompressed size of extractable entries.
    #[must_use]
    pub fn declared_total_size(&self) -> u64 {
        self.entries
            .iter()
            .filter(|e| e.kind == EntryKind::File)
            .map(|e| e.declared_size)
            .sum()
    }

    /// Regular-file entries only.
    pub fn files(&self) -> impl Iterator<Item = &ArchiveEntry> {
        self.entries.iter().filter(|e| e.kind == EntryKind::File)
    }
}

/// An entry the inspector refused, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedEntry {
    /// The raw, un-normalized path exactly as the archive declared it.
    pub raw_path: String,
    /// Why it was rejected, safe to display.
    pub reason: String,
}

/// An extracted file, hashed and confirmed on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFile {
    /// Path relative to the staging root.
    pub path: RelPath,
    /// Actual size on disk after extraction.
    pub size: u64,
    /// BLAKE3 hash of the extracted bytes.
    pub hash: FileHash,
    /// Whether the archive marked the entry executable.
    pub executable: bool,
}

/// The immutable record of what an archive actually produced.
///
/// Created once, after extraction and hashing, and never mutated. Everything
/// downstream reads this rather than re-walking the staging directory, so the
/// plan a user approves cannot drift from what gets deployed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveManifest {
    /// The archive in content-addressed storage this came from.
    pub archive_id: ArchiveId,
    /// Hash of the archive file itself.
    pub archive_hash: FileHash,
    /// Format it was extracted from.
    pub format: ArchiveFormat,
    /// Every extracted regular file, sorted by path.
    pub files: Vec<ManifestFile>,
    /// Directories that were created, sorted by path.
    pub directories: Vec<RelPath>,
}

impl ArchiveManifest {
    /// Build a manifest, sorting entries so the manifest is canonical.
    #[must_use]
    pub fn new(
        archive_id: ArchiveId,
        archive_hash: FileHash,
        format: ArchiveFormat,
        mut files: Vec<ManifestFile>,
        mut directories: Vec<RelPath>,
    ) -> Self {
        files.sort_by(|a, b| a.path.cmp(&b.path));
        directories.sort();
        Self {
            archive_id,
            archive_hash,
            format,
            files,
            directories,
        }
    }

    /// Total extracted byte count.
    #[must_use]
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// Look up an extracted file by path.
    #[must_use]
    pub fn file(&self, path: &RelPath) -> Option<&ManifestFile> {
        self.files.iter().find(|f| &f.path == path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: u64) -> ManifestFile {
        ManifestFile {
            path: RelPath::normalize(path).unwrap(),
            size,
            hash: FileHash::blake3_of(path.as_bytes()),
            executable: false,
        }
    }

    #[test]
    fn manifest_is_canonically_ordered() {
        let m = ArchiveManifest::new(
            ArchiveId::new(),
            FileHash::blake3_of(b"a"),
            ArchiveFormat::Zip,
            vec![file("z.txt", 1), file("a.txt", 2)],
            vec![
                RelPath::normalize("z").unwrap(),
                RelPath::normalize("a").unwrap(),
            ],
        );
        assert_eq!(m.files[0].path.as_str(), "a.txt");
        assert_eq!(m.directories[0].as_str(), "a");
        assert_eq!(m.total_size(), 3);
        assert!(m.file(&RelPath::normalize("z.txt").unwrap()).is_some());
    }

    #[test]
    fn compression_ratio_flags_bombs() {
        let bomb = ArchiveEntry {
            path: RelPath::normalize("bomb").unwrap(),
            kind: EntryKind::File,
            declared_size: 10_000_000_000,
            compressed_size: Some(1_000),
            link_target: None,
        };
        assert!(bomb.compression_ratio().unwrap() > 1_000_000.0);

        let zero = ArchiveEntry {
            compressed_size: Some(0),
            ..bomb.clone()
        };
        assert_eq!(zero.compression_ratio(), Some(f64::INFINITY));

        let empty = ArchiveEntry {
            declared_size: 0,
            compressed_size: Some(0),
            ..bomb
        };
        assert_eq!(empty.compression_ratio(), None);
    }

    #[test]
    fn only_files_and_directories_are_extractable() {
        assert!(EntryKind::File.is_extractable());
        assert!(EntryKind::Directory.is_extractable());
        for kind in [EntryKind::Symlink, EntryKind::Hardlink, EntryKind::Special] {
            assert!(!kind.is_extractable(), "{kind:?} must never be extracted");
        }
    }
}
