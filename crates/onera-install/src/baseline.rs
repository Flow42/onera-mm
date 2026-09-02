//! Read-only baseline capture and verification.
//!
//! This module is the filesystem-facing half of the baseline feature. It takes
//! only provider-neutral core values: game-adapter roots and exclusions,
//! immutable baseline records, and the set of targets Onera currently manages.
//! It neither loads nor persists those values, and it never mutates a scanned
//! root.

use onera_core::domain::baseline::{
    excluded_by, BaselineExclusion, BaselineFile, BaselineFinding, BaselineRoot, BaselineScanRun,
    BaselineVerification, FileClassification, FindingCounts, GameBaseline, ScanEvidence,
    ScanPurpose, ScanScopeFingerprint, ScanState,
};
use onera_core::hash::FileHash;
use onera_core::ids::{BaselineScanRunId, LocalGameId};
use onera_core::paths::{DeployRootKind, RelPath};
use onera_core::plan::TargetLocation;
use onera_core::progress::{CancelToken, ProgressEvent, ProgressSink, Stage};
use onera_core::{CoreError, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};
use tokio::io::AsyncReadExt as _;
use walkdir::{DirEntry, WalkDir};

/// The complete, persistence-neutral result of a baseline capture scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineCapture {
    /// Progress and terminal state of this scan.
    pub run: BaselineScanRun,
    /// Fingerprint of the adapter-declared roots and exclusions.
    pub scope_fingerprint: ScanScopeFingerprint,
    /// Regular, readable files safe to place in the trusted baseline.
    pub files: Vec<BaselineFile>,
    /// Unreadable entries and rejected links or special files.
    pub findings: Vec<BaselineFinding>,
}

/// A verification together with the scan-run metadata needed by persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineVerificationScan {
    /// Progress and terminal state of this scan.
    pub run: BaselineScanRun,
    /// The provider-neutral verification result.
    pub verification: BaselineVerification,
}

/// Provider-neutral inputs for comparing one installation with its baseline.
pub struct BaselineVerificationRequest<'a> {
    /// Installation being verified.
    pub game: LocalGameId,
    /// Baseline metadata, including its identity and captured scope.
    pub baseline: &'a GameBaseline,
    /// Immutable file records belonging to `baseline`.
    pub baseline_files: &'a [BaselineFile],
    /// Store-managed roots declared by the current game adapter.
    pub roots: &'a [BaselineRoot],
    /// Exclusions declared by the current game adapter.
    pub exclusions: &'a [BaselineExclusion],
    /// Active targets Onera recognizes as its own deployments.
    pub managed_targets: &'a BTreeSet<TargetLocation>,
}

#[derive(Debug)]
struct DiskFile {
    hash: FileHash,
    size: u64,
    mode: Option<u32>,
}

#[derive(Debug)]
enum DiskEntry {
    File(DiskFile),
    Special(String),
    Unreadable(String),
}

type EntryMap = BTreeMap<(String, RelPath), DiskEntry>;

struct ScanOutput {
    state: ScanState,
    entries: EntryMap,
    unreadable_prefixes: Vec<(String, RelPath, String)>,
    files_scanned: u64,
    bytes_hashed: u64,
}

