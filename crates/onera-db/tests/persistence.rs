//! Integration tests for the persistence ports.

use onera_core::domain::download::{DownloadJob, JobState};
use onera_core::domain::operation::{OperationKind, OperationState};
use onera_core::domain::provider_stack::{FileProvider, ProviderStack, StackEntry};
use onera_core::domain::reconcile::InstallationMapping;
use onera_core::domain::release::{FileCategory, Mod, ProviderFile, Release};
use onera_core::hash::FileHash;
use onera_core::ids::*;
use onera_core::plan::{
    ConflictChoice, FileClassification, InstallPlan, PlannedFile, ScopedRule, TargetLocation,
};
use onera_core::ports::{
    BackupStore, DeploymentStore, JournalEntry, JournalStatus, OperationJournal,
};
use onera_core::RelPath;
use onera_db::backup::FileBackupStore;
use onera_db::jobs::{InboxRequest, InboxRequestKind, InboxState};
use onera_db::Database;

/// Insert the provider/game/mod/release/archive rows every deployment test
/// needs, and return the ids.
struct Fixture {
    db: Database,
    game: LocalGameId,
    mod_id: ModId,
    release: ReleaseId,
    provider_file: ProviderFileId,
    archive: ArchiveId,
}

async fn fixture() -> Fixture {
    let db = Database::open_in_memory().await.unwrap();
    let provider = ProviderId::nexus();
    db.upsert_provider(&provider, "Nexus Mods", "https://api.nexusmods.com/v3")
        .await
        .unwrap();

    let game = onera_core::domain::game::Game {
        id: GameId::new(),
        provider: provider.clone(),
        provider_slug: "cyberpunk2077".into(),
        name: "Cyberpunk 2077".into(),
        steam_app_id: Some(1_091_500),
    };
    let game_id = db.upsert_game(&game).await.unwrap();

    let install = onera_core::domain::game::LocalGameInstall {
        id: LocalGameId::new(),
        game_id,
        adapter_id: "cyberpunk2077".into(),
        source: onera_core::domain::game::InstallSource::SteamNative,
        install_root: "/games/Cyberpunk 2077".into(),
        compat_prefix: None,
        user_data_roots: vec![],
        confirmed: true,
    };
    let local = db.upsert_local_install(&install).await.unwrap();

    let m = Mod {
        id: ModId::new(),
        provider: provider.clone(),
        provider_mod_id: ProviderModId::new("107"),
        game_slug: "cyberpunk2077".into(),
        name: "Cyber Engine Tweaks".into(),
        author: Some("yamashi".into()),
    };
    let mod_id = db.upsert_mod(&m).await.unwrap();

    let r = Release {
        id: ReleaseId::new(),
        mod_id,
        version: "1.2.3".into(),
        published_at: chrono::DateTime::from_timestamp(1_700_000_000, 0),
        metadata: serde_json::json!({ "provider": "nexus" }),
    };
    let release = db.upsert_release(&r).await.unwrap();

    let provider_file = ProviderFileId::new("9001");
    db.upsert_provider_file(&ProviderFile {
        provider: provider.clone(),
        provider_file_id: provider_file.clone(),
        provider_version_id: None,
        provider_file_group_id: None,
        position: None,
        release_id: release,
        name: "cet-1.2.3.zip".into(),
        size_bytes: Some(13),
        category: FileCategory::Main,
        published_hash: None,
        uploaded_at: r.published_at,
        is_primary: true,
    })
    .await
    .unwrap();

    let archive = db
        .upsert_archive(
            &FileHash::blake3_of(b"archive bytes"),
            13,
            "cet-1.2.3.zip",
            onera_core::domain::archive::ArchiveFormat::Zip,
            std::path::Path::new("/data/archives/blake3/ab/abc"),
        )
        .await
        .unwrap();

    Fixture {
        db,
        game: local,
        mod_id,
        release,
        provider_file,
        archive,
    }
}

#[tokio::test]
async fn download_jobs_survive_restart_state_changes() {
    let f = fixture().await;
    let mut job = DownloadJob::queued(
        ProviderId::nexus(),
        "cyberpunk2077".into(),
        ProviderModId::new("107"),
        f.provider_file.clone(),
        "cet-1.2.3.zip".into(),
        Some(13),
        "/data/downloads/job.part".into(),
    );
    job.bytes_downloaded = 7;
    job.state = JobState::Running;
    f.db.put_download_job(&job).await.unwrap();

    let resumable = f.db.resumable_download_jobs().await.unwrap();
    assert_eq!(resumable, vec![job.clone()]);

    job.state = JobState::Complete;
    job.bytes_downloaded = 13;
    job.archive_id = Some(f.archive);
    f.db.put_download_job(&job).await.unwrap();

    assert!(f.db.resumable_download_jobs().await.unwrap().is_empty());
    assert_eq!(
        f.db.completed_download(&ProviderId::nexus(), &f.provider_file)
            .await
            .unwrap(),
        Some(job)
    );
}

