mod harness;

use harness::{target, World};
use onera_core::domain::operation::OperationKind;
use onera_core::domain::profile::Profile;
use onera_core::domain::provider_stack::{FileProvider, ProviderStack, StackEntry};
use onera_core::domain::reconcile::{reconcile, DesiredGameState, InstallationMapping};
use onera_core::hash::FileHash;
use onera_core::ids::{InstallationId, ProfileId};
use onera_core::ports::{DeploymentStore, FileSystem, OperationJournal, ProfileStore};
use onera_core::progress::{CancelToken, NullProgress};
use onera_install::fs::fault::{FailAt, FaultyFileSystem};
use onera_install::{Publication, RealFileSystem, ReconciliationEngine};
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

/// A reconciliation spanning two mods must roll back as one thing.
///
/// The multi-file rollback above uses a single installation, so it cannot catch
/// the failure that matters most here: one mod's files committed and its
/// installation activated while the other's were undone. A profile switch is
/// exactly this shape, and a half-applied one leaves a game the user cannot
/// reason about — some of profile B deployed, the rest of profile A gone.
#[tokio::test]
async fn a_failed_rename_rolls_back_every_mod_not_just_the_failing_one() {
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

    // Two files from one mod and one from the other, so the injected failure
    // can land after a file of each has already been committed.
    let (a_dir, _) = world.stage("multi-a", &[("a1.bin", b"a1"), ("a2.bin", b"a2")]);
    let (b_dir, _) = world.stage("multi-b", &[("b.bin", b"b")]);
    let mappings = vec![
        InstallationMapping {
            installation_id: a,
            source: onera_core::RelPath::normalize("a1.bin").unwrap(),
            target: target("a1.bin"),
            source_hash: FileHash::blake3_of(b"a1"),
            source_size: 2,
        },
        InstallationMapping {
            installation_id: a,
            source: onera_core::RelPath::normalize("a2.bin").unwrap(),
            target: target("a2.bin"),
            source_hash: FileHash::blake3_of(b"a2"),
            source_size: 2,
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

    // Fail partway through the commit loop, so at least one file is already
    // renamed into place when the operation dies.
    let error = engine_with(&world, Arc::new(FaultyFileSystem::new(FailAt::Rename(1))))
        .apply(
            &plan,
            &mappings,
            &BTreeMap::from([(a, a_dir), (b, b_dir)]),
            &world.roots,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("injected rename failure"));

    // Neither mod is left partly deployed — including the mod whose file was
    // committed before the failure.
    for path in ["a1.bin", "a2.bin", "b.bin"] {
        assert!(
            !world.game_file_exists(path),
            "{path} survived a rolled-back reconciliation"
        );
    }

    // And neither is left partly activated: an installation active without its
    // files is the state every later plan would be computed from.
    assert!(
        world
            .db
            .active_installations(world.local_game)
            .await
            .unwrap()
            .is_empty(),
        "a mod was activated by a rolled-back reconciliation"
    );
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

// ---------------------------------------------------------------------------
// Profile publication
// ---------------------------------------------------------------------------

/// Two profiles for the harness game: an active one and the switch target.
async fn two_profiles(world: &World) -> (ProfileId, ProfileId) {
    let at = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let make = |name: &str, active: bool| Profile {
        id: ProfileId::new(),
        local_game_id: world.local_game,
        name: name.to_owned(),
        description: None,
        is_active: active,
        created_at: at,
        updated_at: at,
    };
    let current = make("Default", true);
    let target = make("Modded", false);
    world.db.put_profile(&current).await.unwrap();
    world.db.put_profile(&target).await.unwrap();
    (current.id, target.id)
}

/// A one-file activation plan backed by a retained artifact.
async fn activation_plan(
    world: &World,
    files: &[(&str, &[u8])],
) -> (
    InstallationId,
    std::path::PathBuf,
    Vec<InstallationMapping>,
    onera_core::domain::reconcile::MutationPlan,
) {
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
    let (staging, _) = world.stage("activation", files);
    let mappings: Vec<_> = files
        .iter()
        .map(|(path, bytes)| InstallationMapping {
            installation_id: installation,
            source: onera_core::RelPath::normalize(path).unwrap(),
            target: target(path),
            source_hash: FileHash::blake3_of(bytes),
            source_size: bytes.len() as u64,
        })
        .collect();
    let plan = reconcile(
        DesiredGameState::new(world.local_game, vec![installation]),
        &BTreeMap::new(),
        &mappings,
    );
    (installation, staging, mappings, plan)
}

#[tokio::test]
async fn a_profile_becomes_active_with_the_deployment_it_describes() {
    let world = World::new().await;
    let (current, switch_to) = two_profiles(&world).await;
    let (installation, staging, mappings, plan) =
        activation_plan(&world, &[("mods/a.bin", b"profile a")]).await;

    let attempt = engine(&world)
        .attempt(
            &plan,
            &mappings,
            &BTreeMap::from([(installation, staging)]),
            &world.roots,
            OperationKind::Reconcile,
            Publication::activating(switch_to),
            &NullProgress,
            &CancelToken::new(),
        )
        .await;
    assert!(attempt.result.is_ok());
    assert!(
        attempt.operation.is_some(),
        "the attempt must name its operation"
    );

    assert_eq!(
        world.read_game_file("mods/a.bin"),
        Some(b"profile a".to_vec())
    );
    let active = world
        .db
        .active_profile(world.local_game)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(active.id, switch_to);
    assert_ne!(active.id, current);
}

#[tokio::test]
async fn a_failed_rename_leaves_the_previous_profile_active() {
    let world = World::new().await;
    let (current, switch_to) = two_profiles(&world).await;
    let (installation, staging, mappings, plan) =
        activation_plan(&world, &[("a", b"a"), ("b", b"b")]).await;

    let attempt = engine_with(&world, Arc::new(FaultyFileSystem::new(FailAt::Rename(1))))
        .attempt(
            &plan,
            &mappings,
            &BTreeMap::from([(installation, staging)]),
            &world.roots,
            OperationKind::Reconcile,
            Publication::activating(switch_to),
            &NullProgress,
            &CancelToken::new(),
        )
        .await;
    let error = attempt.result.unwrap_err();
    assert!(error.to_string().contains("injected rename failure"));
    assert!(attempt.rolled_back, "the failure was undone");

    // Neither half of the publication happened: no files, no switch.
    assert!(!world.game_file_exists("a"));
    assert!(!world.game_file_exists("b"));
    assert!(world
        .db
        .active_installations(world.local_game)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        world
            .db
            .active_profile(world.local_game)
            .await
            .unwrap()
            .unwrap()
            .id,
        current
    );
    assert!(world.db.interrupted().await.unwrap().is_empty());
}

#[tokio::test]
async fn an_unresolved_conflict_publishes_no_profile_and_journals_nothing() {
    let world = World::new().await;
    let (current, switch_to) = two_profiles(&world).await;
    let (first, staging, mut mappings, _) = activation_plan(&world, &[("shared", b"one")]).await;
    let (other_mod, other_release) = world.another_mod("2").await;
    let second = InstallationId::new();
    world
        .db
        .record_installation(
            second,
            world.local_game,
            other_mod,
            other_release,
            world.archive,
        )
        .await
        .unwrap();
    world.db.deactivate_installation(second).await.unwrap();
    mappings.push(InstallationMapping {
        installation_id: second,
        source: onera_core::RelPath::normalize("shared").unwrap(),
        target: target("shared"),
        source_hash: FileHash::blake3_of(b"two"),
        source_size: 3,
    });
    let plan = reconcile(
        DesiredGameState::new(world.local_game, vec![first, second]),
        &BTreeMap::new(),
        &mappings,
    );
    assert!(!plan.is_ready());

    let attempt = engine(&world)
        .attempt(
            &plan,
            &mappings,
            &BTreeMap::from([(first, staging)]),
            &world.roots,
            OperationKind::Reconcile,
            Publication::activating(switch_to),
            &NullProgress,
            &CancelToken::new(),
        )
        .await;
    assert!(matches!(
        attempt.result,
        Err(onera_core::CoreError::DecisionRequired(_))
    ));
    assert!(attempt.operation.is_none(), "nothing may be journaled");
    assert_eq!(
        world
            .db
            .active_profile(world.local_game)
            .await
            .unwrap()
            .unwrap()
            .id,
        current
    );
}
