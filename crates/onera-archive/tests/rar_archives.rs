//! Tests against real RAR archives.
//!
//! RAR is the one format Onera supports that it cannot *write*: there is no
//! free encoder, and `7zz` decodes RAR but never produces it. Rather than
//! leave the format untested — which is how a parser bug survived until now —
//! this file emits the RAR 5.0 container directly. The format is simple enough
//! for stored (uncompressed) entries, and the resulting bytes are read back by
//! the same external `7zz` a user's machine would use.
//!
//! Keeping the writer here rather than checking in a binary fixture follows the
//! same rule as `malicious_archives.rs`: the construction is the documentation.

use onera_archive::{find_sevenz, ExtractionLimits, SafeArchiveBackend};
use onera_core::domain::archive::{ArchiveFormat, EntryKind};
use onera_core::hash::FileHash;
use onera_core::ports::ArchiveBackend;
use onera_core::progress::{CancelToken, NullProgress};
use onera_core::CoreError;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// A minimal RAR 5.0 writer
// ---------------------------------------------------------------------------

/// RAR 5.0 signature. The trailing `01 00` distinguishes it from RAR 4.
const RAR5_SIGNATURE: &[u8] = b"Rar!\x1a\x07\x01\x00";

const BLOCK_MAIN: u64 = 1;
const BLOCK_FILE: u64 = 2;
const BLOCK_END: u64 = 5;

/// Header flag: a data area follows the header.
const HFLAG_DATA: u64 = 0x0002;

/// File flag: this entry is a directory.
const FFLAG_DIRECTORY: u64 = 0x0001;
/// File flag: a CRC32 of the unpacked data is present.
const FFLAG_CRC: u64 = 0x0004;

/// Host OS 1 is unix, which is what makes 7-Zip render the mode as `ls -l`
/// does. With the Windows value it ignores the attribute word entirely.
const HOST_OS_UNIX: u64 = 1;

/// RAR's variable-length integer: 7 bits per byte, high bit means "continues".
fn vint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        out.push(if value == 0 { byte } else { byte | 0x80 });
        if value == 0 {
            return out;
        }
    }
}

/// Assemble one block: `crc32 || vint(header size) || header || data`.
///
/// The CRC covers the size field and the header, but not the data area.
fn block(header_type: u64, header_flags: u64, body: &[u8], data: &[u8]) -> Vec<u8> {
    let mut header = vint(header_type);
    header.extend(vint(header_flags));
    if header_flags & HFLAG_DATA != 0 {
        header.extend(vint(data.len() as u64));
    }
    header.extend_from_slice(body);

    let mut sized = vint(header.len() as u64);
    sized.extend_from_slice(&header);

    let mut out = crc32(&sized).to_le_bytes().to_vec();
    out.extend_from_slice(&sized);
    out.extend_from_slice(data);
    out
}

/// One entry to put in a RAR.
struct RarEntry {
    path: &'static str,
    data: &'static [u8],
    /// The unix mode. `0o040755` is a directory, `0o120777` a symbolic link.
    mode: u32,
}

impl RarEntry {
    fn file(path: &'static str, data: &'static [u8]) -> Self {
        Self {
            path,
            data,
            mode: 0o100_644,
        }
    }

    fn executable(path: &'static str, data: &'static [u8]) -> Self {
        Self {
            path,
            data,
            mode: 0o100_755,
        }
    }

    fn directory(path: &'static str) -> Self {
        Self {
            path,
            data: b"",
            mode: 0o040_755,
        }
    }

    /// A link whose stored "content" is its target, the way `rar` records one.
    fn symlink(path: &'static str, target: &'static [u8]) -> Self {
        Self {
            path,
            data: target,
            mode: 0o120_777,
        }
    }

    fn is_directory(&self) -> bool {
        self.mode & 0o170_000 == 0o040_000
    }

    fn to_block(&self) -> Vec<u8> {
        let mut flags = FFLAG_CRC;
        if self.is_directory() {
            flags |= FFLAG_DIRECTORY;
        }

        let mut body = vint(flags);
        body.extend(vint(self.data.len() as u64)); // unpacked size
        body.extend(vint(u64::from(self.mode))); // attributes
        body.extend(crc32(self.data).to_le_bytes());
        body.extend(vint(0)); // compression info: version 5, method "store"
        body.extend(vint(HOST_OS_UNIX));
        body.extend(vint(self.path.len() as u64));
        body.extend_from_slice(self.path.as_bytes());

        // A directory has no data area, so it must not claim one.
        let header_flags = if self.data.is_empty() { 0 } else { HFLAG_DATA };
        block(BLOCK_FILE, header_flags, &body, self.data)
    }
}

