//! Profile activation, end to end, with no network and no API key.
//!
//! ```text
//! register a game -> create a profile -> add a member that is not downloaded
//!   -> preview the switch -> activate it (download, prepare, apply, verify)
//!   -> switch away and back -> refuse a stale preview -> recover a crash
//! ```
//!
//! The claims these tests exist to pin, all of which are things a mod manager
//! must never get wrong:
//!
//! * previewing and preparing a switch never touch the game directory;
//! * a member with no artifact is a download, and a member with no *chosen
//!   file* is a blocker — Onera does not pick a version for you;
//! * every failure, refusal and cancellation leaves the old profile active; and
//! * the target profile is reported active only when the files match it.

use onera_app::secrets::InMemorySecretStore;
use onera_app::{InstallRequest, Onera, Paths};
use onera_core::domain::game::InstallSource;
use onera_core::domain::profile::{ActivationBlocker, DesiredModState, ProfileActivationState};
use onera_core::ids::{ProfileId, ProviderModId};
use onera_core::progress::{CancelToken, NullProgress};
use onera_core::redact::Secret;
use onera_core::CoreError;
use onera_discovery::DiscoveredGame;
use onera_nexus::{ApiKeyAuth, NexusClient, NexusConfig};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API_KEY: &str = "a-valid-looking-nexus-api-key-0123";
const GAME_SLUG: &str = "cyberpunk2077";
const MOD_ID: &str = "107";
const FILE_ID: &str = "100";

fn build_mod_archive(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buffer);
        for (path, contents) in files {
            zip.start_file(*path, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(contents).unwrap();
        }
        zip.finish().unwrap();
    }
    buffer.into_inner()
}

fn fake_game_dir(root: &Path) -> PathBuf {
    let game = root.join("Cyberpunk 2077");
    std::fs::create_dir_all(game.join("bin/x64")).unwrap();
    std::fs::create_dir_all(game.join("archive/pc/content")).unwrap();
    std::fs::create_dir_all(game.join("archive/pc/mod")).unwrap();
    std::fs::create_dir_all(game.join("r6/scripts")).unwrap();
    std::fs::write(
        game.join("bin/x64/Cyberpunk2077.exe"),
        b"MZ fake executable",
    )
    .unwrap();
    std::fs::write(game.join("version.txt"), b"2.21").unwrap();
    game
}

fn walkdir(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    walkdir(root)
        .into_iter()
        .map(|path| {
            (
                path.strip_prefix(root).unwrap().to_path_buf(),
                std::fs::read(&path).unwrap(),
            )
        })
        .collect()
}

struct Harness {
    onera: Onera,
    _dir: tempfile::TempDir,
    _server: MockServer,
    game_dir: PathBuf,
}

