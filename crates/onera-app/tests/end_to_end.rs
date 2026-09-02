//! The full flow, end to end, with no network and no API key.
//!
//! ```text
//! discover Cyberpunk 2077
//!   -> authenticate with a personal Nexus API key
//!   -> receive a mod id from the extension
//!   -> retrieve metadata through the API
//!   -> select and download a file
//!   -> inspect and map the archive
//!   -> preview conflicts
//!   -> install transactionally
//!   -> verify files
//!   -> remove the mod
//!   -> restore the previous state
//! ```
//!
//! Everything above the HTTP boundary is the real code path: the real archive
//! backend, the real planner, the real journaled installer, the real SQLite
//! schema. Only the Nexus server is a mock, and the "game" is a temporary
//! directory containing the marker files the adapter validates.

use onera_app::secrets::InMemorySecretStore;
use onera_app::{InstallRequest, Onera, Paths};
use onera_core::domain::game::InstallSource;
use onera_core::domain::profile::{DesiredModState, MemberPin, MemberPriority};
use onera_core::ids::ProviderModId;
use onera_core::plan::{
    ConflictChoice, Decision, DecisionScope, FileClassification, TargetLocation,
};
use onera_core::progress::{CancelToken, NullProgress, RecordingProgress};
use onera_core::redact::Secret;
use onera_core::{CoreError, RelPath};
use onera_discovery::DiscoveredGame;
use onera_install::remove::ModifiedFilePolicy;
use onera_install::verify::VerifyStatus;
use onera_nexus::{ApiKeyAuth, NexusClient, NexusConfig};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API_KEY: &str = "a-valid-looking-nexus-api-key-0123";
const GAME_SLUG: &str = "cyberpunk2077";
const MOD_ID: &str = "107";
const FILE_ID: &str = "100";

/// Build a zip the way a real Cyberpunk mod is packaged: content under a
/// cosmetic top-level directory, plus a readme.
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

/// A temporary directory laid out like a real Cyberpunk 2077 installation.
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

struct Harness {
    onera: Onera,
    server: MockServer,
    _dir: tempfile::TempDir,
    game_dir: PathBuf,
    archive_bytes: Vec<u8>,
}

impl Harness {
    async fn new(archive_files: &[(&str, &[u8])]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let game_dir = fake_game_dir(dir.path());
        let archive_bytes = build_mod_archive(archive_files);
        let server = MockServer::start().await;

        // --- credential validation -------------------------------------
        Mock::given(method("GET"))
            .and(path("/v1/users/validate.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "user_id": 12345, "name": "TestUser", "is_premium": true,
                "email": "test@example.test"
            })))
            .mount(&server)
            .await;

        // --- game catalogue --------------------------------------------
        Mock::given(method("GET"))
            .and(path("/v1/games.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "domain_name": GAME_SLUG, "name": "Cyberpunk 2077" }
            ])))
            .mount(&server)
            .await;

        // --- mod metadata ----------------------------------------------
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

        // --- download resolution and payload ---------------------------
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
            .respond_with(ResponseTemplate::new(200).set_body_bytes(archive_bytes.clone()))
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

        Self {
            onera,
            server,
            _dir: dir,
            game_dir,
            archive_bytes,
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

    fn game_file(&self, path: &str) -> Option<Vec<u8>> {
        std::fs::read(self.game_dir.join(path)).ok()
    }
}

fn install_request(
    _h: &Harness,
    game: onera_core::ids::LocalGameId,
    details: &onera_app::flow::ModDetails,
) -> InstallRequest {
    let file = details
        .primary_file()
        .expect("the mock advertises a primary file");
    InstallRequest {
        local_game_id: game,
        game_slug: GAME_SLUG.into(),
        mod_id: details.mod_id,
        release_id: details.releases[0].id,
        provider_mod_id: ProviderModId::new(MOD_ID),
        provider_file_id: file.provider_file_id.clone(),
        filename: "test-mod-1.0.zip".into(),
        expected_size: file.size_bytes,
        expected_hash: None,
    }
}

fn target(path: &str) -> TargetLocation {
    TargetLocation {
        root_key: "game".into(),
        path: RelPath::normalize(path).unwrap(),
    }
}

