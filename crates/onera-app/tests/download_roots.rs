//! Where downloads are kept: the shared setting, a game's own, and the move.
//!
//! Two things make this worth testing at the application layer rather than by
//! reading the code. The resolution order decides where every future download
//! lands, and the move has to rewrite the catalogue as well as the disk — an
//! archive whose row still names the old path is an archive Onera has lost,
//! even though the bytes are right there.

use async_trait::async_trait;
use onera_app::{DownloadScope, Onera, Paths};
use onera_core::domain::game::{Game, InstallSource, InstallValidation};
use onera_core::domain::release::{FileCategory, Mod, ProviderFile, Release};
use onera_core::hash::FileHash;
use onera_core::ids::{
    LocalGameId, ModId, ProviderFileId, ProviderId, ProviderModId, ReleaseId,
};
use onera_core::ports::{
    AccountInfo, AuthProvider, Credential, DownloadGrant, DownloadTarget, ModProvider, Page,
};
use onera_core::progress::CancelToken;
use onera_core::CoreError;
use onera_discovery::DiscoveredGame;
use std::path::{Path, PathBuf};
use std::sync::Arc;

struct NoProvider;

#[async_trait]
impl ModProvider for NoProvider {
    fn id(&self) -> ProviderId {
        ProviderId::nexus()
    }
    async fn games(&self, _: Option<&str>, _: &CancelToken) -> onera_core::Result<Page<Game>> {
        Ok(Page::single(vec![]))
    }
    async fn mod_metadata(
        &self,
        _: &str,
        _: &ProviderModId,
        _: &CancelToken,
    ) -> onera_core::Result<(Mod, Vec<Release>)> {
        Err(CoreError::Unsupported("no provider in this test".into()))
    }
    async fn files(
        &self,
        _: &str,
        _: &ProviderModId,
        _: Option<&str>,
        _: &CancelToken,
    ) -> onera_core::Result<Page<ProviderFile>> {
        Ok(Page::single(vec![]))
    }
    async fn resolve_download(
        &self,
        _: &str,
        _: &ProviderModId,
        _: &ProviderFileId,
        _: Option<&DownloadGrant>,
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

struct Harness {
    onera: Onera,
    dir: tempfile::TempDir,
    game_dir: PathBuf,
}

impl Harness {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let game_dir = dir.path().join("library/Cyberpunk 2077");
        std::fs::create_dir_all(game_dir.join("bin/x64")).unwrap();
        std::fs::write(game_dir.join("bin/x64/Cyberpunk2077.exe"), b"MZ fake").unwrap();

        let onera = Onera::assemble(
            Paths::rooted_at(dir.path().join("xdg")),
            Arc::new(NoAuth),
            Arc::new(NoProvider),
        )
        .await
        .unwrap();
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
        Self {
            onera,
            dir,
            game_dir,
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
                source: InstallSource::Manual,
                validation: InstallValidation::ok(),
            })
            .await
            .unwrap()
    }

