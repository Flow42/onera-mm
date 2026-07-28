//! End-to-end tests against deliberately hostile archives.
//!
//! Each test builds its archive in the test itself rather than checking a
//! binary fixture into the repository: the construction *is* the documentation
//! of what the attack looks like, and a reader can see exactly which bytes are
//! being defended against.

use onera_archive::{ExtractionLimits, SafeArchiveBackend};
use onera_core::domain::archive::EntryKind;
use onera_core::ports::ArchiveBackend;
use onera_core::progress::{CancelToken, NullProgress};
use onera_core::CoreError;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;

fn backend() -> SafeArchiveBackend {
    SafeArchiveBackend::new(ExtractionLimits::strict())
}

/// Build a zip from `(path, contents)` pairs, writing the paths verbatim so
/// hostile names survive into the archive.
fn write_zip(dir: &Path, name: &str, entries: &[(&str, &[u8])]) -> PathBuf {
    let path = dir.join(name);
    let file = std::fs::File::create(&path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    for (entry_path, contents) in entries {
        zip.start_file(*entry_path, SimpleFileOptions::default())
            .unwrap();
        zip.write_all(contents).unwrap();
    }
    zip.finish().unwrap();
    path
}

fn write_tar_gz(dir: &Path, name: &str, build: impl FnOnce(&mut tar::Builder<Vec<u8>>)) -> PathBuf {
    let mut builder = tar::Builder::new(Vec::new());
    build(&mut builder);
    let raw = builder.into_inner().unwrap();

    let path = dir.join(name);
    let mut encoder = flate2::write::GzEncoder::new(
        std::fs::File::create(&path).unwrap(),
        flate2::Compression::default(),
    );
    encoder.write_all(&raw).unwrap();
    encoder.finish().unwrap();
    path
}

fn tar_entry(
    builder: &mut tar::Builder<Vec<u8>>,
    path: &str,
    kind: tar::EntryType,
    link: Option<&str>,
) {
    let mut header = tar::Header::new_gnu();
    header.set_size(0);
    header.set_mode(0o644);
    header.set_entry_type(kind);
    if let Some(link) = link {
        header.set_link_name(link).unwrap();
    }
    header.set_cksum();
    builder
        .append_data(&mut header.clone(), path, std::io::empty())
        .unwrap();
}

#[tokio::test]
async fn extracts_a_benign_zip_and_hashes_every_file() {
    let dir = tempfile::tempdir().unwrap();
    let archive = write_zip(
        dir.path(),
        "mod.zip",
        &[
            ("archive/pc/mod/thing.archive", b"payload bytes"),
            ("readme.txt", b"hello"),
        ],
    );
    let staging = dir.path().join("staging");

    let manifest = backend()
        .extract(&archive, &staging, &NullProgress, &CancelToken::new())
        .await
        .unwrap();

    assert_eq!(manifest.files.len(), 2);
    let thing = manifest
        .file(&onera_core::RelPath::normalize("archive/pc/mod/thing.archive").unwrap())
        .unwrap();
    assert_eq!(thing.size, 13);
    assert_eq!(
        thing.hash,
        onera_core::FileHash::blake3_of(b"payload bytes")
    );
    assert_eq!(
        std::fs::read(staging.join("archive/pc/mod/thing.archive")).unwrap(),
        b"payload bytes"
    );
}

#[tokio::test]
async fn zip_slip_is_rejected_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let victim = dir.path().join("victim");
    std::fs::create_dir_all(&victim).unwrap();
    let archive = write_zip(
        dir.path(),
        "slip.zip",
        &[("../victim/pwned.txt", b"you have been owned")],
    );
    let staging = dir.path().join("staging");

    let err = backend()
        .extract(&archive, &staging, &NullProgress, &CancelToken::new())
        .await
        .unwrap_err();

    assert!(matches!(err, CoreError::ArchiveRejected { .. }), "{err:?}");
    assert!(
        !victim.join("pwned.txt").exists(),
        "zip slip wrote outside staging"
    );
}

#[tokio::test]
async fn windows_style_traversal_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let archive = write_zip(dir.path(), "slip2.zip", &[(r"..\..\evil.dll", b"x")]);
    let err = backend()
        .extract(
            &archive,
            &dir.path().join("staging"),
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::ArchiveRejected { .. }), "{err:?}");
}

#[tokio::test]
async fn absolute_paths_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let archive = write_zip(dir.path(), "abs.zip", &[("/etc/cron.d/backdoor", b"x")]);
    let err = backend()
        .inspect(&archive, &CancelToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::ArchiveRejected { .. }), "{err:?}");
}

