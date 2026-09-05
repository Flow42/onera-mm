//! Injected SQLite failures around journal transitions and profile activation.
//!
//! `docs/recovery.md` makes a specific safety claim: because the journal write
//! always precedes the filesystem effect it describes, a database failure
//! mid-operation leaves the next launch looking at an *earlier* state than the
//! disk, and rolling back from an earlier state is idempotent. The opposite
//! ordering would be the dangerous one — a file renamed but never recorded is
//! a file recovery does not know to undo.
//!
//! Until now that claim was argued from the code rather than tested. These
//! tests fail one persistence call at a time and assert the two properties
//! that have to hold whatever fails:
//!
//! 1. the game directory is never left holding files from a failed operation;
//! 2. the active profile only ever names a deployment that is really on disk.

mod harness;

use harness::{target, World};
use onera_core::domain::operation::{OperationKind, OperationState};
use onera_core::domain::profile::Profile;
use onera_core::domain::reconcile::{
    reconcile, DesiredGameState, InstallationMapping, MutationPlan,
};
use onera_core::hash::FileHash;
use onera_core::ids::{InstallationId, ProfileId};
use onera_core::ports::{DeploymentStore, OperationJournal, ProfileStore};
use onera_core::progress::{CancelToken, NullProgress};
use onera_db::fault::{DbCall, FailAt, FaultyDatabase};
use onera_install::{Publication, RealFileSystem, ReconciliationEngine};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

/// An engine whose journal and reconciliation store are the faulty wrapper,
/// with a real filesystem underneath: the disk works, the database does not.
fn engine_with_db(world: &World, db: Arc<FaultyDatabase>) -> ReconciliationEngine {
    ReconciliationEngine::new(
        Arc::new(RealFileSystem),
        db.clone(),
        world.backups.clone(),
        db,
    )
}

fn faulty(world: &World, fail_at: FailAt) -> Arc<FaultyDatabase> {
    Arc::new(FaultyDatabase::new(world.db.clone(), fail_at))
}

/// Two profiles for the harness game: the active one and a switch target.
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
    let switch_to = make("Modded", false);
    world.db.put_profile(&current).await.unwrap();
    world.db.put_profile(&switch_to).await.unwrap();
    (current.id, switch_to.id)
}