impl Harness {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let game_dir = fake_game_dir(dir.path());
        let archive = build_mod_archive(&[
            ("Test Mod v1.0/readme.txt", b"drag into the game folder"),
            (
                "Test Mod v1.0/archive/pc/mod/testmod.archive",
                b"archive payload",
            ),
        ]);
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/users/validate.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "user_id": 12345, "name": "TestUser", "is_premium": true,
                "email": "test@example.test"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/games.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "domain_name": GAME_SLUG, "name": "Cyberpunk 2077" }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v3/games/{GAME_SLUG}/mods/{MOD_ID}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "id": "1", "game_scoped_id": MOD_ID,
                          "name": "Test Mod", "author": "A Modder" }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v3/mods/1/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "mod_files": [{ "id": "10", "name": "Main file" }] }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v3/mod-files/10/versions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "versions": [{
                    "id": FILE_ID, "name": "Test Mod 1.0", "version": "1.0.0",
                    "category": "main", "uploaded_at": "2025-01-01T00:00:00Z",
                    "is_primary": true
                }] }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/v1/games/{GAME_SLUG}/mods/{MOD_ID}/files/{FILE_ID}/download_link.json"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "name": "CDN", "URI": format!("{}/cdn/mod.zip", server.uri()) }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/cdn/mod.zip"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(archive))
            .mount(&server)
            .await;

        let config = NexusConfig {
            v3_base: format!("{}/v3", server.uri()),
            v1_base: format!("{}/v1", server.uri()),
            ..NexusConfig::default()
        };
        let secrets = Arc::new(InMemorySecretStore::new());
        let auth = Arc::new(
            ApiKeyAuth::new_for_tests(secrets, config.v1_base.clone(), &config.user_agent).unwrap(),
        );
        let provider = Arc::new(NexusClient::new_for_tests(config, auth.clone()).unwrap());
        let onera = Onera::assemble_with(
            Paths::rooted_at(dir.path().join("xdg")),
            auth,
            provider,
            true,
        )
        .await
        .unwrap();
        onera.set_api_key(Secret::new(API_KEY)).await.unwrap();

        Self {
            onera,
            _dir: dir,
            _server: server,
            game_dir,
        }
    }

    fn discovered(&self) -> DiscoveredGame {
        DiscoveredGame {
            adapter_id: "cyberpunk2077".into(),
            provider_slug: Some(GAME_SLUG.into()),
            name: "Cyberpunk 2077".into(),
            install_root: self.game_dir.clone(),
            compat_prefix: None,
            user_data_roots: vec![],
            source: InstallSource::SteamNative,
            validation: onera_core::domain::game::InstallValidation::ok(),
        }
    }

    fn deployed(&self) -> bool {
        self.game_dir
            .join("archive/pc/mod/testmod.archive")
            .is_file()
    }

    async fn active_profile(&self, game: onera_core::ids::LocalGameId) -> ProfileId {
        self.onera
            .profiles(game)
            .await
            .unwrap()
            .into_iter()
            .find(|profile| profile.is_active)
            .expect("a game always has one active profile")
            .id
    }
}

