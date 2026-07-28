//! End-to-end tests for the installation engine.
//!
//! These run against a real SQLite database and a real temporary game
//! directory. Only the filesystem is swapped, and only when a test needs to
//! interrupt an operation at a precise point.

mod harness;

use harness::{target, FlatAdapter, World};
use onera_core::domain::operation::OperationState;
use onera_core::hash::FileHash;
use onera_core::ids::InstallationId;
use onera_core::plan::{
    ConflictChoice, Decision, DecisionScope, FileClassification, InstallPlan, PlannedAction,
    ScopedRule,
};
use onera_core::ports::{DeploymentStore, GameAdapter, OperationJournal};
use onera_core::progress::{CancelToken, NullProgress, RecordingProgress};
use onera_core::CoreError;
use onera_install::fs::fault::{FailAt, FaultyFileSystem};
use onera_install::planner::{plan_install, PlanRequest};
use onera_install::remove::ModifiedFilePolicy;
use onera_install::{recover_all, verify_installation, RemovalReport};
use std::sync::Arc;

/// Plan an install of `files` for `mod_id`, returning the plan and staging dir.
async fn plan(
    world: &World,
    name: &str,
    mod_id: onera_core::ids::ModId,
    installation: InstallationId,
    files: &[(&str, &[u8])],
    rules: &[ScopedRule],
) -> (InstallPlan, std::path::PathBuf) {
    let (staging, manifest) = world.stage(name, files);
    let adapter = FlatAdapter;
    let layout = adapter.resolve_layout(&manifest).unwrap();
    let plan = plan_install(
        PlanRequest {
            local_game_id: world.local_game,
            mod_id,
            installation_id: installation,
            manifest: &manifest,
            mappings: &layout.mappings,
            roots: &world.roots,
            adapter: &adapter,
            rules,
        },
        &onera_install::RealFileSystem,
        &world.db,
        &NullProgress,
        &CancelToken::new(),
    )
    .await
    .unwrap();
    (plan, staging)
}

