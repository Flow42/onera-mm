//! Baseline status, capture and verification through the application layer.
//!
//! The whole stack below `onera_app` is real: the real Steam manifest reader,
//! the real read-only scanner, the real SQLite schema. The "game" is a
//! temporary directory laid out the way Steam lays one out, and no network,
//! API key or keyring is involved — the provider and auth ports are stubs that
//! are never reached by anything here.

use async_trait::async_trait;
use onera_app::secrets::InMemorySecretStore;
use onera_app::{Onera, Paths};
use onera_core::domain::baseline::{
    BaselineFreshness, BaselineSource, BaselineStatus, FileClassification, ScanEvidence, ScanState,
};
use onera_core::domain::game::{Game, InstallSource};
use onera_core::ids::{LocalGameId, ProviderFileId, ProviderId, ProviderModId};
use onera_core::ports::{
    AccountInfo, AuthProvider, BaselineStore, Credential, DownloadTarget, ModProvider, Page,
};
use onera_core::progress::{CancelToken, NullProgress, RecordingProgress};
use onera_core::CoreError;
use onera_discovery::DiscoveredGame;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Stubs for the ports a baseline never touches
// ---------------------------------------------------------------------------

struct NoProvider;

#[async_trait]
impl ModProvider for NoProvider {
    fn id(&self) -> ProviderId {
        ProviderId::nexus()
    }
    async fn games(
        &self,
        _: Option<&str>,
        _: &CancelToken,
    ) -> onera_core::Result<Page<onera_core::domain::game::Game>> {
        Ok(Page::single(vec![]))
    }
    async fn mod_metadata(
        &self,
        _: &str,
        _: &ProviderModId,
        _: &CancelToken,
    ) -> onera_core::Result<(
        onera_core::domain::release::Mod,
        Vec<onera_core::domain::release::Release>,
    )> {
        Err(CoreError::Unsupported("no provider in this test".into()))
    }
    async fn files(
        &self,
        _: &str,
        _: &ProviderModId,
        _: Option<&str>,
        _: &CancelToken,
    ) -> onera_core::Result<Page<onera_core::domain::release::ProviderFile>> {
        Ok(Page::single(vec![]))
    }
    async fn resolve_download(
        &self,
        _: &str,
        _: &ProviderModId,
        _: &ProviderFileId,
        _: &CancelToken,
    ) -> onera_core::Result<DownloadTarget> {
        Err(CoreError::Unsupported("no provider in this test".into()))
    }
}

struct NoAuth;

#[async_trait]
impl AuthProvider for NoAuth {
    fn provider_id(&self) -> ProviderId {
        ProviderId::nexus()
    }
    async fn is_authenticated(&self) -> onera_core::Result<bool> {
        Ok(false)
    }
    async fn credential(&self) -> onera_core::Result<Credential> {
        Err(CoreError::Unauthenticated {
            provider: "nexus".into(),
        })
    }
    async fn validate(&self, _: &Credential) -> onera_core::Result<AccountInfo> {
        Err(CoreError::Unauthenticated {
            provider: "nexus".into(),
        })
    }
    async fn store(&self, _: Credential) -> onera_core::Result<AccountInfo> {
        Err(CoreError::Unauthenticated {
            provider: "nexus".into(),
        })
    }
    async fn forget(&self) -> onera_core::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// A game directory shaped like a Steam library entry
// ---------------------------------------------------------------------------

const APP_ID: &str = "1091500";

/// Write an `appmanifest` with the given build id, the way Steam writes one.
fn write_app_manifest(steamapps: &Path, build_id: &str) {
    std::fs::write(
        steamapps.join(format!("appmanifest_{APP_ID}.acf")),
        format!(
            "\"AppState\"\n{{\n\t\"appid\"\t\t\"{APP_ID}\"\n\t\"name\"\t\t\"Cyberpunk 2077\"\n\
             \t\"installdir\"\t\t\"Cyberpunk 2077\"\n\t\"buildid\"\t\t\"{build_id}\"\n\
             \t\"InstalledDepots\"\n\t{{\n\t\t\"1091501\"\n\t\t{{\n\t\t\t\"manifest\"\t\t\"55\"\n\
             \t\t}}\n\t}}\n}}\n"
        ),
    )
    .unwrap();
}

struct Harness {
    onera: Onera,
    _dir: tempfile::TempDir,
    game_dir: PathBuf,
    steamapps: PathBuf,
    source: InstallSource,
}

impl Harness {
    async fn new(source: InstallSource) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let steamapps = dir.path().join("Steam/steamapps");
        let game_dir = steamapps.join("common/Cyberpunk 2077");
        std::fs::create_dir_all(game_dir.join("bin/x64")).unwrap();
        std::fs::create_dir_all(game_dir.join("archive/pc/content")).unwrap();
        std::fs::create_dir_all(game_dir.join("archive/pc/mod")).unwrap();
        std::fs::create_dir_all(game_dir.join("r6/scripts")).unwrap();
        std::fs::create_dir_all(game_dir.join("r6/cache")).unwrap();
        std::fs::write(game_dir.join("bin/x64/Cyberpunk2077.exe"), b"MZ fake").unwrap();
        std::fs::write(game_dir.join("version.txt"), b"2.21").unwrap();
        std::fs::write(
            game_dir.join("archive/pc/content/basegame.archive"),
            b"base game content",
        )
        .unwrap();
        // Excluded by the adapter: a rebuilt cache must never look modified.
        std::fs::write(game_dir.join("r6/cache/final.redscripts"), b"rebuilt").unwrap();
        write_app_manifest(&steamapps, "18320471");

