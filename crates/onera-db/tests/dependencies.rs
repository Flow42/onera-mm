//! Dependency-store and provider-identity integration tests.

use chrono::{DateTime, Utc};
use onera_core::domain::dependency::{
    CandidateStatus, DependencyAvailability, DependencyCandidate, DependencyFingerprint,
    DependencyGroup, DependencyOverride, DependencySnapshot, DependencySource, DlcRequirement,
    RequirementKind,
};
use onera_core::domain::game::Game;
use onera_core::domain::release::{FileCategory, Mod, ProviderFile, Release};
use onera_core::ids::*;
use onera_core::ports::DependencyStore;
use onera_core::CoreError;
use onera_db::Database;

fn at(seconds: i64, nanos: u32) -> DateTime<Utc> {
    DateTime::from_timestamp(seconds, nanos).unwrap()
}

fn source(mod_id: &str) -> DependencySource {
    DependencySource {
        provider: ProviderId::nexus(),
        game_slug: "test-game".into(),
        provider_mod_id: ProviderModId::new(mod_id),
        provider_file_id: Some(ProviderFileId::new(format!("file-{mod_id}"))),
        provider_version_id: Some(ProviderVersionId::new(format!("version-{mod_id}"))),
    }
}

fn candidate() -> DependencyCandidate {
    DependencyCandidate {
        provider: ProviderId::nexus(),
        game_slug: "test-game".into(),
        provider_mod_id: ProviderModId::new("required-mod"),
        provider_file_id: Some(ProviderFileId::new("candidate-file")),
        provider_version_id: Some(ProviderVersionId::new("candidate-version")),
        provider_file_group_id: Some(ProviderFileGroupId::new("candidate-chain")),
        position: Some(42),
        status: CandidateStatus::Available,
        display_name: Some("Required Mod 2.0".into()),
    }
}

fn group(key: &str) -> DependencyGroup {
    DependencyGroup {
        id: DependencyGroupId::new(),
        provider_group_key: Some(key.into()),
        label: Some("Required Mod".into()),
        kind: RequirementKind::Required,
        candidates: vec![candidate()],
    }
}

async fn database() -> Database {
    let db = Database::open_in_memory().await.unwrap();
    db.upsert_provider(&ProviderId::nexus(), "Nexus", "https://example.invalid")
        .await
        .unwrap();
    db
}

#[tokio::test]
async fn availability_states_and_ordered_batch_misses_round_trip() {
    let db = database().await;
    let fetched = DependencySnapshot::fetched(source("fetched"), vec![], vec![], at(10, 1));
    let unavailable = DependencySnapshot::unavailable(source("unavailable"), "offline", at(20, 2));
    let unsupported = DependencySnapshot::unsupported(source("unsupported"), at(30, 3));
    let mut cached = DependencySnapshot::fetched(source("cached"), vec![], vec![], at(40, 4));
    cached.availability = DependencyAvailability::Cached {
        fetched_at: at(35, 987_654_321),
        stale: false,
    };

    for snapshot in [&fetched, &unavailable, &unsupported, &cached] {
        db.put_snapshot(snapshot).await.unwrap();
    }

    let missing = source("missing");
    let stored = db
        .snapshots(&[
            cached.source.clone(),
            missing,
            fetched.source.clone(),
            unsupported.source.clone(),
            unavailable.source.clone(),
            cached.source.clone(),
        ])
        .await
        .unwrap();
    assert_eq!(
        stored,
        vec![
            Some(cached.clone()),
            None,
            Some(fetched.clone()),
            Some(unsupported.clone()),
            Some(unavailable.clone()),
            Some(cached.clone()),
        ]
    );
    assert!(stored[2].as_ref().unwrap().declares_no_dependencies());
    assert!(!stored[3].as_ref().unwrap().declares_no_dependencies());
    assert!(!stored[4].as_ref().unwrap().declares_no_dependencies());
    assert_eq!(db.snapshots(&[]).await.unwrap(), vec![]);

    let many_misses: Vec<_> = (0..205)
        .map(|index| source(&format!("batch-miss-{index}")))
        .collect();
    let many_stored = db.snapshots(&many_misses).await.unwrap();
    assert_eq!(many_stored.len(), many_misses.len());
    assert!(many_stored.iter().all(Option::is_none));
}