#[tokio::test]
async fn a_wrapped_archive_keeps_its_wrapper_directory_in_the_manifest() {
    // Layout unwrapping is the game adapter's job, not the extractor's: the
    // manifest must reflect the archive exactly as it was.
    let dir = tempfile::tempdir().unwrap();
    let archive = write_zip(
        dir.path(),
        "wrapped.zip",
        &[("My Cool Mod v1.2/archive/pc/mod/a.archive", b"data")],
    );
    let manifest = backend()
        .extract(
            &archive,
            &dir.path().join("staging"),
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(manifest.files[0].path.first_component(), "My Cool Mod v1.2");
}

#[tokio::test]
async fn a_compression_bomb_is_rejected_before_extraction() {
    let dir = tempfile::tempdir().unwrap();
    // 32 MiB of zeroes compresses to almost nothing: a ~30000:1 ratio, well
    // past the 100:1 limit in `ExtractionLimits::strict`.
    let payload = vec![0_u8; 32 * 1024 * 1024];
    let archive = write_zip(dir.path(), "bomb.zip", &[("bomb.bin", &payload)]);

    let err = backend()
        .inspect(&archive, &CancelToken::new())
        .await
        .unwrap_err();
    let message = format!("{err}");
    assert!(message.contains("compression ratio"), "{message}");
}

#[tokio::test]
async fn too_many_entries_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let entries: Vec<(String, Vec<u8>)> = (0..1_500)
        .map(|i| (format!("f{i}.txt"), b"x".to_vec()))
        .collect();
    let borrowed: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_slice()))
        .collect();
    let archive = write_zip(dir.path(), "many.zip", &borrowed);

    // `strict` allows 1000 entries.
    let err = backend()
        .inspect(&archive, &CancelToken::new())
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("more than 1000 entries"), "{err}");
}

#[tokio::test]
async fn total_size_limit_is_enforced() {
    let dir = tempfile::tempdir().unwrap();
    // Incompressible data, so the ratio heuristic does not fire first and the
    // total-size limit is genuinely what stops it.
    let big: Vec<u8> = (0..2_000_000_u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    let archive = write_zip(dir.path(), "big.zip", &[("a.bin", &big), ("b.bin", &big)]);

    let limits = ExtractionLimits {
        max_total_bytes: 3_000_000,
        ..ExtractionLimits::strict()
    };
    let err = SafeArchiveBackend::new(limits)
        .inspect(&archive, &CancelToken::new())
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("in total"), "{err}");
}

#[tokio::test]
async fn deep_nesting_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let deep = vec!["d"; 40].join("/") + "/f.txt";
    let archive = write_zip(dir.path(), "deep.zip", &[(deep.as_str(), b"x")]);
    let err = backend()
        .inspect(&archive, &CancelToken::new())
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("nests deeper"), "{err}");
}

#[tokio::test]
async fn tar_symlinks_are_reported_and_never_created() {
    let dir = tempfile::tempdir().unwrap();
    let archive = write_tar_gz(dir.path(), "links.tar.gz", |b| {
        tar_entry(b, "escape", tar::EntryType::Symlink, Some("/etc/passwd"));
        tar_entry(b, "hard", tar::EntryType::Link, Some("escape"));
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        b.append_data(&mut header, "real.txt", &b"data"[..])
            .unwrap();
    });

    let inspection = backend()
        .inspect(&archive, &CancelToken::new())
        .await
        .unwrap();
    assert_eq!(
        inspection.entries.len(),
        1,
        "only the regular file survives"
    );
    assert_eq!(inspection.entries[0].kind, EntryKind::File);
    assert_eq!(inspection.rejected.len(), 2);
    assert!(inspection
        .rejected
        .iter()
        .any(|r| r.reason.contains("symbolic links")));
    assert!(inspection
        .rejected
        .iter()
        .any(|r| r.reason.contains("hard links")));

    let staging = dir.path().join("staging");
    let manifest = backend()
        .extract(&archive, &staging, &NullProgress, &CancelToken::new())
        .await
        .unwrap();
    assert_eq!(manifest.files.len(), 1);
    assert!(
        std::fs::symlink_metadata(staging.join("escape")).is_err(),
        "a symlink was created on disk"
    );
}

#[tokio::test]
async fn tar_special_files_are_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let archive = write_tar_gz(dir.path(), "special.tar.gz", |b| {
        tar_entry(b, "dev/null", tar::EntryType::Char, None);
        tar_entry(b, "pipe", tar::EntryType::Fifo, None);
    });
    let inspection = backend()
        .inspect(&archive, &CancelToken::new())
        .await
        .unwrap();
    assert!(inspection.entries.is_empty());
    assert_eq!(inspection.rejected.len(), 2);
}