        let onera = Onera::assemble(
            Paths::rooted_at(dir.path().join("xdg")),
            Arc::new(NoAuth),
            Arc::new(NoProvider),
        )
        .await
        .unwrap();
        // A provider game row must exist before an installation can reference it.
        onera
            .database()
            .upsert_game(&Game {
                id: onera_core::ids::GameId::new(),
                provider: ProviderId::nexus(),
                provider_slug: "cyberpunk2077".into(),
                name: "Cyberpunk 2077".into(),
                steam_app_id: Some(1_091_500),
            })
            .await
            .unwrap();
        let _ = InMemorySecretStore::new();

        Self {
            onera,
            _dir: dir,
            game_dir,
            steamapps,
            source,
        }
    }

    async fn register(&self) -> LocalGameId {
        self.onera
            .confirm_game(&DiscoveredGame {
                adapter_id: "cyberpunk2077".into(),
                provider_slug: Some("cyberpunk2077".into()),
                name: "Cyberpunk 2077".into(),
                install_root: self.game_dir.clone(),
                compat_prefix: None,
                user_data_roots: vec![],
                source: self.source,
                validation: onera_core::domain::game::InstallValidation::ok(),
            })
            .await
            .unwrap()
    }
}

// ---------------------------------------------------------------------------
// Status and capture
// ---------------------------------------------------------------------------

/// The panel model before anything is captured, and the confirmation the
/// capture refuses to skip.
#[tokio::test]
async fn a_store_verified_capture_requires_the_user_to_confirm_verification() {
    let h = Harness::new(InstallSource::SteamNative).await;
    let game = h.register().await;

    let status = h.onera.baseline_status(game).await.unwrap();
    assert!(status.baseline.is_none());
    assert_eq!(status.freshness, BaselineFreshness::None);
    assert_eq!(status.active_mod_count, 0);
    assert_eq!(status.capture_blocked_reason, None);
    assert_eq!(
        status
            .observed_build_identity
            .as_ref()
            .and_then(|identity| identity.build_id.clone()),
        Some("18320471".to_owned()),
        "the Steam manifest next to the game is the whole source of identity"
    );

    let preview = h.onera.plan_baseline_capture(game, None).await.unwrap();
    assert_eq!(preview.source, BaselineSource::StoreVerifiedCapture);
    assert!(preview.requires_store_verification);
    assert!(
        preview.estimated_files >= 3,
        "the preview must describe the scope before a long hash run"
    );
    assert!(preview
        .exclusions
        .iter()
        .any(|exclusion| exclusion.note.is_some()));

    let refused = h
        .onera
        .capture_baseline(game, None, false, &NullProgress, &CancelToken::new())
        .await
        .unwrap_err();
    assert!(
        matches!(refused, CoreError::DecisionRequired(_)),
        "Onera cannot check that Steam verified the files, so it must ask: {refused:?}"
    );
}