#[tokio::test]
async fn browser_inbox_exposes_only_actionable_requests() {
    let f = fixture().await;
    let mut request = InboxRequest::queued(
        InboxRequestKind::DownloadAndInstall,
        "cyberpunk2077".into(),
        ProviderModId::new("107"),
        Some(f.provider_file.clone()),
    );
    request.state = InboxState::WaitingForUser;
    f.db.put_inbox_request(&request).await.unwrap();

    let queued = f.db.inbox_requests().await.unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].id, request.id);
    assert_eq!(queued[0].kind, InboxRequestKind::DownloadAndInstall);
    assert_eq!(queued[0].state, InboxState::WaitingForUser);

    f.db.set_inbox_state(request.id, InboxState::Complete, None)
        .await
        .unwrap();
    assert!(f.db.inbox_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn installed_mods_and_provider_archives_are_queryable() {
    let f = fixture().await;
    let installation = InstallationId::new();
    f.db.record_installation(installation, f.game, f.mod_id, f.release, f.archive)
        .await
        .unwrap();
    f.db.link_archive_provider_file(f.archive, &ProviderId::nexus(), &f.provider_file)
        .await
        .unwrap();

    let installed = f.db.installed_mods(f.game).await.unwrap();
    assert_eq!(installed.len(), 1);
    assert_eq!(installed[0].installation_id, installation);
    assert_eq!(installed[0].name, "Cyber Engine Tweaks");
    assert_eq!(installed[0].version, "1.2.3");

    let stored =
        f.db.archive_for_provider_file(&ProviderId::nexus(), &f.provider_file)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(stored.id, f.archive);
    assert_eq!(stored.size, 13);
}

fn target(path: &str) -> TargetLocation {
    TargetLocation {
        root_key: "game".into(),
        path: RelPath::normalize(path).unwrap(),
    }
}

fn entry(id: InstallationId, content: &[u8]) -> StackEntry {
    StackEntry {
        provider: FileProvider::Installation {
            installation_id: id,
        },
        hash: FileHash::blake3_of(content),
        size: content.len() as u64,
    }
}

fn plan_for(f: &Fixture, installation: InstallationId) -> InstallPlan {
    InstallPlan {
        operation_id: OperationId::new(),
        local_game_id: f.game,
        installation_id: installation,
        mod_id: f.mod_id,
        files: vec![PlannedFile {
            source: RelPath::normalize("bin/x64/plugin.dll").unwrap(),
            target: target("bin/x64/plugin.dll"),
            source_hash: FileHash::blake3_of(b"plugin"),
            source_size: 6,
            classification: FileClassification::Create,
            existing_hash: None,
            decision: None,
            notes: vec![],
        }],
    }
}

#[tokio::test]
async fn a_provider_stack_round_trips() {
    let f = fixture().await;
    let install = InstallationId::new();
    f.db.record_installation(install, f.game, f.mod_id, f.release, f.archive)
        .await
        .unwrap();

    let mut stack = ProviderStack::new();
    stack.push(entry(install, b"content"));
    let t = target("archive/pc/mod/a.archive");
    f.db.put_stack(f.game, &t, &stack).await.unwrap();

    let read = f.db.stack(f.game, &t).await.unwrap();
    assert_eq!(read, stack);
    assert_eq!(read.top().unwrap().hash, FileHash::blake3_of(b"content"));
}

#[tokio::test]
async fn stack_order_is_preserved_across_a_reload() {
    let f = fixture().await;
    let (a, b) = (InstallationId::new(), InstallationId::new());
    for id in [a, b] {
        f.db.record_installation(id, f.game, f.mod_id, f.release, f.archive)
            .await
            .unwrap();
    }

    let mut stack = ProviderStack::new();
    stack.push(entry(a, b"from a"));
    stack.push(entry(b, b"from b"));
    let t = target("shared.txt");
    f.db.put_stack(f.game, &t, &stack).await.unwrap();

    let read = f.db.stack(f.game, &t).await.unwrap();
    assert_eq!(read.entries()[0].provider.installation_id(), Some(a));
    assert_eq!(read.top().unwrap().provider.installation_id(), Some(b));
}

#[tokio::test]
async fn an_empty_stack_deletes_the_deployed_file_row() {
    let f = fixture().await;
    let install = InstallationId::new();
    f.db.record_installation(install, f.game, f.mod_id, f.release, f.archive)
        .await
        .unwrap();
    let t = target("gone.txt");

    let mut stack = ProviderStack::new();
    stack.push(entry(install, b"x"));
    f.db.put_stack(f.game, &t, &stack).await.unwrap();
    assert_eq!(f.db.all_targets(f.game).await.unwrap().len(), 1);

    stack.remove_installation(install);
    f.db.put_stack(f.game, &t, &stack).await.unwrap();
    assert!(f.db.all_targets(f.game).await.unwrap().is_empty());
    assert!(f.db.stack(f.game, &t).await.unwrap().is_empty());
}

#[tokio::test]
async fn targets_of_an_installation_are_listed() {
    let f = fixture().await;
    let install = InstallationId::new();
    f.db.record_installation(install, f.game, f.mod_id, f.release, f.archive)
        .await
        .unwrap();
    for path in ["a.txt", "b/c.txt"] {
        let mut stack = ProviderStack::new();
        stack.push(entry(install, path.as_bytes()));
        f.db.put_stack(f.game, &target(path), &stack).await.unwrap();
    }
    let targets = f.db.targets_of(install).await.unwrap();
    assert_eq!(targets.len(), 2);
    assert_eq!(targets[0].path.as_str(), "a.txt");
}

#[tokio::test]
async fn deleting_an_installation_cascades_to_its_provider_rows() {
    let f = fixture().await;
    let install = InstallationId::new();
    f.db.record_installation(install, f.game, f.mod_id, f.release, f.archive)
        .await
        .unwrap();
    let t = target("a.txt");
    let mut stack = ProviderStack::new();
    stack.push(entry(install, b"x"));
    f.db.put_stack(f.game, &t, &stack).await.unwrap();

    f.db.remove_installation(install).await.unwrap();
    // The deployed_files row survives, but nothing claims it any more.
    assert!(f.db.stack(f.game, &t).await.unwrap().is_empty());
}

#[tokio::test]
async fn installations_of_a_mod_are_found() {
    let f = fixture().await;
    let a = InstallationId::new();
    f.db.record_installation(a, f.game, f.mod_id, f.release, f.archive)
        .await
        .unwrap();
    assert_eq!(
        f.db.installations_of_mod(f.game, f.mod_id).await.unwrap(),
        vec![a]
    );
    assert!(f
        .db
        .installations_of_mod(f.game, ModId::new())
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn deactivated_artifacts_keep_their_stable_mappings() {
    let f = fixture().await;
    let installation = InstallationId::new();
    f.db.record_installation(installation, f.game, f.mod_id, f.release, f.archive)
        .await
        .unwrap();
    let mapping = InstallationMapping {
        installation_id: installation,
        source: RelPath::normalize("archive/pc/mod/example.archive").unwrap(),
        target: target("archive/pc/mod/example.archive"),
        source_hash: FileHash::blake3_of(b"artifact bytes"),
        source_size: 14,
    };
    f.db.put_mapping(&mapping).await.unwrap();
    f.db.deactivate_installation(installation).await.unwrap();

    assert!(f.db.active_installations(f.game).await.unwrap().is_empty());
    assert_eq!(
        f.db.archive_for_installation(f.game, installation)
            .await
            .unwrap()
            .unwrap()
            .id,
        f.archive
    );
    assert_eq!(
        f.db.mappings_of(installation).await.unwrap(),
        vec![mapping.clone()]
    );
    assert!(f.db.installed_mods(f.game).await.unwrap().is_empty());

    f.db.activate_installation(installation).await.unwrap();
    assert_eq!(
        f.db.active_installations(f.game).await.unwrap(),
        vec![installation]
    );
    assert_eq!(f.db.mappings_of(installation).await.unwrap(), vec![mapping]);
    assert_eq!(f.db.installed_mods(f.game).await.unwrap().len(), 1);
}

#[tokio::test]
async fn scoped_rules_round_trip_and_stay_scoped() {
    let f = fixture().await;
    let rule = ScopedRule {
        mod_id: f.mod_id,
        root_key: "game".into(),
        path_prefix: "archive/".into(),
        choice: ConflictChoice::ReplaceAfterBackup,
    };
    f.db.put_rule(&rule).await.unwrap();
    assert_eq!(f.db.rules_for(f.mod_id).await.unwrap(), vec![rule.clone()]);
    assert!(f.db.rules_for(ModId::new()).await.unwrap().is_empty());

    // Re-remembering the same scope updates rather than duplicating.
    f.db.put_rule(&ScopedRule {
        choice: ConflictChoice::KeepExisting,
        ..rule
    })
    .await
    .unwrap();
    let stored = f.db.rules_for(f.mod_id).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].choice, ConflictChoice::KeepExisting);
}

#[tokio::test]
async fn the_journal_records_a_plan_and_its_entries() {
    let f = fixture().await;
    let install = InstallationId::new();
    let plan = plan_for(&f, install);
    let op = f.db.begin(&plan, OperationKind::Install).await.unwrap();
    assert_eq!(op.state, OperationState::Planned);

    let stored = f.db.plan(op.id).await.unwrap().unwrap();
    assert_eq!(stored.files.len(), 1);
    assert_eq!(stored.files[0].target.path.as_str(), "bin/x64/plugin.dll");

    let entry = JournalEntry {
        seq: 0,
        target: target("bin/x64/plugin.dll"),
        absolute_path: "/games/Cyberpunk 2077/bin/x64/plugin.dll".into(),
        source_hash: FileHash::blake3_of(b"plugin"),
        previous_hash: None,
        backup_id: None,
        temp_path: Some("/games/Cyberpunk 2077/bin/x64/.onera-tmp".into()),
        status: JournalStatus::Pending,
    };
    f.db.put_entry(op.id, &entry).await.unwrap();
    assert_eq!(f.db.entries(op.id).await.unwrap(), vec![entry.clone()]);

    // Updating the same seq replaces the row rather than adding one.
    f.db.put_entry(
        op.id,
        &JournalEntry {
            status: JournalStatus::Committed,
            ..entry
        },
    )
    .await
    .unwrap();
    let entries = f.db.entries(op.id).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].status, JournalStatus::Committed);
}

