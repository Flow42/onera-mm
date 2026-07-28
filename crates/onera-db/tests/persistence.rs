//! Integration tests for the persistence ports.

use onera_core::domain::operation::{OperationKind, OperationState};
use onera_core::domain::provider_stack::{FileProvider, ProviderStack, StackEntry};
use onera_core::domain::release::{Mod, Release};
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
use onera_db::Database;

/// Insert the provider/game/mod/release/archive rows every deployment test
/// needs, and return the ids.
struct Fixture {
    db: Database,
    game: LocalGameId,
    mod_id: ModId,
    release: ReleaseId,
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
        archive,
    }
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