/// Register the game, cache the mod, and return the game plus its mod lineage.
async fn registered(h: &Harness) -> (onera_core::ids::LocalGameId, onera_core::ids::ModId) {
    let game = h.onera.confirm_game(&h.discovered()).await.unwrap();
    let details = h
        .onera
        .fetch_mod(GAME_SLUG, &ProviderModId::new(MOD_ID), &CancelToken::new())
        .await
        .unwrap();
    (game, details.mod_id)
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_preview_reports_a_download_and_writes_nothing() {
    let h = Harness::new().await;
    let (game, mod_id) = registered(&h).await;
    let before = snapshot(&h.game_dir);

    let modded = h
        .onera
        .create_profile(game, "Modded".into(), None, None)
        .await
        .unwrap();
    let member = h
        .onera
        .add_profile_member(
            modded.id,
            mod_id,
            Some(onera_core::ids::ProviderFileId::new(FILE_ID)),
        )
        .await
        .unwrap();
    // The artifact has never been downloaded, which is a *download*, not a
    // reason to leave the mod out of the plan.
    assert!(member.installation_id.is_none());

    let preview = h.onera.plan_profile_activation(modded.id).await.unwrap();
    assert_eq!(preview.to_profile_id, modded.id);
    assert_eq!(preview.from_profile_id, Some(h.active_profile(game).await));
    assert_eq!(preview.downloads.len(), 1);
    assert_eq!(preview.downloads[0].member_id, member.id);
    assert!(preview.ready, "a pending download does not block a switch");
    assert!(preview.blockers.is_empty());
    // Nothing is deployable yet, so the plan itself is empty — and previewing
    // changed not one byte of the game.
    assert!(preview.plan.steps.is_empty());
    assert_eq!(preview.bytes_to_write, 0);
    assert_eq!(snapshot(&h.game_dir), before);
    assert_eq!(
        h.active_profile(game).await,
        preview.from_profile_id.unwrap()
    );
}

#[tokio::test]
async fn a_member_with_no_chosen_file_blocks_the_switch() {
    let h = Harness::new().await;
    let (game, mod_id) = registered(&h).await;
    let before = snapshot(&h.game_dir);
    let default = h.active_profile(game).await;

    let modded = h
        .onera
        .create_profile(game, "Modded".into(), None, None)
        .await
        .unwrap();
    // No provider file, and nothing installed to infer one from. Choosing a
    // version here is the dependency solver's job, not a guess.
    let member = h
        .onera
        .add_profile_member(modded.id, mod_id, None)
        .await
        .unwrap();
    assert!(!member.selection.is_resolved());

    let preview = h.onera.plan_profile_activation(modded.id).await.unwrap();
    assert!(!preview.ready);
    assert_eq!(
        preview.blockers,
        vec![ActivationBlocker::UnresolvedSelection {
            member_id: member.id
        }]
    );
    assert!(preview.downloads.is_empty());

    let error = h
        .onera
        .activate_profile(modded.id, None, &NullProgress, &CancelToken::new())
        .await
        .unwrap_err();
    assert!(matches!(error, CoreError::DecisionRequired(_)));
    assert_eq!(h.active_profile(game).await, default);
    assert_eq!(snapshot(&h.game_dir), before);
}

#[tokio::test]
async fn dependency_status_is_a_real_profile_command_before_the_solver_exists() {
    let h = Harness::new().await;
    let (game, mod_id) = registered(&h).await;
    let profile = h.active_profile(game).await;
    let member = h
        .onera
        .add_profile_member(
            profile,
            mod_id,
            Some(onera_core::ids::ProviderFileId::new(FILE_ID)),
        )
        .await
        .unwrap();

    let resolution = h.onera.resolve_profile_dependencies(profile).await.unwrap();
    assert!(matches!(
        resolution.outcome,
        onera_core::domain::dependency::ResolutionOutcome::Compatible
    ));
    assert_eq!(resolution.health.len(), 1);
    assert_eq!(resolution.health[0].profile_member_id, member.id);
    assert_eq!(
        resolution.health[0].health,
        onera_core::domain::dependency::DependencyHealth::NotApplicable
    );
    assert_eq!(resolution.evidence.unsupported, 1);

    let missing = h
        .onera
        .resolve_profile_dependencies(ProfileId::new())
        .await
        .unwrap_err();
    assert!(matches!(missing, CoreError::NotFound { .. }));
}

// ---------------------------------------------------------------------------
// Activation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn activating_downloads_the_artifact_and_publishes_the_switch() {
    let h = Harness::new().await;
    let (game, mod_id) = registered(&h).await;
    let default = h.active_profile(game).await;

    let modded = h
        .onera
        .create_profile(game, "Modded".into(), None, None)
        .await
        .unwrap();
    h.onera
        .add_profile_member(
            modded.id,
            mod_id,
            Some(onera_core::ids::ProviderFileId::new(FILE_ID)),
        )
        .await
        .unwrap();

    let activation = h
        .onera
        .activate_profile(modded.id, None, &NullProgress, &CancelToken::new())
        .await
        .unwrap();

    assert_eq!(activation.state, ProfileActivationState::Applied);
    assert!(activation.state.target_is_active());
    assert_eq!(activation.from_profile_id, Some(default));
    assert!(
        activation.operation_id.is_some(),
        "the switch is a journaled operation"
    );
    assert!(activation.finished_at.is_some());
    assert!(activation.error.is_none());

    assert!(h.deployed(), "the mod's files reached the game");
    assert_eq!(h.active_profile(game).await, modded.id);
    // The acquired artifact is retained and now active, so the member no longer
    // needs a download.
    let details = h.onera.profile_details(modded.id).await.unwrap();
    assert!(details.members[0].installation_id.is_some());
    assert!(h
        .onera
        .plan_profile_activation(modded.id)
        .await
        .unwrap()
        .downloads
        .is_empty());

    let history = h.onera.profile_activation_history(game, 10).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].state, ProfileActivationState::Applied);
}