#[tokio::test]
async fn rich_snapshots_preserve_json_fingerprint_position_and_exact_stale_times() {
    let db = database().await;
    let requirement = group("requirement-1");
    let dlc = DlcRequirement {
        id: DependencyGroupId::new(),
        label: Some("Expansion".into()),
        alternatives: vec![StoreDlcId::new("dlc-b"), StoreDlcId::new("dlc-a")],
    };
    let fetched_at = at(1_700_000_000, 123_456_789);
    let cache_origin = at(1_699_999_000, 987_654_321);
    let mut snapshot =
        DependencySnapshot::fetched(source("rich"), vec![requirement], vec![dlc], fetched_at);
    snapshot.availability = DependencyAvailability::Cached {
        fetched_at: cache_origin,
        stale: true,
    };
    snapshot.provider_revision = Some("revision/opaque-7".into());
    snapshot.raw = serde_json::json!({
        "unknown_future_field": {"kept": [1, true, null]},
        "position": 42
    });

    db.put_snapshot(&snapshot).await.unwrap();
    let stored = db.snapshot(&snapshot.source).await.unwrap().unwrap();
    assert_eq!(stored, snapshot);
    assert_eq!(stored.fetched_at, fetched_at);
    assert_eq!(stored.groups[0].candidates[0].position, Some(42));
    assert!(stored.availability.is_stale());

    let mut replacement = DependencySnapshot::unavailable(
        snapshot.source.clone(),
        "endpoint retired",
        at(1_800_000_000, 321),
    );
    replacement.provider_revision = Some("revision/opaque-8".into());
    db.put_snapshot(&replacement).await.unwrap();
    assert_eq!(
        db.snapshot(&replacement.source).await.unwrap(),
        Some(replacement.clone())
    );
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM dependency_snapshots")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1, "source replacement must not keep history rows");
}

struct ProfileFixture {
    db: Database,
    first_profile: ProfileId,
    second_profile: ProfileId,
    first_member: ProfileMemberId,
    second_member: ProfileMemberId,
}

async fn profiles() -> ProfileFixture {
    let db = database().await;
    let game = GameId::new();
    let local = LocalGameId::new();
    let first_profile = ProfileId::new();
    let second_profile = ProfileId::new();
    let first_member = ProfileMemberId::new();
    let second_member = ProfileMemberId::new();
    let first_mod = ModId::new();
    let second_mod = ModId::new();
    let now = "2026-01-01T00:00:00Z";
    sqlx::query(
        "INSERT INTO games (id, provider_id, provider_slug, name, cached_at)
         VALUES (?1, 'nexus', 'test-game', 'Test Game', ?2)",
    )
    .bind(game.to_string())
    .bind(now)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO local_game_installs
            (id, game_id, adapter_id, source, install_root, user_data_roots, confirmed, created_at)
         VALUES (?1, ?2, 'test', 'manual', '/game', '[]', 1, ?3)",
    )
    .bind(local.to_string())
    .bind(game.to_string())
    .bind(now)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO mods (id, provider_id, provider_mod_id, game_slug, name, updated_at)
         VALUES (?1, 'nexus', 'one', 'test-game', 'One', ?3),
                (?2, 'nexus', 'two', 'test-game', 'Two', ?3)",
    )
    .bind(first_mod.to_string())
    .bind(second_mod.to_string())
    .bind(now)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO profiles (id, local_game_id, name, is_active, created_at, updated_at)
         VALUES (?1, ?3, 'First', 1, ?4, ?4), (?2, ?3, 'Second', 0, ?4, ?4)",
    )
    .bind(first_profile.to_string())
    .bind(second_profile.to_string())
    .bind(local.to_string())
    .bind(now)
    .execute(db.pool())
    .await
    .unwrap();
    for (member, profile, mod_id, provider_mod, priority) in [
        (first_member, first_profile, first_mod, "one", 10_i64),
        (second_member, second_profile, second_mod, "two", 20_i64),
    ] {
        sqlx::query(
            "INSERT INTO profile_members
                (id, profile_id, mod_id, provider_id, provider_mod_id,
                 desired, pinned, priority, added_at)
             VALUES (?1, ?2, ?3, 'nexus', ?4, 'enabled', 0, ?5, ?6)",
        )
        .bind(member.to_string())
        .bind(profile.to_string())
        .bind(mod_id.to_string())
        .bind(provider_mod)
        .bind(priority)
        .bind(now)
        .execute(db.pool())
        .await
        .unwrap();
    }
    ProfileFixture {
        db,
        first_profile,
        second_profile,
        first_member,
        second_member,
    }
}

fn fingerprint(key: &str) -> DependencyFingerprint {
    DependencyFingerprint::of(&[group(key)], &[])
}