fn game_snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    walkdir(root)
        .into_iter()
        .map(|path| {
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            (relative, std::fs::read(path).unwrap())
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The headline flow
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_full_install_verify_remove_restore_flow() {
    let h = Harness::new(&[
        (
            "Test Mod v1.0/readme.txt",
            b"install by dragging into the game folder",
        ),
        (
            "Test Mod v1.0/archive/pc/mod/testmod.archive",
            b"archive payload",
        ),
        ("Test Mod v1.0/r6/scripts/testmod.reds", b"script payload"),
    ])
    .await;

    // --- authenticate ---------------------------------------------------
    assert!(!h.onera.is_authenticated().await.unwrap());
    let account = h.onera.set_api_key(Secret::new(API_KEY)).await.unwrap();
    assert_eq!(account.username, "TestUser");
    assert_eq!(account.premium, Some(true));
    assert!(h.onera.is_authenticated().await.unwrap());

    // --- register the discovered game -----------------------------------
    let game = h.onera.confirm_game(&h.discovered()).await.unwrap();
    assert_eq!(h.onera.local_games().await.unwrap().len(), 1);

    // --- receive a mod id from the extension and fetch metadata ---------
    let details = h
        .onera
        .fetch_mod(GAME_SLUG, &ProviderModId::new(MOD_ID), &CancelToken::new())
        .await
        .unwrap();
    assert_eq!(details.name, "Test Mod");
    assert_eq!(details.author.as_deref(), Some("A Modder"));
    assert_eq!(details.releases.len(), 1);
    // Version strings are stored exactly as published.
    assert_eq!(details.releases[0].version, "1.0.0");
    assert!(
        !details.needs_file_selection(),
        "a primary file needs no prompt"
    );

    // Browser handoff is durable even when the desktop still needs a choice.
    let inbox_request = h
        .onera
        .enqueue_download_selection_request(GAME_SLUG.into(), ProviderModId::new(MOD_ID), true)
        .await
        .unwrap();
    assert!(inbox_request.provider_file_id.is_none());
    assert_eq!(h.onera.inbox_requests().await.unwrap().len(), 1);
    h.onera
        .complete_inbox_request(inbox_request.id)
        .await
        .unwrap();
    assert!(h.onera.inbox_requests().await.unwrap().is_empty());

    // --- download, inspect, map, plan (nothing written yet) -------------
    let progress = RecordingProgress::default();
    let prepared = h
        .onera
        .prepare_install(
            &install_request(&h, game, &details),
            &progress,
            &CancelToken::new(),
        )
        .await
        .unwrap();

    assert!(
        prepared.layout_rationale.contains("stripped 1 wrapper"),
        "the cosmetic wrapper should be unwrapped: {}",
        prepared.layout_rationale
    );
    assert_eq!(
        prepared.ignored, 1,
        "the readme should be ignored, not deployed"
    );
    assert_eq!(prepared.plan.files.len(), 2);
    assert!(
        prepared.plan.is_ready(),
        "a clean install should need no decisions"
    );
    assert!(prepared
        .plan
        .files
        .iter()
        .all(|f| f.classification == FileClassification::Create));

    // The dry run really is dry.
    assert!(h.game_file("archive/pc/mod/testmod.archive").is_none());

    // --- install transactionally ----------------------------------------
    let report = h
        .onera
        .apply(&prepared, &progress, &CancelToken::new())
        .await
        .unwrap();
    assert_eq!(report.written, 2);
    assert_eq!(
        report.operation.state,
        onera_core::domain::operation::OperationState::Complete
    );
    assert_eq!(
        h.game_file("archive/pc/mod/testmod.archive").unwrap(),
        b"archive payload"
    );
    assert_eq!(
        h.game_file("r6/scripts/testmod.reds").unwrap(),
        b"script payload"
    );
    assert!(
        h.game_file("readme.txt").is_none(),
        "documentation must not be deployed"
    );

    // The desktop read models are backed by the completed installation and
    // persisted download, rather than placeholder command responses.
    let installed = h.onera.installed_mods(game).await.unwrap();
    assert_eq!(installed.len(), 1);
    assert_eq!(installed[0].name, "Test Mod");
    assert_eq!(installed[0].version, "1.0.0");
    let updates = h
        .onera
        .check_updates(game, &CancelToken::new())
        .await
        .unwrap();
    assert_eq!(updates.len(), 1);
    assert!(!updates[0].update_available);
    let downloads = h.onera.downloads().await.unwrap();
    assert_eq!(downloads.len(), 1);
    assert_eq!(downloads[0].state, onera_download::JobState::Complete);

    // --- verify ----------------------------------------------------------
    let installation = prepared.plan.installation_id;
    let verified = h
        .onera
        .verify(game, installation, &NullProgress, &CancelToken::new())
        .await
        .unwrap();
    assert!(verified.is_clean(), "{:?}", verified.counts());
    assert_eq!(verified.files.len(), 2);

    // --- ownership history ----------------------------------------------
    let stack = h
        .onera
        .ownership(game, &target("archive/pc/mod/testmod.archive"))
        .await
        .unwrap();
    assert_eq!(stack.len(), 1);
    assert_eq!(
        stack.top().unwrap().provider.installation_id(),
        Some(installation)
    );

    // --- remove and restore ---------------------------------------------
    let preview = h.onera.preview_removal(game, installation).await.unwrap();
    assert_eq!(preview.deleted.len(), 2);
    assert!(
        h.game_file("archive/pc/mod/testmod.archive").is_some(),
        "a preview writes nothing"
    );

    let removal = h
        .onera
        .remove(
            game,
            installation,
            ModifiedFilePolicy::Ask,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(removal.deleted.len(), 2);
    assert!(h.game_file("archive/pc/mod/testmod.archive").is_none());
    assert!(h.game_file("r6/scripts/testmod.reds").is_none());

    // The game's own files are untouched throughout.
    assert!(h.game_file("bin/x64/Cyberpunk2077.exe").is_some());
    assert!(
        h.game_dir.join("archive/pc/mod").is_dir(),
        "a game directory must survive removal"
    );

    // Nothing is left half-done.
    assert!(h.onera.interrupted_operations().await.unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Conflict handling through the whole stack
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unmanaged_file_is_backed_up_and_restored_on_removal() {
    let h = Harness::new(&[("Mod/archive/pc/mod/conflict.archive", b"from the mod")]).await;
    h.onera.set_api_key(Secret::new(API_KEY)).await.unwrap();
    let game = h.onera.confirm_game(&h.discovered()).await.unwrap();

    // The user already has a file there that Onera has never seen.
    std::fs::write(
        h.game_dir.join("archive/pc/mod/conflict.archive"),
        b"the user put this here",
    )
    .unwrap();

    let details = h
        .onera
        .fetch_mod(GAME_SLUG, &ProviderModId::new(MOD_ID), &CancelToken::new())
        .await
        .unwrap();
    let mut prepared = h
        .onera
        .prepare_install(
            &install_request(&h, game, &details),
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();

    // It must stop and ask.
    assert_eq!(
        prepared.plan.files[0].classification,
        FileClassification::UnmanagedExisting
    );
    assert!(!prepared.plan.is_ready());
    let err = h
        .onera
        .apply(&prepared, &NullProgress, &CancelToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::DecisionRequired(_)), "{err:?}");
    assert_eq!(
        h.game_file("archive/pc/mod/conflict.archive").unwrap(),
        b"the user put this here",
        "an unresolved conflict must change nothing"
    );

    // The user chooses to replace it, keeping a backup.
    let t = prepared.plan.files[0].target.clone();
    prepared.plan.apply_decision(
        &t,
        &Decision {
            choice: ConflictChoice::ReplaceAfterBackup,
            scope: DecisionScope::ThisFile,
        },
    );
    let report = h
        .onera
        .apply(&prepared, &NullProgress, &CancelToken::new())
        .await
        .unwrap();
    assert_eq!(report.backed_up, 1);
    assert_eq!(
        h.game_file("archive/pc/mod/conflict.archive").unwrap(),
        b"from the mod"
    );

    // The stack records the original underneath the mod.
    let stack = h.onera.ownership(game, &t).await.unwrap();
    assert_eq!(stack.len(), 2);
    assert!(stack.has_unmanaged_original());

    // Removing the mod puts the user's file back byte for byte.
    let removal = h
        .onera
        .remove(
            game,
            prepared.plan.installation_id,
            ModifiedFilePolicy::Ask,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(removal.restored.len(), 1);
    assert_eq!(
        h.game_file("archive/pc/mod/conflict.archive").unwrap(),
        b"the user put this here"
    );
}

#[tokio::test]
async fn a_file_edited_after_installation_is_never_overwritten_or_deleted() {
    let h = Harness::new(&[("Mod/r6/scripts/tweak.reds", b"original script")]).await;
    h.onera.set_api_key(Secret::new(API_KEY)).await.unwrap();
    let game = h.onera.confirm_game(&h.discovered()).await.unwrap();

    let details = h
        .onera
        .fetch_mod(GAME_SLUG, &ProviderModId::new(MOD_ID), &CancelToken::new())
        .await
        .unwrap();
    let prepared = h
        .onera
        .prepare_install(
            &install_request(&h, game, &details),
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    h.onera
        .apply(&prepared, &NullProgress, &CancelToken::new())
        .await
        .unwrap();

    // The user tweaks the deployed script.
    std::fs::write(h.game_dir.join("r6/scripts/tweak.reds"), b"my own edits").unwrap();

    // Verification reports it rather than silently repairing it.
    let verified = h
        .onera
        .verify(
            game,
            prepared.plan.installation_id,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert!(!verified.is_clean());
    assert_eq!(
        verified.problems().next().unwrap().status,
        VerifyStatus::Modified
    );

    // Removal refuses to touch it without a decision.
    let err = h
        .onera
        .remove(
            game,
            prepared.plan.installation_id,
            ModifiedFilePolicy::Ask,
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::DecisionRequired(_)), "{err:?}");
    assert_eq!(
        h.game_file("r6/scripts/tweak.reds").unwrap(),
        b"my own edits"
    );
}

// ---------------------------------------------------------------------------
// Safety properties of the whole pipeline
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_malicious_archive_never_reaches_the_game_directory() {
    // The archive tries to escape into the game's parent directory.
    let h = Harness::new(&[("../../escaped.txt", b"you should never see this")]).await;
    h.onera.set_api_key(Secret::new(API_KEY)).await.unwrap();
    let game = h.onera.confirm_game(&h.discovered()).await.unwrap();

    let details = h
        .onera
        .fetch_mod(GAME_SLUG, &ProviderModId::new(MOD_ID), &CancelToken::new())
        .await
        .unwrap();
    let err = h
        .onera
        .prepare_install(
            &install_request(&h, game, &details),
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap_err();

    assert!(matches!(err, CoreError::ArchiveRejected { .. }), "{err:?}");
    assert!(
        !h.game_dir.parent().unwrap().join("escaped.txt").exists(),
        "an archive escaped the staging directory"
    );
}

#[tokio::test]
async fn the_api_key_never_appears_in_stored_state() {
    let h = Harness::new(&[("Mod/archive/pc/mod/a.archive", b"payload")]).await;
    h.onera.set_api_key(Secret::new(API_KEY)).await.unwrap();
    h.onera.confirm_game(&h.discovered()).await.unwrap();
    h.onera
        .fetch_mod(GAME_SLUG, &ProviderModId::new(MOD_ID), &CancelToken::new())
        .await
        .unwrap();

    // Walk every file Onera wrote and confirm the key is in none of them.
    let mut checked = 0;
    for entry in walkdir(&h.onera.paths.data)
        .into_iter()
        .chain(walkdir(&h.onera.paths.state))
    {
        let Ok(bytes) = std::fs::read(&entry) else {
            continue;
        };
        checked += 1;
        assert!(
            !bytes
                .windows(API_KEY.len())
                .any(|w| w == API_KEY.as_bytes()),
            "the API key was written to {}",
            entry.display()
        );
    }
    assert!(
        checked > 0,
        "nothing was written, so the check proved nothing"
    );
}

#[tokio::test]
async fn an_unavailable_secret_store_refuses_rather_than_writing_plain_text() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/users/validate.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "user_id": 1, "name": "TestUser"
        })))
        .mount(&server)
        .await;

    let config = NexusConfig {
        v3_base: format!("{}/v3", server.uri()),
        v1_base: format!("{}/v1", server.uri()),
        ..NexusConfig::default()
    };
    let secrets = Arc::new(InMemorySecretStore::unavailable());
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

    let err = onera.set_api_key(Secret::new(API_KEY)).await.unwrap_err();
    assert!(matches!(err, CoreError::SecretStore(_)), "{err:?}");
    assert!(!onera.is_authenticated().await.unwrap());

    // And nothing was written to disk as a consolation prize.
    for entry in walkdir(dir.path()) {
        let Ok(bytes) = std::fs::read(&entry) else {
            continue;
        };
        assert!(!bytes
            .windows(API_KEY.len())
            .any(|w| w == API_KEY.as_bytes()));
    }
}

#[tokio::test]
async fn a_second_install_of_the_same_archive_is_deduplicated() {
    let h = Harness::new(&[("Mod/archive/pc/mod/a.archive", b"payload")]).await;
    h.onera.set_api_key(Secret::new(API_KEY)).await.unwrap();
    let game = h.onera.confirm_game(&h.discovered()).await.unwrap();
    let details = h
        .onera
        .fetch_mod(GAME_SLUG, &ProviderModId::new(MOD_ID), &CancelToken::new())
        .await
        .unwrap();

    let first = h
        .onera
        .prepare_install(
            &install_request(&h, game, &details),
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
    let second = h
        .onera
        .prepare_install(
            &install_request(&h, game, &details),
            &NullProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(
        first.archive_hash, second.archive_hash,
        "the same bytes must hash the same"
    );
    assert_eq!(
        first.archive_id, second.archive_id,
        "the archive record must be reused rather than duplicated"
    );
    // Each preparation gets its own staging directory even so.
    assert_ne!(first.staging, second.staging);
    let _ = h.archive_bytes.len();
    let _ = &h.server;
}

#[tokio::test]
async fn profile_crud_duplicates_desired_state_without_touching_the_game() {
    let h = Harness::new(&[("Mod/archive/pc/mod/a.archive", b"payload")]).await;
    let game = h.onera.confirm_game(&h.discovered()).await.unwrap();
    let before = game_snapshot(&h.game_dir);

    let default = h.onera.profiles(game).await.unwrap();
    assert_eq!(default.len(), 1);
    assert!(default[0].is_active);
    assert_eq!(default[0].name, "Default");

    h.onera.set_api_key(Secret::new(API_KEY)).await.unwrap();
    let details = h
        .onera
        .fetch_mod(GAME_SLUG, &ProviderModId::new(MOD_ID), &CancelToken::new())
        .await
        .unwrap();
    let custom = h
        .onera
        .create_profile(game, "Custom".into(), Some("desired only".into()), None)
        .await
        .unwrap();
    let mut member = h
        .onera
        .add_profile_member(
            custom.id,
            details.mod_id,
            Some(details.primary_file().unwrap().provider_file_id.clone()),
        )
        .await
        .unwrap();
    assert!(member.installation_id.is_none());

    member = h
        .onera
        .set_member_pin(member.id, true, Some("known good".into()))
        .await
        .unwrap();
    assert!(matches!(member.pin, MemberPin::Pinned { .. }));
    member = h
        .onera
        .set_member_state(member.id, DesiredModState::Disabled)
        .await
        .unwrap();
    assert_eq!(member.desired, DesiredModState::Disabled);
    member = h
        .onera
        .reorder_profile_member(member.id, MemberPriority(-20))
        .await
        .unwrap();
    assert_eq!(member.priority, MemberPriority(-20));

    let copy = h
        .onera
        .create_profile(game, "Copy".into(), None, Some(custom.id))
        .await
        .unwrap();
    let copied = h.onera.profile_details(copy.id).await.unwrap();
    assert_eq!(copied.members.len(), 1);
    assert_ne!(copied.members[0].id, member.id);
    assert_eq!(copied.members[0].selection, member.selection);
    assert_eq!(copied.members[0].priority, member.priority);

    assert!(matches!(
        h.onera
            .create_profile(game, "copy".into(), None, None)
            .await
            .unwrap_err(),
        CoreError::Conflict(_)
    ));
    assert!(matches!(
        h.onera.delete_profile(default[0].id).await.unwrap_err(),
        CoreError::Conflict(_)
    ));
    h.onera.remove_profile_member(member.id).await.unwrap();
    assert!(h
        .onera
        .profile_details(custom.id)
        .await
        .unwrap()
        .members
        .is_empty());
    h.onera.delete_profile(custom.id).await.unwrap();

    assert_eq!(game_snapshot(&h.game_dir), before);
    assert!(h
        .onera
        .database()
        .active_installations(game)
        .await
        .unwrap()
        .is_empty());
}

fn walkdir(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walkdir(&path));
        } else {
            out.push(path);
        }
    }
    out
}