#[tokio::test]
async fn switching_away_and_back_restores_both_directions() {
    let h = Harness::new().await;
    let (game, mod_id) = registered(&h).await;
    let clean = snapshot(&h.game_dir);
    let default = h.active_profile(game).await;

    // Profile A deploys the mod.
    h.onera
        .add_profile_member(
            default,
            mod_id,
            Some(onera_core::ids::ProviderFileId::new(FILE_ID)),
        )
        .await
        .unwrap();
    h.onera
        .activate_profile(default, None, &NullProgress, &CancelToken::new())
        .await
        .unwrap();
    let modded_state = snapshot(&h.game_dir);
    assert!(h.deployed());

    // Profile B deploys nothing.
    let empty = h
        .onera
        .create_profile(game, "Vanilla".into(), None, None)
        .await
        .unwrap();
    h.onera
        .activate_profile(empty.id, None, &NullProgress, &CancelToken::new())
        .await
        .unwrap();
    assert!(!h.deployed());
    assert_eq!(h.active_profile(game).await, empty.id);
    assert_eq!(
        snapshot(&h.game_dir),
        clean,
        "switching away restored the untouched installation exactly"
    );

    // And back again, with no network: the artifact is retained.
    h.onera
        .activate_profile(default, None, &NullProgress, &CancelToken::new())
        .await
        .unwrap();
    assert_eq!(h.active_profile(game).await, default);
    assert_eq!(snapshot(&h.game_dir), modded_state);
}

#[tokio::test]
async fn a_disabled_member_is_kept_but_not_deployed() {
    let h = Harness::new().await;
    let (game, mod_id) = registered(&h).await;
    let default = h.active_profile(game).await;
    let member = h
        .onera
        .add_profile_member(
            default,
            mod_id,
            Some(onera_core::ids::ProviderFileId::new(FILE_ID)),
        )
        .await
        .unwrap();
    h.onera
        .activate_profile(default, None, &NullProgress, &CancelToken::new())
        .await
        .unwrap();
    assert!(h.deployed());

    h.onera
        .set_member_state(member.id, DesiredModState::Disabled)
        .await
        .unwrap();
    h.onera
        .activate_profile(default, None, &NullProgress, &CancelToken::new())
        .await
        .unwrap();

    assert!(!h.deployed(), "a disabled member deploys nothing");
    // The membership and its artifact survive, so re-enabling needs no network.
    let details = h.onera.profile_details(default).await.unwrap();
    assert!(details.members[0].installation_id.is_some());
    h.onera
        .set_member_state(member.id, DesiredModState::Enabled)
        .await
        .unwrap();
    h.onera
        .activate_profile(default, None, &NullProgress, &CancelToken::new())
        .await
        .unwrap();
    assert!(h.deployed());
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_stale_preview_is_refused_rather_than_silently_re_planned() {
    let h = Harness::new().await;
    let (game, mod_id) = registered(&h).await;
    let default = h.active_profile(game).await;
    let member = h
        .onera
        .add_profile_member(
            default,
            mod_id,
            Some(onera_core::ids::ProviderFileId::new(FILE_ID)),
        )
        .await
        .unwrap();
    h.onera
        .activate_profile(default, None, &NullProgress, &CancelToken::new())
        .await
        .unwrap();

    let approved = h.onera.plan_profile_activation(default).await.unwrap();
    // Somebody disables the mod between the preview and the apply.
    h.onera
        .set_member_state(member.id, DesiredModState::Disabled)
        .await
        .unwrap();

    let error = h
        .onera
        .activate_profile(
            default,
            Some(&approved.fingerprint),
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, CoreError::Conflict(_)));
    assert!(
        h.deployed(),
        "a refused apply changes nothing on disk either"
    );

    // A fresh preview describes the new decision and applies cleanly.
    let current = h.onera.plan_profile_activation(default).await.unwrap();
    assert_ne!(current.fingerprint, approved.fingerprint);
    h.onera
        .activate_profile(
            default,
            Some(&current.fingerprint),
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert!(!h.deployed());
}

#[tokio::test]
async fn a_cancelled_activation_keeps_the_old_profile_active() {
    let h = Harness::new().await;
    let (game, mod_id) = registered(&h).await;
    let default = h.active_profile(game).await;
    let before = snapshot(&h.game_dir);

    let modded = h
        .onera
        .create_profile(game, "Modded".into(), None, None)
        .await
        .unwrap();
    h.onera
        .add_profile_member(
            modded.id,
            mod_id,
            Some(onera_core::ids::ProviderFileId::new(FILE_ID)),
        )
        .await
        .unwrap();

    let cancel = CancelToken::new();
    cancel.cancel();
    let error = h
        .onera
        .activate_profile(modded.id, None, &NullProgress, &cancel)
        .await
        .unwrap_err();
    assert!(matches!(error, CoreError::Cancelled));

    assert_eq!(h.active_profile(game).await, default);
    assert_eq!(snapshot(&h.game_dir), before);
    let history = h.onera.profile_activation_history(game, 10).await.unwrap();
    assert_eq!(history[0].state, ProfileActivationState::RolledBack);
    assert!(history[0].error.is_some());
    assert!(
        !history[0].state.target_is_active(),
        "only Applied may claim the target"
    );
}

// ---------------------------------------------------------------------------
// Restart recovery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recovery_finishes_an_activation_whose_process_died() {
    use onera_core::ports::ProfileStore as _;

    let h = Harness::new().await;
    let (game, _) = registered(&h).await;
    let default = h.active_profile(game).await;
    let modded = h
        .onera
        .create_profile(game, "Modded".into(), None, None)
        .await
        .unwrap();

    // A process that died between recording the attempt and journaling anything.
    let abandoned = onera_core::domain::profile::ProfileActivation {
        from_profile_id: Some(default),
        to_profile_id: modded.id,
        operation_id: None,
        state: ProfileActivationState::Preparing,
        started_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        finished_at: None,
        error: None,
    };
    h.onera
        .database()
        .record_activation(&abandoned)
        .await
        .unwrap();

    let recovered = h.onera.recover_profile_activations().await.unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].state, ProfileActivationState::RolledBack);
    assert!(recovered[0].error.is_some());
    // Recovery never promotes a target profile: only the completion
    // transaction does, and it never ran.
    assert_eq!(h.active_profile(game).await, default);
    // Idempotent — a second startup finds nothing left to finish.
    assert!(h
        .onera
        .recover_profile_activations()
        .await
        .unwrap()
        .is_empty());
}