/// A capture records exactly the adapter's declared scope, and nothing under it
/// is touched.
#[tokio::test]
async fn a_confirmed_capture_records_the_declared_scope_and_changes_nothing() {
    let h = Harness::new(InstallSource::SteamNative).await;
    let game = h.register().await;
    let before = snapshot(&h.game_dir);

    let progress = RecordingProgress::default();
    let baseline = h
        .onera
        .capture_baseline(game, None, true, &progress, &CancelToken::new())
        .await
        .unwrap();

    assert_eq!(baseline.status, BaselineStatus::Current);
    assert_eq!(baseline.source, BaselineSource::StoreVerifiedCapture);
    assert_eq!(baseline.adapter_id, "cyberpunk2077");
    assert_eq!(baseline.reported_version.as_deref(), Some("2.21"));
    assert_eq!(
        baseline.build_identity.and_then(|i| i.build_id),
        Some("18320471".to_owned()),
        "a capture is stamped with the build it saw"
    );

    let files = BaselineStore::baseline_files(h.onera.database(), baseline.id)
        .await
        .unwrap();
    let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
    assert!(paths.contains(&"bin/x64/Cyberpunk2077.exe"));
    assert!(paths.contains(&"archive/pc/content/basegame.archive"));
    assert!(
        !paths.iter().any(|path| path.starts_with("r6/cache/")),
        "an adapter exclusion keeps a rebuilt cache out of the baseline: {paths:?}"
    );
    assert_eq!(baseline.file_count, files.len() as u64);

    assert_eq!(
        snapshot(&h.game_dir),
        before,
        "a capture must never write inside a baseline root"
    );
}

