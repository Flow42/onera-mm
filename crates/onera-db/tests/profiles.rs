//! Profile-store integration tests.

use chrono::{DateTime, Utc};
use onera_core::domain::archive::ArchiveFormat;
use onera_core::domain::game::{Game, InstallSource, LocalGameInstall};
use onera_core::domain::operation::{OperationKind, OperationState};
use onera_core::domain::profile::{
    validate_profile_set, DesiredModState, MemberPin, MemberPriority, MemberSelection, Profile,
    ProfileActivation, ProfileActivationState, ProfileMember, DEFAULT_PROFILE_NAME,
};
use onera_core::domain::reconcile::{DesiredGameState, MutationPlan};
use onera_core::domain::release::{FileCategory, Mod, ProviderFile, Release};
use onera_core::hash::FileHash;
use onera_core::ids::*;
use onera_core::ports::{DeploymentStore, OperationJournal, ProfileStore, ReconciliationStore};
use onera_core::CoreError;
use onera_db::Database;

struct Fixture {
    db: Database,
    game: LocalGameId,
    other_game: LocalGameId,
    default_profile: Profile,
    other_profile: Profile,
    mod_id: ModId,
    installation: InstallationId,
}

async fn fixture() -> Fixture {
    let db = Database::open_in_memory().await.unwrap();
    let provider = ProviderId::nexus();
    db.upsert_provider(&provider, "Nexus", "https://example.invalid")
        .await
        .unwrap();
    let catalogue_game = db
        .upsert_game(&Game {
            id: GameId::new(),
            provider: provider.clone(),
            provider_slug: "test".into(),
            name: "Test Game".into(),
            steam_app_id: None,
        })
        .await
        .unwrap();

    let local = |path: &str| LocalGameInstall {
        id: LocalGameId::new(),
        game_id: catalogue_game,
        adapter_id: "test".into(),
        source: InstallSource::Manual,
        install_root: path.into(),
        compat_prefix: None,
        user_data_roots: vec![],
        confirmed: true,
    };
    let game = db.upsert_local_install(&local("/games/one")).await.unwrap();
    db.confirm_local_install(game).await.unwrap();
    db.confirm_local_install(game).await.unwrap();
    let other_game = db.upsert_local_install(&local("/games/two")).await.unwrap();
    db.confirm_local_install(other_game).await.unwrap();

    let default_profile = db.active_profile(game).await.unwrap().unwrap();
    let other_profile = db.active_profile(other_game).await.unwrap().unwrap();
    assert_eq!(default_profile.name, DEFAULT_PROFILE_NAME);
    assert_eq!(db.profiles(game).await.unwrap().len(), 1);

    let mod_id = db
        .upsert_mod(&Mod {
            id: ModId::new(),
            provider: provider.clone(),
            provider_mod_id: ProviderModId::new("42"),
            game_slug: "test".into(),
            name: "Example".into(),
            author: None,
        })
        .await
        .unwrap();
    let release = db
        .upsert_release(&Release {
            id: ReleaseId::new(),
            mod_id,
            version: "1.0".into(),
            published_at: DateTime::from_timestamp(1_700_000_000, 0),
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();
    let provider_file = ProviderFileId::new("file-42");
    db.upsert_provider_file(&ProviderFile {
        provider: provider.clone(),
        provider_file_id: provider_file.clone(),
        release_id: release,
        name: "example.zip".into(),
        size_bytes: Some(4),
        category: FileCategory::Main,
        published_hash: None,
        uploaded_at: None,
        is_primary: true,
    })
    .await
    .unwrap();
    sqlx::query(
        "UPDATE provider_files SET provider_version_id = 'opaque-version',
         provider_file_group_id = 'opaque-group'
         WHERE provider_id = 'nexus' AND provider_file_id = 'file-42'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let archive = db
        .upsert_archive(
            &FileHash::blake3_of(b"data"),
            4,
            "example.zip",
            ArchiveFormat::Zip,
            std::path::Path::new("/archives/example.zip"),
        )
        .await
        .unwrap();
    db.link_archive_provider_file(archive, &provider, &provider_file)
        .await
        .unwrap();
    let installation = InstallationId::new();
    db.record_installation(installation, game, mod_id, release, archive)
        .await
        .unwrap();

    Fixture {
        db,
        game,
        other_game,
        default_profile,
        other_profile,
        mod_id,
        installation,
    }
}

fn profile(game: LocalGameId, name: &str) -> Profile {
    let now = Utc::now();
    Profile {
        id: ProfileId::new(),
        local_game_id: game,
        name: name.into(),
        description: None,
        is_active: false,
        created_at: now,
        updated_at: now,
    }
}

#[tokio::test]
async fn default_creation_crud_names_and_active_guard() {
    let f = fixture().await;
    let alternate = profile(f.game, "Alternate");
    f.db.put_profile(&alternate).await.unwrap();

    let duplicate = profile(f.game, "alternate");
    assert!(matches!(
        f.db.put_profile(&duplicate).await.unwrap_err(),
        CoreError::Conflict(_)
    ));
    assert!(matches!(
        f.db.delete_profile(f.default_profile.id).await.unwrap_err(),
        CoreError::Conflict(_)
    ));

    f.db.set_active_profile(f.game, alternate.id).await.unwrap();
    assert_eq!(
        f.db.active_profile(f.game).await.unwrap().unwrap().id,
        alternate.id
    );
    f.db.delete_profile(f.default_profile.id).await.unwrap();
    let remaining = f.db.profiles(f.game).await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, alternate.id);
    assert!(remaining[0].is_active);

    assert!(matches!(
        f.db.set_active_profile(
            f.other_game,
            f.db.active_profile(f.game).await.unwrap().unwrap().id
        )
        .await
        .unwrap_err(),
        CoreError::Conflict(_)
    ));
}

#[tokio::test]
async fn members_order_pin_toggle_and_cascade_overrides() {
    let f = fixture().await;
    let (selection, retained) =
        f.db.selection_for_profile_member(
            f.default_profile.id,
            f.mod_id,
            Some(&ProviderFileId::new("file-42")),
        )
        .await
        .unwrap();
    assert_eq!(retained, Some(f.installation));
    assert_eq!(
        selection.provider_version_id.as_ref().unwrap().as_str(),
        "opaque-version"
    );
    assert_eq!(
        selection.provider_file_group_id.as_ref().unwrap().as_str(),
        "opaque-group"
    );
    let now = Utc::now();
    let first = ProfileMember {
        id: ProfileMemberId::new(),
        profile_id: f.default_profile.id,
        mod_id: f.mod_id,
        selection,
        installation_id: retained,
        desired: DesiredModState::Disabled,
        pin: MemberPin::Pinned {
            pinned_at: now,
            reason: Some("known good".into()),
        },
        priority: MemberPriority(50),
        added_at: now,
    };
    f.db.put_member(&first).await.unwrap();

    let second_mod = ModId::new();
    sqlx::query(
        "INSERT INTO mods
         (id, provider_id, provider_mod_id, game_slug, name, author, updated_at)
         VALUES (?1, 'nexus', '43', 'test', 'Second', NULL, ?2)",
    )
    .bind(second_mod.to_string())
    .bind(now.to_rfc3339())
    .execute(f.db.pool())
    .await
    .unwrap();
    let second = ProfileMember {
        id: ProfileMemberId::new(),
        profile_id: f.default_profile.id,
        mod_id: second_mod,
        selection: MemberSelection::unresolved(ProviderId::nexus(), ProviderModId::new("43")),
        installation_id: None,
        desired: DesiredModState::Enabled,
        pin: MemberPin::Unpinned,
        priority: MemberPriority(-5),
        added_at: now,
    };
    f.db.put_member(&second).await.unwrap();
    assert_eq!(
        f.db.members(f.default_profile.id)
            .await
            .unwrap()
            .into_iter()
            .map(|member| member.id)
            .collect::<Vec<_>>(),
        vec![second.id, first.id]
    );

    // This mirrors migration 0007's FK contract without making migration 0006
    // depend on dependency tables that do not exist yet.
    sqlx::query(
        "CREATE TABLE test_dependency_overrides (
            member_id TEXT NOT NULL REFERENCES profile_members(id) ON DELETE CASCADE,
            fingerprint TEXT NOT NULL
         ) STRICT",
    )
    .execute(f.db.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO test_dependency_overrides VALUES (?1, 'fingerprint')")
        .bind(first.id.to_string())
        .execute(f.db.pool())
        .await
        .unwrap();
    f.db.remove_member(first.id).await.unwrap();
    let count: (i64,) = sqlx::query_as("SELECT count(*) FROM test_dependency_overrides")
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    assert_eq!(count.0, 0);
}

#[tokio::test]
async fn cross_game_installations_are_rejected_by_store_and_schema() {
    let f = fixture().await;
    let member = ProfileMember {
        id: ProfileMemberId::new(),
        profile_id: f.other_profile.id,
        mod_id: f.mod_id,
        selection: MemberSelection::unresolved(ProviderId::nexus(), ProviderModId::new("42")),
        installation_id: Some(f.installation),
        desired: DesiredModState::Enabled,
        pin: MemberPin::Unpinned,
        priority: MemberPriority(10),
        added_at: Utc::now(),
    };
    assert!(matches!(
        f.db.put_member(&member).await.unwrap_err(),
        CoreError::Conflict(_)
    ));

    let raw = sqlx::query(
        "INSERT INTO profile_members
         (id, profile_id, mod_id, provider_id, provider_mod_id,
          installation_id, desired, pinned, priority, added_at)
         VALUES (?1, ?2, ?3, 'nexus', '42', ?4, 'enabled', 0, 10, ?5)",
    )
    .bind(ProfileMemberId::new().to_string())
    .bind(f.other_profile.id.to_string())
    .bind(f.mod_id.to_string())
    .bind(f.installation.to_string())
    .bind(Utc::now().to_rfc3339())
    .execute(f.db.pool())
    .await;
    assert!(raw.is_err());
}

#[tokio::test]
async fn activation_history_round_trips_newest_first() {
    let f = fixture().await;
    let target = profile(f.game, "Target");
    f.db.put_profile(&target).await.unwrap();
    let started = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let mut activation = ProfileActivation {
        from_profile_id: Some(f.default_profile.id),
        to_profile_id: target.id,
        operation_id: None,
        state: ProfileActivationState::Preparing,
        started_at: started,
        finished_at: None,
        error: None,
    };
    f.db.record_activation(&activation).await.unwrap();
    activation.state = ProfileActivationState::Failed;
    activation.finished_at = Some(started + chrono::Duration::seconds(1));
    activation.error = Some("not applied".into());
    f.db.record_activation(&activation).await.unwrap();
    assert_eq!(
        f.db.activation_history(f.game, 10).await.unwrap(),
        vec![activation]
    );
}

// ---------------------------------------------------------------------------
// Activation publication
// ---------------------------------------------------------------------------

/// An empty reconciliation for one game: enough to drive the completion
/// transaction without needing a filesystem.
fn empty_plan(game: LocalGameId) -> MutationPlan {
    onera_core::domain::reconcile::reconcile(
        DesiredGameState::new(game, vec![]),
        &std::collections::BTreeMap::new(),
        &[],
    )
}

/// Advance an operation to the only state the completion transaction accepts.
async fn committing(db: &Database, plan: &MutationPlan) -> OperationId {
    let operation = db
        .begin_reconciliation(plan, OperationKind::Reconcile)
        .await
        .unwrap();
    db.set_state(operation.id, OperationState::Prepared, None)
        .await
        .unwrap();
    db.set_state(operation.id, OperationState::Committing, None)
        .await
        .unwrap();
    operation.id
}

fn attempt(
    from: ProfileId,
    to: ProfileId,
    state: ProfileActivationState,
    started_at: DateTime<Utc>,
) -> ProfileActivation {
    ProfileActivation {
        from_profile_id: Some(from),
        to_profile_id: to,
        operation_id: None,
        state,
        started_at,
        finished_at: None,
        error: None,
    }
}

#[tokio::test]
async fn an_acquired_artifact_is_retained_without_disturbing_the_deployment() {
    let f = fixture().await;
    let retained = InstallationId::new();
    f.db.record_retained_installation(
        retained,
        f.game,
        f.mod_id,
        release_of(&f.db, f.installation).await,
        archive_of(&f.db, f.installation).await,
    )
    .await
    .unwrap();

    // The artifact exists and is addressable...
    assert!(f
        .db
        .archive_for_installation(f.game, retained)
        .await
        .unwrap()
        .is_some());
    // ...but preparing a profile changed nothing about what is deployed. The
    // previously active artifact keeps its unique active slot.
    assert_eq!(
        f.db.active_installations(f.game).await.unwrap(),
        vec![f.installation]
    );
}

#[tokio::test]
async fn the_profile_switch_commits_with_the_deployment_it_describes() {
    let f = fixture().await;
    let target = f.db.put_profile(&profile(f.game, "Modded")).await;
    assert!(target.is_ok());
    let target =
        f.db.profiles(f.game)
            .await
            .unwrap()
            .into_iter()
            .find(|p| p.name == "Modded")
            .unwrap();

    let started = DateTime::from_timestamp(1_700_000_100, 0).unwrap();
    f.db.record_activation(&attempt(
        f.default_profile.id,
        target.id,
        ProfileActivationState::Applying,
        started,
    ))
    .await
    .unwrap();

    let plan = empty_plan(f.game);
    let operation = committing(&f.db, &plan).await;
    f.db.complete_reconciliation_publishing(operation, &plan, Some(target.id))
        .await
        .unwrap();

    // One active profile, and it is the target.
    let profiles = f.db.profiles(f.game).await.unwrap();
    assert!(validate_profile_set(&profiles).is_ok());
    assert_eq!(
        f.db.active_profile(f.game).await.unwrap().unwrap().id,
        target.id
    );
    // The attempt was finished by the same transaction, carrying its operation.
    let history = f.db.activation_history(f.game, 10).await.unwrap();
    assert_eq!(history[0].state, ProfileActivationState::Applied);
    assert_eq!(history[0].operation_id, Some(operation));
    assert!(history[0].finished_at.is_some());
    assert!(f.db.interrupted_activations().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_refused_completion_leaves_the_old_profile_active() {
    let f = fixture().await;
    f.db.put_profile(&profile(f.game, "Modded")).await.unwrap();
    let target =
        f.db.profiles(f.game)
            .await
            .unwrap()
            .into_iter()
            .find(|p| p.name == "Modded")
            .unwrap();
    let plan = empty_plan(f.game);

    // An operation that never reached `committing` cannot publish anything...
    let operation =
        f.db.begin_reconciliation(&plan, OperationKind::Reconcile)
            .await
            .unwrap();
    assert!(matches!(
        f.db.complete_reconciliation_publishing(operation.id, &plan, Some(target.id))
            .await,
        Err(CoreError::Conflict(_))
    ));

    // ...and neither can one asked to activate another game's profile.
    let committing_id = committing(&f.db, &plan).await;
    assert!(matches!(
        f.db.complete_reconciliation_publishing(committing_id, &plan, Some(f.other_profile.id))
            .await,
        Err(CoreError::Conflict(_))
    ));

    assert_eq!(
        f.db.active_profile(f.game).await.unwrap().unwrap().id,
        f.default_profile.id
    );
    // The whole transaction rolled back, operation state included.
    assert_eq!(
        f.db.get(committing_id).await.unwrap().unwrap().state,
        OperationState::Committing
    );
}

#[tokio::test]
async fn only_unfinished_attempts_are_offered_for_recovery() {
    let f = fixture().await;
    f.db.put_profile(&profile(f.game, "Modded")).await.unwrap();
    let target =
        f.db.profiles(f.game)
            .await
            .unwrap()
            .into_iter()
            .find(|p| p.name == "Modded")
            .unwrap();
    let at = |offset: i64| DateTime::from_timestamp(1_700_000_000 + offset, 0).unwrap();

    for (offset, state) in [
        (1, ProfileActivationState::Preparing),
        (2, ProfileActivationState::Applying),
        (3, ProfileActivationState::Applied),
        (4, ProfileActivationState::RolledBack),
        (5, ProfileActivationState::Failed),
    ] {
        f.db.record_activation(&attempt(f.default_profile.id, target.id, state, at(offset)))
            .await
            .unwrap();
    }

    let interrupted = f.db.interrupted_activations().await.unwrap();
    assert_eq!(interrupted.len(), 2);
    assert_eq!(interrupted[0].state, ProfileActivationState::Preparing);
    assert_eq!(interrupted[1].state, ProfileActivationState::Applying);
    // Recovery never reads a terminal record as unfinished, and never makes one
    // active on its own.
    assert_eq!(
        f.db.active_profile(f.game).await.unwrap().unwrap().id,
        f.default_profile.id
    );
}

async fn release_of(db: &Database, installation: InstallationId) -> ReleaseId {
    let (id,): (String,) = sqlx::query_as("SELECT release_id FROM installations WHERE id = ?1")
        .bind(installation.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    ReleaseId::from(uuid::Uuid::parse_str(&id).unwrap())
}

async fn archive_of(db: &Database, installation: InstallationId) -> ArchiveId {
    let (id,): (String,) = sqlx::query_as("SELECT archive_id FROM installations WHERE id = ?1")
        .bind(installation.to_string())
        .fetch_one(db.pool())
        .await
        .unwrap();
    ArchiveId::from(uuid::Uuid::parse_str(&id).unwrap())
}
