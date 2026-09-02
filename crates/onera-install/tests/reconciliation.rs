mod harness;

use harness::{target, World};
use onera_core::domain::provider_stack::{FileProvider, ProviderStack, StackEntry};
use onera_core::domain::reconcile::{reconcile, DesiredGameState, InstallationMapping};
use onera_core::hash::FileHash;
use onera_core::ids::InstallationId;
use onera_core::ports::{DeploymentStore, FileSystem, OperationJournal};
use onera_core::progress::{CancelToken, NullProgress};
use onera_install::fs::fault::{FailAt, FaultyFileSystem};
use onera_install::{RealFileSystem, ReconciliationEngine};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

fn engine(world: &World) -> ReconciliationEngine {
    engine_with(world, Arc::new(RealFileSystem))
}

fn engine_with(world: &World, fs: Arc<dyn FileSystem>) -> ReconciliationEngine {
    ReconciliationEngine::new(
        fs,
        Arc::new(world.db.clone()),
        world.backups.clone(),
        Arc::new(world.db.clone()),
    )
}

#[tokio::test]
async fn retained_artifacts_enable_and_disable_without_a_download() {
    let world = World::new().await;
    let installation = InstallationId::new();
    world
        .db
        .record_installation(
            installation,
            world.local_game,
            world.mod_id,
            world.release,
            world.archive,
        )
        .await
        .unwrap();
    world
        .db
        .deactivate_installation(installation)
        .await
        .unwrap();
    let (staging, _) = world.stage("reactivate", &[("mods/a.bin", b"retained")]);
    let mapping = InstallationMapping {
        installation_id: installation,
        source: onera_core::RelPath::normalize("mods/a.bin").unwrap(),
        target: target("mods/a.bin"),
        source_hash: FileHash::blake3_of(b"retained"),
        source_size: 8,
    };
    let enabled = reconcile(
        DesiredGameState::new(world.local_game, vec![installation]),
        &BTreeMap::new(),
        std::slice::from_ref(&mapping),
    );
    engine(&world)
        .apply(
            &enabled,
            std::slice::from_ref(&mapping),
            &BTreeMap::from([(installation, staging)]),
            &world.roots,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        world.read_game_file("mods/a.bin"),
        Some(b"retained".to_vec())
    );
    assert_eq!(
        world
            .db
            .active_installations(world.local_game)
            .await
            .unwrap(),
        vec![installation]
    );

    let current = BTreeMap::from([(
        target("mods/a.bin"),
        world
            .db
            .stack(world.local_game, &target("mods/a.bin"))
            .await
            .unwrap(),
    )]);
    let disabled = reconcile(
        DesiredGameState::new(world.local_game, vec![]),
        &current,
        &[],
    );
    engine(&world)
        .apply(
            &disabled,
            &[],
            &BTreeMap::new(),
            &world.roots,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert!(!world.game_file_exists("mods/a.bin"));
    assert!(world
        .db
        .active_installations(world.local_game)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn two_artifacts_change_under_one_journaled_operation() {
    let world = World::new().await;
    let (other_mod, other_release) = world.another_mod("2").await;
    let a = InstallationId::new();
    let b = InstallationId::new();
    for (installation, mod_id, release) in [
        (a, world.mod_id, world.release),
        (b, other_mod, other_release),
    ] {
        world
            .db
            .record_installation(
                installation,
                world.local_game,
                mod_id,
                release,
                world.archive,
            )
            .await
            .unwrap();
        world
            .db
            .deactivate_installation(installation)
            .await
            .unwrap();
    }
    let (a_dir, _) = world.stage("state-a", &[("a.bin", b"a")]);
    let (b_dir, _) = world.stage("state-b", &[("b.bin", b"b")]);
    let mappings = vec![
        InstallationMapping {
            installation_id: a,
            source: onera_core::RelPath::normalize("a.bin").unwrap(),
            target: target("a.bin"),
            source_hash: FileHash::blake3_of(b"a"),
            source_size: 1,
        },
        InstallationMapping {
            installation_id: b,
            source: onera_core::RelPath::normalize("b.bin").unwrap(),
            target: target("b.bin"),
            source_hash: FileHash::blake3_of(b"b"),
            source_size: 1,
        },
    ];
    let plan = reconcile(
        DesiredGameState::new(world.local_game, vec![a, b]),
        &BTreeMap::new(),
        &mappings,
    );
    engine(&world)
        .apply(
            &plan,
            &mappings,
            &BTreeMap::from([(a, a_dir), (b, b_dir)]),
            &world.roots,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(world.read_game_file("a.bin"), Some(vec![b'a']));
    assert_eq!(world.read_game_file("b.bin"), Some(vec![b'b']));
    assert_eq!(
        world
            .db
            .active_installations(world.local_game)
            .await
            .unwrap()
            .into_iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([a, b])
    );
    let operations = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM operations WHERE kind = 'reconcile' AND state = 'complete'",
    )
    .fetch_one(world.db.pool())
    .await
    .unwrap();
    assert_eq!(operations, 1);
}

#[tokio::test]
async fn a_stale_preview_is_rejected_without_changing_the_file() {
    let world = World::new().await;
    let installation = InstallationId::new();
    world
        .db
        .record_installation(
            installation,
            world.local_game,
            world.mod_id,
            world.release,
            world.archive,
        )
        .await
        .unwrap();
    world
        .db
        .deactivate_installation(installation)
        .await
        .unwrap();
    let (staging, _) = world.stage("stale", &[("a.bin", b"new")]);
    let mapping = InstallationMapping {
        installation_id: installation,
        source: onera_core::RelPath::normalize("a.bin").unwrap(),
        target: target("a.bin"),
        source_hash: FileHash::blake3_of(b"new"),
        source_size: 3,
    };
    let plan = reconcile(
        DesiredGameState::new(world.local_game, vec![installation]),
        &BTreeMap::new(),
        std::slice::from_ref(&mapping),
    );
    world.write_unmanaged("a.bin", b"changed later");
    let error = engine(&world)
        .apply(
            &plan,
            &[mapping],
            &BTreeMap::from([(installation, staging)]),
            &world.roots,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("changed after"));
    assert_eq!(
        world.read_game_file("a.bin"),
        Some(b"changed later".to_vec())
    );
    let interrupted = world.db.interrupted().await.unwrap();
    assert!(interrupted.is_empty());
}

#[tokio::test]
async fn a_metadata_only_change_still_checks_the_previewed_file() {
    let world = World::new().await;
    let installation = InstallationId::new();
    world
        .db
        .record_installation(
            installation,
            world.local_game,
            world.mod_id,
            world.release,
            world.archive,
        )
        .await
        .unwrap();
    let mapping = InstallationMapping {
        installation_id: installation,
        source: onera_core::RelPath::normalize("same").unwrap(),
        target: target("same"),
        source_hash: FileHash::blake3_of(b"expected"),
        source_size: 8,
    };
    let stack = ProviderStack::from_entries(vec![StackEntry {
        provider: FileProvider::Installation {
            installation_id: installation,
        },
        hash: mapping.source_hash.clone(),
        size: mapping.source_size,
    }]);
    world
        .db
        .put_stack(world.local_game, &mapping.target, &stack)
        .await
        .unwrap();
    world.write_unmanaged("same", b"expected");
    let plan = reconcile(
        DesiredGameState::new(world.local_game, vec![installation]),
        &BTreeMap::from([(mapping.target.clone(), stack)]),
        std::slice::from_ref(&mapping),
    );
    assert!(plan.steps.is_empty());

    world.write_unmanaged("same", b"edited after preview");
    let error = engine(&world)
        .apply(
            &plan,
            &[mapping],
            &BTreeMap::new(),
            &world.roots,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("changed after"));
    assert_eq!(
        world.read_game_file("same"),
        Some(b"edited after preview".to_vec())
    );
}

#[tokio::test]
async fn a_failed_rename_rolls_back_every_file_and_database_change() {
    let world = World::new().await;
    let installation = InstallationId::new();
    world
        .db
        .record_installation(
            installation,
            world.local_game,
            world.mod_id,
            world.release,
            world.archive,
        )
        .await
        .unwrap();
    world
        .db
        .deactivate_installation(installation)
        .await
        .unwrap();
    let (staging, _) = world.stage("rename-failure", &[("a", b"a"), ("b", b"b")]);
    let mappings = [
        InstallationMapping {
            installation_id: installation,
            source: onera_core::RelPath::normalize("a").unwrap(),
            target: target("a"),
            source_hash: FileHash::blake3_of(b"a"),
            source_size: 1,
        },
        InstallationMapping {
            installation_id: installation,
            source: onera_core::RelPath::normalize("b").unwrap(),
            target: target("b"),
            source_hash: FileHash::blake3_of(b"b"),
            source_size: 1,
        },
    ];
    let plan = reconcile(
        DesiredGameState::new(world.local_game, vec![installation]),
        &BTreeMap::new(),
        &mappings,
    );
    let error = engine_with(&world, Arc::new(FaultyFileSystem::new(FailAt::Rename(1))))
        .apply(
            &plan,
            &mappings,
            &BTreeMap::from([(installation, staging)]),
            &world.roots,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("injected rename failure"));
    assert!(!world.game_file_exists("a"));
    assert!(!world.game_file_exists("b"));
    assert!(world
        .db
        .active_installations(world.local_game)
        .await
        .unwrap()
        .is_empty());
    assert!(world.db.interrupted().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_staging_failure_removes_earlier_temporary_files() {
    let world = World::new().await;
    let installation = InstallationId::new();
    world
        .db
        .record_installation(
            installation,
            world.local_game,
            world.mod_id,
            world.release,
            world.archive,
        )
        .await
        .unwrap();
    world
        .db
        .deactivate_installation(installation)
        .await
        .unwrap();
    let (staging, _) = world.stage("stage-failure", &[("a", b"a"), ("b", b"b")]);
    let mappings: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|path| InstallationMapping {
            installation_id: installation,
            source: onera_core::RelPath::normalize(path).unwrap(),
            target: target(path),
            source_hash: FileHash::blake3_of(path.as_bytes()),
            source_size: 1,
        })
        .collect();
    let plan = reconcile(
        DesiredGameState::new(world.local_game, vec![installation]),
        &BTreeMap::new(),
        &mappings,
    );
    engine_with(
        &world,
        Arc::new(FaultyFileSystem::new(FailAt::TempWrite(1))),
    )
    .apply(
        &plan,
        &mappings,
        &BTreeMap::from([(installation, staging)]),
        &world.roots,
        &NullProgress,
        &CancelToken::new(),
    )
    .await
    .unwrap_err();
    assert!(!world.game_file_exists("a"));
    assert!(!world.game_file_exists("b"));
    let entries: Vec<_> = std::fs::read_dir(&world.game_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(entries.iter().all(|name| !name.contains(".onera-tmp")));
    assert!(world.db.interrupted().await.unwrap().is_empty());
}
