//! 7-Zip and RAR backend, driven by an external `7zz`/`7z` process.
//!
//! There is no mature pure-Rust 7z decoder, so this backend shells out — but
//! never *through* a shell. The binary is invoked directly with an argument
//! vector, so archive names containing `;`, `$(...)`, newlines or quotes are
//! inert. The archive path is passed after `--` and the output directory is
//! passed as a single `-o` argument.
//!
//! Because the external tool does its own path handling, Onera does not trust
//! it. After extraction the staging tree is walked and re-validated: anything
//! that is not a regular file or directory, and anything that escaped the
//! staging root, fails the whole operation.

use crate::validate::{Outcome, Validator};
use onera_core::domain::archive::{
    ArchiveEntry, ArchiveFormat, ArchiveInspection, ArchiveManifest, EntryKind, ManifestFile,
    RejectedEntry,
};
use onera_core::hash::FileHash;
use onera_core::ids::ArchiveId;
use onera_core::paths::RelPath;
use onera_core::progress::{CancelToken, ProgressEvent, ProgressSink, Stage};
use onera_core::{CoreError, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Locate a usable 7-Zip binary.
///
/// `7zz` is the official upstream build and is preferred; `7z` is what most
/// distributions package.
#[must_use]
pub fn find_sevenz() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for name in ["7zz", "7z", "7za"] {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn missing_tool() -> CoreError {
    CoreError::Unsupported(
        "7-Zip archives need the `7zz` or `7z` binary; install p7zip-full or 7zip".to_owned(),
    )
}

/// Parse the `-slt` ("show technical listing") output of `7zz l`.
///
/// The format is a blank-line-separated list of `Key = Value` records, which is
/// far easier to parse correctly than the default aligned table.
fn parse_listing(
    stdout: &str,
    validator: &mut Validator,
) -> Result<(Vec<ArchiveEntry>, Vec<RejectedEntry>)> {
    let mut entries = Vec::new();
    let mut rejected = Vec::new();

    // Records start after the `----------` separator line.
    let body = stdout
        .split_once("\n----------\n")
        .map_or("", |(_, rest)| rest);
    for record in body.split("\n\n") {
        let mut path = None;
        let mut size = 0_u64;
        let mut packed = None;
        let mut attributes = String::new();
        let mut symlink_target = None;

        for line in record.lines() {
            let Some((key, value)) = line.split_once(" = ") else {
                continue;
            };
            match key.trim() {
                "Path" => path = Some(value.to_owned()),
                "Size" => size = value.trim().parse().unwrap_or(0),
                "Packed Size" => packed = value.trim().parse().ok(),
                "Attributes" => attributes = value.trim().to_owned(),
                "Symbolic Link" => symlink_target = Some(value.to_owned()),
                _ => {}
            }
        }

        let Some(path) = path else { continue };
        // `D` marks a directory; a leading `l` in the unix mode string marks a
        // symlink, which 7-Zip also reports via `Symbolic Link`.
        let kind = if attributes.starts_with('D') {
            EntryKind::Directory
        } else if symlink_target.is_some() || attributes.contains("l") && attributes.len() > 5 {
            EntryKind::Symlink
        } else {
            EntryKind::File
        };

        match validator.accept(&path, kind, size, packed, symlink_target)? {
            Outcome::Accept(e) => entries.push(*e),
            Outcome::Skip(r) => rejected.push(r),
        }
    }

    Ok((entries, rejected))
}

/// List an archive's contents with the external tool.
pub(crate) async fn inspect(
    binary: &Path,
    path: &Path,
    format: ArchiveFormat,
    validator: &mut Validator,
) -> Result<ArchiveInspection> {
    // No shell: the argument vector is passed to execve as-is.
    let output = tokio::process::Command::new(binary)
        .arg("l")
        .arg("-slt")
        .arg("-p") // empty password: never prompt, fail instead of hanging
        .arg("--")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| CoreError::Archive(format!("could not run {}: {e}", binary.display())))?;

    if !output.status.success() {
        return Err(CoreError::ArchiveRejected {
            reason: format!(
                "7-Zip could not list the archive: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }

    let (entries, rejected) = parse_listing(&String::from_utf8_lossy(&output.stdout), validator)?;
    Ok(ArchiveInspection {
        format,
        entries,
        rejected,
    })
}

/// Extract with the external tool, then re-validate everything it produced.
// Each argument is a distinct required input; grouping them would only move the
// same list into a struct read in one place.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn extract(
    binary: &Path,
    path: &Path,
    format: ArchiveFormat,
    staging: &Path,
    archive_hash: FileHash,
    validator: &mut Validator,
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
) -> Result<ArchiveManifest> {
    let inspection = inspect(binary, path, format, validator).await?;
    cancel.check()?;
    progress.emit(ProgressEvent::Started {
        operation: None,
        stage: Stage::Extracting,
        total: Some(inspection.files().count() as u64),
    });

    let status = tokio::process::Command::new(binary)
        .arg("x")
        .arg("-y")
        .arg("-bd") // no progress indicator on stdout
        .arg("-snld-") // do not restore symbolic links
        .arg("-p")
        .arg(format!("-o{}", staging.display()))
        .arg("--")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| CoreError::Archive(format!("could not run {}: {e}", binary.display())))?;

    if !status.status.success() {
        return Err(CoreError::ArchiveRejected {
            reason: format!(
                "7-Zip extraction failed: {}",
                String::from_utf8_lossy(&status.stderr).trim()
            ),
        });
    }

    // The external tool is not trusted. Walk what it actually wrote.
    let (files, directories) = revalidate_tree(staging, validator)?;
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

/// Walk an extracted tree and rebuild the manifest from what is really there.
///
/// Rejects anything that is not a regular file or directory, and anything whose
/// path does not normalize — either would mean the external tool wrote
/// something Onera did not sanction.
pub(crate) fn revalidate_tree(
    staging: &Path,
    validator: &mut Validator,
) -> Result<(Vec<ManifestFile>, Vec<RelPath>)> {
    let mut files = Vec::new();
    let mut directories = Vec::new();
    let mut total_bytes = 0_u64;

    for entry in walkdir::WalkDir::new(staging).follow_links(false) {
        let entry = entry.map_err(|e| CoreError::Archive(format!("walking staging tree: {e}")))?;
        let abs = entry.path();
        if abs == staging {
            continue;
        }
        let Ok(suffix) = abs.strip_prefix(staging) else {
            return Err(CoreError::ArchiveRejected {
                reason: format!(
                    "extracted file {} escaped the staging directory",
                    abs.display()
                ),
            });
        };
        let rel = RelPath::normalize(&suffix.to_string_lossy()).map_err(|e| {
            CoreError::ArchiveRejected {
                reason: format!("extracted path {suffix:?} is unsafe: {e}"),
            }
        })?;

        let meta = entry
            .metadata()
            .map_err(|e| CoreError::Archive(format!("stat {}: {e}", abs.display())))?;
        let file_type = meta.file_type();

        if file_type.is_symlink() {
            // 7-Zip was told not to restore links; if one exists anyway, remove
            // it and fail rather than deploy from a tree we do not understand.
            let _ = std::fs::remove_file(abs);
            return Err(CoreError::ArchiveRejected {
                reason: format!("extraction produced the symlink {rel}, which is never allowed"),
            });
        }
        if file_type.is_dir() {
            directories.push(rel);
            continue;
        }
        if !file_type.is_file() {
            let _ = std::fs::remove_file(abs);
            return Err(CoreError::ArchiveRejected {
                reason: format!("extraction produced the special file {rel}"),
            });
        }

        let size = meta.len();
        total_bytes = total_bytes.saturating_add(size);
        if total_bytes > validator.limits().max_total_bytes {
            return Err(CoreError::ArchiveRejected {
                reason: "extracted tree is larger than the configured total size limit".to_owned(),
            });
        }
        if size > validator.limits().max_file_bytes {
            return Err(CoreError::ArchiveRejected {
                reason: format!("extracted file {rel} is larger than the per-file limit"),
            });
        }

        let executable = {
            use std::os::unix::fs::PermissionsExt as _;
            meta.permissions().mode() & 0o111 != 0
        };
        let hash = hash_path(abs)?;
        files.push(ManifestFile {
            path: rel,
            size,
            hash,
            executable,
        });
    }

    Ok((files, directories))
}

fn hash_path(path: &Path) -> Result<FileHash> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path).map_err(|e| CoreError::fs(path, e))?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0_u8; 256 * 1024];
    loop {
        let read = file.read(&mut buf).map_err(|e| CoreError::fs(path, e))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(FileHash::blake3(*hasher.finalize().as_bytes()))
}

/// Error used when the tool is absent, exposed for the facade.
pub(crate) fn require_binary(configured: Option<&PathBuf>) -> Result<PathBuf> {
    configured
        .cloned()
        .or_else(find_sevenz)
        .ok_or_else(missing_tool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::ExtractionLimits;

    const LISTING: &str = "\
7-Zip 23.01

Listing archive: mod.7z

----------
Path = archive
Size = 0
Packed Size = 0
Attributes = D_ drwxr-xr-x

Path = archive/pc/mod/thing.archive
Size = 1048576
Packed Size = 524288
Attributes = A_ -rw-r--r--

Path = evil
Size = 0
Packed Size = 0
Attributes = A_ lrwxrwxrwx
Symbolic Link = ../../../../etc/passwd
";

    fn validator() -> Validator {
        Validator::new(ExtractionLimits::strict())
    }

    #[test]
    fn parses_a_technical_listing() {
        let (entries, rejected) = parse_listing(LISTING, &mut validator()).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].kind, EntryKind::Directory);
        assert_eq!(entries[1].path.as_str(), "archive/pc/mod/thing.archive");
        assert_eq!(entries[1].declared_size, 1_048_576);
        assert_eq!(entries[1].compressed_size, Some(524_288));

        assert_eq!(rejected.len(), 1, "the symlink must be dropped");
        assert!(rejected[0].reason.contains("etc/passwd"));
    }

    #[test]
    fn a_traversal_entry_in_a_listing_fails_the_archive() {
        let hostile = "x\n----------\nPath = ../../escape.txt\nSize = 1\nAttributes = A_\n";
        let err = parse_listing(hostile, &mut validator()).unwrap_err();
        assert!(matches!(err, CoreError::ArchiveRejected { .. }), "{err:?}");
    }

    #[test]
    fn revalidation_rejects_a_symlink_the_tool_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ok.txt"), b"fine").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("sneaky")).unwrap();

        let err = revalidate_tree(dir.path(), &mut validator()).unwrap_err();
        assert!(format!("{err}").contains("symlink"), "{err}");
        assert!(
            !dir.path().join("sneaky").exists(),
            "the link must be removed"
        );
    }

    #[test]
    fn revalidation_builds_a_manifest_from_what_is_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("archive/pc")).unwrap();
        std::fs::write(dir.path().join("archive/pc/a.archive"), b"payload").unwrap();

        let (files, dirs) = revalidate_tree(dir.path(), &mut validator()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path.as_str(), "archive/pc/a.archive");
        assert_eq!(files[0].hash, FileHash::blake3_of(b"payload"));
        assert_eq!(dirs.len(), 2);
    }

    #[test]
    fn revalidation_enforces_the_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("big"), vec![0_u8; 4096]).unwrap();
        let limits = ExtractionLimits {
            max_total_bytes: 100,
            ..ExtractionLimits::strict()
        };
        assert!(revalidate_tree(dir.path(), &mut Validator::new(limits)).is_err());
    }
}
