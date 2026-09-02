use chrono::Utc;
use onera_core::domain::baseline::{
    BaselineExclusion, BaselineFile, BaselineRoot, BaselineSource, BaselineStatus,
    ExclusionPattern, ExclusionReason, FileClassification, GameBaseline, ScanEvidence,
    ScanScopeFingerprint, ScanState,
};
use onera_core::ids::{BaselineId, LocalGameId};
use onera_core::paths::{DeployRootKind, RelPath};
use onera_core::plan::TargetLocation;
use onera_core::progress::{CancelToken, ProgressEvent, ProgressSink, RecordingProgress};
use onera_core::FileHash;
use onera_install::{capture_baseline, verify_baseline, BaselineVerificationRequest};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Mutex;

fn rel(path: &str) -> RelPath {
    RelPath::normalize(path).unwrap()
}

fn root(key: &str, path: &Path) -> BaselineRoot {
    BaselineRoot {
        key: key.to_owned(),
        kind: DeployRootKind::GameInstall,
        path: path.to_path_buf(),
    }
}

fn exclusion(path: &str, reason: ExclusionReason) -> BaselineExclusion {
    BaselineExclusion {
        root_key: Some("game".to_owned()),
        pattern: ExclusionPattern::Prefix { path: rel(path) },
        reason,
        note: None,
    }
}

fn baseline(
    game: LocalGameId,
    roots: &[BaselineRoot],
    exclusions: &[BaselineExclusion],
    files: &[BaselineFile],
) -> GameBaseline {
    GameBaseline {
        id: BaselineId::new(),
        local_game_id: game,
        source: BaselineSource::LocalSnapshot,
        build_identity: None,
        adapter_id: "test".to_owned(),
        reported_version: None,
        status: BaselineStatus::Current,
        captured_at: Utc::now(),
        scope_fingerprint: ScanScopeFingerprint::of(roots, exclusions),
        file_count: files.len() as u64,
        total_bytes: files.iter().map(|file| file.size).sum(),
    }
}

fn baseline_file(path: &str, contents: &[u8]) -> BaselineFile {
    BaselineFile {
        root_key: "game".to_owned(),
        path: rel(path),
        hash: FileHash::blake3_of(contents),
        size: contents.len() as u64,
        mode: None,
    }
}