/// Hash every included regular file in the adapter-declared baseline scope.
///
/// Symlinks and non-regular files are findings, never baseline records. An I/O
/// failure on an entry is also a finding so one bad file does not hide the rest
/// of the scan. Cancellation returns a partial capture with
/// [`ScanState::Cancelled`].
///
/// # Errors
///
/// Returns an error when the declared scope itself is invalid or inaccessible,
/// or when a filesystem path cannot be represented by the core [`RelPath`]
/// contract.
pub async fn capture_baseline(
    game: LocalGameId,
    roots: &[BaselineRoot],
    exclusions: &[BaselineExclusion],
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
) -> Result<BaselineCapture> {
    validate_roots(roots).await?;
    let started_at = chrono::Utc::now();
    let run_id = BaselineScanRunId::new();
    progress.emit(ProgressEvent::Started {
        operation: None,
        stage: Stage::Hashing,
        total: None,
    });

    let output = scan_scope(roots, exclusions, Stage::Hashing, progress, cancel).await?;
    let mut files = Vec::new();
    let mut findings = Vec::new();
    for ((root_key, path), entry) in output.entries {
        match entry {
            DiskEntry::File(file) => files.push(BaselineFile {
                root_key,
                path,
                hash: file.hash,
                size: file.size,
                mode: file.mode,
            }),
            DiskEntry::Special(detail) => findings.push(finding(
                root_key,
                path,
                FileClassification::SpecialFile,
                None,
                None,
                Some(detail),
            )),
            DiskEntry::Unreadable(detail) => findings.push(finding(
                root_key,
                path,
                FileClassification::Unreadable,
                None,
                None,
                Some(detail),
            )),
        }
    }
    sort_findings(&mut findings);
    let counts = FindingCounts::of(&findings);
    let finished_at = chrono::Utc::now();
    let run = BaselineScanRun {
        id: run_id,
        local_game_id: game,
        baseline_id: None,
        purpose: ScanPurpose::Capture,
        state: output.state,
        evidence: ScanEvidence::ContentHashed,
        started_at,
        finished_at: Some(finished_at),
        files_scanned: output.files_scanned,
        bytes_hashed: output.bytes_hashed,
        counts,
        error: None,
    };
    progress.emit(ProgressEvent::Finished {
        stage: Stage::Hashing,
        success: output.state == ScanState::Completed,
    });

    Ok(BaselineCapture {
        run,
        scope_fingerprint: ScanScopeFingerprint::of(roots, exclusions),
        files,
        findings,
    })
}

/// Compare the current adapter-declared scope with immutable baseline files.
///
/// `managed_targets` is normally the result of
/// [`onera_core::ports::DeploymentStore::all_targets`]. Keeping it as data
/// rather than a store dependency makes classification independent of SQLite.
/// Every included regular file is content-hashed; this API intentionally has no
/// metadata-only clean path.
///
/// Cancellation returns a partial result and does not infer `missing` findings
/// for paths the interrupted scan did not reach.
///
/// # Errors
///
/// Returns an error for an invalid scan scope, duplicate baseline records, or a
/// filesystem path that cannot be represented by [`RelPath`].
pub async fn verify_baseline(
    request: BaselineVerificationRequest<'_>,
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
) -> Result<BaselineVerificationScan> {
    let BaselineVerificationRequest {
        game,
        baseline,
        baseline_files,
        roots,
        exclusions,
        managed_targets,
    } = request;
    if baseline.local_game_id != game {
        return Err(CoreError::InvalidInput(format!(
            "baseline {} belongs to a different game installation",
            baseline.id
        )));
    }
    validate_roots(roots).await?;
    let expected = index_baseline_files(baseline_files)?;
    let started_at = chrono::Utc::now();
    let run_id = BaselineScanRunId::new();
    progress.emit(ProgressEvent::Started {
        operation: None,
        stage: Stage::Verifying,
        total: None,
    });

    let output = scan_scope(roots, exclusions, Stage::Verifying, progress, cancel).await?;
    let mut findings = Vec::new();
    let mut seen = BTreeSet::new();
    for ((root_key, path), entry) in &output.entries {
        let key = (root_key.clone(), path.clone());
        seen.insert(key.clone());
        let baseline_file = expected.get(&key).copied();
        match entry {
            DiskEntry::File(actual) => {
                let (classification, expected_hash, detail) = match baseline_file {
                    Some(recorded)
                        if recorded.hash == actual.hash
                            && recorded
                                .mode
                                .is_none_or(|expected_mode| Some(expected_mode) == actual.mode) =>
                    {
                        (
                            FileClassification::Matching,
                            Some(recorded.hash.clone()),
                            None,
                        )
                    }
                    Some(recorded) => {
                        let detail = mode_change_detail(recorded, actual);
                        (
                            FileClassification::Modified,
                            Some(recorded.hash.clone()),
                            detail,
                        )
                    }
                    None if managed_targets.contains(&TargetLocation {
                        root_key: root_key.clone(),
                        path: path.clone(),
                    }) =>
                    {
                        (FileClassification::ExtraManaged, None, None)
                    }
                    None => (FileClassification::ExtraUnknown, None, None),
                };
                findings.push(finding(
                    root_key.clone(),
                    path.clone(),
                    classification,
                    expected_hash,
                    Some(actual.hash.clone()),
                    detail,
                ));
            }
            DiskEntry::Special(detail) => findings.push(finding(
                root_key.clone(),
                path.clone(),
                FileClassification::SpecialFile,
                baseline_file.map(|f| f.hash.clone()),
                None,
                Some(detail.clone()),
            )),
            DiskEntry::Unreadable(detail) => findings.push(finding(
                root_key.clone(),
                path.clone(),
                FileClassification::Unreadable,
                baseline_file.map(|f| f.hash.clone()),
                None,
                Some(detail.clone()),
            )),
        }
    }

    // Only a complete walk proves absence. On cancellation, unseen paths are
    // unknown rather than missing and therefore are deliberately omitted from
    // this partial result.
    if output.state == ScanState::Completed {
        let current_root_keys: BTreeSet<&str> =
            roots.iter().map(|root| root.key.as_str()).collect();
        for ((root_key, path), recorded) in expected {
            if seen.contains(&(root_key.clone(), path.clone())) {
                continue;
            }
            // A changed adapter scope is already made non-clean by its
            // fingerprint. Do not mislabel records outside the new scope as
            // files that are known to be absent from disk.
            if !current_root_keys.contains(root_key.as_str())
                || excluded_entry(exclusions, &root_key, &path, false)
            {
                continue;
            }
            if let Some(detail) = unreadable_ancestor(&output.unreadable_prefixes, &root_key, &path)
            {
                findings.push(finding(
                    root_key,
                    path,
                    FileClassification::Unreadable,
                    Some(recorded.hash.clone()),
                    None,
                    Some(detail.to_owned()),
                ));
            } else {
                findings.push(finding(
                    root_key,
                    path,
                    FileClassification::Missing,
                    Some(recorded.hash.clone()),
                    None,
                    None,
                ));
            }
        }
    }

    sort_findings(&mut findings);
    let counts = FindingCounts::of(&findings);
    let finished_at = chrono::Utc::now();
    let scope_fingerprint = ScanScopeFingerprint::of(roots, exclusions);
    let verification = BaselineVerification {
        baseline_id: baseline.id,
        scan_run_id: run_id,
        state: output.state,
        evidence: ScanEvidence::ContentHashed,
        scope_fingerprint,
        findings,
        counts,
        verified_at: finished_at,
    };
    let run = BaselineScanRun {
        id: run_id,
        local_game_id: game,
        baseline_id: Some(baseline.id),
        purpose: ScanPurpose::Verify,
        state: output.state,
        evidence: ScanEvidence::ContentHashed,
        started_at,
        finished_at: Some(finished_at),
        files_scanned: output.files_scanned,
        bytes_hashed: output.bytes_hashed,
        counts,
        error: None,
    };
    progress.emit(ProgressEvent::Finished {
        stage: Stage::Verifying,
        success: output.state == ScanState::Completed,
    });
    Ok(BaselineVerificationScan { run, verification })
}