// ---------------------------------------------------------------------------
// Single-mod install path, unchanged
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_installed_mod_can_be_adopted_by_a_profile_without_a_download() {
    let h = Harness::new().await;
    let (game, mod_id) = registered(&h).await;
    let details = h
        .onera
        .fetch_mod(GAME_SLUG, &ProviderModId::new(MOD_ID), &CancelToken::new())
        .await
        .unwrap();
    let file = details.primary_file().unwrap();
    let prepared = h
        .onera
        .prepare_install(
            &InstallRequest {
                local_game_id: game,
                game_slug: GAME_SLUG.into(),
                mod_id,
                release_id: details.releases[0].id,
                provider_mod_id: ProviderModId::new(MOD_ID),
                provider_file_id: file.provider_file_id.clone(),
                filename: "test-mod-1.0.zip".into(),
                expected_size: file.size_bytes,
                expected_hash: None,
            },
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    h.onera
        .apply(&prepared, &NullProgress, &CancelToken::new())
        .await
        .unwrap();
    assert!(h.deployed());

    // Adding the already-installed mod links its retained artifact, so the
    // activation preview has nothing to download and nothing to write.
    let default = h.active_profile(game).await;
    let member = h
        .onera
        .add_profile_member(default, mod_id, Some(file.provider_file_id.clone()))
        .await
        .unwrap();
    assert!(member.installation_id.is_some());
    let preview = h.onera.plan_profile_activation(default).await.unwrap();
    assert!(preview.downloads.is_empty());
    assert!(preview.plan.steps.is_empty(), "the state already matches");
    assert!(preview.ready);
}