/// Emit a complete RAR 5.0 archive.
fn build_rar(entries: &[RarEntry]) -> Vec<u8> {
    let mut out = RAR5_SIGNATURE.to_vec();
    out.extend(block(BLOCK_MAIN, 0, &vint(0), b"")); // no archive flags
    for entry in entries {
        out.extend(entry.to_block());
    }
    out.extend(block(BLOCK_END, 0, &vint(0), b""));
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

fn write_rar(dir: &Path, name: &str, entries: &[RarEntry]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, build_rar(entries)).unwrap();
    path
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn backend() -> SafeArchiveBackend {
    SafeArchiveBackend::new(ExtractionLimits::strict())
}

/// RAR needs the external tool. CI installs it; a contributor's machine may
/// not have it, and skipping is better than a failure they cannot act on.
///
/// Returns `false` when the suite should be skipped.
fn sevenz_available() -> bool {
    if find_sevenz().is_some() {
        return true;
    }
    eprintln!("skipping: no 7zz/7z/7za on PATH (install p7zip-full)");
    false
}

/// The layout a real Cyberpunk mod arrives in, as a RAR — the format a large
/// share of Nexus archives actually use.
fn mod_entries() -> Vec<RarEntry> {
    vec![
        RarEntry::directory("archive"),
        RarEntry::directory("archive/pc"),
        RarEntry::directory("archive/pc/mod"),
        RarEntry::file("archive/pc/mod/thing.archive", b"cyberpunk archive payload"),
        RarEntry::directory("r6"),
        RarEntry::directory("r6/scripts"),
        RarEntry::file("r6/scripts/thing.reds", b"module Thing\n"),
        RarEntry::file("readme.txt", b"docs, not mod content\n"),
    ]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_rar_is_detected_from_its_signature_not_its_name() {
    let dir = tempfile::tempdir().unwrap();
    // Named `.zip`, but the bytes say RAR.
    let path = write_rar(dir.path(), "mislabelled.zip", &mod_entries());

    assert_eq!(
        onera_archive::detect_format(&path).await.unwrap(),
        ArchiveFormat::Rar
    );
}

/// The regression that motivated this file.
///
/// `7z l -slt` prints a `Symbolic Link` key for every RAR entry and leaves it
/// blank for ordinary ones. Reading a blank value as a link classified every
/// entry of every RAR as an unextractable symlink, so a user previewing a RAR
/// mod saw an archive with no content and a page of rejections.
#[tokio::test]
async fn inspecting_a_rar_lists_its_real_entries() {
    if !sevenz_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = write_rar(dir.path(), "mod.rar", &mod_entries());

    let inspection = backend().inspect(&path, &CancelToken::new()).await.unwrap();

    assert_eq!(inspection.format, ArchiveFormat::Rar);
    assert!(
        inspection.rejected.is_empty(),
        "a benign RAR rejected entries: {:?}",
        inspection.rejected
    );

    let files: Vec<&str> = inspection.files().map(|f| f.path.as_str()).collect();
    assert_eq!(
        files,
        vec![
            "archive/pc/mod/thing.archive",
            "r6/scripts/thing.reds",
            "readme.txt"
        ]
    );
    assert!(inspection
        .entries
        .iter()
        .any(|e| e.kind == EntryKind::Directory && e.path.as_str() == "archive/pc/mod"));

    let payload = inspection
        .files()
        .find(|f| f.path.as_str() == "archive/pc/mod/thing.archive")
        .unwrap();
    assert_eq!(payload.declared_size, 25);
}

#[tokio::test]
async fn extracting_a_rar_writes_every_file_and_hashes_what_landed() {
    if !sevenz_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = write_rar(dir.path(), "mod.rar", &mod_entries());
    let staging = dir.path().join("staging");

    let manifest = backend()
        .extract(&path, &staging, &NullProgress, &CancelToken::new())
        .await
        .unwrap();

    assert_eq!(manifest.format, ArchiveFormat::Rar);

    let mut paths: Vec<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(
        paths,
        vec![
            "archive/pc/mod/thing.archive",
            "r6/scripts/thing.reds",
            "readme.txt"
        ]
    );

    // The bytes on disk are the bytes the archive carried.
    let payload = staging.join("archive/pc/mod/thing.archive");
    assert_eq!(
        std::fs::read(&payload).unwrap(),
        b"cyberpunk archive payload"
    );

    // And the manifest hashes what landed, not what the header claimed.
    let entry = manifest
        .files
        .iter()
        .find(|f| f.path.as_str() == "archive/pc/mod/thing.archive")
        .unwrap();
    assert_eq!(
        entry.hash,
        FileHash::blake3_of(b"cyberpunk archive payload")
    );
    assert_eq!(entry.size, 25);

    // Directories the archive declared are recorded, so an empty one a mod
    // needs is not silently lost.
    let dirs: Vec<&str> = manifest.directories.iter().map(|d| d.as_str()).collect();
    assert!(dirs.contains(&"archive/pc/mod"), "{dirs:?}");
}

#[tokio::test]
async fn a_rar_records_the_executable_bit_from_disk() {
    if !sevenz_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = write_rar(
        dir.path(),
        "tools.rar",
        &[
            RarEntry::executable("tools/run.sh", b"#!/bin/sh\nexit 0\n"),
            RarEntry::file("tools/notes.txt", b"plain\n"),
        ],
    );
    let staging = dir.path().join("staging");

    let manifest = backend()
        .extract(&path, &staging, &NullProgress, &CancelToken::new())
        .await
        .unwrap();

    let script = manifest
        .files
        .iter()
        .find(|f| f.path.as_str() == "tools/run.sh")
        .unwrap();
    let notes = manifest
        .files
        .iter()
        .find(|f| f.path.as_str() == "tools/notes.txt")
        .unwrap();
    assert!(script.executable, "the executable bit was lost");
    assert!(!notes.executable, "a plain file was marked executable");
}

#[tokio::test]
async fn a_traversal_entry_fails_the_whole_rar() {
    if !sevenz_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = write_rar(
        dir.path(),
        "hostile.rar",
        &[
            RarEntry::file("archive/pc/mod/ok.archive", b"benign"),
            RarEntry::file("../../../../etc/cron.d/pwn", b"* * * * * root sh\n"),
        ],
    );
    let staging = dir.path().join("staging");

    // A `..` component is never an accident, so the archive is refused whole
    // rather than having the one entry dropped.
    let err = backend()
        .inspect(&path, &CancelToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::ArchiveRejected { .. }), "{err:?}");

    let err = backend()
        .extract(&path, &staging, &NullProgress, &CancelToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::ArchiveRejected { .. }), "{err:?}");

    assert!(
        !dir.path().join("etc").exists() && !staging.join("etc").exists(),
        "a traversal entry escaped"
    );
}