#[tokio::test]
async fn capture_hashes_only_declared_store_roots_and_applies_every_exclusion() {
    let temp = tempfile::tempdir().unwrap();
    let game_dir = temp.path().join("game");
    let outside = temp.path().join("outside");
    tokio::fs::create_dir_all(&game_dir).await.unwrap();
    tokio::fs::create_dir_all(&outside).await.unwrap();
    tokio::fs::write(game_dir.join("content.bin"), b"content")
        .await
        .unwrap();
    tokio::fs::write(outside.join("not-declared.bin"), b"outside")
        .await
        .unwrap();

    let excluded = [
        ("saves", ExclusionReason::UserData),
        ("logs", ExclusionReason::Logs),
        ("cache", ExclusionReason::Cache),
        ("generated-config", ExclusionReason::GeneratedConfig),
        ("shader-cache", ExclusionReason::ShaderCache),
    ];
    for (directory, _) in excluded {
        tokio::fs::create_dir_all(game_dir.join(directory))
            .await
            .unwrap();
        tokio::fs::write(game_dir.join(directory).join("ignored.bin"), b"changing")
            .await
            .unwrap();
    }
    let exclusions: Vec<_> = excluded
        .iter()
        .map(|(path, reason)| exclusion(path, *reason))
        .collect();
    let mut exclusions = exclusions;
    exclusions.pop();
    exclusions.push(BaselineExclusion {
        root_key: Some("game".to_owned()),
        pattern: ExclusionPattern::DirectoryName {
            name: "shader-cache".to_owned(),
        },
        reason: ExclusionReason::ShaderCache,
        note: None,
    });
    let roots = vec![root("game", &game_dir)];
    let progress = RecordingProgress::default();

    let capture = capture_baseline(
        LocalGameId::new(),
        &roots,
        &exclusions,
        &progress,
        &CancelToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(capture.run.state, ScanState::Completed);
    assert_eq!(capture.run.evidence, ScanEvidence::ContentHashed);
    assert_eq!(capture.run.files_scanned, 1);
    assert_eq!(capture.run.bytes_hashed, 7);
    assert!(capture.findings.is_empty());
    assert_eq!(capture.files.len(), 1);
    assert_eq!(capture.files[0].path, rel("content.bin"));
    assert_eq!(capture.files[0].hash, FileHash::blake3_of(b"content"));
    assert_eq!(capture.files[0].size, 7);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            capture.files[0].mode,
            Some(
                std::fs::metadata(game_dir.join("content.bin"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o7777
            )
        );
    }
    #[cfg(not(unix))]
    assert_eq!(capture.files[0].mode, None);
    assert!(progress
        .events()
        .iter()
        .any(|event| matches!(event, ProgressEvent::Advanced { .. })));
}

#[tokio::test]
async fn user_data_and_proton_roots_are_rejected_even_if_misdeclared_by_an_adapter() {
    let temp = tempfile::tempdir().unwrap();
    for kind in [DeployRootKind::UserData, DeployRootKind::CompatPrefix] {
        let roots = [BaselineRoot {
            key: "unsafe".to_owned(),
            kind,
            path: temp.path().to_path_buf(),
        }];
        let error = capture_baseline(
            LocalGameId::new(),
            &roots,
            &[],
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("not store-managed"), "{error}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn capture_rejects_symlinks_and_other_special_files() {
    let temp = tempfile::tempdir().unwrap();
    tokio::fs::write(temp.path().join("target"), b"target")
        .await
        .unwrap();
    std::os::unix::fs::symlink("target", temp.path().join("link")).unwrap();
    let status = std::process::Command::new("mkfifo")
        .arg(temp.path().join("fifo"))
        .status()
        .unwrap();
    assert!(status.success());

    let capture = capture_baseline(
        LocalGameId::new(),
        &[root("game", temp.path())],
        &[],
        &RecordingProgress::default(),
        &CancelToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(capture.files.len(), 1);
    assert_eq!(capture.run.counts.special, 2);
    assert_eq!(
        capture
            .findings
            .iter()
            .map(|finding| (&finding.path, finding.classification))
            .collect::<Vec<_>>(),
        vec![
            (&rel("fifo"), FileClassification::SpecialFile),
            (&rel("link"), FileClassification::SpecialFile),
        ]
    );
}

#[tokio::test]
async fn verification_classifies_matching_modified_missing_and_both_extra_kinds() {
    let temp = tempfile::tempdir().unwrap();
    tokio::fs::write(temp.path().join("matching"), b"same")
        .await
        .unwrap();
    // Same size as the expected bytes: size-based acceleration must not be able
    // to turn this into a false clean result.
    tokio::fs::write(temp.path().join("modified"), b"WXYZ")
        .await
        .unwrap();
    tokio::fs::write(temp.path().join("managed-extra"), b"managed")
        .await
        .unwrap();
    tokio::fs::write(temp.path().join("unknown-extra"), b"unknown")
        .await
        .unwrap();

    let roots = vec![root("game", temp.path())];
    let files = vec![
        baseline_file("matching", b"same"),
        baseline_file("modified", b"ABCD"),
        baseline_file("missing", b"gone"),
    ];
    let game = LocalGameId::new();
    let expected = baseline(game, &roots, &[], &files);
    let managed = BTreeSet::from([TargetLocation {
        root_key: "game".to_owned(),
        path: rel("managed-extra"),
    }]);

    let scan = verify_baseline(
        BaselineVerificationRequest {
            game,
            baseline: &expected,
            baseline_files: &files,
            roots: &roots,
            exclusions: &[],
            managed_targets: &managed,
        },
        &RecordingProgress::default(),
        &CancelToken::new(),
    )
    .await
    .unwrap();

    let counts = scan.verification.counts;
    assert_eq!(counts.matching, 1);
    assert_eq!(counts.modified, 1);
    assert_eq!(counts.missing, 1);
    assert_eq!(counts.extra_managed, 1);
    assert_eq!(counts.extra_unknown, 1);
    assert_eq!(counts.unreadable, 0);
    assert!(!scan.verification.is_clean(&expected));
    let modified = scan
        .verification
        .findings
        .iter()
        .find(|finding| finding.path == rel("modified"))
        .unwrap();
    assert_eq!(modified.expected, Some(FileHash::blake3_of(b"ABCD")));
    assert_eq!(modified.observed, Some(FileHash::blake3_of(b"WXYZ")));
}

#[tokio::test]
async fn a_complete_content_hashed_match_is_clean() {
    let temp = tempfile::tempdir().unwrap();
    tokio::fs::write(temp.path().join("file"), b"trusted")
        .await
        .unwrap();
    let roots = vec![root("game", temp.path())];
    let game = LocalGameId::new();
    let captured = capture_baseline(
        game,
        &roots,
        &[],
        &RecordingProgress::default(),
        &CancelToken::new(),
    )
    .await
    .unwrap();
    let expected = baseline(game, &roots, &[], &captured.files);

    let managed = BTreeSet::new();
    let scan = verify_baseline(
        BaselineVerificationRequest {
            game,
            baseline: &expected,
            baseline_files: &captured.files,
            roots: &roots,
            exclusions: &[],
            managed_targets: &managed,
        },
        &RecordingProgress::default(),
        &CancelToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(scan.verification.evidence, ScanEvidence::ContentHashed);
    assert_eq!(scan.verification.counts.matching, 1);
    assert!(scan.verification.is_clean(&expected));
}

#[cfg(unix)]
#[tokio::test]
async fn verification_reports_an_unreadable_file_instead_of_missing() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("unreadable");
    tokio::fs::write(&path, b"secret").await.unwrap();
    let roots = vec![root("game", temp.path())];
    let game = LocalGameId::new();
    let files = vec![baseline_file("unreadable", b"secret")];
    let expected = baseline(game, &roots, &[], &files);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();

    let managed = BTreeSet::new();
    let result = verify_baseline(
        BaselineVerificationRequest {
            game,
            baseline: &expected,
            baseline_files: &files,
            roots: &roots,
            exclusions: &[],
            managed_targets: &managed,
        },
        &RecordingProgress::default(),
        &CancelToken::new(),
    )
    .await;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let scan = result.unwrap();

    assert_eq!(scan.verification.counts.unreadable, 1);
    assert_eq!(
        scan.verification.findings[0].classification,
        FileClassification::Unreadable
    );
    assert_eq!(scan.verification.counts.missing, 0);
}

#[cfg(unix)]
#[tokio::test]
async fn verification_reports_a_symlink_over_a_baseline_file_as_special() {
    let temp = tempfile::tempdir().unwrap();
    tokio::fs::write(temp.path().join("target"), b"expected")
        .await
        .unwrap();
    std::os::unix::fs::symlink("target", temp.path().join("file")).unwrap();
    let roots = vec![root("game", temp.path())];
    let game = LocalGameId::new();
    let files = vec![baseline_file("file", b"expected")];
    let expected = baseline(game, &roots, &[], &files);

    let managed = BTreeSet::new();
    let scan = verify_baseline(
        BaselineVerificationRequest {
            game,
            baseline: &expected,
            baseline_files: &files,
            roots: &roots,
            exclusions: &[],
            managed_targets: &managed,
        },
        &RecordingProgress::default(),
        &CancelToken::new(),
    )
    .await
    .unwrap();

    let finding = scan
        .verification
        .findings
        .iter()
        .find(|finding| finding.path == rel("file"))
        .unwrap();
    assert_eq!(finding.classification, FileClassification::SpecialFile);
    assert_eq!(finding.expected, Some(FileHash::blake3_of(b"expected")));
    assert!(finding.observed.is_none());
}

struct CancellingProgress {
    token: CancelToken,
    events: Mutex<Vec<ProgressEvent>>,
}

impl ProgressSink for CancellingProgress {
    fn emit(&self, event: ProgressEvent) {
        if matches!(event, ProgressEvent::Advanced { completed: 1, .. }) {
            self.token.cancel();
        }
        self.events.lock().unwrap().push(event);
    }
}

#[tokio::test]
async fn cancellation_returns_a_partial_scan_and_progress_terminal_event() {
    let temp = tempfile::tempdir().unwrap();
    tokio::fs::write(temp.path().join("a"), b"first")
        .await
        .unwrap();
    tokio::fs::write(temp.path().join("b"), b"second")
        .await
        .unwrap();
    let token = CancelToken::new();
    let progress = CancellingProgress {
        token: token.clone(),
        events: Mutex::new(Vec::new()),
    };

    let capture = capture_baseline(
        LocalGameId::new(),
        &[root("game", temp.path())],
        &[],
        &progress,
        &token,
    )
    .await
    .unwrap();

    assert_eq!(capture.run.state, ScanState::Cancelled);
    assert_eq!(capture.files.len(), 1);
    assert!(matches!(
        progress.events.lock().unwrap().last(),
        Some(ProgressEvent::Finished { success: false, .. })
    ));
}

#[tokio::test]
async fn scan_results_and_scope_fingerprints_are_deterministic() {
    let temp = tempfile::tempdir().unwrap();
    let game_dir = temp.path().join("game");
    let auxiliary_dir = temp.path().join("auxiliary");
    tokio::fs::create_dir_all(&game_dir).await.unwrap();
    tokio::fs::create_dir_all(&auxiliary_dir).await.unwrap();
    tokio::fs::write(game_dir.join("z"), b"z").await.unwrap();
    tokio::fs::write(game_dir.join("a"), b"a").await.unwrap();
    tokio::fs::write(auxiliary_dir.join("m"), b"m")
        .await
        .unwrap();
    let roots = vec![
        root("game", &game_dir),
        BaselineRoot {
            key: "auxiliary".to_owned(),
            kind: DeployRootKind::Auxiliary,
            path: auxiliary_dir,
        },
    ];
    let exclusions = vec![
        exclusion("cache", ExclusionReason::Cache),
        exclusion("logs", ExclusionReason::Logs),
    ];
    let mut reversed_roots = roots.clone();
    reversed_roots.reverse();
    let mut reversed_exclusions = exclusions.clone();
    reversed_exclusions.reverse();

    let first = capture_baseline(
        LocalGameId::new(),
        &roots,
        &exclusions,
        &RecordingProgress::default(),
        &CancelToken::new(),
    )
    .await
    .unwrap();
    let second = capture_baseline(
        LocalGameId::new(),
        &reversed_roots,
        &reversed_exclusions,
        &RecordingProgress::default(),
        &CancelToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(first.scope_fingerprint, second.scope_fingerprint);
    assert_eq!(first.files, second.files);
    assert_eq!(
        first
            .files
            .iter()
            .map(|file| (file.root_key.as_str(), file.path.as_str()))
            .collect::<Vec<_>>(),
        vec![("auxiliary", "m"), ("game", "a"), ("game", "z")]
    );
}

#[tokio::test]
async fn mode_changes_are_modified_even_when_content_still_matches() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("executable");
        tokio::fs::write(&path, b"same bytes").await.unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let roots = vec![root("game", temp.path())];
        let game = LocalGameId::new();
        let mut expected_file = baseline_file("executable", b"same bytes");
        expected_file.mode = Some(0o644);
        let files = vec![expected_file];
        let expected = baseline(game, &roots, &[], &files);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

        let managed = BTreeSet::new();
        let scan = verify_baseline(
            BaselineVerificationRequest {
                game,
                baseline: &expected,
                baseline_files: &files,
                roots: &roots,
                exclusions: &[],
                managed_targets: &managed,
            },
            &RecordingProgress::default(),
            &CancelToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(scan.verification.counts.modified, 1);
        let finding = &scan.verification.findings[0];
        assert_eq!(finding.expected, finding.observed);
        assert!(finding.detail.as_deref().unwrap().contains("mode changed"));
    }
}
