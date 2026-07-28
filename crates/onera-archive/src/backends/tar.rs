//! TAR backend, including the gzip, bzip2, xz and zstd variants.
//!
//! Tar is a stream format: entries are read in order and there is no central
//! directory to consult first. That means inspection and extraction each make
//! their own pass, and it also means tar entries carry no per-entry compressed
//! size — so the compression-ratio heuristic does not apply and the running
//! [`WriteBudget`](crate::validate::WriteBudget) is what stops a bomb.
//!
//! Tar is also the format most likely to carry links, device nodes and setuid
//! bits. Every one of those is classified and dropped by the shared validator.

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
use std::io::{BufReader, Read};
use std::path::Path;
use tar::EntryType;

/// Wrap a file in the right decompressor for `format`.
fn reader_for(path: &Path, format: ArchiveFormat) -> Result<Box<dyn Read>> {
    let file = BufReader::new(File::open(path).map_err(|e| CoreError::fs(path, e))?);
    Ok(match format {
        ArchiveFormat::Tar => Box::new(file),
        ArchiveFormat::TarGz => Box::new(flate2::read::GzDecoder::new(file)),
        ArchiveFormat::TarBz2 => Box::new(bzip2::read::BzDecoder::new(file)),
        ArchiveFormat::TarXz => Box::new(xz2::read::XzDecoder::new(file)),
        ArchiveFormat::TarZstd => Box::new(
            zstd::stream::read::Decoder::new(file)
                .map_err(|e| CoreError::Archive(format!("zstd stream error: {e}")))?,
        ),
        other => {
            return Err(CoreError::Unsupported(format!(
                "{other:?} is not a tar variant"
            )));
        }
    })
}

fn entry_kind(entry_type: EntryType) -> EntryKind {
    match entry_type {
        EntryType::Regular | EntryType::Continuous | EntryType::GNUSparse => EntryKind::File,
        EntryType::Directory => EntryKind::Directory,
        EntryType::Symlink => EntryKind::Symlink,
        EntryType::Link => EntryKind::Hardlink,
        // Long-name and extended-header pseudo entries are metadata the `tar`
        // crate applies for us; anything else is a device, fifo or socket.
        _ => EntryKind::Special,
    }
}

/// Enumerate a tar without writing anything.
pub(crate) fn inspect_blocking(
    path: &Path,
    format: ArchiveFormat,
    validator: &mut Validator,
) -> Result<ArchiveInspection> {
    let mut archive = tar::Archive::new(reader_for(path, format)?);
    let mut entries: Vec<ArchiveEntry> = Vec::new();
    let mut rejected: Vec<RejectedEntry> = Vec::new();

    let iter = archive.entries().map_err(|e| CoreError::ArchiveRejected {
        reason: format!("unreadable tar: {e}"),
    })?;
    for entry in iter {
        let entry = entry.map_err(|e| CoreError::ArchiveRejected {
            reason: format!("unreadable tar entry: {e}"),
        })?;
        let header = entry.header();
        let kind = entry_kind(header.entry_type());
        let raw = entry.path_bytes();
        let raw = String::from_utf8_lossy(&raw).into_owned();
        let size = header.size().unwrap_or(0);
        let link_target = entry
            .link_name_bytes()
            .map(|b| String::from_utf8_lossy(&b).into_owned());

        // Tar has no per-entry compressed size, so the ratio heuristic is fed
        // `None` and the write budget carries the load instead.
        match validator.accept(&raw, kind, size, None, link_target)? {
            Outcome::Accept(e) => entries.push(*e),
            Outcome::Skip(r) => rejected.push(r),
        }
    }

    Ok(ArchiveInspection {
        format,
        entries,
        rejected,
    })
}

/// Extract a tar into an empty staging directory.
pub(crate) fn extract_blocking(
    path: &Path,
    format: ArchiveFormat,
    staging: &Path,
    archive_hash: FileHash,
    validator: &mut Validator,
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
) -> Result<ArchiveManifest> {
    let inspection = inspect_blocking(path, format, validator)?;
    let total = inspection.files().count() as u64;
    progress.emit(ProgressEvent::Started {
        operation: None,
        stage: Stage::Extracting,
        total: Some(total),
    });

    let accepted: std::collections::HashMap<String, EntryKind> = inspection
        .entries
        .iter()
        .map(|e| (e.path.as_str().to_owned(), e.kind))
        .collect();

    let writer = StagingWriter::new(staging)?;
    let mut budget = WriteBudget::new(validator.limits());
    let mut files: Vec<ManifestFile> = Vec::new();
    let mut directories: Vec<RelPath> = Vec::new();
    let mut done = 0_u64;

    let mut archive = tar::Archive::new(reader_for(path, format)?);
    let iter = archive.entries().map_err(|e| CoreError::ArchiveRejected {
        reason: format!("unreadable tar: {e}"),
    })?;
    for entry in iter {
        cancel.check()?;
        let mut entry = entry.map_err(|e| CoreError::ArchiveRejected {
            reason: format!("unreadable tar entry: {e}"),
        })?;
        let executable = entry.header().mode().unwrap_or(0) & 0o111 != 0;
        let raw = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let Ok(rel) = RelPath::normalize(&raw) else {
            continue;
        };
        let Some(kind) = accepted.get(rel.as_str()).copied() else {
            continue;
        };

        match kind {
            EntryKind::Directory => {
                writer.create_dir(&rel)?;
                directories.push(rel);
            }
            EntryKind::File => {
                let written = writer.write_file(&rel, &mut entry, budget.max_file())?;
                budget.consume(written)?;
                files.push(hash_and_record(&writer, rel.clone(), written, executable)?);
                done += 1;
                progress.emit(ProgressEvent::Advanced {
                    stage: Stage::Extracting,
                    completed: done,
                    total: Some(total),
                    detail: Some(rel.to_string()),
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
        format,
        files,
        directories,
    ))
}