/// A retained artifact and the plan that deploys it.
async fn plan_for(
    world: &World,
    name: &str,
    files: &[(&str, &[u8])],
) -> (
    InstallationId,
    PathBuf,
    Vec<InstallationMapping>,
    MutationPlan,
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

    let (staging, _) = world.stage(name, files);
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

/// Nothing from the operation may remain in the game directory — neither a
/// deployed file nor a staged temporary one.
fn game_directory_is_clean(world: &World, paths: &[&str]) {
    for path in paths {
        assert!(
            !world.game_file_exists(path),
            "{path} survived a failed operation"
        );
    }
    let leftovers: Vec<String> = walkdir::WalkDir::new(&world.game_dir)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .map(|e| e.path().to_string_lossy().into_owned())
        .filter(|p| p.contains(onera_install::fs::TEMP_SUFFIX))
        .collect();
    assert!(
        leftovers.is_empty(),
        "staged temporary files remain: {leftovers:?}"
    );
}

// ---------------------------------------------------------------------------
// Journal transitions
// ---------------------------------------------------------------------------

/// The journal cannot even be opened. Nothing has been staged yet, so the
/// operation must fail before it touches the game at all.
#[tokio::test]
async fn a_failure_opening_the_journal_touches_nothing() {
    let world = World::new().await;
    let (installation, staging, mappings, plan) =
        plan_for(&world, "begin-fails", &[("a", b"a"), ("b", b"b")]).await;

    let db = faulty(&world, FailAt::Nth(DbCall::Begin, 0));
    let error = engine_with_db(&world, db)
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

    assert!(error.to_string().contains("opening the journal"), "{error}");
    game_directory_is_clean(&world, &["a", "b"]);
    assert!(
        world.db.interrupted().await.unwrap().is_empty(),
        "an operation that never began cannot be interrupted"
    );
}

/// The first transition out of `Planned` fails. Staging has happened; the
/// commit loop has not. The game must be untouched and nothing left claiming
/// to be in progress.
#[tokio::test]
async fn a_failure_advancing_the_journal_rolls_back_and_reaches_a_terminal_state() {
    let world = World::new().await;
    let (installation, staging, mappings, plan) =
        plan_for(&world, "setstate-fails", &[("a", b"a"), ("b", b"b")]).await;

    let db = faulty(&world, FailAt::Nth(DbCall::SetState, 0));
    let error = engine_with_db(&world, db.clone())
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

    assert!(error.to_string().contains("state transition"), "{error}");
    game_directory_is_clean(&world, &["a", "b"]);

    // The installation must not have been activated by a half-run operation.
    assert!(world
        .db
        .active_installations(world.local_game)
        .await
        .unwrap()
        .is_empty());
}

/// A per-file journal entry fails midway through the commit loop.
///
/// This is the case the write ordering exists for: the entry is written
/// *before* the rename it describes, so a failure here means the rename never
/// happened and recovery has a complete picture of everything that did.
#[tokio::test]
async fn a_failure_writing_a_journal_entry_leaves_no_unrecorded_file() {
    let world = World::new().await;
    let (installation, staging, mappings, plan) = plan_for(
        &world,
        "putentry-fails",
        &[("a", b"a"), ("b", b"b"), ("c", b"c")],
    )
    .await;

    // Let the first entries through and fail a later one, so the operation
    // dies with some files already committed.
    let db = faulty(&world, FailAt::Nth(DbCall::PutEntry, 1));
    let error = engine_with_db(&world, db.clone())
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
    assert!(error.to_string().contains("journal entry"), "{error}");

    // Every file is either rolled back or was never written. Recovery is only
    // able to undo what the journal knows about, so anything left here would be
    // a file no future run could clean up.
    game_directory_is_clean(&world, &["a", "b", "c"]);
    assert!(world
        .db
        .active_installations(world.local_game)
        .await
        .unwrap()
        .is_empty());
}

/// The database goes away entirely, so the rollback cannot record its own
/// progress either.
///
/// `docs/recovery.md` says a rollback that cannot finish must not be reported
/// as done and must not be retried automatically, because recorded state and
/// disk state now disagree. The operation therefore has to be left in a
/// non-terminal state, which is exactly what puts it in front of the user on
/// the next launch.
#[tokio::test]
async fn a_rollback_that_cannot_be_recorded_is_reported_and_left_for_recovery() {
    let world = World::new().await;
    let (installation, staging, mappings, plan) =
        plan_for(&world, "rollback-fails", &[("a", b"a"), ("b", b"b")]).await;

    // Transition 0 (Prepared) succeeds; transition 1 (Committing) and every one
    // after it — including the rollback's own — fail.
    let db = faulty(&world, FailAt::EveryAfter(DbCall::SetState, 1));
    let error = engine_with_db(&world, db.clone())
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

    // The caller is told both what failed and that the undo did not complete,
    // rather than being handed a bare error that looks recoverable.
    let message = error.to_string();
    assert!(message.contains("state transition"), "{message}");
    assert!(message.contains("rollback also failed"), "{message}");

    // Nothing was deployed: the failure landed before the commit loop.
    assert!(!world.game_file_exists("a"));
    assert!(!world.game_file_exists("b"));
    assert!(world
        .db
        .active_installations(world.local_game)
        .await
        .unwrap()
        .is_empty());

    // The operation is still open, so the next launch offers it for recovery.
    // A rollback that could not be recorded must never look finished.
    let interrupted = world.db.interrupted().await.unwrap();
    assert_eq!(
        interrupted.len(),
        1,
        "an unfinished rollback must be offered for recovery"
    );
    assert!(!interrupted[0].state.is_terminal());
    assert_eq!(interrupted[0].state, OperationState::Prepared);
}

/// Once the database is working again, the recovery path finishes the job.
///
/// This is the other half of the claim: leaving the operation open is only safe
/// if a later run can actually complete the rollback from it.
#[tokio::test]
async fn a_recovered_rollback_completes_once_the_database_works_again() {
    let world = World::new().await;
    let (installation, staging, mappings, plan) =
        plan_for(&world, "rollback-recovers", &[("a", b"a"), ("b", b"b")]).await;

    let db = faulty(&world, FailAt::EveryAfter(DbCall::SetState, 1));
    engine_with_db(&world, db)
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

    let interrupted = world.db.interrupted().await.unwrap();
    assert_eq!(interrupted.len(), 1);

    // A fresh engine on a healthy database, as the next launch would build.
    let healthy = faulty(&world, FailAt::Never);
    engine_with_db(&world, healthy)
        .rollback(interrupted[0].id)
        .await
        .unwrap();

    game_directory_is_clean(&world, &["a", "b"]);
    assert!(
        world.db.interrupted().await.unwrap().is_empty(),
        "recovery left the operation open"
    );
    assert_eq!(
        world
            .db
            .get(interrupted[0].id)
            .await
            .unwrap()
            .unwrap()
            .state,
        OperationState::RolledBack
    );
}

// ---------------------------------------------------------------------------
// Profile activation
// ---------------------------------------------------------------------------

/// The publishing transaction fails after every file is verified on disk.
///
/// Both halves of the switch live in that one transaction, so the profile must
/// stay where it was and the files must be rolled back with it. A profile
/// marked active over a deployment that was undone is exactly the lie the
/// activation flow exists to prevent.
#[tokio::test]
async fn a_failed_publish_leaves_the_previous_profile_active() {
    let world = World::new().await;
    let (current, switch_to) = two_profiles(&world).await;
    let (installation, staging, mappings, plan) =
        plan_for(&world, "publish-fails", &[("mods/a.bin", b"profile a")]).await;

    let db = faulty(&world, FailAt::Nth(DbCall::CompleteReconciliation, 0));
    let attempt = engine_with_db(&world, db.clone())
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
    assert!(
        error.to_string().contains("publishing a reconciliation"),
        "{error}"
    );
    assert!(attempt.rolled_back, "the deployment was not undone");

    // Neither half happened.
    game_directory_is_clean(&world, &["mods/a.bin"]);
    assert_eq!(
        world
            .db
            .active_profile(world.local_game)
            .await
            .unwrap()
            .unwrap()
            .id,
        current,
        "the profile switched without the deployment behind it"
    );
    assert!(world
        .db
        .active_installations(world.local_game)
        .await
        .unwrap()
        .is_empty());
    assert!(world.db.interrupted().await.unwrap().is_empty());
}

/// The publishing transaction is attempted exactly once.
///
/// A retry would be a second chance to activate a profile whose files have
/// since been rolled back.
#[tokio::test]
async fn a_failed_publish_is_not_retried() {
    let world = World::new().await;
    let (_, switch_to) = two_profiles(&world).await;
    let (installation, staging, mappings, plan) =
        plan_for(&world, "publish-once", &[("mods/a.bin", b"profile a")]).await;

    let db = faulty(&world, FailAt::Nth(DbCall::CompleteReconciliation, 0));
    let attempt = engine_with_db(&world, db.clone())
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

    assert!(attempt.result.is_err());
    assert_eq!(
        db.attempts(DbCall::CompleteReconciliation),
        1,
        "publication was retried after a failure"
    );
}

/// A successful activation through the faulty wrapper, with nothing selected to
/// fail, must behave exactly as the real database does.
///
/// Without this the tests above could pass because the wrapper is broken rather
/// than because the engine is careful.
#[tokio::test]
async fn the_wrapper_is_transparent_when_nothing_is_selected_to_fail() {
    let world = World::new().await;
    let (_, switch_to) = two_profiles(&world).await;
    let (installation, staging, mappings, plan) =
        plan_for(&world, "no-fault", &[("mods/a.bin", b"profile a")]).await;

    let db = faulty(&world, FailAt::Never);
    let attempt = engine_with_db(&world, db.clone())
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

    assert!(attempt.result.is_ok(), "{:?}", attempt.result);
    assert_eq!(
        world.read_game_file("mods/a.bin"),
        Some(b"profile a".to_vec())
    );
    assert_eq!(
        world
            .db
            .active_profile(world.local_game)
            .await
            .unwrap()
            .unwrap()
            .id,
        switch_to
    );
    assert!(db.attempts(DbCall::CompleteReconciliation) >= 1);
}