#[tokio::test]
async fn reconciliation_operation_kinds_round_trip() {
    let f = fixture().await;
    for kind in [OperationKind::Reconcile, OperationKind::CleanRestore] {
        let plan = plan_for(&f, InstallationId::new());
        let stored = f.db.begin(&plan, kind).await.unwrap();
        assert_eq!(stored.kind, kind);
        assert_eq!(f.db.get(stored.id).await.unwrap().unwrap().kind, kind);
    }
}

#[tokio::test]
async fn illegal_state_transitions_are_refused() {
    let f = fixture().await;
    let plan = plan_for(&f, InstallationId::new());
    let op = f.db.begin(&plan, OperationKind::Install).await.unwrap();

    let err =
        f.db.set_state(op.id, OperationState::Complete, None)
            .await
            .unwrap_err();
    assert!(
        format!("{err}").contains("cannot move from planned to complete"),
        "{err}"
    );

    f.db.set_state(op.id, OperationState::Prepared, None)
        .await
        .unwrap();
    f.db.set_state(op.id, OperationState::Committing, None)
        .await
        .unwrap();
    f.db.set_state(op.id, OperationState::Complete, None)
        .await
        .unwrap();
    assert_eq!(
        f.db.get(op.id).await.unwrap().unwrap().state,
        OperationState::Complete
    );
}