/// Assemble a tar entry byte-for-byte.
///
/// The `tar` crate deliberately refuses to *write* a `..` path, which is
/// exactly the entry this test needs, so the 512-byte header is built by hand.
/// That is also what a real attacker does.
fn raw_tar_entry(name: &str, data: &[u8]) -> Vec<u8> {
    let mut header = [0_u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    header[100..107].copy_from_slice(b"0000644");
    header[108..115].copy_from_slice(b"0000000");
    header[116..123].copy_from_slice(b"0000000");
    let size = format!("{:011o}\0", data.len());
    header[124..136].copy_from_slice(size.as_bytes());
    header[136..148].copy_from_slice(b"00000000000\0");
    header[156] = b'0'; // regular file
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");

    // The checksum is computed with the checksum field itself read as spaces.
    header[148..156].copy_from_slice(b"        ");
    let sum: u32 = header.iter().map(|b| u32::from(*b)).sum();
    let checksum = format!("{sum:06o}\0 ");
    header[148..156].copy_from_slice(checksum.as_bytes());

    let mut out = header.to_vec();
    out.extend_from_slice(data);
    out.resize(out.len().div_ceil(512) * 512, 0);
    out
}

fn write_raw_tar_gz(dir: &Path, name: &str, entries: &[(&str, &[u8])]) -> PathBuf {
    let mut raw = Vec::new();
    for (path, data) in entries {
        raw.extend_from_slice(&raw_tar_entry(path, data));
    }
    raw.extend_from_slice(&[0_u8; 1024]); // end-of-archive marker

    let path = dir.join(name);
    let mut encoder = flate2::write::GzEncoder::new(
        std::fs::File::create(&path).unwrap(),
        flate2::Compression::default(),
    );
    encoder.write_all(&raw).unwrap();
    encoder.finish().unwrap();
    path
}

#[tokio::test]
async fn a_tar_traversal_entry_fails_the_whole_archive() {
    let dir = tempfile::tempdir().unwrap();
    let archive = write_raw_tar_gz(dir.path(), "slip.tar.gz", &[("../../escape.txt", b"bad")]);
    let err = backend()
        .inspect(&archive, &CancelToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::ArchiveRejected { .. }), "{err:?}");
}

#[tokio::test]
async fn a_hand_built_tar_still_round_trips_when_it_is_benign() {
    // Guards the hand-built header above: if it were malformed, the traversal
    // test could pass for the wrong reason.
    let dir = tempfile::tempdir().unwrap();
    let archive = write_raw_tar_gz(dir.path(), "ok.tar.gz", &[("mods/a.txt", b"hello")]);
    let manifest = backend()
        .extract(
            &archive,
            &dir.path().join("staging"),
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(manifest.files.len(), 1);
    assert_eq!(manifest.files[0].path.as_str(), "mods/a.txt");
    assert_eq!(manifest.files[0].size, 5);
}

#[tokio::test]
async fn extraction_refuses_a_dirty_staging_directory() {
    let dir = tempfile::tempdir().unwrap();
    let archive = write_zip(dir.path(), "m.zip", &[("a.txt", b"x")]);
    let staging = dir.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(staging.join("leftover"), b"from a previous run").unwrap();

    let err = backend()
        .extract(&archive, &staging, &NullProgress, &CancelToken::new())
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("not empty"), "{err}");
}

#[tokio::test]
async fn cancellation_stops_extraction() {
    let dir = tempfile::tempdir().unwrap();
    let entries: Vec<(String, Vec<u8>)> = (0..200)
        .map(|i| (format!("f{i}.txt"), vec![b'x'; 1024]))
        .collect();
    let borrowed: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_slice()))
        .collect();
    let archive = write_zip(dir.path(), "many.zip", &borrowed);

    let cancel = CancelToken::new();
    cancel.cancel();
    let err = backend()
        .extract(
            &archive,
            &dir.path().join("staging"),
            &NullProgress,
            &cancel,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Cancelled), "{err:?}");
}

#[tokio::test]
async fn progress_is_reported_while_extracting() {
    let dir = tempfile::tempdir().unwrap();
    let archive = write_zip(dir.path(), "p.zip", &[("a.txt", b"1"), ("b.txt", b"2")]);
    let sink = onera_core::progress::RecordingProgress::default();

    backend()
        .extract(
            &archive,
            &dir.path().join("staging"),
            &sink,
            &CancelToken::new(),
        )
        .await
        .unwrap();

    let events = sink.events();
    assert!(
        events.len() >= 4,
        "expected start, two advances and finish: {events:?}"
    );
    assert!(matches!(
        events.last(),
        Some(onera_core::progress::ProgressEvent::Finished { success: true, .. })
    ));
}
