//! The end-to-end flows the drivers call.
//!
//! One type, [`Onera`], holds every wired-up port. The Tauri commands, the CLI
//! and the Native Messaging host are all thin translations onto the methods
//! here — none of them contains logic of its own.
//!
//! The headline flow is:
//!
//! ```text
//! discover -> authenticate -> resolve a mod id -> pick a file -> download
//!   -> inspect and extract -> map onto deployment roots -> plan -> apply
//!   -> verify -> remove -> restore
//! ```

use onera_archive::SafeArchiveBackend;
use onera_core::domain::game::{Game, LocalGameInstall};
use onera_core::domain::release::{ProviderFile, Release};
use onera_core::hash::FileHash;
use onera_core::ids::{
    ArchiveId, InstallationId, LocalGameId, ModId, ProviderFileId, ProviderId, ProviderModId,
    ReleaseId,
};
use onera_core::plan::{InstallPlan, ScopedRule, TargetLocation};
use onera_core::ports::{
    AccountInfo, ArchiveBackend, ArchiveStore, AuthProvider, Credential, DeploymentStore,
    GameAdapter, ModProvider, SecretStore,
};
use onera_core::progress::{CancelToken, ProgressSink};
use onera_core::redact::Secret;
use onera_core::{CoreError, Result};
use onera_db::backup::FileBackupStore;
use onera_db::Database;
use onera_discovery::DiscoveredGame;
use onera_download::{ContentAddressedStore, DownloadConfig, Downloader};
use onera_install::planner::{plan_install, PlanRequest, RootMap};
use onera_install::remove::{ModifiedFilePolicy, RemovalReport, Remover};
use onera_install::{
    recover_all, verify_installation, GameLocks, InstallReport, Installer, InterruptedOperation,
    RealFileSystem, VerifyReport,
};
use onera_nexus::{ApiKeyAuth, NexusClient, NexusConfig};
use std::path::PathBuf;
use std::sync::Arc;

/// How long a cached game catalogue is considered fresh.
///
/// The list of games Nexus supports changes a few times a month; refetching it
/// on every launch would waste a request from a limited hourly budget.
pub const CATALOGUE_TTL_HOURS: i64 = 24;

/// A fully wired Onera instance.
pub struct Onera {
    /// Resolved XDG directories.
    pub paths: crate::Paths,
    db: Database,
    provider: Arc<dyn ModProvider>,
    auth: Arc<dyn AuthProvider>,
    archives: Arc<dyn ArchiveBackend>,
    downloader: Arc<Downloader>,
    installer: Arc<Installer>,
    remover: Arc<Remover>,
    locks: GameLocks,
}

impl Onera {
    /// Build an instance with the shipped adapters.
    ///
    /// # Errors
    /// Fails if the directories cannot be created, the database cannot be
    /// opened or migrated, or the HTTP stack cannot be initialized.
    pub async fn new(paths: crate::Paths) -> Result<Self> {
        let secrets: Arc<dyn SecretStore> = Arc::new(crate::KeyringSecretStore::default());
        Self::with_secret_store(paths, secrets).await
    }

    /// Build an instance with a specific secret store.
    ///
    /// # Errors
    /// As [`Onera::new`].
    pub async fn with_secret_store(
        paths: crate::Paths,
        secrets: Arc<dyn SecretStore>,
    ) -> Result<Self> {
        let config = NexusConfig::default();
        let auth: Arc<dyn AuthProvider> = Arc::new(ApiKeyAuth::new(
            secrets,
            config.v1_base.clone(),
            &config.user_agent,
        )?);
        let provider: Arc<dyn ModProvider> = Arc::new(NexusClient::new(config, Arc::clone(&auth))?);
        Self::assemble(paths, auth, provider).await
    }

    /// Build an instance with explicit auth and provider adapters.
    ///
    /// This is the constructor the end-to-end tests use to point Onera at a mock
    /// server. Production code calls [`Onera::new`].
    ///
    /// # Errors
    /// As [`Onera::new`].
    pub async fn assemble(
        paths: crate::Paths,
        auth: Arc<dyn AuthProvider>,
        provider: Arc<dyn ModProvider>,
    ) -> Result<Self> {
        Self::assemble_with(paths, auth, provider, false).await
    }