#[tokio::test]
async fn interrupted_operations_are_the_non_terminal_ones() {
    let f = fixture().await;
    let done =
        f.db.begin(&plan_for(&f, InstallationId::new()), OperationKind::Install)
            .await
            .unwrap();
    f.db.set_state(done.id, OperationState::Prepared, None)
        .await
        .unwrap();
    f.db.set_state(done.id, OperationState::Committing, None)
        .await
        .unwrap();
    f.db.set_state(done.id, OperationState::Complete, None)
        .await
        .unwrap();

    let stuck =
        f.db.begin(&plan_for(&f, InstallationId::new()), OperationKind::Install)
            .await
            .unwrap();
    f.db.set_state(stuck.id, OperationState::Prepared, None)
        .await
        .unwrap();

    let interrupted = f.db.interrupted().await.unwrap();
    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0].id, stuck.id);
    assert_eq!(interrupted[0].state, OperationState::Prepared);
}

#[tokio::test]
async fn backups_are_content_addressed_and_shared() {
    let f = fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let store = FileBackupStore::new(f.db.clone(), dir.path().join("backups"));

    let original = dir.path().join("vanilla.dll");
    std::fs::write(&original, b"vanilla bytes").unwrap();
    let hash = FileHash::blake3_of(b"vanilla bytes");

    let first = store
        .create(f.game, &target("a.dll"), &original, &hash, 13)
        .await
        .unwrap();
    let second = store
        .create(f.game, &target("b.dll"), &original, &hash, 13)
        .await
        .unwrap();
    assert_ne!(first, second, "two paths get two backup records");

    let blob = store.path_of(first).await.unwrap().unwrap();
    assert_eq!(
        store.path_of(second).await.unwrap().unwrap(),
        blob,
        "identical bytes share a blob"
    );
    assert_eq!(std::fs::read(&blob).unwrap(), b"vanilla bytes");

    // Deleting one record must not remove bytes the other still needs.
    store.delete(first).await.unwrap();
    assert!(blob.exists(), "shared blob deleted too early");
    store.delete(second).await.unwrap();
    assert!(!blob.exists(), "orphaned blob should be reclaimed");
}

