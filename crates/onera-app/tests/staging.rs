//! Moving a game's staging directory.
//!
//! Everything under a staging root is deleted when Onera starts, so the rules
//! about which directory may become one are not ergonomics — they are the only
//! thing standing between a settings change and a wiped game directory. That is
//! what this file is about: the refusals, and the promise that unfinished work
//! in the old directory travels to the new one rather than being abandoned.

use async_trait::async_trait;
use onera_app::{Onera, Paths};
use onera_core::domain::game::{Game, InstallSource, InstallValidation};
use onera_core::ids::{LocalGameId, ProviderFileId, ProviderId, ProviderModId};
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
        std::fs::create_dir_all(game_dir.join("archive/pc/mod")).unwrap();
        std::fs::create_dir_all(game_dir.join("r6/scripts")).unwrap();
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

    /// An empty directory outside everything Onera protects.
    fn empty(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join("elsewhere").join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}

#[tokio::test]
async fn a_game_stages_in_oneras_own_directory_until_it_is_told_otherwise() {
    let h = Harness::new().await;
    let game = h.register().await;

    let info = h.onera.staging_info(game).await.unwrap();
    assert!(info.is_default);
    assert_eq!(info.root, h.onera.paths.staging());
    assert_eq!(info.entries, 0);

    // Every operation still gets its own directory under it; the root is the
    // only thing the setting moves.
    let operation = onera_core::ids::OperationId::new();
    assert_eq!(
        h.onera.staging_for(game, operation).await.unwrap(),
        h.onera.paths.staging().join(operation.to_string())
    );
}

#[tokio::test]
async fn a_chosen_directory_becomes_the_root_every_extraction_happens_under() {
    let h = Harness::new().await;
    let game = h.register().await;
    let chosen = h.empty("fast-disk");

    let change = h.onera.set_staging_root(game, &chosen).await.unwrap();
    assert_eq!(change.root, chosen);
    assert_eq!(change.moved, 0);

    let info = h.onera.staging_info(game).await.unwrap();
    assert_eq!(info.root, chosen);
    assert!(!info.is_default);

    let operation = onera_core::ids::OperationId::new();
    assert_eq!(
        h.onera.staging_for(game, operation).await.unwrap(),
        chosen.join(operation.to_string())
    );

    // And it survives a restart, because the setting is a row and not a field
    // on something that is re-detected.
    let reopened = Onera::assemble(
        Paths::rooted_at(h.dir.path().join("xdg")),
        Arc::new(NoAuth),
        Arc::new(NoProvider),
    )
    .await
    .unwrap();
    assert_eq!(reopened.staging_root(game).await.unwrap(), chosen);
}

#[tokio::test]
async fn unfinished_work_travels_with_the_setting() {
    let h = Harness::new().await;
    let game = h.register().await;

    // What a half-finished install leaves behind: a directory named after its
    // operation, with the extracted tree inside it.
    let old = h.onera.paths.staging();
    std::fs::create_dir_all(old.join("op-1/archive/pc/mod")).unwrap();
    std::fs::write(old.join("op-1/archive/pc/mod/a.archive"), b"staged bytes").unwrap();
    assert_eq!(h.onera.staging_info(game).await.unwrap().entries, 1);

    let chosen = h.empty("fast-disk");
    let change = h.onera.set_staging_root(game, &chosen).await.unwrap();

    assert_eq!(change.moved, 1);
    assert_eq!(
        std::fs::read(chosen.join("op-1/archive/pc/mod/a.archive")).unwrap(),
        b"staged bytes",
        "the extraction was carried over rather than abandoned"
    );
    assert!(!old.join("op-1").exists(), "and not left behind as well");
}

#[tokio::test]
async fn a_directory_with_anything_in_it_is_refused() {
    let h = Harness::new().await;
    let game = h.register().await;

    let occupied = h.empty("photos");
    std::fs::write(occupied.join("holiday.jpg"), b"not a mod").unwrap();

    let error = h
        .onera
        .set_staging_root(game, &occupied)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("not empty"), "{error}");
    // The refusal is total: the setting is unchanged and the file is untouched.
    assert!(h.onera.staging_info(game).await.unwrap().is_default);
    assert!(occupied.join("holiday.jpg").exists());
}

#[tokio::test]
async fn a_directory_holding_something_that_matters_is_refused() {
    let h = Harness::new().await;
    let game = h.register().await;

    // Each of these would be deleted, wholly or in part, on the next startup.
    let library = h.game_dir.parent().unwrap().to_path_buf();
    let empty_inside_the_game = h.game_dir.join("staging");
    std::fs::create_dir_all(&empty_inside_the_game).unwrap();

    for candidate in [
        h.game_dir.clone(),
        library,
        empty_inside_the_game,
        h.onera.paths.data.clone(),
        h.onera.paths.config.clone(),
    ] {
        let error = h
            .onera
            .set_staging_root(game, &candidate)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("staging") || error.contains("not empty"),
            "{} was accepted: {error}",
            candidate.display()
        );
    }
    assert!(h.game_dir.join("bin/x64/Cyberpunk2077.exe").exists());
    assert!(h.onera.staging_info(game).await.unwrap().is_default);
}

/// The rule is "must not swallow something", not "must not be inside
/// anything": a staging directory in the user's own home is the obvious
/// choice, and refusing it would leave nowhere sensible to put one.
#[tokio::test]
async fn an_empty_directory_nested_under_the_users_own_is_accepted() {
    let h = Harness::new().await;
    let game = h.register().await;

    let nested = h.dir.path().join("home/documents/onera staging");
    std::fs::create_dir_all(&nested).unwrap();
    let change = h.onera.set_staging_root(game, &nested).await.unwrap();
    assert_eq!(change.root, nested);
}

#[tokio::test]
async fn a_relative_path_is_refused_rather_than_resolved() {
    let h = Harness::new().await;
    let game = h.register().await;
    let error = h
        .onera
        .set_staging_root(game, Path::new("staging"))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("absolute"), "{error}");
}

#[tokio::test]
async fn a_game_can_be_put_back_on_the_default_root() {
    let h = Harness::new().await;
    let game = h.register().await;
    let chosen = h.empty("fast-disk");
    h.onera.set_staging_root(game, &chosen).await.unwrap();

    std::fs::create_dir_all(chosen.join("op-2")).unwrap();
    let change = h.onera.reset_staging_root(game).await.unwrap();

    assert_eq!(change.root, h.onera.paths.staging());
    assert_eq!(change.moved, 1);
    assert!(h.onera.paths.staging().join("op-2").exists());
    assert!(h.onera.staging_info(game).await.unwrap().is_default);
}

#[tokio::test]
async fn the_startup_sweep_reaches_a_chosen_root_too() {
    // A leftover extraction is stale wherever the user put it, and leaving one
    // behind would make the next install plan against a directory it did not
    // create.
    let h = Harness::new().await;
    let game = h.register().await;
    let chosen = h.empty("fast-disk");
    h.onera.set_staging_root(game, &chosen).await.unwrap();
    std::fs::create_dir_all(chosen.join("op-3/archive")).unwrap();

    let _reopened = Onera::assemble(
        Paths::rooted_at(h.dir.path().join("xdg")),
        Arc::new(NoAuth),
        Arc::new(NoProvider),
    )
    .await
    .unwrap();

    assert!(chosen.exists(), "the root itself stays");
    assert!(
        !chosen.join("op-3").exists(),
        "but what an unfinished operation left in it does not"
    );
}