/// A manual installation cannot claim a store-verified baseline, whatever the
/// caller asks for.
#[tokio::test]
async fn a_manual_installation_gets_a_labelled_local_snapshot() {
    let h = Harness::new(InstallSource::Manual).await;
    let game = h.register().await;

    let preview = h
        .onera
        .plan_baseline_capture(game, Some(BaselineSource::StoreVerifiedCapture))
        .await
        .unwrap();
    assert_eq!(preview.source, BaselineSource::LocalSnapshot);
    assert!(!preview.requires_store_verification);

    // No confirmation is asked for, because none would mean anything.
    let baseline = h
        .onera
        .capture_baseline(
            game,
            Some(BaselineSource::StoreVerifiedCapture),
            false,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(baseline.source, BaselineSource::LocalSnapshot);
    assert!(!baseline.source.is_store_verified());
    assert!(
        baseline.build_identity.is_none(),
        "Onera did not learn this path from Steam and must not stamp a Steam build on it"
    );

    let status = h.onera.baseline_status(game).await.unwrap();
    assert!(
        matches!(status.freshness, BaselineFreshness::Unknown { .. }),
        "a snapshot with no store identity is unverifiable, not fresh: {:?}",
        status.freshness
    );
}

/// Capturing over Onera's own deployments would record modded files as clean.
#[tokio::test]
async fn capture_is_blocked_while_onera_mods_are_active() {
    let h = Harness::new(InstallSource::SteamNative).await;
    let game = h.register().await;
    activate_a_fake_mod(&h.onera, game).await;

    let status = h.onera.baseline_status(game).await.unwrap();
    assert_eq!(status.active_mod_count, 1);
    let reason = status
        .capture_blocked_reason
        .expect("an active mod must block capture");
    assert!(reason.contains("active"), "{reason}");

    let error = h
        .onera
        .capture_baseline(game, None, true, &NullProgress, &CancelToken::new())
        .await
        .unwrap_err();
    assert!(matches!(error, CoreError::Conflict(_)), "{error:?}");
}

// ---------------------------------------------------------------------------
// Freshness
// ---------------------------------------------------------------------------

/// A Steam build change makes the prior baseline visibly stale, and a manifest
/// that vanishes makes it unknown — never fresh.
#[tokio::test]
async fn a_changed_build_is_stale_and_a_missing_identity_is_unknown() {
    let h = Harness::new(InstallSource::SteamNative).await;
    let game = h.register().await;
    h.onera
        .capture_baseline(game, None, true, &NullProgress, &CancelToken::new())
        .await
        .unwrap();
    assert_eq!(
        h.onera.baseline_freshness(game).await.unwrap(),
        BaselineFreshness::Fresh
    );

    write_app_manifest(&h.steamapps, "18400000");
    let freshness = h.onera.baseline_freshness(game).await.unwrap();
    let BaselineFreshness::Stale { captured, observed } = freshness else {
        panic!("a changed BuildID must be stale, got {freshness:?}");
    };
    assert_eq!(captured.build_id.as_deref(), Some("18320471"));
    assert_eq!(observed.build_id.as_deref(), Some("18400000"));
    assert!(freshness_needs_recapture(game, &h).await);

    std::fs::remove_file(h.steamapps.join(format!("appmanifest_{APP_ID}.acf"))).unwrap();
    let freshness = h.onera.baseline_freshness(game).await.unwrap();
    assert!(
        matches!(freshness, BaselineFreshness::Unknown { .. }),
        "a missing manifest is unknown, never fresh: {freshness:?}"
    );
}

async fn freshness_needs_recapture(game: LocalGameId, h: &Harness) -> bool {
    h.onera
        .baseline_freshness(game)
        .await
        .unwrap()
        .needs_recapture()
}

/// A recapture after a game update supersedes the old baseline and keeps it.
#[tokio::test]
async fn a_recapture_supersedes_the_stale_baseline_and_keeps_its_history() {
    let h = Harness::new(InstallSource::SteamNative).await;
    let game = h.register().await;
    let first = h
        .onera
        .capture_baseline(game, None, true, &NullProgress, &CancelToken::new())
        .await
        .unwrap();

    write_app_manifest(&h.steamapps, "18400000");
    std::fs::write(h.game_dir.join("bin/x64/Cyberpunk2077.exe"), b"MZ patched").unwrap();
    let second = h
        .onera
        .capture_baseline(game, None, true, &NullProgress, &CancelToken::new())
        .await
        .unwrap();

    assert_ne!(first.id, second.id);
    let history = h.onera.baseline_history(game).await.unwrap();
    assert_eq!(
        history.iter().map(|b| (b.id, b.status)).collect::<Vec<_>>(),
        vec![
            (second.id, BaselineStatus::Current),
            (first.id, BaselineStatus::Superseded),
        ]
    );
    assert!(
        !BaselineStore::baseline_files(h.onera.database(), first.id)
            .await
            .unwrap()
            .is_empty(),
        "a superseded capture stays inspectable"
    );
    assert_eq!(
        h.onera.baseline_freshness(game).await.unwrap(),
        BaselineFreshness::Fresh
    );
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

/// The headline property: a captured installation matches itself byte for byte,
/// and every class of difference is reported rather than acted on.
#[tokio::test]
async fn a_captured_installation_verifies_clean_and_then_reports_every_difference() {
    let h = Harness::new(InstallSource::SteamNative).await;
    let game = h.register().await;
    let baseline = h
        .onera
        .capture_baseline(game, None, true, &NullProgress, &CancelToken::new())
        .await
        .unwrap();

    let clean = h
        .onera
        .verify_baseline(game, false, &NullProgress, &CancelToken::new())
        .await
        .unwrap();
    assert_eq!(clean.state, ScanState::Completed);
    assert_eq!(clean.evidence, ScanEvidence::ContentHashed);
    assert!(clean.is_clean(&baseline), "counts: {:?}", clean.counts);
    assert!(clean.requires_user_decision().is_empty());

    // A modified store file, a vanished one, and an extra nobody claims.
    std::fs::write(h.game_dir.join("bin/x64/Cyberpunk2077.exe"), b"MZ tampered").unwrap();
    std::fs::remove_file(h.game_dir.join("archive/pc/content/basegame.archive")).unwrap();
    std::fs::write(
        h.game_dir.join("r6/scripts/mystery.reds"),
        b"who put this here",
    )
    .unwrap();
    // A cache the adapter excludes must still not appear.
    std::fs::write(
        h.game_dir.join("r6/cache/final.redscripts"),
        b"rebuilt again",
    )
    .unwrap();

    let dirty = h
        .onera
        .verify_baseline(game, false, &NullProgress, &CancelToken::new())
        .await
        .unwrap();
    assert!(!dirty.is_clean(&baseline));
    assert_eq!(dirty.counts.modified, 1);
    assert_eq!(dirty.counts.missing, 1);
    assert_eq!(dirty.counts.extra_unknown, 1);
    assert!(
        dirty
            .findings
            .iter()
            .all(|finding| !finding.path.as_str().starts_with("r6/cache/")),
        "an excluded path is not a finding"
    );
    let decisions = dirty.requires_user_decision();
    assert_eq!(decisions.len(), 1);
    assert_eq!(
        decisions[0].classification,
        FileClassification::ExtraUnknown
    );

    // The findings were persisted against the run that produced them.
    let stored = BaselineStore::findings(h.onera.database(), dirty.scan_run_id)
        .await
        .unwrap();
    assert_eq!(stored, dirty.findings);
    let run = BaselineStore::scan_run(h.onera.database(), dirty.scan_run_id)
        .await
        .unwrap()
        .expect("the verification run is persisted");
    assert_eq!(run.baseline_id, Some(baseline.id));
    assert_eq!(run.counts, dirty.counts);
}

/// A symlink is a finding, never a followed path and never a baseline record.
#[cfg(unix)]
#[tokio::test]
async fn a_symlink_is_reported_and_never_followed() {
    let h = Harness::new(InstallSource::SteamNative).await;
    let game = h.register().await;
    let baseline = h
        .onera
        .capture_baseline(game, None, true, &NullProgress, &CancelToken::new())
        .await
        .unwrap();

    let outside = h._dir.path().join("outside-secret");
    std::fs::write(&outside, b"never read").unwrap();
    std::os::unix::fs::symlink(&outside, h.game_dir.join("r6/scripts/link.reds")).unwrap();

    let verification = h
        .onera
        .verify_baseline(game, false, &NullProgress, &CancelToken::new())
        .await
        .unwrap();
    assert!(!verification.is_clean(&baseline));
    let link = verification
        .findings
        .iter()
        .find(|finding| finding.path.as_str() == "r6/scripts/link.reds")
        .expect("the symlink must be reported");
    assert_eq!(link.classification, FileClassification::SpecialFile);
    assert!(link.observed.is_none(), "a link's target is never hashed");
    assert!(verification
        .requires_user_decision()
        .iter()
        .any(|finding| finding.classification == FileClassification::SpecialFile));
}

/// The responsive scan can prove a change and can never prove its absence.
#[tokio::test]
async fn a_quick_scan_is_metadata_only_and_never_clean() {
    let h = Harness::new(InstallSource::SteamNative).await;
    let game = h.register().await;
    let baseline = h
        .onera
        .capture_baseline(game, None, true, &NullProgress, &CancelToken::new())
        .await
        .unwrap();

    let quick = h
        .onera
        .verify_baseline(game, true, &NullProgress, &CancelToken::new())
        .await
        .unwrap();
    assert_eq!(quick.evidence, ScanEvidence::MetadataOnly);
    assert_eq!(quick.state, ScanState::Completed);
    assert!(!quick.counts.has_differences(), "nothing has changed yet");
    assert!(
        !quick.is_clean(&baseline),
        "a metadata-only result may never be presented as clean"
    );

    // A same-size edit is exactly what a quick scan cannot see, which is why it
    // is never enough on its own.
    std::fs::write(h.game_dir.join("bin/x64/Cyberpunk2077.exe"), b"MZ faky").unwrap();
    let quick = h
        .onera
        .verify_baseline(game, true, &NullProgress, &CancelToken::new())
        .await
        .unwrap();
    assert_eq!(
        quick.counts.modified, 0,
        "same size, so metadata sees nothing"
    );
    let full = h
        .onera
        .verify_baseline(game, false, &NullProgress, &CancelToken::new())
        .await
        .unwrap();
    assert_eq!(full.counts.modified, 1, "content hashing sees it");
    assert!(!full.is_clean(&baseline));
}

/// Verifying a game that was never captured is a missing baseline, not a clean
/// one.
#[tokio::test]
async fn verifying_without_a_baseline_is_not_found() {
    let h = Harness::new(InstallSource::SteamNative).await;
    let game = h.register().await;
    let error = h
        .onera
        .verify_baseline(game, false, &NullProgress, &CancelToken::new())
        .await
        .unwrap_err();
    assert!(matches!(error, CoreError::NotFound { .. }), "{error:?}");
}

/// A cancelled capture leaves a terminal scan run behind and no baseline: an
/// abandoned capture must not be mistakable for one that finished.
#[tokio::test]
async fn a_cancelled_capture_records_the_run_and_no_baseline() {
    let h = Harness::new(InstallSource::SteamNative).await;
    let game = h.register().await;
    let cancel = CancelToken::new();
    cancel.cancel();

    let error = h
        .onera
        .capture_baseline(game, None, true, &NullProgress, &cancel)
        .await
        .unwrap_err();
    assert!(matches!(error, CoreError::Cancelled), "{error:?}");
    assert!(BaselineStore::current_baseline(h.onera.database(), game)
        .await
        .unwrap()
        .is_none());
    let (runs,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM baseline_scan_runs WHERE state = 'cancelled' AND baseline_id IS NULL",
    )
    .fetch_one(h.onera.database().pool())
    .await
    .unwrap();
    assert_eq!(runs, 1);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Path, size and contents of everything under a directory.
fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut entries: Vec<(PathBuf, Vec<u8>)> = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            (
                entry.path().to_path_buf(),
                std::fs::read(entry.path()).unwrap_or_default(),
            )
        })
        .collect();
    entries.sort();
    entries
}