#[tokio::test]
async fn history_is_appended_for_tracked_paths() {
    let f = fixture().await;
    let install = InstallationId::new();
    f.db.record_installation(install, f.game, f.mod_id, f.release, f.archive)
        .await
        .unwrap();
    let t = target("a.txt");
    let e = entry(install, b"x");
    let mut stack = ProviderStack::new();
    stack.push(e.clone());
    f.db.put_stack(f.game, &t, &stack).await.unwrap();

    let op = OperationId::new();
    f.db.record_history(f.game, &t, op, "deployed", Some(&e))
        .await
        .unwrap();
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM file_provider_history")
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);

    // An untracked path has nothing to attach history to and is a no-op.
    f.db.record_history(f.game, &target("untracked"), op, "deployed", None)
        .await
        .unwrap();
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM file_provider_history")
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn catalogue_upserts_are_idempotent() {
    let f = fixture().await;
    let provider = ProviderId::nexus();
    let before = f.db.games(&provider).await.unwrap();

    let same_game = onera_core::domain::game::Game {
        id: GameId::new(), // a different local id
        provider: provider.clone(),
        provider_slug: "cyberpunk2077".into(),
        name: "Cyberpunk 2077 Ultimate".into(),
        steam_app_id: Some(1_091_500),
    };
    let id = f.db.upsert_game(&same_game).await.unwrap();
    let after = f.db.games(&provider).await.unwrap();

    assert_eq!(
        after.len(),
        before.len(),
        "matching on provider slug must not duplicate"
    );
    assert_eq!(id, before[0].id, "the original identity is kept");
    assert_eq!(
        after[0].name, "Cyberpunk 2077 Ultimate",
        "metadata is refreshed"
    );
}

#[tokio::test]
async fn an_archive_with_identical_bytes_is_deduplicated() {
    let f = fixture().await;
    let again =
        f.db.upsert_archive(
            &FileHash::blake3_of(b"archive bytes"),
            13,
            "renamed-by-the-user.zip",
            onera_core::domain::archive::ArchiveFormat::Zip,
            std::path::Path::new("/data/archives/blake3/ab/abc"),
        )
        .await
        .unwrap();
    assert_eq!(again, f.archive);
}

#[tokio::test]
async fn version_strings_survive_storage_unchanged() {
    let f = fixture().await;
    let odd = Release {
        id: ReleaseId::new(),
        mod_id: f.mod_id,
        version: "  v2.0 RC-1 (hotfix) ".into(),
        published_at: chrono::DateTime::from_timestamp(1_800_000_000, 0),
        metadata: serde_json::Value::Null,
    };
    let id = f.db.upsert_release(&odd).await.unwrap();
    let (stored,): (String,) = sqlx::query_as("SELECT version FROM releases WHERE id = ?1")
        .bind(id.to_string())
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    assert_eq!(stored, "  v2.0 RC-1 (hotfix) ");
}

// ---------------------------------------------------------------------------
// Baselines
// ---------------------------------------------------------------------------

fn build_identity(build: &str) -> onera_core::domain::baseline::StoreBuildIdentity {
    use onera_core::domain::baseline::{DepotIdentity, GameStoreKind, StoreBuildIdentity};
    StoreBuildIdentity {
        store: GameStoreKind::Steam,
        app_id: Some("1091500".into()),
        build_id: Some(build.into()),
        branch: None,
        depots: vec![DepotIdentity {
            depot_id: "1091501".into(),
            manifest_id: "77".into(),
        }],
        manifest_path: Some("/games/steamapps/appmanifest_1091500.acf".into()),
        observed_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
    }
}