async fn validate_roots(roots: &[BaselineRoot]) -> Result<()> {
    if roots.is_empty() {
        return Err(CoreError::InvalidInput(
            "the game adapter declared no store-managed baseline roots".to_owned(),
        ));
    }
    let mut keys = BTreeSet::new();
    for root in roots {
        if !keys.insert(root.key.as_str()) {
            return Err(CoreError::InvalidInput(format!(
                "duplicate baseline root key {:?}",
                root.key
            )));
        }
        if !matches!(
            root.kind,
            DeployRootKind::GameInstall | DeployRootKind::Auxiliary
        ) {
            return Err(CoreError::InvalidInput(format!(
                "baseline root {:?} is not store-managed ({:?})",
                root.key, root.kind
            )));
        }
        if !root.path.is_absolute() {
            return Err(CoreError::InvalidInput(format!(
                "baseline root {:?} is not absolute",
                root.key
            )));
        }
        let metadata = tokio::fs::symlink_metadata(&root.path)
            .await
            .map_err(|error| CoreError::fs(&root.path, error))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(CoreError::InvalidInput(format!(
                "baseline root {:?} must be a real directory, not a link or special file",
                root.key
            )));
        }
    }
    Ok(())
}

async fn scan_scope(
    roots: &[BaselineRoot],
    exclusions: &[BaselineExclusion],
    stage: Stage,
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
) -> Result<ScanOutput> {
    let mut entries = EntryMap::new();
    let mut unreadable_prefixes = Vec::new();
    let mut files_scanned = 0_u64;
    let mut bytes_hashed = 0_u64;
    let mut ordered_roots: Vec<&BaselineRoot> = roots.iter().collect();
    ordered_roots.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.path.cmp(&b.path)));

    for root in ordered_roots {
        let walker = WalkDir::new(&root.path)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|entry| should_descend(entry, root, exclusions));
        for walked in walker {
            if cancel.is_cancelled() {
                return Ok(ScanOutput {
                    state: ScanState::Cancelled,
                    entries,
                    unreadable_prefixes,
                    files_scanned,
                    bytes_hashed,
                });
            }
            let entry = match walked {
                Ok(entry) if entry.depth() == 0 => continue,
                Ok(entry) => entry,
                Err(error) => {
                    let Some(path) = error.path() else {
                        return Err(CoreError::InvalidInput(format!(
                            "baseline directory walk failed: {error}"
                        )));
                    };
                    let relative = relative_path(&root.path, path)?;
                    if excluded_entry(exclusions, &root.key, &relative, true) {
                        continue;
                    }
                    let detail = error.to_string();
                    entries.insert(
                        (root.key.clone(), relative.clone()),
                        DiskEntry::Unreadable(detail.clone()),
                    );
                    unreadable_prefixes.push((root.key.clone(), relative.clone(), detail.clone()));
                    files_scanned += 1;
                    emit_advance(progress, stage, files_scanned, &root.key, &relative);
                    progress.emit(ProgressEvent::Warning { message: detail });
                    continue;
                }
            };
            let relative = relative_path(&root.path, entry.path())?;
            if excluded_entry(exclusions, &root.key, &relative, entry.file_type().is_dir())
                || entry.file_type().is_dir()
            {
                continue;
            }
            files_scanned += 1;
            let disk_entry = if entry.file_type().is_symlink() {
                DiskEntry::Special("symbolic link rejected from the trusted baseline".to_owned())
            } else if !entry.file_type().is_file() {
                DiskEntry::Special("non-regular file rejected from the trusted baseline".to_owned())
            } else {
                match hash_regular_file(entry.path(), cancel, &mut bytes_hashed).await {
                    Ok(Some(file)) => DiskEntry::File(file),
                    Ok(None) => {
                        return Ok(ScanOutput {
                            state: ScanState::Cancelled,
                            entries,
                            unreadable_prefixes,
                            files_scanned,
                            bytes_hashed,
                        });
                    }
                    Err(error) => DiskEntry::Unreadable(error.to_string()),
                }
            };
            if let DiskEntry::Unreadable(detail) = &disk_entry {
                unreadable_prefixes.push((root.key.clone(), relative.clone(), detail.clone()));
                progress.emit(ProgressEvent::Warning {
                    message: detail.clone(),
                });
            }
            entries.insert((root.key.clone(), relative.clone()), disk_entry);
            emit_advance(progress, stage, files_scanned, &root.key, &relative);
        }
    }

    Ok(ScanOutput {
        state: ScanState::Completed,
        entries,
        unreadable_prefixes,
        files_scanned,
        bytes_hashed,
    })
}