/// Record one active installation, so the capture guard has something to see.
async fn activate_a_fake_mod(onera: &Onera, game: LocalGameId) {
    use onera_core::domain::release::{FileCategory, Mod, ProviderFile, Release};
    use onera_core::hash::FileHash;
    use onera_core::ids::{InstallationId, ModId, ProviderModId, ReleaseId};
    use onera_core::ports::DeploymentStore;

    let db = onera.database();
    let mod_id = db
        .upsert_mod(&Mod {
            id: ModId::new(),
            provider: ProviderId::nexus(),
            provider_mod_id: ProviderModId::new("107"),
            game_slug: "cyberpunk2077".into(),
            name: "A Mod".into(),
            author: None,
        })
        .await
        .unwrap();
    let release = db
        .upsert_release(&Release {
            id: ReleaseId::new(),
            mod_id,
            version: "1.0.0".into(),
            published_at: None,
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();
    db.upsert_provider_file(&ProviderFile {
        provider: ProviderId::nexus(),
        provider_file_id: ProviderFileId::new("9001"),
        release_id: release,
        name: "a-mod.zip".into(),
        size_bytes: Some(4),
        category: FileCategory::Main,
        published_hash: None,
        uploaded_at: None,
        is_primary: true,
    })
    .await
    .unwrap();
    let archive = db
        .upsert_archive(
            &FileHash::blake3_of(b"mod bytes"),
            9,
            "a-mod.zip",
            onera_core::domain::archive::ArchiveFormat::Zip,
            Path::new("/data/archives/blake3/ab/abc"),
        )
        .await
        .unwrap();
    db.record_installation(InstallationId::new(), game, mod_id, release, archive)
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// The warning installation preparation raises
// ---------------------------------------------------------------------------

/// `prepare_install` reads freshness before doing any work. A changed known
/// Steam identity warns; a missing or incomparable one is Unknown and warns too
/// — neither may pass silently as if the baseline still described the game.
#[test]
fn only_a_fresh_or_absent_baseline_prepares_an_install_silently() {
    use onera_app::baseline::freshness_warning;
    use onera_core::domain::baseline::{GameStoreKind, StoreBuildIdentity};

    let identity = |build: &str| StoreBuildIdentity {
        store: GameStoreKind::Steam,
        app_id: Some(APP_ID.into()),
        build_id: Some(build.into()),
        branch: None,
        depots: vec![],
        manifest_path: None,
        observed_at: chrono::Utc::now(),
    };

    assert_eq!(freshness_warning(&BaselineFreshness::None), None);
    assert_eq!(freshness_warning(&BaselineFreshness::Fresh), None);

    let stale = freshness_warning(&BaselineFreshness::Stale {
        captured: Box::new(identity("18320471")),
        observed: Box::new(identity("18400000")),
    })
    .expect("a changed build must warn");
    assert!(stale.contains("stale"), "{stale}");

    let unknown = freshness_warning(&BaselineFreshness::Unknown {
        reason: "the store did not expose a comparable build identity".into(),
    })
    .expect("an incomparable identity must warn, never pass as fresh");
    assert!(unknown.contains("cannot be determined"), "{unknown}");
}

/// The same rule at the domain boundary: an unknown identity on either side is
/// never `Fresh`.
#[test]
fn an_incomparable_identity_is_never_fresh() {
    use onera_core::domain::baseline::{GameStoreKind, StoreBuildIdentity};

    let known = StoreBuildIdentity {
        store: GameStoreKind::Steam,
        app_id: Some(APP_ID.into()),
        build_id: Some("18320471".into()),
        branch: None,
        depots: vec![],
        manifest_path: None,
        observed_at: chrono::Utc::now(),
    };
    let blank = StoreBuildIdentity::unknown(GameStoreKind::Steam, chrono::Utc::now());

    for (captured, observed) in [
        (Some(&known), None),
        (None, Some(&known)),
        (Some(&blank), Some(&known)),
        (Some(&known), Some(&blank)),
    ] {
        assert!(
            matches!(
                BaselineFreshness::evaluate(captured, observed),
                BaselineFreshness::Unknown { .. }
            ),
            "a missing side must be Unknown, not Fresh"
        );
    }
    assert_eq!(
        BaselineFreshness::evaluate(Some(&known), Some(&known)),
        BaselineFreshness::Fresh
    );
}

// ---------------------------------------------------------------------------
// Wire shapes
// ---------------------------------------------------------------------------

/// The payloads must serialize exactly as `docs/frontend-contracts.md` says,
/// because the desktop mocks and the `--json` CLI output are both written
/// against that document.
#[tokio::test]
async fn the_payloads_match_the_documented_wire_shapes() {
    let h = Harness::new(InstallSource::SteamNative).await;
    let game = h.register().await;

    let preview = h.onera.plan_baseline_capture(game, None).await.unwrap();
    let json = serde_json::to_value(&preview).unwrap();
    assert_eq!(json["roots"][0]["key"], "game");
    assert_eq!(json["roots"][0]["kind"], "game_install");
    assert!(json["roots"][0]["path"].is_string());
    assert_eq!(json["exclusions"][0]["pattern"]["kind"], "prefix");
    assert!(json["estimated_files"].is_number());
    assert!(json["estimated_bytes"].is_number());

    let baseline = h
        .onera
        .capture_baseline(game, None, true, &NullProgress, &CancelToken::new())
        .await
        .unwrap();
    let status = serde_json::to_value(h.onera.baseline_status(game).await.unwrap()).unwrap();
    assert_eq!(status["baseline"]["source"], "store_verified_capture");
    assert_eq!(status["baseline"]["status"], "current");
    assert_eq!(status["baseline"]["build_identity"]["store"], "steam");
    assert_eq!(status["baseline"]["build_identity"]["app_id"], APP_ID);
    assert_eq!(
        status["baseline"]["build_identity"]["depots"][0]["depot_id"],
        "1091501"
    );
    assert_eq!(status["baseline"]["adapter_id"], "cyberpunk2077");
    assert_eq!(status["baseline"]["reported_version"], "2.21");
    assert!(status["baseline"]["scope_fingerprint"].is_string());
    assert!(status["baseline"]["file_count"].is_number());
    assert!(status["baseline"]["total_bytes"].is_number());
    assert_eq!(status["freshness"]["kind"], "fresh");
    assert!(status["observed_build_identity"]["build_id"].is_string());
    assert_eq!(status["active_mod_count"], 0);
    assert!(status["capture_blocked_reason"].is_null());

    std::fs::write(h.game_dir.join("r6/scripts/extra.reds"), b"extra").unwrap();
    let verification = serde_json::to_value(
        h.onera
            .verify_baseline(game, false, &NullProgress, &CancelToken::new())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(verification["baseline_id"], baseline.id.to_string());
    assert_eq!(verification["state"], "completed");
    assert_eq!(verification["evidence"], "content_hashed");
    for key in [
        "matching",
        "modified",
        "missing",
        "extra_managed",
        "extra_unknown",
        "unreadable",
        "special",
    ] {
        assert!(
            verification["counts"][key].is_number(),
            "counts.{key} is missing"
        );
    }
    let finding = verification["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["classification"] == "extra_unknown")
        .expect("the extra file is reported");
    assert_eq!(finding["root_key"], "game");
    assert_eq!(finding["path"], "r6/scripts/extra.reds");
    assert!(finding["expected"].is_null());
    assert!(finding["observed"].is_object() || finding["observed"].is_string());
    assert!(verification["verified_at"].is_string());
    assert!(verification["scope_fingerprint"].is_string());

    // Freshness is internally tagged, so a stale answer carries both sides.
    write_app_manifest(&h.steamapps, "18400000");
    let status = serde_json::to_value(h.onera.baseline_status(game).await.unwrap()).unwrap();
    assert_eq!(status["freshness"]["kind"], "stale");
    assert_eq!(status["freshness"]["captured"]["build_id"], "18320471");
    assert_eq!(status["freshness"]["observed"]["build_id"], "18400000");
}