fn a_baseline(
    game: LocalGameId,
    build: &str,
    captured_at: i64,
    status: onera_core::domain::baseline::BaselineStatus,
    files: &[onera_core::domain::baseline::BaselineFile],
) -> onera_core::domain::baseline::GameBaseline {
    use onera_core::domain::baseline::{BaselineSource, GameBaseline, ScanScopeFingerprint};
    GameBaseline {
        id: BaselineId::new(),
        local_game_id: game,
        source: BaselineSource::StoreVerifiedCapture,
        build_identity: Some(build_identity(build)),
        adapter_id: "cyberpunk2077".into(),
        reported_version: Some("2.21".into()),
        status,
        captured_at: chrono::DateTime::from_timestamp(captured_at, 0).unwrap(),
        scope_fingerprint: ScanScopeFingerprint::from("b3fingerprint".to_owned()),
        file_count: files.len() as u64,
        total_bytes: files.iter().map(|f| f.size).sum(),
    }
}

fn a_baseline_file(path: &str, contents: &[u8]) -> onera_core::domain::baseline::BaselineFile {
    onera_core::domain::baseline::BaselineFile {
        root_key: "game".into(),
        path: RelPath::normalize(path).unwrap(),
        hash: FileHash::blake3_of(contents),
        size: contents.len() as u64,
        mode: Some(0o644),
    }
}

fn a_scan_run(
    game: LocalGameId,
    state: onera_core::domain::baseline::ScanState,
) -> onera_core::domain::baseline::BaselineScanRun {
    use onera_core::domain::baseline::{
        BaselineScanRun, FindingCounts, ScanEvidence, ScanPurpose, ScanState,
    };
    BaselineScanRun {
        id: BaselineScanRunId::new(),
        local_game_id: game,
        baseline_id: None,
        purpose: ScanPurpose::Capture,
        state,
        evidence: ScanEvidence::ContentHashed,
        started_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        finished_at: (state != ScanState::Running)
            .then(|| chrono::DateTime::from_timestamp(1_700_000_500, 0).unwrap()),
        files_scanned: 3,
        bytes_hashed: 300,
        counts: FindingCounts::default(),
        error: None,
    }
}

/// A recapture must keep the old baseline and its files: history is the whole
/// point of superseding rather than overwriting.
#[tokio::test]
async fn a_new_current_baseline_supersedes_the_old_one_without_deleting_it() {
    use onera_core::domain::baseline::BaselineStatus;
    use onera_core::ports::BaselineStore;

    let f = fixture().await;
    let first_files = [a_baseline_file("bin/x64/game.exe", b"v1")];
    let first = a_baseline(
        f.game,
        "18320471",
        1_700_000_000,
        BaselineStatus::Current,
        &first_files,
    );
    f.db.put_baseline(&first, &first_files).await.unwrap();

    let second_files = [
        a_baseline_file("bin/x64/game.exe", b"v2"),
        a_baseline_file("archive/pc/content/basegame.archive", b"content"),
    ];
    let second = a_baseline(
        f.game,
        "18400000",
        1_700_100_000,
        BaselineStatus::Current,
        &second_files,
    );
    f.db.put_baseline(&second, &second_files).await.unwrap();

    let current = f.db.current_baseline(f.game).await.unwrap().unwrap();
    assert_eq!(current.id, second.id, "the newest capture must be current");
    assert_eq!(current.build_identity, Some(build_identity("18400000")));

    let history = f.db.baselines(f.game).await.unwrap();
    assert_eq!(
        history.iter().map(|b| b.id).collect::<Vec<_>>(),
        vec![second.id, first.id],
        "history is newest first and keeps the superseded capture"
    );
    assert_eq!(history[1].status, BaselineStatus::Superseded);
    assert_eq!(
        f.db.baseline_files(first.id).await.unwrap().len(),
        1,
        "superseding must not delete the old baseline's file records"
    );
}

/// Writing a baseline twice is a bug in the caller, not an update.
#[tokio::test]
async fn a_captured_baseline_cannot_be_rewritten() {
    use onera_core::domain::baseline::BaselineStatus;
    use onera_core::ports::BaselineStore;

    let f = fixture().await;
    let files = [a_baseline_file("bin/x64/game.exe", b"v1")];
    let baseline = a_baseline(
        f.game,
        "18320471",
        1_700_000_000,
        BaselineStatus::Current,
        &files,
    );
    f.db.put_baseline(&baseline, &files).await.unwrap();

    let error = f.db.put_baseline(&baseline, &files).await.unwrap_err();
    assert!(
        matches!(error, onera_core::CoreError::Conflict(_)),
        "expected a conflict, got {error:?}"
    );

    // The schema refuses an in-place edit even when the port is bypassed.
    let direct = sqlx::query("UPDATE game_baselines SET total_bytes = 0 WHERE id = ?1")
        .bind(baseline.id.to_string())
        .execute(f.db.pool())
        .await;
    assert!(direct.is_err(), "a baseline's contents must be immutable");

    let direct_file = sqlx::query("UPDATE baseline_files SET size = 0 WHERE baseline_id = ?1")
        .bind(baseline.id.to_string())
        .execute(f.db.pool())
        .await;
    assert!(
        direct_file.is_err(),
        "a baseline's file records must be immutable"
    );
}