    /// As [`Onera::assemble`], but optionally allowing plain-HTTP downloads.
    ///
    /// `allow_plain_http` exists solely so the end-to-end tests can serve
    /// archives from a local mock server. Every production constructor passes
    /// `false`, and the downloader then refuses anything but HTTPS.
    ///
    /// # Errors
    /// As [`Onera::new`].
    pub async fn assemble_with(
        paths: crate::Paths,
        auth: Arc<dyn AuthProvider>,
        provider: Arc<dyn ModProvider>,
        allow_plain_http: bool,
    ) -> Result<Self> {
        paths.ensure().await?;
        let db = Database::open(&paths.database()).await?;
        db.upsert_provider(
            &ProviderId::nexus(),
            "Nexus Mods",
            onera_nexus::DEFAULT_V3_BASE,
        )
        .await?;

        let archive_store: Arc<dyn ArchiveStore> =
            Arc::new(ContentAddressedStore::new(paths.archives()));
        // The store is owned by the downloader; nothing above it needs to
        // address archive storage directly.
        let build_downloader = if allow_plain_http {
            Downloader::new_for_tests
        } else {
            Downloader::new
        };
        let downloader = Arc::new(build_downloader(
            archive_store,
            paths.downloads(),
            DownloadConfig::default(),
        )?);
        let backups = Arc::new(FileBackupStore::new(db.clone(), paths.backups()));
        let fs = Arc::new(RealFileSystem);

        Ok(Self {
            db: db.clone(),
            provider,
            auth,
            archives: Arc::new(SafeArchiveBackend::default()),
            downloader,
            installer: Arc::new(Installer::new(
                fs.clone(),
                Arc::new(db.clone()),
                Arc::new(db.clone()),
                backups.clone(),
            )),
            remover: Arc::new(Remover::new(fs, Arc::new(db.clone()), backups)),
            locks: GameLocks::new(),
            paths,
        })
    }

    /// The database, for drivers that need to read catalogue tables directly.
    #[must_use]
    pub fn database(&self) -> &Database {
        &self.db
    }

    // -----------------------------------------------------------------------
    // Authentication
    // -----------------------------------------------------------------------

    /// Whether a credential is stored.
    ///
    /// # Errors
    /// Fails if the secret store is unavailable.
    pub async fn is_authenticated(&self) -> Result<bool> {
        self.auth.is_authenticated().await
    }

    /// Validate and store a personal API key.
    ///
    /// The key is validated against the provider before it is written, and it is
    /// written only to the platform secret store. The returned account is what
    /// the onboarding screen shows so the user can confirm they signed in as
    /// themselves.
    ///
    /// # Errors
    /// Returns [`CoreError::Unauthenticated`] if the provider rejects the key,
    /// or [`CoreError::SecretStore`] if it cannot be stored. There is no
    /// plaintext fallback.
    pub async fn set_api_key(&self, key: Secret) -> Result<AccountInfo> {
        let account = self.auth.store(Credential::ApiKey(key)).await?;
        tracing::info!(username = %account.username, "authenticated with Nexus Mods");
        Ok(account)
    }

    /// Delete the stored credential.
    ///
    /// # Errors
    /// Fails if the secret store is unavailable.
    pub async fn forget_api_key(&self) -> Result<()> {
        self.auth.forget().await
    }

    /// Confirm who the stored credential belongs to.
    ///
    /// # Errors
    /// Fails if nothing is stored or the provider rejects the credential.
    pub async fn account(&self) -> Result<AccountInfo> {
        let credential = self.auth.credential().await?;
        self.auth.validate(&credential).await
    }

    // -----------------------------------------------------------------------
    // Games
    // -----------------------------------------------------------------------

    /// Return the supported-game catalogue, refreshing it if it is stale.
    ///
    /// # Errors
    /// Falls back to the cache on a network failure and only errors when there
    /// is no cache either.
    pub async fn supported_games(&self, cancel: &CancelToken) -> Result<Vec<Game>> {
        let provider = ProviderId::nexus();
        let cached_at = self.db.games_cached_at(&provider).await?;
        let stale = cached_at.is_none_or(|at| {
            chrono::Utc::now() - at > chrono::Duration::hours(CATALOGUE_TTL_HOURS)
        });

        if stale {
            match self.provider.games(None, cancel).await {
                Ok(page) => {
                    for game in &page.items {
                        self.db.upsert_game(game).await?;
                    }
                }
                Err(e) => {
                    // A stale catalogue is far better than no game list; the
                    // only fatal case is having neither.
                    tracing::warn!(error = %e, "could not refresh the game catalogue; using the cache");
                    if cached_at.is_none() {
                        return Err(e);
                    }
                }
            }
        }
        self.db.games(&provider).await
    }