#[tokio::test]
async fn overrides_are_profile_scoped_replace_fingerprints_and_cascade() {
    let f = profiles().await;
    let group_id = DependencyGroupId::new();
    let original = DependencyOverride {
        profile_member_id: f.first_member,
        fingerprint: fingerprint("old"),
        group_id,
        reason: "tested locally".into(),
        created_at: at(100, 111),
    };
    f.db.put_override(&original).await.unwrap();
    assert_eq!(
        f.db.overrides(f.first_profile).await.unwrap(),
        vec![original]
    );
    assert!(f.db.overrides(f.second_profile).await.unwrap().is_empty());

    let replacement = DependencyOverride {
        profile_member_id: f.first_member,
        fingerprint: fingerprint("changed"),
        group_id,
        reason: "accept changed definition".into(),
        created_at: at(200, 222),
    };
    f.db.put_override(&replacement).await.unwrap();
    assert_eq!(
        f.db.overrides(f.first_profile).await.unwrap(),
        vec![replacement.clone()]
    );
    f.db.delete_override(f.first_member, group_id)
        .await
        .unwrap();
    assert!(f.db.overrides(f.first_profile).await.unwrap().is_empty());

    f.db.put_override(&replacement).await.unwrap();
    let second = DependencyOverride {
        profile_member_id: f.second_member,
        fingerprint: fingerprint("second"),
        group_id: DependencyGroupId::new(),
        reason: "second profile only".into(),
        created_at: at(300, 333),
    };
    f.db.put_override(&second).await.unwrap();
    sqlx::query("DELETE FROM profile_members WHERE id = ?1")
        .bind(f.first_member.to_string())
        .execute(f.db.pool())
        .await
        .unwrap();
    assert!(f.db.overrides(f.first_profile).await.unwrap().is_empty());
    assert_eq!(
        f.db.overrides(f.second_profile).await.unwrap(),
        vec![second]
    );

    sqlx::query("DELETE FROM profiles WHERE id = ?1")
        .bind(f.second_profile.to_string())
        .execute(f.db.pool())
        .await
        .unwrap();
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM dependency_overrides")
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn provider_candidate_identity_and_unresolved_legacy_metadata_round_trip() {
    let db = database().await;
    let game = db
        .upsert_game(&Game {
            id: GameId::new(),
            provider: ProviderId::nexus(),
            provider_slug: "test-game".into(),
            name: "Test Game".into(),
            steam_app_id: None,
        })
        .await
        .unwrap();
    let mod_id = db
        .upsert_mod(&Mod {
            id: ModId::new(),
            provider: ProviderId::nexus(),
            provider_mod_id: ProviderModId::new("catalog-mod"),
            game_slug: "test-game".into(),
            name: "Catalog Mod".into(),
            author: None,
        })
        .await
        .unwrap();
    let release = db
        .upsert_release(&Release {
            id: ReleaseId::new(),
            mod_id,
            version: "free-form".into(),
            published_at: None,
            metadata: serde_json::Value::Null,
        })
        .await
        .unwrap();
    let resolved = ProviderFile {
        provider: ProviderId::nexus(),
        provider_file_id: ProviderFileId::new("resolved-file"),
        provider_version_id: Some(ProviderVersionId::new("opaque-version")),
        provider_file_group_id: Some(ProviderFileGroupId::new("opaque-chain")),
        position: Some(9_223_372_036_854_775_000),
        release_id: release,
        name: "resolved.zip".into(),
        size_bytes: Some(1),
        category: FileCategory::Main,
        published_hash: None,
        uploaded_at: None,
        is_primary: true,
    };
    let unresolved = ProviderFile {
        provider_file_id: ProviderFileId::new("legacy-file"),
        provider_version_id: None,
        provider_file_group_id: None,
        position: None,
        name: "legacy.zip".into(),
        is_primary: false,
        ..resolved.clone()
    };
    db.upsert_provider_file(&resolved).await.unwrap();
    db.upsert_provider_file(&unresolved).await.unwrap();

    assert_eq!(
        db.provider_file(&ProviderId::nexus(), &resolved.provider_file_id)
            .await
            .unwrap(),
        Some(resolved.clone())
    );
    assert_eq!(
        db.provider_file(&ProviderId::nexus(), &unresolved.provider_file_id)
            .await
            .unwrap(),
        Some(unresolved.clone())
    );
    let files = db.provider_files(release).await.unwrap();
    assert!(files.contains(&resolved));
    assert!(files.contains(&unresolved));
    let _ = game;
}

#[tokio::test]
async fn malformed_stored_snapshot_data_returns_a_typed_database_error() {
    let db = database().await;
    let snapshot = DependencySnapshot::fetched(source("corrupt"), vec![], vec![], at(1, 0));
    db.put_snapshot(&snapshot).await.unwrap();
    sqlx::query("UPDATE dependency_snapshots SET groups_json = '[{\"unexpected\":true}]'")
        .execute(db.pool())
        .await
        .unwrap();

    let error = db.snapshot(&snapshot.source).await.unwrap_err();
    assert!(matches!(error, CoreError::Database(_)));
    assert!(format!("{error}").contains("malformed stored dependency groups"));
}