#[tokio::test]
async fn rar_symlinks_are_reported_and_never_created() {
    if !sevenz_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = write_rar(
        dir.path(),
        "linky.rar",
        &[
            RarEntry::file("archive/pc/mod/ok.archive", b"benign"),
            RarEntry::symlink("archive/escape", b"../../../../etc"),
        ],
    );
    let staging = dir.path().join("staging");

    // Unlike traversal, a link is dropped rather than fatal: archives made by
    // ordinary tooling contain them.
    let inspection = backend().inspect(&path, &CancelToken::new()).await.unwrap();
    assert_eq!(inspection.rejected.len(), 1);
    assert_eq!(inspection.rejected[0].raw_path, "archive/escape");
    assert!(
        inspection.rejected[0].reason.contains("symbolic link"),
        "{}",
        inspection.rejected[0].reason
    );
    assert_eq!(inspection.files().count(), 1);

    let manifest = backend()
        .extract(&path, &staging, &NullProgress, &CancelToken::new())
        .await
        .unwrap();
    assert_eq!(manifest.files.len(), 1);
    assert!(
        std::fs::symlink_metadata(staging.join("archive/escape")).is_err(),
        "a symbolic link was created on disk"
    );
}

#[tokio::test]
async fn a_rar_is_held_to_the_same_size_limits_as_every_other_format() {
    if !sevenz_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = write_rar(
        dir.path(),
        "big.rar",
        &[RarEntry::file("archive/pc/mod/big.archive", &[0_u8; 4096])],
    );

    let limits = ExtractionLimits {
        max_file_bytes: 100,
        ..ExtractionLimits::strict()
    };
    let err = SafeArchiveBackend::new(limits)
        .extract(
            &path,
            &dir.path().join("staging"),
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::ArchiveRejected { .. }), "{err:?}");
}

/// A RAR that 7-Zip cannot read must fail as a rejected archive, not as a
/// panic or a silently empty manifest.
#[tokio::test]
async fn a_corrupt_rar_is_rejected() {
    if !sevenz_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("corrupt.rar");
    // A valid signature followed by rubbish, so detection succeeds and the
    // failure has to come from the backend.
    let mut bytes = RAR5_SIGNATURE.to_vec();
    bytes.extend_from_slice(&[0xFF; 64]);
    std::fs::write(&path, bytes).unwrap();

    let err = backend()
        .inspect(&path, &CancelToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::ArchiveRejected { .. }), "{err:?}");
}