    fn directory(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join("elsewhere").join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// Record a downloaded archive for one mod of `game_slug`, with real bytes
    /// on disk in Onera's own directory, exactly as a download leaves it.
    async fn store_archive(&self, game_slug: &str, name: &str, bytes: &[u8]) -> PathBuf {
        let db = self.onera.database();
        let mod_id = db
            .upsert_mod(&Mod {
                id: ModId::new(),
                provider: ProviderId::nexus(),
                provider_mod_id: ProviderModId::new(name),
                game_slug: game_slug.to_owned(),
                name: name.to_owned(),
                author: None,
                thumbnail_url: None,
            })
            .await
            .unwrap();
        let release = db
            .upsert_release(&Release {
                id: ReleaseId::new(),
                mod_id,
                version: "1.0".into(),
                published_at: None,
                metadata: serde_json::json!({}),
            })
            .await
            .unwrap();
        let provider_file = ProviderFileId::new(format!("file-{name}"));
        db.upsert_provider_file(&ProviderFile {
            provider: ProviderId::nexus(),
            provider_file_id: provider_file.clone(),
            provider_version_id: None,
            provider_download_id: None,
            provider_file_group_id: None,
            position: None,
            release_id: release,
            name: format!("{name}.zip"),
            size_bytes: Some(bytes.len() as u64),
            category: FileCategory::Main,
            published_hash: None,
            uploaded_at: None,
            is_primary: true,
        })
        .await
        .unwrap();

        let hash = FileHash::blake3_of(bytes);
        let path = self
            .onera
            .paths
            .archives()
            .join(hash.algorithm.as_str())
            .join(&hash.hex[..2])
            .join(&hash.hex);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();

        let archive = db
            .upsert_archive(
                &hash,
                bytes.len() as u64,
                &format!("{name}.zip"),
                onera_core::domain::archive::ArchiveFormat::Zip,
                &path,
            )
            .await
            .unwrap();
        db.link_archive_provider_file(archive, &ProviderId::nexus(), &provider_file)
            .await
            .unwrap();
        path
    }
}

#[tokio::test]
async fn the_most_specific_setting_wins() {
    let h = Harness::new().await;
    let game = h.register().await;

    // Nothing chosen: Onera's own directory, for every game.
    let info = h.onera.download_dir_info(Some(game)).await.unwrap();
    assert_eq!(info.root, h.onera.paths.archives());
    assert_eq!(info.scope, DownloadScope::Default);

    // A shared choice covers every game that has not made its own.
    let shared = h.directory("all-mods");
    h.onera.set_download_root(&shared).await.unwrap();
    let info = h.onera.download_dir_info(Some(game)).await.unwrap();
    assert_eq!(info.root, shared);
    assert_eq!(info.scope, DownloadScope::Global);

    // The game's own choice overrides it, and remembers what it overrode.
    let mine = h.directory("cp2077-mods");
    h.onera.set_game_download_root(game, &mine).await.unwrap();
    let info = h.onera.download_dir_info(Some(game)).await.unwrap();
    assert_eq!(info.root, mine);
    assert_eq!(info.scope, DownloadScope::Game);
    assert_eq!(info.inherited.as_deref(), Some(shared.as_path()));

    // And a download names its game by the provider's slug, which is how a
    // transfer in flight finds the same answer.
    assert_eq!(
        h.onera
            .download_root_for_slug("cyberpunk2077")
            .await
            .unwrap(),
        mine
    );
    // A game Onera does not manage falls back to the shared setting.
    assert_eq!(
        h.onera
            .download_root_for_slug("skyrimspecialedition")
            .await
            .unwrap(),
        shared
    );
}

#[tokio::test]
async fn a_setting_survives_a_restart() {
    let h = Harness::new().await;
    let game = h.register().await;
    let mine = h.directory("cp2077-mods");
    h.onera.set_game_download_root(game, &mine).await.unwrap();

    let reopened = Onera::assemble(
        Paths::rooted_at(h.dir.path().join("xdg")),
        Arc::new(NoAuth),
        Arc::new(NoProvider),
    )
    .await
    .unwrap();
    assert_eq!(reopened.download_root(Some(game)).await.unwrap(), mine);
}

#[tokio::test]
async fn the_archives_move_and_the_catalogue_follows_them() {
    let h = Harness::new().await;
    let game = h.register().await;
    let old = h.store_archive("cyberpunk2077", "archivexl", b"archive bytes").await;
    assert!(old.exists());

    let mine = h.directory("cp2077-mods");
    let change = h.onera.set_game_download_root(game, &mine).await.unwrap();

    assert_eq!(change.moved, 1);
    assert_eq!(change.bytes, b"archive bytes".len() as u64);
    assert!(!old.exists(), "the archive did not stay behind");

    // The catalogue names the new location, so nothing has to be downloaded
    // again — which is the entire point of moving rather than re-fetching.
    let stored = h.onera.database().archives().await.unwrap();
    assert_eq!(stored.len(), 1);
    assert!(stored[0].path.starts_with(&mine), "{:?}", stored[0].path);
    assert!(stored[0].path.exists());
    assert_eq!(std::fs::read(&stored[0].path).unwrap(), b"archive bytes");

    let info = h.onera.download_dir_info(Some(game)).await.unwrap();
    assert_eq!(info.archives, 1);
}

#[tokio::test]
async fn one_games_directory_leaves_another_games_downloads_alone() {
    let h = Harness::new().await;
    let game = h.register().await;
    let ours = h
        .store_archive("cyberpunk2077", "archivexl", b"ours")
        .await;
    let theirs = h.store_archive("skyrimspecialedition", "skse", b"theirs").await;

    let mine = h.directory("cp2077-mods");
    h.onera.set_game_download_root(game, &mine).await.unwrap();

    assert!(!ours.exists());
    assert!(
        theirs.exists(),
        "another game's archive is not this game's to move"
    );
}

#[tokio::test]
async fn a_shared_move_carries_only_what_no_game_claimed_for_itself() {
    let h = Harness::new().await;
    let game = h.register().await;

    // This game's archive, moved into the directory it chose for itself.
    h.store_archive("cyberpunk2077", "archivexl", b"ours").await;
    let mine = h.directory("cp2077-mods");
    h.onera.set_game_download_root(game, &mine).await.unwrap();

    // Another game's, which is still sitting in the shared directory.
    let skyrim = h
        .store_archive("skyrimspecialedition", "skse", b"theirs")
        .await;

    let shared = h.directory("all-mods");
    let change = h.onera.set_download_root(&shared).await.unwrap();

    assert_eq!(change.moved, 1, "only what was in the shared directory moves");
    assert!(!skyrim.exists());

    let stored = h.onera.database().archives().await.unwrap();
    let ours = stored
        .iter()
        .find(|a| a.path.starts_with(&mine))
        .expect("the game that chose its own directory kept its archive there");
    assert!(ours.path.exists());
    assert!(
        stored.iter().any(|a| a.path.starts_with(&shared)),
        "and the unclaimed one followed the shared setting"
    );
}

#[tokio::test]
async fn a_game_can_be_put_back_on_the_shared_directory() {
    let h = Harness::new().await;
    let game = h.register().await;
    let mine = h.directory("cp2077-mods");
    h.onera.set_game_download_root(game, &mine).await.unwrap();
    h.store_archive("cyberpunk2077", "archivexl", b"ours").await;

    let change = h.onera.reset_game_download_root(game).await.unwrap();
    assert_eq!(change.root, h.onera.paths.archives());
    let info = h.onera.download_dir_info(Some(game)).await.unwrap();
    assert_eq!(info.scope, DownloadScope::Default);
}

#[tokio::test]
async fn a_directory_that_would_endanger_the_archives_is_refused() {
    let h = Harness::new().await;
    let game = h.register().await;

    // Inside the game: verification and the baseline would read the archives as
    // game files that nobody installed.
    let inside_game = h.game_dir.join("mods");
    std::fs::create_dir_all(&inside_game).unwrap();
    // Inside staging: swept on the next startup.
    let inside_staging = h.onera.paths.staging().join("downloads");
    std::fs::create_dir_all(&inside_staging).unwrap();

    for candidate in [inside_game, inside_staging] {
        let error = h
            .onera
            .set_game_download_root(game, &candidate)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("inside"),
            "{} was accepted: {error}",
            candidate.display()
        );
    }
    let error = h
        .onera
        .set_download_root(Path::new("mods"))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("absolute"), "{error}");

    assert_eq!(
        h.onera.download_dir_info(Some(game)).await.unwrap().scope,
        DownloadScope::Default
    );
}

#[tokio::test]
async fn a_directory_that_already_holds_something_is_perfectly_acceptable() {
    // The opposite of a staging root, and for a reason: nothing ever deletes
    // the contents of a download directory, so there is nothing to protect.
    let h = Harness::new().await;
    let game = h.register().await;
    let occupied = h.directory("downloads");
    std::fs::write(occupied.join("something-else.txt"), b"not Onera's").unwrap();

    h.onera
        .set_game_download_root(game, &occupied)
        .await
        .unwrap();
    assert!(occupied.join("something-else.txt").exists());
}