/// Plan and apply in one step, asserting the plan was ready.
async fn install(
    world: &World,
    name: &str,
    mod_id: onera_core::ids::ModId,
    release: onera_core::ids::ReleaseId,
    files: &[(&str, &[u8])],
) -> InstallationId {
    let installation = InstallationId::new();
    let (plan, staging) = plan(world, name, mod_id, installation, files, &[]).await;
    assert!(
        plan.is_ready(),
        "unexpected conflicts: {:?}",
        plan.summary()
    );
    world
        .installer()
        .apply(
            &plan,
            &staging,
            &world.roots,
            release,
            world.archive,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    installation
}

// ---------------------------------------------------------------------------
// Clean install
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_clean_install_writes_every_file_and_records_ownership() {
    let world = World::new().await;
    let installation = install(
        &world,
        "clean",
        world.mod_id,
        world.release,
        &[
            ("bin/plugin.dll", b"plugin bytes"),
            ("data/config.ini", b"config"),
        ],
    )
    .await;

    assert_eq!(
        world.read_game_file("bin/plugin.dll").unwrap(),
        b"plugin bytes"
    );
    assert_eq!(world.read_game_file("data/config.ini").unwrap(), b"config");

    let stack = world
        .db
        .stack(world.local_game, &target("bin/plugin.dll"))
        .await
        .unwrap();
    assert_eq!(stack.len(), 1);
    assert_eq!(
        stack.top().unwrap().provider.installation_id(),
        Some(installation)
    );
    assert_eq!(
        stack.top().unwrap().hash,
        FileHash::blake3_of(b"plugin bytes")
    );
}

#[tokio::test]
async fn no_temporary_files_survive_a_successful_install() {
    let world = World::new().await;
    install(
        &world,
        "tidy",
        world.mod_id,
        world.release,
        &[("a.txt", b"a")],
    )
    .await;

    let leftovers: Vec<_> = walk(&world.game_dir)
        .into_iter()
        .filter(|p| p.to_string_lossy().contains(onera_install::fs::TEMP_SUFFIX))
        .collect();
    assert!(
        leftovers.is_empty(),
        "temporary files left behind: {leftovers:?}"
    );
}

#[tokio::test]
async fn the_operation_ends_in_the_complete_state() {
    let world = World::new().await;
    let installation = InstallationId::new();
    let (plan, staging) = plan(
        &world,
        "states",
        world.mod_id,
        installation,
        &[("a.txt", b"a")],
        &[],
    )
    .await;
    let report = world
        .installer()
        .apply(
            &plan,
            &staging,
            &world.roots,
            world.release,
            world.archive,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(report.operation.state, OperationState::Complete);
    assert_eq!(report.written, 1);
    assert!(world.db.interrupted().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_dry_run_plan_writes_nothing() {
    let world = World::new().await;
    let (plan, _staging) = plan(
        &world,
        "dry",
        world.mod_id,
        InstallationId::new(),
        &[("a.txt", b"a")],
        &[],
    )
    .await;

    assert_eq!(plan.files[0].classification, FileClassification::Create);
    assert!(
        !world.game_file_exists("a.txt"),
        "planning must not touch the game directory"
    );
    let preview = onera_install::render_preview(&plan);
    assert!(preview.contains("game:a.txt"), "{preview}");
}

#[tokio::test]
async fn an_invalid_target_is_rejected_by_the_adapter() {
    let world = World::new().await;
    let (plan, _) = plan(
        &world,
        "invalid",
        world.mod_id,
        InstallationId::new(),
        &[("forbidden/evil.dll", b"x")],
        &[],
    )
    .await;
    assert_eq!(
        plan.files[0].classification,
        FileClassification::InvalidTarget
    );
    assert_eq!(plan.files[0].effective_action(), PlannedAction::Reject);
    assert!(!plan.files[0].notes.is_empty());
}

// ---------------------------------------------------------------------------
// Identical and shared files
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_mods_shipping_identical_bytes_share_one_file() {
    let world = World::new().await;
    let first = install(
        &world,
        "a",
        world.mod_id,
        world.release,
        &[("shared.dat", b"same")],
    )
    .await;
    let (other_mod, other_release) = world.another_mod("2").await;

    let second = InstallationId::new();
    let (plan, staging) = plan(
        &world,
        "b",
        other_mod,
        second,
        &[("shared.dat", b"same")],
        &[],
    )
    .await;
    assert_eq!(plan.files[0].classification, FileClassification::Identical);
    assert!(plan.is_ready(), "identical content must never prompt");

    let report = world
        .installer()
        .apply(
            &plan,
            &staging,
            &world.roots,
            other_release,
            world.archive,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(report.written, 0, "identical content must not be rewritten");
    assert_eq!(report.shared, 1);

    let stack = world
        .db
        .stack(world.local_game, &target("shared.dat"))
        .await
        .unwrap();
    assert_eq!(stack.len(), 2, "both mods must claim the file");
    let claimants: Vec<_> = stack.claiming_installations().collect();
    assert!(claimants.contains(&first) && claimants.contains(&second));
}

#[tokio::test]
async fn removing_one_owner_of_a_shared_file_leaves_it_in_place() {
    let world = World::new().await;
    let first = install(
        &world,
        "a",
        world.mod_id,
        world.release,
        &[("shared.dat", b"same")],
    )
    .await;
    let (other_mod, other_release) = world.another_mod("2").await;
    let second = install(
        &world,
        "b",
        other_mod,
        other_release,
        &[("shared.dat", b"same")],
    )
    .await;

    let report = world
        .remover()
        .remove(
            world.local_game,
            second,
            &world.roots,
            ModifiedFilePolicy::Ask,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(report.kept_shared.len(), 1);
    assert!(report.deleted.is_empty());
    assert_eq!(world.read_game_file("shared.dat").unwrap(), b"same");

    // The last owner going away does delete it.
    let report = world
        .remover()
        .remove(
            world.local_game,
            first,
            &world.roots,
            ModifiedFilePolicy::Ask,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(report.deleted.len(), 1);
    assert!(!world.game_file_exists("shared.dat"));
}

// ---------------------------------------------------------------------------
// Same-mod upgrades and downgrades
// ---------------------------------------------------------------------------

#[tokio::test]
async fn upgrading_a_mod_replaces_its_own_files_without_prompting() {
    let world = World::new().await;
    install(
        &world,
        "v1",
        world.mod_id,
        world.release,
        &[("mod.dat", b"version one")],
    )
    .await;

    let v2_release = world
        .db
        .upsert_release(&onera_core::domain::release::Release {
            id: onera_core::ids::ReleaseId::new(),
            mod_id: world.mod_id,
            version: "2.0".into(),
            published_at: chrono::DateTime::from_timestamp(9_000, 0),
            metadata: serde_json::Value::Null,
        })
        .await
        .unwrap();

    let v2 = InstallationId::new();
    let (plan, staging) = plan(
        &world,
        "v2",
        world.mod_id,
        v2,
        &[("mod.dat", b"version two")],
        &[],
    )
    .await;
    assert_eq!(
        plan.files[0].classification,
        FileClassification::ReplacePreviousRelease
    );
    assert!(plan.is_ready(), "a same-mod update must not prompt");

    world
        .installer()
        .apply(
            &plan,
            &staging,
            &world.roots,
            v2_release,
            world.archive,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(world.read_game_file("mod.dat").unwrap(), b"version two");
}

#[tokio::test]
async fn downgrading_restores_the_older_content_of_the_same_mod() {
    let world = World::new().await;
    install(
        &world,
        "v2",
        world.mod_id,
        world.release,
        &[("mod.dat", b"version two")],
    )
    .await;

    let v1 = InstallationId::new();
    let (plan, staging) = plan(
        &world,
        "v1",
        world.mod_id,
        v1,
        &[("mod.dat", b"version one")],
        &[],
    )
    .await;
    assert_eq!(
        plan.files[0].classification,
        FileClassification::ReplacePreviousRelease
    );
    world
        .installer()
        .apply(
            &plan,
            &staging,
            &world.roots,
            world.release,
            world.archive,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(world.read_game_file("mod.dat").unwrap(), b"version one");
}

#[tokio::test]
async fn a_same_mod_update_whose_target_was_hand_edited_asks_first() {
    let world = World::new().await;
    install(
        &world,
        "v1",
        world.mod_id,
        world.release,
        &[("mod.dat", b"version one")],
    )
    .await;
    // The user edits the deployed file themselves.
    world.write_unmanaged("mod.dat", b"hand edited by the user");

    let (plan, _) = plan(
        &world,
        "v2",
        world.mod_id,
        InstallationId::new(),
        &[("mod.dat", b"version two")],
        &[],
    )
    .await;
    assert_eq!(
        plan.files[0].classification,
        FileClassification::ExternallyModified
    );
    assert!(
        !plan.is_ready(),
        "a hand-edited file must never be overwritten silently"
    );
}

// ---------------------------------------------------------------------------
// Conflicts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unmanaged_file_always_asks_and_is_backed_up_before_replacement() {
    let world = World::new().await;
    world.write_unmanaged("vanilla.dat", b"original game file");

    let installation = InstallationId::new();
    let (mut plan, staging) = plan(
        &world,
        "over",
        world.mod_id,
        installation,
        &[("vanilla.dat", b"modded file")],
        &[],
    )
    .await;
    assert_eq!(
        plan.files[0].classification,
        FileClassification::UnmanagedExisting
    );
    assert!(!plan.is_ready());

    let t = plan.files[0].target.clone();
    plan.apply_decision(
        &t,
        &Decision {
            choice: ConflictChoice::ReplaceAfterBackup,
            scope: DecisionScope::ThisFile,
        },
    );
    let report = world
        .installer()
        .apply(
            &plan,
            &staging,
            &world.roots,
            world.release,
            world.archive,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(report.backed_up, 1);
    assert_eq!(world.read_game_file("vanilla.dat").unwrap(), b"modded file");

    // The stack must record the unmanaged original underneath the mod.
    let stack = world.db.stack(world.local_game, &t).await.unwrap();
    assert_eq!(stack.len(), 2);
    assert!(stack.has_unmanaged_original());
}

#[tokio::test]
async fn removing_a_mod_restores_the_unmanaged_original() {
    let world = World::new().await;
    world.write_unmanaged("vanilla.dat", b"original game file");

    let installation = InstallationId::new();
    let (mut plan, staging) = plan(
        &world,
        "over",
        world.mod_id,
        installation,
        &[("vanilla.dat", b"modded")],
        &[],
    )
    .await;
    let t = plan.files[0].target.clone();
    plan.apply_decision(
        &t,
        &Decision {
            choice: ConflictChoice::ReplaceAfterBackup,
            scope: DecisionScope::ThisFile,
        },
    );
    world
        .installer()
        .apply(
            &plan,
            &staging,
            &world.roots,
            world.release,
            world.archive,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();

    let report = world
        .remover()
        .remove(
            world.local_game,
            installation,
            &world.roots,
            ModifiedFilePolicy::Ask,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(report.restored.len(), 1);
    assert_eq!(
        world.read_game_file("vanilla.dat").unwrap(),
        b"original game file",
        "the pre-Onera file must come back byte for byte"
    );
}

#[tokio::test]
async fn keeping_the_existing_file_writes_nothing() {
    let world = World::new().await;
    world.write_unmanaged("vanilla.dat", b"original");

    let (mut plan, staging) = plan(
        &world,
        "keep",
        world.mod_id,
        InstallationId::new(),
        &[("vanilla.dat", b"modded")],
        &[],
    )
    .await;
    let t = plan.files[0].target.clone();
    plan.apply_decision(
        &t,
        &Decision {
            choice: ConflictChoice::KeepExisting,
            scope: DecisionScope::ThisFile,
        },
    );

    let report = world
        .installer()
        .apply(
            &plan,
            &staging,
            &world.roots,
            world.release,
            world.archive,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(report.written, 0);
    assert_eq!(world.read_game_file("vanilla.dat").unwrap(), b"original");
}

#[tokio::test]
async fn adopting_an_existing_file_records_ownership_without_writing() {
    let world = World::new().await;
    world.write_unmanaged("manual.dat", b"installed by hand");

    let installation = InstallationId::new();
    let (mut plan, staging) = plan(
        &world,
        "adopt",
        world.mod_id,
        installation,
        &[("manual.dat", b"from the archive")],
        &[],
    )
    .await;
    let t = plan.files[0].target.clone();
    plan.apply_decision(
        &t,
        &Decision {
            choice: ConflictChoice::AdoptExisting,
            scope: DecisionScope::ThisFile,
        },
    );

    let report = world
        .installer()
        .apply(
            &plan,
            &staging,
            &world.roots,
            world.release,
            world.archive,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(report.written, 0);
    assert_eq!(
        world.read_game_file("manual.dat").unwrap(),
        b"installed by hand"
    );
    assert_eq!(
        world.db.stack(world.local_game, &t).await.unwrap().len(),
        1,
        "the adopting installation must now claim the path"
    );
}

#[tokio::test]
async fn two_mods_claiming_one_path_is_a_cross_mod_conflict() {
    let world = World::new().await;
    let first = install(
        &world,
        "a",
        world.mod_id,
        world.release,
        &[("contested.dat", b"from mod a")],
    )
    .await;
    let (other_mod, other_release) = world.another_mod("2").await;

    let second = InstallationId::new();
    let (mut plan, staging) = plan(
        &world,
        "b",
        other_mod,
        second,
        &[("contested.dat", b"from mod b")],
        &[],
    )
    .await;
    assert_eq!(
        plan.files[0].classification,
        FileClassification::ConflictWithOtherMod
    );
    assert!(!plan.is_ready());

    let t = plan.files[0].target.clone();
    plan.apply_decision(
        &t,
        &Decision {
            choice: ConflictChoice::ReplaceAfterBackup,
            scope: DecisionScope::ThisFile,
        },
    );
    world
        .installer()
        .apply(
            &plan,
            &staging,
            &world.roots,
            other_release,
            world.archive,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(
        world.read_game_file("contested.dat").unwrap(),
        b"from mod b"
    );

    // Removing the winner restores the loser's file.
    let report = world
        .remover()
        .remove(
            world.local_game,
            second,
            &world.roots,
            ModifiedFilePolicy::Ask,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(report.restored.len(), 1);
    assert_eq!(
        world.read_game_file("contested.dat").unwrap(),
        b"from mod a",
        "the overridden mod's file must come back"
    );
    assert!(world
        .db
        .stack(world.local_game, &t)
        .await
        .unwrap()
        .claiming_installations()
        .any(|i| i == first));
}

#[tokio::test]
async fn one_decision_can_resolve_every_equivalent_conflict() {
    let world = World::new().await;
    world.write_unmanaged("a.dat", b"original a");
    world.write_unmanaged("b.dat", b"original b");

    let (mut plan, staging) = plan(
        &world,
        "bulk",
        world.mod_id,
        InstallationId::new(),
        &[("a.dat", b"new a"), ("b.dat", b"new b")],
        &[],
    )
    .await;
    assert_eq!(plan.unresolved().count(), 2);

    let t = plan.files[0].target.clone();
    let resolved = plan.apply_decision(
        &t,
        &Decision {
            choice: ConflictChoice::ReplaceAfterBackup,
            scope: DecisionScope::EquivalentInThisOperation {
                classification: FileClassification::UnmanagedExisting,
            },
        },
    );
    assert_eq!(resolved, 2);
    assert!(plan.is_ready());

    world
        .installer()
        .apply(
            &plan,
            &staging,
            &world.roots,
            world.release,
            world.archive,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(world.read_game_file("a.dat").unwrap(), b"new a");
    assert_eq!(world.read_game_file("b.dat").unwrap(), b"new b");
}

#[tokio::test]
async fn a_remembered_rule_pre_resolves_matching_conflicts_only() {
    let world = World::new().await;
    world.write_unmanaged("archive/x.dat", b"original");
    world.write_unmanaged("bin/y.dll", b"original");

    let rule = ScopedRule {
        mod_id: world.mod_id,
        root_key: "game".into(),
        path_prefix: "archive/".into(),
        choice: ConflictChoice::ReplaceAfterBackup,
    };
    let (plan, _) = plan(
        &world,
        "ruled",
        world.mod_id,
        InstallationId::new(),
        &[("archive/x.dat", b"new"), ("bin/y.dll", b"new")],
        std::slice::from_ref(&rule),
    )
    .await;

    let archive_file = plan
        .files
        .iter()
        .find(|f| f.target.path.as_str() == "archive/x.dat")
        .unwrap();
    let bin_file = plan
        .files
        .iter()
        .find(|f| f.target.path.as_str() == "bin/y.dll")
        .unwrap();
    assert_eq!(
        archive_file.decision,
        Some(ConflictChoice::ReplaceAfterBackup)
    );
    assert_eq!(
        bin_file.decision, None,
        "the rule must not reach outside its prefix"
    );
    assert!(!plan.is_ready());
}

#[tokio::test]
async fn aborting_a_conflict_refuses_the_whole_plan() {
    let world = World::new().await;
    world.write_unmanaged("a.dat", b"original");
    let (mut plan, staging) = plan(
        &world,
        "abort",
        world.mod_id,
        InstallationId::new(),
        &[("a.dat", b"new")],
        &[],
    )
    .await;
    let t = plan.files[0].target.clone();
    plan.apply_decision(
        &t,
        &Decision {
            choice: ConflictChoice::Abort,
            scope: DecisionScope::ThisFile,
        },
    );

    let err = world
        .installer()
        .apply(
            &plan,
            &staging,
            &world.roots,
            world.release,
            world.archive,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::DecisionRequired(_)), "{err:?}");
    assert_eq!(world.read_game_file("a.dat").unwrap(), b"original");
}

#[tokio::test]
async fn applying_a_plan_with_open_conflicts_is_refused() {
    let world = World::new().await;
    world.write_unmanaged("a.dat", b"original");
    let (plan, staging) = plan(
        &world,
        "open",
        world.mod_id,
        InstallationId::new(),
        &[("a.dat", b"new")],
        &[],
    )
    .await;

    let err = world
        .installer()
        .apply(
            &plan,
            &staging,
            &world.roots,
            world.release,
            world.archive,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::DecisionRequired(_)), "{err:?}");
}

// ---------------------------------------------------------------------------
// Interruption, rollback and recovery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_failed_rename_rolls_the_whole_install_back() {
    let world = World::new().await;
    world.write_unmanaged("existing.dat", b"do not lose me");

    let (mut plan, staging) = plan(
        &world,
        "fail",
        world.mod_id,
        InstallationId::new(),
        &[
            ("a.dat", b"first"),
            ("existing.dat", b"second"),
            ("c.dat", b"third"),
        ],
        &[],
    )
    .await;
    let t = plan
        .files
        .iter()
        .find(|f| f.target.path.as_str() == "existing.dat")
        .unwrap()
        .target
        .clone();
    plan.apply_decision(
        &t,
        &Decision {
            choice: ConflictChoice::ReplaceAfterBackup,
            scope: DecisionScope::ThisFile,
        },
    );

    // Fail the second rename: one file is already swapped, one is mid-flight.
    let fs = Arc::new(FaultyFileSystem::new(FailAt::Rename(1)));
    let err = world
        .installer_with(fs)
        .apply(
            &plan,
            &staging,
            &world.roots,
            world.release,
            world.archive,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("injected rename failure"),
        "{err}"
    );

    // Everything must be back as it was.
    assert!(
        !world.game_file_exists("a.dat"),
        "a committed file was not rolled back"
    );
    assert!(!world.game_file_exists("c.dat"));
    assert_eq!(
        world.read_game_file("existing.dat").unwrap(),
        b"do not lose me",
        "the pre-existing file must survive a failed install"
    );

    let interrupted = world.db.interrupted().await.unwrap();
    assert!(
        interrupted.is_empty(),
        "the rollback should have reached a terminal state"
    );
    let op = world.db.get(plan.operation_id).await.unwrap().unwrap();
    assert_eq!(op.state, OperationState::RolledBack);
}

#[tokio::test]
async fn a_failed_staging_write_leaves_the_game_untouched() {
    let world = World::new().await;
    let (plan, staging) = plan(
        &world,
        "stagefail",
        world.mod_id,
        InstallationId::new(),
        &[("a.dat", b"first"), ("b.dat", b"second")],
        &[],
    )
    .await;

    let fs = Arc::new(FaultyFileSystem::new(FailAt::TempWrite(1)));
    let err = world
        .installer_with(fs)
        .apply(
            &plan,
            &staging,
            &world.roots,
            world.release,
            world.archive,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("injected staging failure"),
        "{err}"
    );

    // Staging failures happen before any rename, so no target exists yet.
    assert!(!world.game_file_exists("a.dat"));
    assert!(!world.game_file_exists("b.dat"));
    let leftovers: Vec<_> = walk(&world.game_dir)
        .into_iter()
        .filter(|p| p.to_string_lossy().contains(onera_install::fs::TEMP_SUFFIX))
        .collect();
    assert!(
        leftovers.is_empty(),
        "staged temp files were not cleaned up: {leftovers:?}"
    );
}

#[tokio::test]
async fn an_interrupted_operation_is_found_and_can_be_rolled_back_on_restart() {
    let world = World::new().await;
    let installation = InstallationId::new();
    let (plan, staging) = plan(
        &world,
        "crash",
        world.mod_id,
        installation,
        &[("a.dat", b"x")],
        &[],
    )
    .await;

    // Simulate a crash: journal the plan and prepare, then stop.
    let installer = world.installer();
    let op = world
        .db
        .begin(&plan, onera_core::domain::operation::OperationKind::Install)
        .await
        .unwrap();
    world
        .db
        .set_state(op.id, OperationState::Prepared, None)
        .await
        .unwrap();
    let _ = staging;

    let interrupted = recover_all(&installer).await.unwrap();
    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0].operation.id, op.id);
    assert_eq!(
        interrupted[0].recovery,
        onera_core::domain::operation::Recovery::ContinueOrRollBack
    );

    onera_install::recovery::apply_choice(
        &installer,
        op.id,
        onera_install::RecoveryChoice::RollBack,
        &NullProgress,
    )
    .await
    .unwrap();
    assert!(recover_all(&installer).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_plan_that_never_ran_is_discarded_rather_than_rolled_back() {
    let world = World::new().await;
    let (plan, _) = plan(
        &world,
        "never",
        world.mod_id,
        InstallationId::new(),
        &[("a.dat", b"x")],
        &[],
    )
    .await;
    let installer = world.installer();
    let op = world
        .db
        .begin(&plan, onera_core::domain::operation::OperationKind::Install)
        .await
        .unwrap();

    let interrupted = recover_all(&installer).await.unwrap();
    assert_eq!(
        interrupted[0].recovery,
        onera_core::domain::operation::Recovery::DiscardPlan
    );
    installer.rollback(op.id, &NullProgress).await.unwrap();
    assert_eq!(
        world.db.get(op.id).await.unwrap().unwrap().state,
        OperationState::RolledBack
    );
}

#[tokio::test]
async fn cancelling_during_planning_stops_cleanly() {
    let world = World::new().await;
    let (staging, manifest) = world.stage("cancel", &[("a.dat", b"x")]);
    let _ = staging;
    let adapter = FlatAdapter;
    let layout = adapter.resolve_layout(&manifest).unwrap();
    let cancel = CancelToken::new();
    cancel.cancel();

    let err = plan_install(
        PlanRequest {
            local_game_id: world.local_game,
            mod_id: world.mod_id,
            installation_id: InstallationId::new(),
            manifest: &manifest,
            mappings: &layout.mappings,
            roots: &world.roots,
            adapter: &adapter,
            rules: &[],
        },
        &onera_install::RealFileSystem,
        &world.db,
        &NullProgress,
        &cancel,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CoreError::Cancelled));
}

// ---------------------------------------------------------------------------
// Verification and repair
// ---------------------------------------------------------------------------

#[tokio::test]
async fn verification_passes_on_a_clean_install() {
    let world = World::new().await;
    let installation = install(
        &world,
        "v",
        world.mod_id,
        world.release,
        &[("a.dat", b"a"), ("b.dat", b"b")],
    )
    .await;

    let report = verify_installation(
        world.local_game,
        installation,
        &world.roots,
        &onera_install::RealFileSystem,
        &world.db,
        &NullProgress,
        &CancelToken::new(),
    )
    .await
    .unwrap();
    assert!(report.is_clean());
    assert_eq!(report.files.len(), 2);
}

#[tokio::test]
async fn verification_reports_modified_and_missing_files() {
    let world = World::new().await;
    let installation = install(
        &world,
        "v",
        world.mod_id,
        world.release,
        &[
            ("ok.dat", b"ok"),
            ("edited.dat", b"original"),
            ("deleted.dat", b"gone soon"),
        ],
    )
    .await;

    world.write_unmanaged("edited.dat", b"tampered with");
    std::fs::remove_file(world.game_dir.join("deleted.dat")).unwrap();

    let report = verify_installation(
        world.local_game,
        installation,
        &world.roots,
        &onera_install::RealFileSystem,
        &world.db,
        &NullProgress,
        &CancelToken::new(),
    )
    .await
    .unwrap();

    assert!(!report.is_clean());
    assert_eq!(report.problems().count(), 2);
    let counts = report.counts();
    assert_eq!(counts.get("Modified"), Some(&1));
    assert_eq!(counts.get("Missing"), Some(&1));
    assert_eq!(counts.get("Ok"), Some(&1));
}

// ---------------------------------------------------------------------------
// Removal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn removal_deletes_files_and_the_directories_it_created() {
    let world = World::new().await;
    let installation = install(
        &world,
        "r",
        world.mod_id,
        world.release,
        &[("deep/nested/a.dat", b"a")],
    )
    .await;
    assert!(world.game_dir.join("deep/nested").is_dir());

    let report = world
        .remover()
        .remove(
            world.local_game,
            installation,
            &world.roots,
            ModifiedFilePolicy::Ask,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(report.deleted.len(), 1);
    assert!(!world.game_file_exists("deep/nested/a.dat"));
    assert!(
        !world.game_dir.join("deep/nested").exists(),
        "an emptied directory should be removed"
    );
}

#[tokio::test]
async fn removal_keeps_directories_that_still_hold_user_files() {
    let world = World::new().await;
    let installation = install(
        &world,
        "r",
        world.mod_id,
        world.release,
        &[("shared_dir/a.dat", b"a")],
    )
    .await;
    world.write_unmanaged("shared_dir/users_own_file.txt", b"mine");

    world
        .remover()
        .remove(
            world.local_game,
            installation,
            &world.roots,
            ModifiedFilePolicy::Ask,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();

    assert!(
        world.game_dir.join("shared_dir").is_dir(),
        "a directory with user files must survive"
    );
    assert_eq!(
        world
            .read_game_file("shared_dir/users_own_file.txt")
            .unwrap(),
        b"mine"
    );
}

#[tokio::test]
async fn removal_tolerates_files_the_user_already_deleted() {
    let world = World::new().await;
    let installation = install(
        &world,
        "r",
        world.mod_id,
        world.release,
        &[("a.dat", b"a"), ("b.dat", b"b")],
    )
    .await;
    std::fs::remove_file(world.game_dir.join("a.dat")).unwrap();

    let report = world
        .remover()
        .remove(
            world.local_game,
            installation,
            &world.roots,
            ModifiedFilePolicy::Ask,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(report.already_missing.len(), 1);
    assert_eq!(report.deleted.len(), 1);
}

#[tokio::test]
async fn removal_refuses_to_touch_externally_modified_files() {
    let world = World::new().await;
    let installation = install(
        &world,
        "r",
        world.mod_id,
        world.release,
        &[("a.dat", b"original")],
    )
    .await;
    world.write_unmanaged("a.dat", b"the user changed this");

    let err = world
        .remover()
        .remove(
            world.local_game,
            installation,
            &world.roots,
            ModifiedFilePolicy::Ask,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::DecisionRequired(_)), "{err:?}");
    assert_eq!(
        world.read_game_file("a.dat").unwrap(),
        b"the user changed this"
    );

    // With an explicit decision to keep them, the file stays but the claim goes.
    let report = world
        .remover()
        .remove(
            world.local_game,
            installation,
            &world.roots,
            ModifiedFilePolicy::Keep,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(report.externally_modified.len(), 1);
    assert_eq!(
        world.read_game_file("a.dat").unwrap(),
        b"the user changed this"
    );
}

#[tokio::test]
async fn a_removal_preview_changes_nothing() {
    let world = World::new().await;
    let installation = install(&world, "r", world.mod_id, world.release, &[("a.dat", b"a")]).await;

    let preview: RemovalReport = world
        .remover()
        .preview(world.local_game, installation, &world.roots)
        .await
        .unwrap();
    assert_eq!(preview.deleted.len(), 1);
    assert!(
        world.game_file_exists("a.dat"),
        "a preview must not delete anything"
    );
}

#[tokio::test]
async fn install_remove_reinstall_round_trips() {
    let world = World::new().await;
    let files: &[(&str, &[u8])] = &[("a.dat", b"a"), ("deep/b.dat", b"b")];

    let first = install(&world, "one", world.mod_id, world.release, files).await;
    world
        .remover()
        .remove(
            world.local_game,
            first,
            &world.roots,
            ModifiedFilePolicy::Ask,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert!(!world.game_file_exists("a.dat"));
    assert!(world
        .db
        .all_targets(world.local_game)
        .await
        .unwrap()
        .is_empty());

    let second = install(&world, "two", world.mod_id, world.release, files).await;
    assert_eq!(world.read_game_file("a.dat").unwrap(), b"a");
    assert_eq!(world.read_game_file("deep/b.dat").unwrap(), b"b");
    assert_ne!(first, second);
}

#[tokio::test]
async fn progress_is_streamed_for_a_long_install() {
    let world = World::new().await;
    let installation = InstallationId::new();
    let (plan, staging) = plan(
        &world,
        "progress",
        world.mod_id,
        installation,
        &[("a.dat", b"a"), ("b.dat", b"b"), ("c.dat", b"c")],
        &[],
    )
    .await;

    let sink = RecordingProgress::default();
    world
        .installer()
        .apply(
            &plan,
            &staging,
            &world.roots,
            world.release,
            world.archive,
            &sink,
            &CancelToken::new(),
        )
        .await
        .unwrap();

    let events = sink.events();
    let deploying = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                onera_core::progress::ProgressEvent::Advanced {
                    stage: onera_core::progress::Stage::Deploying,
                    ..
                }
            )
        })
        .count();
    assert_eq!(deploying, 3, "one advance per deployed file: {events:?}");
}

fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}