fn should_descend(entry: &DirEntry, root: &BaselineRoot, exclusions: &[BaselineExclusion]) -> bool {
    if entry.depth() == 0 {
        return true;
    }
    relative_path(&root.path, entry.path())
        .ok()
        .is_none_or(|path| !excluded_entry(exclusions, &root.key, &path, true))
}

fn excluded_entry(
    exclusions: &[BaselineExclusion],
    root_key: &str,
    path: &RelPath,
    is_directory: bool,
) -> bool {
    excluded_by(exclusions, root_key, path).is_some()
        || (is_directory
            && exclusions.iter().any(|exclusion| {
                exclusion
                    .root_key
                    .as_deref()
                    .is_none_or(|key| key == root_key)
                    && matches!(
                        &exclusion.pattern,
                        onera_core::domain::baseline::ExclusionPattern::DirectoryName { name }
                            if path.file_name().eq_ignore_ascii_case(name)
                    )
            }))
}

async fn hash_regular_file(
    path: &Path,
    cancel: &CancelToken,
    bytes_hashed: &mut u64,
) -> std::io::Result<Option<DiskFile>> {
    let before = tokio::fs::symlink_metadata(path).await?;
    if before.file_type().is_symlink() || !before.is_file() {
        return Err(std::io::Error::other(
            "entry changed type while it was being scanned",
        ));
    }
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 256 * 1024];
    let mut bytes_read = 0_u64;
    loop {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        *bytes_hashed += read as u64;
        bytes_read += read as u64;
        hasher.update(&buffer[..read]);
    }
    let after = tokio::fs::symlink_metadata(path).await?;
    if after.file_type().is_symlink() || !after.is_file() {
        return Err(std::io::Error::other(
            "entry changed type while it was being scanned",
        ));
    }
    if before.len() != bytes_read
        || after.len() != bytes_read
        || before.modified()? != after.modified()?
    {
        return Err(std::io::Error::other(
            "entry changed while it was being scanned",
        ));
    }
    Ok(Some(DiskFile {
        hash: FileHash::blake3(*hasher.finalize().as_bytes()),
        size: after.len(),
        mode: file_mode(&after),
    }))
}

