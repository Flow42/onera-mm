//! ZIP backend.
//!
//! Uses the pure-Rust `zip` crate rather than an external process, and never
//! uses its convenience extraction API: `ZipArchive::extract` honours entry
//! paths as the archive supplies them. Onera reads each entry by index and
//! decides for itself where — and whether — the bytes go.

use crate::validate::{Outcome, Validator, WriteBudget};
use crate::write::{hash_and_record, StagingWriter};
use onera_core::domain::archive::{
    ArchiveEntry, ArchiveFormat, ArchiveInspection, ArchiveManifest, EntryKind, ManifestFile,
    RejectedEntry,
};
use onera_core::hash::FileHash;
use onera_core::ids::ArchiveId;
use onera_core::paths::RelPath;
use onera_core::progress::{CancelToken, ProgressEvent, ProgressSink, Stage};
use onera_core::{CoreError, Result};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use zip::ZipArchive;

type Zip = ZipArchive<BufReader<File>>;

fn open(path: &Path) -> Result<Zip> {
    let file = File::open(path).map_err(|e| CoreError::fs(path, e))?;
    ZipArchive::new(BufReader::new(file)).map_err(|e| CoreError::ArchiveRejected {
        reason: format!("unreadable zip: {e}"),
    })
}

/// Classify a zip entry, honouring the unix mode bits when present.
fn entry_kind(entry: &zip::read::ZipFile<'_>) -> EntryKind {
    const S_IFMT: u32 = 0o170_000;
    const S_IFLNK: u32 = 0o120_000;
    const S_IFREG: u32 = 0o100_000;
    const S_IFDIR: u32 = 0o040_000;

    if let Some(mode) = entry.unix_mode() {
        return match mode & S_IFMT {
            S_IFLNK => EntryKind::Symlink,
            S_IFDIR => EntryKind::Directory,
            S_IFREG | 0 => {
                if entry.is_dir() {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                }
            }
            _ => EntryKind::Special,
        };
    }
    if entry.is_dir() {
        EntryKind::Directory
    } else {
        EntryKind::File
    }
}

/// Enumerate a zip without writing anything.
pub(crate) fn inspect_blocking(
    path: &Path,
    validator: &mut Validator,
) -> Result<ArchiveInspection> {
    let mut zip = open(path)?;
    let mut entries: Vec<ArchiveEntry> = Vec::new();
    let mut rejected: Vec<RejectedEntry> = Vec::new();

    for i in 0..zip.len() {
        let entry = zip
            .by_index_raw(i)
            .map_err(|e| CoreError::ArchiveRejected {
                reason: format!("unreadable zip entry: {e}"),
            })?;
        // `name_raw` is used deliberately: `name()` performs its own
        // sanitization in some versions, and Onera must see exactly what the
        // archive declared so its own rules are the only ones that apply.
        let raw = String::from_utf8_lossy(entry.name_raw()).into_owned();
        let kind = entry_kind(&entry);
        let declared = entry.size();
        let compressed = Some(entry.compressed_size());
        drop(entry);

        match validator.accept(&raw, kind, declared, compressed, None)? {
            Outcome::Accept(e) => entries.push(*e),
            Outcome::Skip(r) => rejected.push(r),
        }
    }

    Ok(ArchiveInspection {
        format: ArchiveFormat::Zip,
        entries,
        rejected,
    })
}

/// Extract a zip into an already-created, empty staging directory.
pub(crate) fn extract_blocking(
    path: &Path,
    staging: &Path,
    archive_hash: FileHash,
    validator: &mut Validator,
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
) -> Result<ArchiveManifest> {
    let inspection = inspect_blocking(path, validator)?;
    let total = inspection.files().count() as u64;
    progress.emit(ProgressEvent::Started {
        operation: None,
        stage: Stage::Extracting,
        total: Some(total),
    });

    let mut zip = open(path)?;
    let mut budget = WriteBudget::new(validator.limits());
    let writer = StagingWriter::new(staging)?;
    let mut files: Vec<ManifestFile> = Vec::new();
    let mut directories: Vec<RelPath> = Vec::new();
    let mut done = 0_u64;

    // Extract in the order the validator accepted them, which is archive order.
    let wanted: std::collections::HashMap<String, &ArchiveEntry> = inspection
        .entries
        .iter()
        .map(|e| (e.path.as_str().to_owned(), e))
        .collect();

    for i in 0..zip.len() {
        cancel.check()?;
        let mut entry = zip.by_index(i).map_err(|e| CoreError::ArchiveRejected {
            reason: format!("unreadable zip entry: {e}"),
        })?;
        let raw = String::from_utf8_lossy(entry.name_raw()).into_owned();
        let Ok(rel) = RelPath::normalize(&raw) else {
            continue;
        };
        let Some(accepted) = wanted.get(rel.as_str()) else {
            continue;
        };

        match accepted.kind {
            EntryKind::Directory => {
                writer.create_dir(&rel)?;
                directories.push(rel);
            }
            EntryKind::File => {
                let executable = entry.unix_mode().is_some_and(|m| m & 0o111 != 0);
                let written = writer.write_file(&rel, &mut entry, budget.max_file())?;
                budget.consume(written)?;
                files.push(hash_and_record(&writer, rel, written, executable)?);
                done += 1;
                progress.emit(ProgressEvent::Advanced {
                    stage: Stage::Extracting,
                    completed: done,
                    total: Some(total),
                    detail: Some(accepted.path.to_string()),
                });
            }
            _ => unreachable!("validator only accepts files and directories"),
        }
    }

    progress.emit(ProgressEvent::Finished {
        stage: Stage::Extracting,
        success: true,
    });
    Ok(ArchiveManifest::new(
        ArchiveId::new(),
        archive_hash,
        ArchiveFormat::Zip,
        files,
        directories,
    ))
}