/// Two reads of the same baseline must produce the same list in the same order,
/// or a diff between two captures means nothing.
#[tokio::test]
async fn baseline_files_come_back_in_a_deterministic_order() {
    use onera_core::domain::baseline::BaselineStatus;
    use onera_core::ports::BaselineStore;

    let f = fixture().await;
    let files = [
        a_baseline_file("r6/scripts/z.reds", b"z"),
        a_baseline_file("archive/pc/content/a.archive", b"a"),
        a_baseline_file("bin/x64/game.exe", b"exe"),
        a_baseline_file("archive/pc/content/b.archive", b"b"),
    ];
    let baseline = a_baseline(
        f.game,
        "18320471",
        1_700_000_000,
        BaselineStatus::Current,
        &files,
    );
    f.db.put_baseline(&baseline, &files).await.unwrap();

    let stored = f.db.baseline_files(baseline.id).await.unwrap();
    assert_eq!(
        stored
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "archive/pc/content/a.archive",
            "archive/pc/content/b.archive",
            "bin/x64/game.exe",
            "r6/scripts/z.reds",
        ]
    );
    assert_eq!(stored, f.db.baseline_files(baseline.id).await.unwrap());
    assert_eq!(stored[2].mode, Some(0o644));
    assert_eq!(stored[2].hash, FileHash::blake3_of(b"exe"));
}

/// A scan is progress, not a verdict: the same run is written repeatedly as it
/// advances and finally as it stops.
#[tokio::test]
async fn a_scan_run_records_progress_and_then_its_terminal_state() {
    use onera_core::domain::baseline::{FindingCounts, ScanState};
    use onera_core::ports::BaselineStore;

    let f = fixture().await;
    let mut run = a_scan_run(f.game, ScanState::Running);
    run.finished_at = None;
    run.files_scanned = 12;
    run.bytes_hashed = 4096;
    f.db.put_scan_run(&run).await.unwrap();

    let stored = f.db.scan_run(run.id).await.unwrap().unwrap();
    assert_eq!(stored.state, ScanState::Running);
    assert_eq!(stored.files_scanned, 12);
    assert_eq!(stored.finished_at, None);

    run.state = ScanState::Cancelled;
    run.finished_at = Some(chrono::DateTime::from_timestamp(1_700_000_900, 0).unwrap());
    run.files_scanned = 20;
    run.bytes_hashed = 8192;
    run.counts = FindingCounts {
        matching: 18,
        modified: 1,
        extra_unknown: 1,
        ..FindingCounts::default()
    };
    run.error = Some("the user stopped the scan".into());
    f.db.put_scan_run(&run).await.unwrap();

    let stored = f.db.scan_run(run.id).await.unwrap().unwrap();
    assert_eq!(stored, run, "the terminal state replaces the running one");
    assert!(
        !stored.state.is_complete(),
        "a cancelled scan never covered its whole scope"
    );
}

/// Findings round-trip in the scanner's own order, and a re-run replaces them
/// rather than appending a second partial result to the first.
#[tokio::test]
async fn findings_round_trip_in_order_and_a_rerun_replaces_them() {
    use onera_core::domain::baseline::{BaselineFinding, FileClassification as Class, ScanState};
    use onera_core::ports::BaselineStore;

    let f = fixture().await;
    let run = a_scan_run(f.game, ScanState::Completed);
    f.db.put_scan_run(&run).await.unwrap();

    let finding = |path: &str, class: Class, detail: Option<&str>| BaselineFinding {
        root_key: "game".into(),
        path: RelPath::normalize(path).unwrap(),
        classification: class,
        expected: Some(FileHash::blake3_of(b"expected")),
        observed: (class != Class::Missing).then(|| FileHash::blake3_of(b"observed")),
        detail: detail.map(str::to_owned),
    };
    let partial = vec![
        finding("bin/x64/game.exe", Class::Modified, None),
        finding("r6/scripts/gone.reds", Class::Missing, None),
    ];
    f.db.put_findings(run.id, &partial).await.unwrap();
    assert_eq!(f.db.findings(run.id).await.unwrap(), partial);

    let complete = vec![
        finding("archive/pc/mod/x.archive", Class::ExtraUnknown, None),
        finding("bin/x64/game.exe", Class::Modified, None),
        finding(
            "bin/x64/link",
            Class::SpecialFile,
            Some("symbolic link rejected from the trusted baseline"),
        ),
    ];
    f.db.put_findings(run.id, &complete).await.unwrap();
    assert_eq!(
        f.db.findings(run.id).await.unwrap(),
        complete,
        "a re-run replaces its findings; a mixed result would be a lie"
    );
}