    /// Scan the machine for installed games Onera can manage.
    ///
    /// Results are candidates only: nothing is registered until
    /// [`Onera::confirm_game`] is called. A wrong match would aim deployments at
    /// the wrong directory, so the user always confirms.
    ///
    /// # Errors
    /// Propagates errors from reading Steam metadata.
    pub async fn discover_games(&self, cancel: &CancelToken) -> Result<Vec<DiscoveredGame>> {
        let catalogue = self.supported_games(cancel).await.unwrap_or_default();
        let adapters = onera_games::all_adapters();
        onera_discovery::discover(&onera_discovery::steam::home_dir()?, &adapters, &catalogue)
    }

    /// Register a game the user confirmed, or a manual path.
    ///
    /// # Errors
    /// Fails if the adapter is unknown, the directory does not validate, or the
    /// game has no catalogue entry.
    pub async fn confirm_game(&self, discovered: &DiscoveredGame) -> Result<LocalGameId> {
        let adapter = onera_games::adapter_by_id(&discovered.adapter_id).ok_or_else(|| {
            CoreError::Unsupported(format!("no adapter named {:?}", discovered.adapter_id))
        })?;
        let validation = adapter.validate_install(&discovered.install_root);
        if !validation.valid {
            return Err(CoreError::InvalidGameInstall(
                validation.findings.join("; "),
            ));
        }

        let slug = discovered
            .provider_slug
            .clone()
            .or_else(|| adapter.provider_slugs().first().map(|s| (*s).to_owned()))
            .ok_or_else(|| {
                CoreError::InvalidGameInstall("the adapter claims no provider game".into())
            })?;
        let game_id = self
            .db
            .upsert_game(&Game {
                id: onera_core::ids::GameId::new(),
                provider: ProviderId::nexus(),
                provider_slug: slug,
                name: discovered.name.clone(),
                steam_app_id: adapter.steam_app_ids().first().copied(),
            })
            .await?;

        let local = self
            .db
            .upsert_local_install(&LocalGameInstall {
                id: LocalGameId::new(),
                game_id,
                adapter_id: adapter.id().to_owned(),
                source: discovered.source,
                install_root: discovered.install_root.clone(),
                compat_prefix: discovered.compat_prefix.clone(),
                user_data_roots: discovered.user_data_roots.clone(),
                confirmed: true,
            })
            .await?;
        self.db.confirm_local_install(local).await?;
        self.db.set_adapter_version(adapter.id(), 1).await?;
        Ok(local)
    }