#[cfg(unix)]
fn file_mode(metadata: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt as _;
    Some(metadata.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn file_mode(_metadata: &std::fs::Metadata) -> Option<u32> {
    None
}

fn relative_path(root: &Path, path: &Path) -> Result<RelPath> {
    let relative = path.strip_prefix(root).map_err(|_| {
        CoreError::InvalidInput(format!(
            "{} is outside baseline root {}",
            path.display(),
            root.display()
        ))
    })?;
    let mut names = Vec::new();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(CoreError::InvalidInput(format!(
                "{} is not a normal path under baseline root {}",
                path.display(),
                root.display()
            )));
        };
        let name = name.to_str().ok_or_else(|| {
            CoreError::InvalidInput(format!(
                "{} contains a non-UTF-8 name that the baseline contract cannot represent",
                path.display()
            ))
        })?;
        // RelPath treats backslashes as separators for archive safety. A real
        // Unix filename containing one must not be normalized into a different
        // on-disk location and accidentally trusted.
        if name.contains(['/', '\\']) {
            return Err(CoreError::InvalidInput(format!(
                "{} contains a separator-ambiguous filename",
                path.display()
            )));
        }
        names.push(name);
    }
    RelPath::normalize(&names.join("/")).map_err(CoreError::from)
}

fn index_baseline_files(
    files: &[BaselineFile],
) -> Result<BTreeMap<(String, RelPath), &BaselineFile>> {
    let mut indexed = BTreeMap::new();
    for file in files {
        let key = (file.root_key.clone(), file.path.clone());
        if indexed.insert(key, file).is_some() {
            return Err(CoreError::InvalidInput(format!(
                "duplicate baseline file {}:{}",
                file.root_key, file.path
            )));
        }
    }
    Ok(indexed)
}

fn unreadable_ancestor<'a>(
    prefixes: &'a [(String, RelPath, String)],
    root_key: &str,
    path: &RelPath,
) -> Option<&'a str> {
    prefixes.iter().find_map(|(root, prefix, detail)| {
        let path_text = path.as_str();
        let prefix_text = prefix.as_str();
        (root == root_key
            && (path_text == prefix_text
                || (path_text.starts_with(prefix_text)
                    && path_text.as_bytes().get(prefix_text.len()) == Some(&b'/'))))
        .then_some(detail.as_str())
    })
}

fn mode_change_detail(expected: &BaselineFile, actual: &DiskFile) -> Option<String> {
    (expected.hash == actual.hash && expected.mode.is_some() && expected.mode != actual.mode).then(
        || {
            format!(
                "file mode changed from {} to {}",
                display_mode(expected.mode),
                display_mode(actual.mode)
            )
        },
    )
}

fn display_mode(mode: Option<u32>) -> String {
    mode.map_or_else(|| "unknown".to_owned(), |mode| format!("{mode:#06o}"))
}

fn finding(
    root_key: String,
    path: RelPath,
    classification: FileClassification,
    expected: Option<FileHash>,
    observed: Option<FileHash>,
    detail: Option<String>,
) -> BaselineFinding {
    BaselineFinding {
        root_key,
        path,
        classification,
        expected,
        observed,
        detail,
    }
}

fn sort_findings(findings: &mut [BaselineFinding]) {
    findings.sort_by(|a, b| {
        a.root_key
            .cmp(&b.root_key)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.classification.cmp(&b.classification))
    });
}

fn emit_advance(
    progress: &dyn ProgressSink,
    stage: Stage,
    completed: u64,
    root_key: &str,
    path: &RelPath,
) {
    progress.emit(ProgressEvent::Advanced {
        stage,
        completed,
        total: None,
        detail: Some(format!("{root_key}:{path}")),
    });
}