/// The schema, not application discipline, is what stops two current baselines
/// and orphaned baseline rows.
#[tokio::test]
async fn baseline_rows_obey_their_foreign_keys_and_uniqueness() {
    use onera_core::domain::baseline::{BaselineStatus, ScanState};
    use onera_core::ports::BaselineStore;

    let f = fixture().await;
    let files = [a_baseline_file("bin/x64/game.exe", b"v1")];
    let baseline = a_baseline(
        f.game,
        "18320471",
        1_700_000_000,
        BaselineStatus::Current,
        &files,
    );
    f.db.put_baseline(&baseline, &files).await.unwrap();

    // A second `current` row for the same game is refused by the partial index.
    let clash = sqlx::query(
        "INSERT INTO game_baselines
            (id, local_game_id, source, build_identity, adapter_id, reported_version,
             status, captured_at, scope_fingerprint, file_count, total_bytes)
         VALUES ('clash', ?1, 'local_snapshot', NULL, 'cyberpunk2077', NULL,
                 'current', '2026-01-01T00:00:00Z', 'b3', 0, 0)",
    )
    .bind(f.game.to_string())
    .execute(f.db.pool())
    .await;
    assert!(clash.is_err(), "a game may have only one current baseline");

    // A baseline for a game that does not exist is refused.
    let orphan = sqlx::query(
        "INSERT INTO game_baselines
            (id, local_game_id, source, build_identity, adapter_id, reported_version,
             status, captured_at, scope_fingerprint, file_count, total_bytes)
         VALUES ('orphan', 'no-such-game', 'local_snapshot', NULL, 'cyberpunk2077', NULL,
                 'current', '2026-01-01T00:00:00Z', 'b3', 0, 0)",
    )
    .execute(f.db.pool())
    .await;
    assert!(orphan.is_err(), "a baseline must belong to a real game");

    // Findings cannot exist without a run.
    let run = a_scan_run(f.game, ScanState::Completed);
    let missing_run = f.db.put_findings(run.id, &[]).await;
    assert!(
        missing_run.is_err(),
        "findings need a scan run to belong to"
    );

    // Deleting the game takes the whole baseline record set with it.
    f.db.put_scan_run(&run).await.unwrap();
    sqlx::query("DELETE FROM local_game_installs WHERE id = ?1")
        .bind(f.game.to_string())
        .execute(f.db.pool())
        .await
        .unwrap();
    let (baselines,): (i64,) = sqlx::query_as("SELECT count(*) FROM game_baselines")
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    let (baseline_files,): (i64,) = sqlx::query_as("SELECT count(*) FROM baseline_files")
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    let (runs,): (i64,) = sqlx::query_as("SELECT count(*) FROM baseline_scan_runs")
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    assert_eq!((baselines, baseline_files, runs), (0, 0, 0));
}

/// Superseding is a lifecycle change, and a failed capture was never
/// authoritative enough to have one.
#[tokio::test]
async fn only_a_usable_baseline_can_be_superseded() {
    use onera_core::domain::baseline::BaselineStatus;
    use onera_core::ports::BaselineStore;

    let f = fixture().await;
    let files = [a_baseline_file("bin/x64/game.exe", b"v1")];
    let current = a_baseline(
        f.game,
        "18320471",
        1_700_000_000,
        BaselineStatus::Current,
        &files,
    );
    f.db.put_baseline(&current, &files).await.unwrap();
    f.db.supersede_baseline(current.id).await.unwrap();
    assert_eq!(f.db.current_baseline(f.game).await.unwrap(), None);

    let failed = a_baseline(
        f.game,
        "18320471",
        1_700_000_100,
        BaselineStatus::Failed,
        &[],
    );
    f.db.put_baseline(&failed, &[]).await.unwrap();
    assert!(f.db.supersede_baseline(failed.id).await.is_err());

    let absent =
        f.db.supersede_baseline(BaselineId::new())
            .await
            .unwrap_err();
    assert!(matches!(absent, onera_core::CoreError::NotFound { .. }));
}
