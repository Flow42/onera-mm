//! Container format detection.
//!
//! Detection reads magic bytes rather than trusting the filename. Mod archives
//! are routinely misnamed — a `.zip` that is really a 7z, a `.rar` that is
//! really a zip — and picking a backend from the extension would hand a
//! hostile file to a parser that was not expecting it.

use onera_core::domain::archive::ArchiveFormat;
use onera_core::{CoreError, Result};
use std::path::Path;
use tokio::io::AsyncReadExt as _;

/// Bytes read from the start of a file to identify it.
const MAGIC_LEN: usize = 512;

/// Identify an archive by content, falling back to its extension only for
/// formats that have no distinguishing header.
///
/// # Errors
/// Returns [`CoreError::ArchiveRejected`] if the format is not recognized.
pub async fn detect_format(path: &Path) -> Result<ArchiveFormat> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| CoreError::fs(path, e))?;
    let mut head = vec![0_u8; MAGIC_LEN];
    let read = file
        .read(&mut head)
        .await
        .map_err(|e| CoreError::fs(path, e))?;
    head.truncate(read);

    detect_from_bytes(&head).ok_or_else(|| CoreError::ArchiveRejected {
        reason: format!(
            "unrecognized archive format (first bytes: {})",
            head.iter()
                .take(8)
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ),
    })
}

/// Identify a format from a file's leading bytes.
///
/// Returns `None` when nothing matches.
#[must_use]
pub fn detect_from_bytes(head: &[u8]) -> Option<ArchiveFormat> {
    if head.starts_with(b"PK\x03\x04")
        || head.starts_with(b"PK\x05\x06")
        || head.starts_with(b"PK\x07\x08")
    {
        return Some(ArchiveFormat::Zip);
    }
    if head.starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]) {
        return Some(ArchiveFormat::SevenZ);
    }
    if head.starts_with(b"Rar!\x1A\x07") {
        return Some(ArchiveFormat::Rar);
    }
    if head.starts_with(&[0x1F, 0x8B]) {
        return Some(ArchiveFormat::TarGz);
    }
    if head.starts_with(b"BZh") {
        return Some(ArchiveFormat::TarBz2);
    }
    if head.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) {
        return Some(ArchiveFormat::TarXz);
    }
    if head.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return Some(ArchiveFormat::TarZstd);
    }
    // Tar has no header magic at offset 0; the `ustar` marker sits at 257.
    if head.len() > 262 && (&head[257..262] == b"ustar") {
        return Some(ArchiveFormat::Tar);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_formats_by_magic() {
        assert_eq!(
            detect_from_bytes(b"PK\x03\x04rest"),
            Some(ArchiveFormat::Zip)
        );
        assert_eq!(
            detect_from_bytes(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C, 0]),
            Some(ArchiveFormat::SevenZ)
        );
        assert_eq!(
            detect_from_bytes(&[0x1F, 0x8B, 0x08]),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(detect_from_bytes(b"BZh9x"), Some(ArchiveFormat::TarBz2));
        assert_eq!(
            detect_from_bytes(b"Rar!\x1A\x07\x00"),
            Some(ArchiveFormat::Rar)
        );
    }

    #[test]
    fn detects_plain_tar_by_its_offset_marker() {
        let mut head = vec![0_u8; 300];
        head[257..262].copy_from_slice(b"ustar");
        assert_eq!(detect_from_bytes(&head), Some(ArchiveFormat::Tar));
    }

    #[test]
    fn rejects_unknown_content() {
        assert_eq!(detect_from_bytes(b"not an archive at all"), None);
        assert_eq!(detect_from_bytes(b""), None);
    }

    #[tokio::test]
    async fn a_zip_named_dot_rar_is_still_detected_as_a_zip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trap.rar");
        tokio::fs::write(&path, b"PK\x03\x04and then some")
            .await
            .unwrap();
        assert_eq!(detect_format(&path).await.unwrap(), ArchiveFormat::Zip);
    }

    #[tokio::test]
    async fn unrecognized_files_are_rejected_not_guessed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mod.zip");
        tokio::fs::write(&path, b"this is a readme, not a zip")
            .await
            .unwrap();
        let err = detect_format(&path).await.unwrap_err();
        assert!(matches!(err, CoreError::ArchiveRejected { .. }), "{err:?}");
    }
}