    /// Every registered game installation.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn local_games(&self) -> Result<Vec<LocalGameInstall>> {
        self.db.local_installs().await
    }

    /// Resolve a game's deployment roots through its adapter.
    ///
    /// # Errors
    /// Fails if the game or its adapter is unknown.
    pub async fn roots_for(
        &self,
        game: LocalGameId,
    ) -> Result<(RootMap, &'static dyn GameAdapter)> {
        let install = self
            .db
            .local_installs()
            .await?
            .into_iter()
            .find(|i| i.id == game)
            .ok_or_else(|| CoreError::NotFound {
                kind: "game installation",
                id: game.to_string(),
            })?;
        let adapter = onera_games::adapter_by_id(&install.adapter_id).ok_or_else(|| {
            CoreError::Unsupported(format!("no adapter named {:?}", install.adapter_id))
        })?;
        let roots = adapter
            .deploy_roots(&install)?
            .into_iter()
            .map(|r| (r.key, r.path))
            .collect();
        Ok((roots, adapter))
    }

    // -----------------------------------------------------------------------
    // Mods
    // -----------------------------------------------------------------------

    /// Fetch a mod's metadata and files, caching them.
    ///
    /// This is the entry point the browser extension ultimately reaches: it
    /// supplies only a game domain and a mod id, and every other detail comes
    /// from the API.
    ///
    /// # Errors
    /// Propagates provider errors.
    pub async fn fetch_mod(
        &self,
        game_slug: &str,
        provider_mod_id: &ProviderModId,
        cancel: &CancelToken,
    ) -> Result<ModDetails> {
        let (the_mod, releases) = self
            .provider
            .mod_metadata(game_slug, provider_mod_id, cancel)
            .await?;
        let mod_id = self.db.upsert_mod(&the_mod).await?;

        let mut stored_releases = Vec::new();
        for release in releases {
            let release = Release { mod_id, ..release };
            let id = self.db.upsert_release(&release).await?;
            stored_releases.push(Release { id, ..release });
        }

        let files = self
            .provider
            .files(game_slug, provider_mod_id, None, cancel)
            .await?
            .items;

        // Each provider file is attached to the release whose publication date
        // and version it matches; the provider does not know Onera's ids.
        let mut stored_files = Vec::new();
        for file in files {
            let release_id = stored_releases
                .iter()
                .find(|r| r.published_at == file.uploaded_at)
                .or_else(|| stored_releases.first())
                .map(|r| r.id);
            let Some(release_id) = release_id else {
                continue;
            };
            let file = ProviderFile { release_id, ..file };
            self.db.upsert_provider_file(&file).await?;
            stored_files.push(file);
        }

        Ok(ModDetails {
            mod_id,
            name: the_mod.name,
            author: the_mod.author,
            game_slug: game_slug.to_owned(),
            provider_mod_id: provider_mod_id.clone(),
            releases: stored_releases,
            files: stored_files,
        })
    }

    // -----------------------------------------------------------------------
    // Install
    // -----------------------------------------------------------------------

    /// Download a file, extract it and build a plan — without writing to the
    /// game.
    ///
    /// Everything up to and including the dry-run preview happens here. Nothing
    /// in the game directory changes until [`Onera::apply`] is called with the
    /// returned plan.
    ///
    /// # Errors
    /// Propagates download, archive and planning errors. Returns
    /// [`CoreError::AmbiguousLayout`] when the adapter cannot map the archive
    /// unambiguously, which the UI turns into a question.
    pub async fn prepare_install(
        &self,
        request: &InstallRequest,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<PreparedInstall> {
        let (roots, adapter) = self.roots_for(request.local_game_id).await?;

        // 1. Download (or reuse a stored archive).
        let target = self
            .provider
            .resolve_download(
                &request.game_slug,
                &request.provider_mod_id,
                &request.provider_file_id,
                cancel,
            )
            .await?;
        let outcome = self
            .downloader
            .fetch(&target, request.expected_hash.as_ref(), progress, cancel)
            .await?;

        // 2. Inspect before extracting.
        let inspection = self.archives.inspect(&outcome.path, cancel).await?;
        tracing::info!(
            entries = inspection.entries.len(),
            rejected = inspection.rejected.len(),
            "inspected archive"
        );

        // 3. Extract into a staging directory unique to this operation.
        let staging_key = onera_core::ids::OperationId::new();
        let staging = self.paths.staging_for(staging_key);
        let manifest = self
            .archives
            .extract(&outcome.path, &staging, progress, cancel)
            .await?;

        let archive_id = self
            .db
            .upsert_archive(
                &outcome.hash,
                outcome.bytes,
                &request.filename,
                manifest.format,
                &outcome.path,
            )
            .await?;
        self.db
            .record_archive_entries(archive_id, &manifest)
            .await?;

        // 4. Map the archive onto deployment roots.
        let layout = adapter.resolve_layout(&manifest)?;

        // 5. Plan, without touching anything.
        let installation_id = InstallationId::new();
        let rules = self.db.rules_for(request.mod_id).await?;
        let plan = plan_install(
            PlanRequest {
                local_game_id: request.local_game_id,
                mod_id: request.mod_id,
                installation_id,
                manifest: &manifest,
                mappings: &layout.mappings,
                roots: &roots,
                adapter,
                rules: &rules,
            },
            &RealFileSystem,
            &self.db,
            progress,
            cancel,
        )
        .await?;

        Ok(PreparedInstall {
            plan,
            staging,
            roots,
            archive_id,
            archive_hash: outcome.hash,
            release_id: request.release_id,
            layout_rationale: layout.rationale,
            ignored: layout.ignored.len(),
            rejected_entries: inspection.rejected,
        })
    }

    /// Apply a prepared plan transactionally.
    ///
    /// Deployments into one game are serialized here: the per-game lock is held
    /// for the whole operation.
    ///
    /// # Errors
    /// Returns [`CoreError::DecisionRequired`] if conflicts remain unresolved.
    /// Any failure after work begins rolls back before returning.
    pub async fn apply(
        &self,
        prepared: &PreparedInstall,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<InstallReport> {
        let _guard = self.locks.acquire(prepared.plan.local_game_id).await;
        let report = self
            .installer
            .apply(
                &prepared.plan,
                &prepared.staging,
                &prepared.roots,
                prepared.release_id,
                prepared.archive_id,
                progress,
                cancel,
            )
            .await?;

        // Staging is only cleaned up once the operation is complete; if it
        // failed, the extracted tree is left for inspection and recovery.
        let _ = tokio::fs::remove_dir_all(&prepared.staging).await;
        Ok(report)
    }

    /// Remember a narrowly scoped conflict rule.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn remember_rule(&self, rule: &ScopedRule) -> Result<()> {
        self.db.put_rule(rule).await
    }

    // -----------------------------------------------------------------------
    // Verify, remove, recover
    // -----------------------------------------------------------------------

    /// Re-read every file an installation claims.
    ///
    /// # Errors
    /// Propagates store and filesystem errors.
    pub async fn verify(
        &self,
        game: LocalGameId,
        installation: InstallationId,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<VerifyReport> {
        let (roots, _) = self.roots_for(game).await?;
        verify_installation(
            game,
            installation,
            &roots,
            self.installer.filesystem().as_ref(),
            &self.db,
            progress,
            cancel,
        )
        .await
    }

    /// Show what removing an installation would do.
    ///
    /// # Errors
    /// Propagates store errors.
    pub async fn preview_removal(
        &self,
        game: LocalGameId,
        installation: InstallationId,
    ) -> Result<RemovalReport> {
        let (roots, _) = self.roots_for(game).await?;
        self.remover.preview(game, installation, &roots).await
    }

    /// Remove an installation and restore what it covered.
    ///
    /// # Errors
    /// Returns [`CoreError::DecisionRequired`] when files changed since they
    /// were deployed and `policy` is [`ModifiedFilePolicy::Ask`].
    pub async fn remove(
        &self,
        game: LocalGameId,
        installation: InstallationId,
        policy: ModifiedFilePolicy,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<RemovalReport> {
        let _guard = self.locks.acquire(game).await;
        let (roots, _) = self.roots_for(game).await?;
        self.remover
            .remove(game, installation, &roots, policy, progress, cancel)
            .await
    }

    /// The provider stack recorded for one deployed path.
    ///
    /// This is what the file-ownership-history view renders.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn ownership(
        &self,
        game: LocalGameId,
        target: &TargetLocation,
    ) -> Result<onera_core::domain::provider_stack::ProviderStack> {
        self.db.stack(game, target).await
    }

    /// Operations that were interrupted, with what can be done about each.
    ///
    /// Called on every launch.
    ///
    /// # Errors
    /// Propagates journal errors.
    pub async fn interrupted_operations(&self) -> Result<Vec<InterruptedOperation>> {
        recover_all(&self.installer).await
    }

    /// Roll an interrupted operation back.
    ///
    /// # Errors
    /// Fails if the operation is unknown or already terminal.
    pub async fn roll_back(
        &self,
        operation: onera_core::ids::OperationId,
        progress: &dyn ProgressSink,
    ) -> Result<()> {
        self.installer.rollback(operation, progress).await
    }
}

/// Everything needed to install one file.
#[derive(Debug, Clone)]
pub struct InstallRequest {
    /// Game to install into.
    pub local_game_id: LocalGameId,
    /// Provider slug of that game.
    pub game_slug: String,
    /// Mod lineage.
    pub mod_id: ModId,
    /// Release being installed.
    pub release_id: ReleaseId,
    /// Provider's mod identifier.
    pub provider_mod_id: ProviderModId,
    /// Provider's file identifier.
    pub provider_file_id: ProviderFileId,
    /// Filename, for display and for the archive record.
    pub filename: String,
    /// Hash to check against, when the provider published one.
    pub expected_hash: Option<FileHash>,
}

/// A downloaded, extracted, planned install that has not been applied.
#[derive(Debug)]
pub struct PreparedInstall {
    /// The dry-run plan. Show this before applying.
    pub plan: InstallPlan,
    /// Staging directory holding the extracted files.
    pub staging: PathBuf,
    /// Resolved deployment roots.
    pub roots: RootMap,
    /// The archive record.
    pub archive_id: ArchiveId,
    /// Hash of the archive.
    pub archive_hash: FileHash,
    /// Release being installed.
    pub release_id: ReleaseId,
    /// How the adapter arrived at its mapping, for the preview.
    pub layout_rationale: String,
    /// How many archive files the adapter ignored.
    pub ignored: usize,
    /// Entries the archive inspector refused, for the preview.
    pub rejected_entries: Vec<onera_core::domain::archive::RejectedEntry>,
}

/// A mod as Onera knows it after fetching metadata.
#[derive(Debug, Clone)]
pub struct ModDetails {
    /// Onera's mod identifier.
    pub mod_id: ModId,
    /// Display name.
    pub name: String,
    /// Author.
    pub author: Option<String>,
    /// Provider slug of the game.
    pub game_slug: String,
    /// Provider's mod identifier.
    pub provider_mod_id: ProviderModId,
    /// Published releases.
    pub releases: Vec<Release>,
    /// Downloadable files.
    pub files: Vec<ProviderFile>,
}

impl ModDetails {
    /// The file the mod page offers by default, if there is one.
    #[must_use]
    pub fn primary_file(&self) -> Option<&ProviderFile> {
        self.files.iter().find(|f| f.is_primary)
    }

    /// Whether the user must choose between several plausible files.
    ///
    /// More than one main-category file, and no primary, means Onera cannot pick
    /// for the user — so it asks instead of guessing.
    #[must_use]
    pub fn needs_file_selection(&self) -> bool {
        if self.primary_file().is_some() {
            return false;
        }
        self.selectable_files().count() != 1
    }

    /// Files worth offering: current, downloadable ones.
    pub fn selectable_files(&self) -> impl Iterator<Item = &ProviderFile> {
        use onera_core::domain::release::FileCategory;
        self.files.iter().filter(|f| {
            matches!(
                f.category,
                FileCategory::Main | FileCategory::Optional | FileCategory::Update
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onera_core::domain::release::FileCategory;

    fn file(name: &str, category: FileCategory, primary: bool) -> ProviderFile {
        ProviderFile {
            provider: ProviderId::nexus(),
            provider_file_id: ProviderFileId::new(name),
            release_id: ReleaseId::new(),
            name: name.to_owned(),
            size_bytes: Some(1),
            category,
            published_hash: None,
            uploaded_at: None,
            is_primary: primary,
        }
    }

    fn details(files: Vec<ProviderFile>) -> ModDetails {
        ModDetails {
            mod_id: ModId::new(),
            name: "A mod".into(),
            author: None,
            game_slug: "cyberpunk2077".into(),
            provider_mod_id: ProviderModId::new("107"),
            releases: vec![],
            files,
        }
    }

    #[test]
    fn a_primary_file_is_chosen_without_asking() {
        let d = details(vec![
            file("main.zip", FileCategory::Main, true),
            file("optional.zip", FileCategory::Optional, false),
        ]);
        assert_eq!(d.primary_file().unwrap().name, "main.zip");
        assert!(!d.needs_file_selection());
    }

    #[test]
    fn several_plausible_files_and_no_primary_means_asking() {
        let d = details(vec![
            file("standard.zip", FileCategory::Main, false),
            file("hd-textures.zip", FileCategory::Main, false),
        ]);
        assert!(
            d.needs_file_selection(),
            "Onera must not guess between two main files"
        );
        assert_eq!(d.selectable_files().count(), 2);
    }

    #[test]
    fn a_single_candidate_is_used_without_asking() {
        let d = details(vec![
            file("only.zip", FileCategory::Main, false),
            file("old.zip", FileCategory::OldVersion, false),
        ]);
        assert!(!d.needs_file_selection());
        assert_eq!(d.selectable_files().count(), 1);
    }

    #[test]
    fn a_mod_with_no_downloadable_files_asks_rather_than_failing_silently() {
        let d = details(vec![file("archived.zip", FileCategory::Unknown, false)]);
        assert!(d.needs_file_selection());
        assert_eq!(d.selectable_files().count(), 0);
    }
}
