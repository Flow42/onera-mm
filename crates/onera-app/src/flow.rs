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
use onera_core::domain::operation::OperationKind;
use onera_core::domain::profile::{
    DesiredModState, MemberPin, MemberPriority, Profile, ProfileMember,
};
use onera_core::domain::reconcile::{
    reconcile_with_decisions, DesiredGameState, InstallationMapping, MutationPlan, MutationStep,
};
use onera_core::domain::release::{ProviderFile, Release};
use onera_core::hash::FileHash;
use onera_core::ids::{
    ArchiveId, DownloadJobId, InboxRequestId, InstallationId, LocalGameId, ModId, ProfileId,
    ProfileMemberId, ProviderFileId, ProviderId, ProviderModId, ReleaseId,
};
use onera_core::plan::{InstallPlan, ScopedRule, TargetLocation};
use onera_core::ports::{
    AccountInfo, ArchiveBackend, ArchiveStore, AuthProvider, Credential, DeploymentStore,
    DownloadGrant, GameAdapter, ModProvider, OperationJournal, ProfileStore, SecretStore,
};
use onera_core::progress::{CancelToken, ProgressSink};
use onera_core::redact::Secret;
use onera_core::{CoreError, Result};
use onera_db::backup::FileBackupStore;
use onera_db::jobs::{InboxRequest, InboxRequestKind, InboxState};
use onera_db::Database;
use onera_discovery::DiscoveredGame;
use onera_download::{ContentAddressedStore, DownloadConfig, DownloadJob, Downloader, JobState};
use onera_install::planner::{plan_install, PlanRequest, RootMap};
use onera_install::remove::{ModifiedFilePolicy, RemovalReport, Remover};
use onera_install::{
    recover_all, verify_installation, GameLocks, InstallReport, Installer, InterruptedOperation,
    Publication, RealFileSystem, ReconciliationAttempt, ReconciliationEngine, VerifyReport,
};
use onera_nexus::{ApiKeyAuth, NexusClient, NexusConfig};
use std::collections::HashMap;
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
    pub(crate) provider: Arc<dyn ModProvider>,
    auth: Arc<dyn AuthProvider>,
    pub(crate) archives: Arc<dyn ArchiveBackend>,
    downloader: Arc<Downloader>,
    installer: Arc<Installer>,
    remover: Arc<Remover>,
    pub(crate) reconciler: Arc<ReconciliationEngine>,
    backups: Arc<dyn onera_core::ports::BackupStore>,
    pub(crate) locks: GameLocks,
    download_lock: tokio::sync::Mutex<()>,
    /// Cancellation tokens for the transfers running right now.
    ///
    /// A queued job is cancelled by writing its state; one that is already
    /// moving bytes has to be told to stop, and the token that can do that
    /// belongs to whoever started it — the window, the watcher or the CLI.
    /// Holding it here is what lets a cancel reach a transfer none of them
    /// would otherwise be able to name.
    active_downloads: tokio::sync::Mutex<HashMap<DownloadJobId, CancelToken>>,
    expired_prepared_plans: u64,
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
        // Swept after the database opens, because a game may stage somewhere
        // other than the default root and a leftover there is just as stale.
        let mut roots = vec![paths.staging()];
        roots.extend(db.staging_roots().await?);
        roots.dedup();
        let mut expired_prepared_plans = 0;
        for root in &roots {
            expired_prepared_plans += cleanup_expired_staging(root).await?;
        }
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
            remover: Arc::new(Remover::new(
                fs.clone(),
                Arc::new(db.clone()),
                backups.clone(),
            )),
            reconciler: Arc::new(ReconciliationEngine::new(
                fs,
                Arc::new(db.clone()),
                backups.clone(),
                Arc::new(db.clone()),
            )),
            backups,
            locks: GameLocks::new(),
            download_lock: tokio::sync::Mutex::new(()),
            active_downloads: tokio::sync::Mutex::new(HashMap::new()),
            expired_prepared_plans,
            paths,
        })
    }

    /// The database, for drivers that need to read catalogue tables directly.
    #[must_use]
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// The backup store, for callers that must ask whether Onera can restore
    /// bytes it once set aside.
    #[must_use]
    pub fn backups(&self) -> &Arc<dyn onera_core::ports::BackupStore> {
        &self.backups
    }

    /// Number of abandoned preparation directories removed during startup.
    #[must_use]
    pub const fn expired_prepared_plans(&self) -> u64 {
        self.expired_prepared_plans
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

    // -----------------------------------------------------------------------
    // Profiles (desired state only)
    // -----------------------------------------------------------------------

    /// List profiles for one concrete game, with the active profile first.
    pub async fn profiles(&self, game: LocalGameId) -> Result<Vec<Profile>> {
        self.db.profiles(game).await
    }

    /// Return one profile and its deterministically ordered members.
    pub async fn profile_details(&self, id: ProfileId) -> Result<ProfileDetails> {
        let profile = self
            .db
            .profile(id)
            .await?
            .ok_or_else(|| CoreError::NotFound {
                kind: "profile",
                id: id.to_string(),
            })?;
        let members = self.db.members(id).await?;
        Ok(ProfileDetails { profile, members })
    }

    /// Create an empty profile or duplicate another profile as desired state.
    pub async fn create_profile(
        &self,
        game: LocalGameId,
        name: String,
        description: Option<String>,
        copy_from: Option<ProfileId>,
    ) -> Result<Profile> {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return Err(CoreError::InvalidInput(
                "profile name cannot be empty".into(),
            ));
        }
        let source_members = if let Some(source) = copy_from {
            let details = self.profile_details(source).await?;
            if details.profile.local_game_id != game {
                return Err(CoreError::Conflict(
                    "a profile can only be duplicated within the same local game".into(),
                ));
            }
            details.members
        } else {
            Vec::new()
        };
        let timestamp = chrono::Utc::now();
        let profile = Profile {
            id: ProfileId::new(),
            local_game_id: game,
            name,
            description,
            is_active: false,
            created_at: timestamp,
            updated_at: timestamp,
        };
        self.db.put_profile(&profile).await?;

        // A failed clone is compensated by deleting the newly-created inactive
        // profile, so callers never observe a partially duplicated set.
        for source in source_members {
            let member = ProfileMember {
                id: ProfileMemberId::new(),
                profile_id: profile.id,
                added_at: timestamp,
                ..source
            };
            if let Err(error) = self.db.put_member(&member).await {
                let _ = self.db.delete_profile(profile.id).await;
                return Err(error);
            }
        }
        self.db
            .profile(profile.id)
            .await?
            .ok_or_else(|| CoreError::NotFound {
                kind: "profile",
                id: profile.id.to_string(),
            })
    }

    /// Rename a profile without changing its members or active deployment.
    pub async fn rename_profile(&self, id: ProfileId, name: String) -> Result<Profile> {
        let mut profile = self
            .db
            .profile(id)
            .await?
            .ok_or_else(|| CoreError::NotFound {
                kind: "profile",
                id: id.to_string(),
            })?;
        profile.name = name.trim().to_owned();
        profile.updated_at = chrono::Utc::now();
        self.db.put_profile(&profile).await?;
        Ok(profile)
    }

    /// Delete an inactive profile and all of its desired members.
    pub async fn delete_profile(&self, id: ProfileId) -> Result<()> {
        self.db.delete_profile(id).await
    }

    /// Add one mod lineage to a profile, selecting a cached provider file when
    /// supplied and linking a retained artifact when one already satisfies it.
    pub async fn add_profile_member(
        &self,
        profile: ProfileId,
        mod_id: ModId,
        provider_file: Option<ProviderFileId>,
    ) -> Result<ProfileMember> {
        let (selection, installation_id) = self
            .db
            .selection_for_profile_member(profile, mod_id, provider_file.as_ref())
            .await?;
        let existing = self.db.members(profile).await?;
        let priority = existing.last().map_or(MemberPriority(10), |member| {
            MemberPriority(member.priority.0.saturating_add(10))
        });
        let member = ProfileMember {
            id: ProfileMemberId::new(),
            profile_id: profile,
            mod_id,
            selection,
            installation_id,
            desired: DesiredModState::Enabled,
            pin: MemberPin::Unpinned,
            priority,
            added_at: chrono::Utc::now(),
        };
        self.db.put_member(&member).await?;
        Ok(member)
    }

    /// Remove a desired member. Foreign-key cascades also discard any
    /// member-scoped dependency overrides once the dependency schema exists.
    pub async fn remove_profile_member(&self, member: ProfileMemberId) -> Result<()> {
        self.db.remove_member(member).await
    }

    /// Enable or disable a member in desired state only.
    pub async fn set_member_state(
        &self,
        id: ProfileMemberId,
        desired: DesiredModState,
    ) -> Result<ProfileMember> {
        let mut member = self
            .db
            .profile_member(id)
            .await?
            .ok_or_else(|| CoreError::NotFound {
                kind: "profile member",
                id: id.to_string(),
            })?;
        member.desired = desired;
        self.db.put_member(&member).await?;
        Ok(member)
    }

    /// Pin or unpin the selected opaque provider version.
    pub async fn set_member_pin(
        &self,
        id: ProfileMemberId,
        pinned: bool,
        reason: Option<String>,
    ) -> Result<ProfileMember> {
        let mut member = self
            .db
            .profile_member(id)
            .await?
            .ok_or_else(|| CoreError::NotFound {
                kind: "profile member",
                id: id.to_string(),
            })?;
        if pinned && !member.selection.is_resolved() {
            return Err(CoreError::InvalidInput(
                "an unresolved provider file cannot be pinned".into(),
            ));
        }
        member.pin = if pinned {
            MemberPin::Pinned {
                pinned_at: chrono::Utc::now(),
                reason: reason.and_then(|value| {
                    let value = value.trim().to_owned();
                    (!value.is_empty()).then_some(value)
                }),
            }
        } else {
            MemberPin::Unpinned
        };
        self.db.put_member(&member).await?;
        Ok(member)
    }

    /// Change one member's signed provider-stack priority.
    pub async fn reorder_profile_member(
        &self,
        id: ProfileMemberId,
        priority: MemberPriority,
    ) -> Result<ProfileMember> {
        let mut member = self
            .db
            .profile_member(id)
            .await?
            .ok_or_else(|| CoreError::NotFound {
                kind: "profile member",
                id: id.to_string(),
            })?;
        member.priority = priority;
        self.db.put_member(&member).await?;
        Ok(member)
    }

    /// Installed mods for one local game.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn installed_mods(&self, game: LocalGameId) -> Result<Vec<InstalledModInfo>> {
        Ok(self
            .db
            .installed_mods(game)
            .await?
            .into_iter()
            .map(|record| InstalledModInfo::from_record(record, None))
            .collect())
    }

    /// Refresh installed mods and report newer releases from the same lineage.
    ///
    /// Version strings remain display-only; publication timestamps determine
    /// ordering, just as they do everywhere else in Onera.
    ///
    /// # Errors
    /// Propagates provider or database errors.
    pub async fn check_updates(
        &self,
        game: LocalGameId,
        cancel: &CancelToken,
    ) -> Result<Vec<InstalledModInfo>> {
        let records = self.db.installed_mods(game).await?;
        let mut result = Vec::with_capacity(records.len());
        for record in records {
            cancel.check()?;
            let details = self
                .fetch_mod(&record.game_slug, &record.provider_mod_id, cancel)
                .await?;
            let latest = newest_release(&details.releases);
            result.push(InstalledModInfo::from_record(record, latest));
        }
        Ok(result)
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

    /// What Onera already knows about the mod on a page the user is viewing.
    ///
    /// Called once per mod page by the browser extension, so it is built to be
    /// cheap in the common case: a mod Onera has never seen is answered from
    /// the local catalogue alone. The provider is asked only when the mod is
    /// already downloaded or installed — exactly the case where "is there
    /// something newer?" is a question worth a request.
    ///
    /// # Errors
    /// Propagates database errors. A provider failure is *not* an error: the
    /// local half of the answer is still worth returning, so an offline user
    /// is told what they have rather than being told nothing.
    pub async fn mod_state(
        &self,
        game_slug: &str,
        provider_mod_id: &ProviderModId,
        cancel: &CancelToken,
    ) -> Result<ModStateInfo> {
        let provider = ProviderId::nexus();
        let local_game_id = self.local_game_for_slug(game_slug).await?;
        let cached = self
            .db
            .find_mod(&provider, game_slug, provider_mod_id)
            .await?;
        let records = self
            .db
            .installed_records_of_mod(&provider, game_slug, provider_mod_id)
            .await?;
        let downloaded = self
            .db
            .has_archive_for_mod(&provider, game_slug, provider_mod_id)
            .await?;

        // Only a mod the user already has can have an update, so only that case
        // spends a request. Failing to reach the provider downgrades the answer
        // to the local facts instead of discarding them.
        let latest = if records.is_empty() && !downloaded {
            None
        } else {
            match self.fetch_mod(game_slug, provider_mod_id, cancel).await {
                Ok(details) => newest_release(&details.releases).cloned(),
                Err(error) => {
                    tracing::debug!(%error, "could not refresh mod state from the provider");
                    None
                }
            }
        };

        let installations: Vec<InstalledModInfo> = records
            .into_iter()
            .map(|record| InstalledModInfo::from_record(record, latest.as_ref()))
            .collect();

        Ok(ModStateInfo {
            game_slug: game_slug.to_owned(),
            provider_mod_id: provider_mod_id.clone(),
            game_registered: local_game_id.is_some(),
            local_game_id,
            name: cached.map(|the_mod| the_mod.name),
            downloaded,
            update_available: installations.iter().any(|i| i.update_available),
            latest_version: latest.as_ref().map(|release| release.version.clone()),
            latest_published_at: latest.as_ref().and_then(|release| release.published_at),
            installations,
        })
    }

    /// The registered game a provider slug belongs to, when exactly one does.
    ///
    /// Ambiguity is answered with `None` rather than a guess: two registered
    /// installations of the same game are a question for the user, and a
    /// browser request that picked one silently could install into the wrong
    /// copy.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn local_game_for_slug(&self, game_slug: &str) -> Result<Option<LocalGameId>> {
        let Some(adapter) = onera_games::adapter_for_provider_slug(game_slug) else {
            return Ok(None);
        };
        let mut matching = self
            .db
            .local_installs()
            .await?
            .into_iter()
            .filter(|install| install.adapter_id == adapter.id() && install.confirmed);
        match (matching.next(), matching.next()) {
            (Some(only), None) => Ok(Some(only.id)),
            _ => Ok(None),
        }
    }

    /// Provider artwork for one mod, as bytes ready to embed.
    ///
    /// The image is fetched once and cached under `$XDG_CACHE_HOME`, keyed by a
    /// hash of its address, so a mod list redraws without touching the network
    /// and no frontend ever makes a request of its own. `Ok(None)` means the
    /// mod has no artwork, which is an ordinary state and not a failure.
    ///
    /// # Errors
    /// Propagates database errors and cache-write failures. A provider failure
    /// is reported as "no artwork": a list of mods must still draw when an
    /// image server is unreachable.
    pub async fn mod_artwork(
        &self,
        mod_id: ModId,
        cancel: &CancelToken,
    ) -> Result<Option<ModArtwork>> {
        let Some(the_mod) = self.db.mod_by_id(mod_id).await? else {
            return Err(CoreError::NotFound {
                kind: "mod",
                id: mod_id.to_string(),
            });
        };
        let Some(url) = the_mod.thumbnail_url else {
            return Ok(None);
        };

        let key = FileHash::blake3_of(url.as_bytes()).hex;
        let cached = self.paths.thumbnails().join(&key);
        if let Ok(bytes) = tokio::fs::read(&cached).await {
            let content_type = tokio::fs::read_to_string(cached.with_extension("type"))
                .await
                .unwrap_or_else(|_| "image/jpeg".to_owned());
            return Ok(Some(ModArtwork {
                bytes,
                content_type,
            }));
        }

        let fetched = match self.provider.fetch_image(&url, cancel).await {
            Ok(Some(image)) => image,
            Ok(None) => return Ok(None),
            Err(error) => {
                tracing::debug!(%error, "could not fetch mod artwork");
                return Ok(None);
            }
        };

        let dir = self.paths.thumbnails();
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| CoreError::fs(&dir, e))?;
        tokio::fs::write(&cached, &fetched.bytes)
            .await
            .map_err(|e| CoreError::fs(&cached, e))?;
        // The type travels beside the bytes: a cached image whose format was
        // forgotten would have to be sniffed to be displayed.
        let type_path = cached.with_extension("type");
        tokio::fs::write(&type_path, fetched.content_type.as_bytes())
            .await
            .map_err(|e| CoreError::fs(&type_path, e))?;

        Ok(Some(ModArtwork {
            bytes: fetched.bytes,
            content_type: fetched.content_type,
        }))
    }

    // -----------------------------------------------------------------------
    // Browser inbox and downloads
    // -----------------------------------------------------------------------

    /// Persist a browser-extension request for the desktop application.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn enqueue_browser_request(
        &self,
        kind: InboxRequestKind,
        game_slug: String,
        provider_mod_id: ProviderModId,
        provider_file_id: Option<ProviderFileId>,
    ) -> Result<InboxRequest> {
        let request = InboxRequest::queued(kind, game_slug, provider_mod_id, provider_file_id);
        self.db.put_inbox_request(&request).await?;
        Ok(request)
    }

    /// Mark a request as being worked on right now.
    ///
    /// Taking the lease before any work starts is what stops two watchers, or
    /// one watcher and a restart, from downloading the same file twice.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn lease_inbox_request(&self, request: &InboxRequest) -> Result<InboxRequest> {
        let leased = InboxRequest {
            started_at: Some(chrono::Utc::now()),
            updated_at: chrono::Utc::now(),
            ..request.clone()
        };
        self.db.put_inbox_request(&leased).await?;
        Ok(leased)
    }

    /// Record that a request failed, keeping the redacted reason for the UI.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn fail_inbox_request(&self, id: InboxRequestId, error: &str) -> Result<()> {
        self.db
            .set_inbox_state(id, InboxState::Failed, Some(error))
            .await
    }

    /// Queue an Add Mod request from the browser.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn enqueue_add_mod(
        &self,
        game_slug: String,
        provider_mod_id: ProviderModId,
    ) -> Result<InboxRequest> {
        self.enqueue_browser_action(BrowserAction {
            kind: InboxRequestKind::AddMod,
            game_slug,
            provider_mod_id,
            provider_file_id: None,
            page_url: None,
            download_grant: None,
            auto_run: false,
        })
        .await
    }

    /// Queue a browser download, optionally continuing into installation.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn enqueue_download_request(
        &self,
        game_slug: String,
        provider_mod_id: ProviderModId,
        provider_file_id: ProviderFileId,
        install: bool,
    ) -> Result<InboxRequest> {
        self.enqueue_browser_action(BrowserAction {
            kind: if install {
                InboxRequestKind::DownloadAndInstall
            } else {
                InboxRequestKind::Download
            },
            game_slug,
            provider_mod_id,
            provider_file_id: Some(provider_file_id),
            page_url: None,
            download_grant: None,
            auto_run: false,
        })
        .await
    }

    /// Queue a browser action with everything the desktop needs to run it.
    ///
    /// This is the path the extension's own buttons take. A request only
    /// becomes auto-runnable when it names a file *and* resolves to exactly one
    /// registered game: anything less would have the desktop choosing on the
    /// user's behalf, which is the one thing the inbox exists to avoid.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn enqueue_browser_action(&self, action: BrowserAction) -> Result<InboxRequest> {
        let local_game_id = self.local_game_for_slug(&action.game_slug).await?;
        let kind = action.kind;
        // A transfer with no file named is a question, not an instruction: the
        // page offered several and only the user can say which they meant.
        let needs_selection = kind != InboxRequestKind::AddMod && action.provider_file_id.is_none();
        let mut request = InboxRequest {
            page_url: action.page_url,
            local_game_id,
            download_grant: action.download_grant,
            // An install with nowhere to install to waits for the user; a plain
            // download needs no game at all.
            auto_run: action.auto_run
                && !needs_selection
                && (local_game_id.is_some() || kind != InboxRequestKind::DownloadAndInstall),
            ..InboxRequest::queued(
                kind,
                action.game_slug,
                action.provider_mod_id,
                action.provider_file_id,
            )
        };
        if needs_selection {
            request.state = InboxState::WaitingForUser;
        }
        self.db.put_inbox_request(&request).await?;
        Ok(request)
    }

    /// Queue a browser action that needs desktop file selection first.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn enqueue_download_selection_request(
        &self,
        game_slug: String,
        provider_mod_id: ProviderModId,
        install: bool,
    ) -> Result<InboxRequest> {
        self.enqueue_browser_action(BrowserAction {
            kind: if install {
                InboxRequestKind::DownloadAndInstall
            } else {
                InboxRequestKind::Download
            },
            game_slug,
            provider_mod_id,
            provider_file_id: None,
            page_url: None,
            download_grant: None,
            auto_run: false,
        })
        .await
    }

    /// Actionable requests received from the browser extension.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn inbox_requests(&self) -> Result<Vec<InboxRequest>> {
        self.db.inbox_requests().await
    }

    /// Mark a browser request complete or dismissed.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn set_inbox_state(&self, id: InboxRequestId, state: InboxState) -> Result<()> {
        self.db.set_inbox_state(id, state, None).await
    }

    /// Dismiss an actionable browser request.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn dismiss_inbox_request(&self, id: InboxRequestId) -> Result<()> {
        self.set_inbox_state(id, InboxState::Dismissed).await
    }

    /// Mark a browser request as successfully handed off.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn complete_inbox_request(&self, id: InboxRequestId) -> Result<()> {
        self.set_inbox_state(id, InboxState::Complete).await
    }

    /// Every persisted download, with byte counts refreshed from partial files.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn downloads(&self) -> Result<Vec<DownloadJob>> {
        let mut jobs = self.db.download_jobs().await?;
        for job in &mut jobs {
            if job.state.is_active() {
                if let Ok(metadata) = tokio::fs::metadata(&job.temp_path).await {
                    job.bytes_downloaded = metadata.len();
                    self.db.put_download_job(job).await?;
                }
            }
        }
        Ok(jobs)
    }

    /// Stop a download the user no longer wants.
    ///
    /// Two halves, and both are needed. A transfer that is moving bytes is told
    /// to stop through the token whoever started it registered, and stops at
    /// its next safe point; the row is written here as well, because a job that
    /// was only ever queued has nobody to tell. Either way the job ends in
    /// [`JobState::Cancelled`], which is not resumable, so nothing picks it up
    /// again on the next launch.
    ///
    /// Cancelling a job that has already finished does nothing: the archive is
    /// in the store and removing it is a different request.
    ///
    /// # Errors
    /// [`CoreError::NotFound`] when no such job exists. Propagates database
    /// errors.
    pub async fn cancel_download(&self, id: DownloadJobId) -> Result<()> {
        let mut job = self
            .db
            .download_jobs()
            .await?
            .into_iter()
            .find(|candidate| candidate.id == id)
            .ok_or_else(|| CoreError::NotFound {
                kind: "download job",
                id: id.to_string(),
            })?;
        if !job.state.is_active() {
            return Ok(());
        }

        let running = self.active_downloads.lock().await.get(&id).cloned();
        if let Some(token) = running {
            // The transfer owns its partial file and writes its own final row
            // when it unwinds; asking twice would race it. The state is still
            // written here so the window reflects the cancel immediately.
            token.cancel();
        } else {
            let _ = tokio::fs::remove_file(&job.temp_path).await;
            job.bytes_downloaded = 0;
        }
        job.state = JobState::Cancelled;
        job.error = None;
        self.db.put_download_job(&job).await?;
        Ok(())
    }

    /// Download and safety-inspect a provider file into archive storage.
    ///
    /// A previously associated archive is reused without a network call.
    ///
    /// # Errors
    /// Propagates provider, download, archive, and database errors.
    pub async fn download(
        &self,
        request: &DownloadRequest,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<DownloadedArchive> {
        let _guard = self.download_lock.lock().await;
        if let Some(stored) = self
            .db
            .archive_for_provider_file(&ProviderId::nexus(), &request.provider_file_id)
            .await?
        {
            if stored.path.is_file() {
                return Ok(DownloadedArchive {
                    archive_id: stored.id,
                    path: stored.path,
                    hash: stored.hash,
                    bytes: stored.size,
                    deduplicated: true,
                });
            }
        }

        if let Some(job) = self
            .db
            .resumable_download_jobs()
            .await?
            .into_iter()
            .find(|job| {
                job.provider == ProviderId::nexus()
                    && job.provider_file_id == request.provider_file_id
            })
        {
            return self
                .run_download_job(job, request.grant.as_ref(), progress, cancel, false)
                .await;
        }

        let mut job = DownloadJob::queued(
            ProviderId::nexus(),
            request.game_slug.clone(),
            request.provider_mod_id.clone(),
            request.provider_file_id.clone(),
            request.filename.clone(),
            request.expected_size,
            PathBuf::new(),
        );
        // The partial lands beside its destination rather than in the cache:
        // finishing a download is then a rename within one filesystem, which is
        // the whole reason to put a game's downloads on the disk it lives on.
        let incoming = self
            .download_root_for_slug(&request.game_slug)
            .await?
            .join(".incoming");
        tokio::fs::create_dir_all(&incoming)
            .await
            .map_err(|error| CoreError::fs(&incoming, error))?;
        job.temp_path = incoming.join(format!("{}.part", job.id));
        job.expected_hash = trusted_provider_hash(request.expected_hash.as_ref()).cloned();
        self.db.put_download_job(&job).await?;
        self.run_download_job(job, request.grant.as_ref(), progress, cancel, false)
            .await
    }

    /// Resume all downloads left active by an earlier process.
    ///
    /// Individual failures are recorded on their jobs and do not prevent other
    /// jobs from resuming.
    ///
    /// # Errors
    /// Fails only when the job list itself cannot be read.
    pub async fn resume_downloads(
        &self,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<()> {
        let _guard = self.download_lock.lock().await;
        for job in self.db.resumable_download_jobs().await? {
            if cancel.is_cancelled() {
                break;
            }
            // A resumed job carries no grant: whatever authorised it expired
            // with the click that made it.
            if let Err(error) = self.run_download_job(job, None, progress, cancel, true).await {
                tracing::warn!(error = %error, "could not resume download");
            }
        }
        Ok(())
    }

    async fn run_download_job(
        &self,
        mut job: DownloadJob,
        grant: Option<&DownloadGrant>,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
        keep_resumable_on_error: bool,
    ) -> Result<DownloadedArchive> {
        // A user who cancelled this job while it sat in the queue has already
        // said what they want; picking it up now would ignore them.
        if !job.state.is_active() {
            return Err(CoreError::Cancelled);
        }
        self.active_downloads
            .lock()
            .await
            .insert(job.id, cancel.clone());
        job.state = JobState::Running;
        job.attempts = job.attempts.saturating_add(1);
        job.error = None;
        if let Ok(metadata) = tokio::fs::metadata(&job.temp_path).await {
            job.bytes_downloaded = metadata.len();
        }
        self.db.put_download_job(&job).await?;

        let result = async {
            let target = self
                .provider
                .resolve_download(
                    &job.game_slug,
                    &job.provider_mod_id,
                    &job.provider_file_id,
                    grant,
                    cancel,
                )
                .await?;
            // Where this download is filed: the directory chosen for this
            // game, the one chosen for everything, or Onera's own. Resolved per
            // job rather than held by the downloader, because the answer
            // depends on which game the transfer is for.
            let store = ContentAddressedStore::new(
                self.download_root_for_slug(&job.game_slug).await?,
            );
            let outcome = self
                .downloader
                .fetch_resumable_into(
                    &store,
                    &target,
                    job.expected_hash.as_ref(),
                    &job.temp_path,
                    progress,
                    cancel,
                )
                .await?;
            let inspection = self.archives.inspect(&outcome.path, cancel).await?;
            let archive_size = if outcome.deduplicated {
                tokio::fs::metadata(&outcome.path)
                    .await
                    .map_err(|error| CoreError::fs(&outcome.path, error))?
                    .len()
            } else {
                outcome.bytes
            };
            let archive_id = self
                .db
                .upsert_archive(
                    &outcome.hash,
                    archive_size,
                    &job.filename,
                    inspection.format,
                    &outcome.path,
                )
                .await?;
            self.db
                .link_archive_provider_file(archive_id, &job.provider, &job.provider_file_id)
                .await?;
            Ok::<_, CoreError>((outcome, archive_id, archive_size))
        }
        .await;

        self.active_downloads.lock().await.remove(&job.id);

        match result {
            Ok((outcome, archive_id, archive_size)) => {
                job.state = JobState::Complete;
                job.bytes_downloaded = archive_size;
                job.archive_id = Some(archive_id);
                job.error = None;
                self.db.put_download_job(&job).await?;
                Ok(DownloadedArchive {
                    archive_id,
                    path: outcome.path,
                    hash: outcome.hash,
                    bytes: archive_size,
                    deduplicated: outcome.deduplicated,
                })
            }
            Err(error) => {
                job.bytes_downloaded = tokio::fs::metadata(&job.temp_path)
                    .await
                    .map_or(0, |metadata| metadata.len());
                // A cancelled transfer is decided by the token, not only by the
                // error: a stop can surface as a torn connection, and a job left
                // resumable would be picked back up on the next launch — which
                // is exactly what the user asked not to happen.
                job.state = if cancel.is_cancelled() || matches!(error, CoreError::Cancelled) {
                    // Nothing will resume the partial, so it is not left behind.
                    let _ = tokio::fs::remove_file(&job.temp_path).await;
                    job.bytes_downloaded = 0;
                    JobState::Cancelled
                } else if keep_resumable_on_error || error.is_retryable() {
                    JobState::Paused
                } else {
                    JobState::Failed
                };
                job.error = Some(error.to_string());
                self.db.put_download_job(&job).await?;
                Err(error)
            }
        }
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

        // 0. Check the baseline before doing any work. A changed store build
        //    means the recorded clean state no longer describes this
        //    installation; an identity Onera cannot compare means it does not
        //    know either way. Neither blocks an install — both are reported.
        let baseline_freshness = self.baseline_freshness(request.local_game_id).await?;
        if let Some(message) = crate::baseline::freshness_warning(&baseline_freshness) {
            progress.emit(onera_core::progress::ProgressEvent::Warning { message });
        }

        // 1. Download (or reuse a stored archive).
        let outcome = self
            .download(
                &DownloadRequest {
                    game_slug: request.game_slug.clone(),
                    provider_mod_id: request.provider_mod_id.clone(),
                    provider_file_id: request.provider_file_id.clone(),
                    filename: request.filename.clone(),
                    expected_size: request.expected_size,
                    expected_hash: request.expected_hash.clone(),
                    grant: request.grant.clone(),
                },
                progress,
                cancel,
            )
            .await?;

        // 2. Inspect before extracting.
        let inspection = self.archives.inspect(&outcome.path, cancel).await?;
        tracing::info!(
            entries = inspection.entries.len(),
            rejected = inspection.rejected.len(),
            "inspected archive"
        );

        // 3. Extract into a staging directory unique to this operation, under
        //    the root this game stages in — the default one unless the user
        //    moved it, which they do when the game lives on another disk.
        let staging_key = onera_core::ids::OperationId::new();
        let staging = self.staging_for(request.local_game_id, staging_key).await?;
        let manifest = self
            .archives
            .extract(&outcome.path, &staging, progress, cancel)
            .await?;

        let archive_id = outcome.archive_id;
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
            baseline_freshness,
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

    /// Build a deterministic desired-state preview without extracting archives
    /// or touching the game directory.
    pub async fn plan_state(
        &self,
        game: LocalGameId,
        installations: Vec<InstallationId>,
    ) -> Result<PreparedState> {
        self.plan_state_with_decisions(game, installations, &std::collections::BTreeMap::new())
            .await
    }

    /// Preview a desired state with explicit winners for cross-mod paths.
    ///
    /// Decisions are part of the returned plan and therefore of the operation
    /// journal record that is ultimately applied.
    pub async fn plan_state_with_decisions(
        &self,
        game: LocalGameId,
        installations: Vec<InstallationId>,
        decisions: &std::collections::BTreeMap<TargetLocation, InstallationId>,
    ) -> Result<PreparedState> {
        let (roots, _) = self.roots_for(game).await?;
        let desired = DesiredGameState::new(game, installations);
        let mut lineages = std::collections::BTreeMap::new();
        for installation in &desired.installations {
            let mod_id = self
                .db
                .mod_for_installation(game, *installation)
                .await?
                .ok_or_else(|| CoreError::NotFound {
                    kind: "retained installation",
                    id: installation.to_string(),
                })?;
            if let Some(previous) = lineages.insert(mod_id, *installation) {
                return Err(CoreError::Conflict(format!(
                    "installations {previous} and {installation} are versions of the same mod"
                )));
            }
        }
        let mut current = std::collections::BTreeMap::new();
        for target in self.db.all_targets(game).await? {
            current.insert(target.clone(), self.db.stack(game, &target).await?);
        }
        let mut mappings = Vec::new();
        for installation in &desired.installations {
            if self
                .db
                .archive_for_installation(game, *installation)
                .await?
                .is_none()
            {
                return Err(CoreError::NotFound {
                    kind: "retained installation",
                    id: installation.to_string(),
                });
            }
            let stored = self.db.mappings_of(*installation).await?;
            if stored.is_empty() {
                return Err(CoreError::Conflict(format!(
                    "installation {installation} has no retained layout mappings and must be reinstalled"
                )));
            }
            mappings.extend(stored);
        }
        Ok(PreparedState {
            plan: reconcile_with_decisions(desired, &current, &mappings, decisions),
            mappings,
            roots,
        })
    }

    /// Apply an approved state preview. Required retained archives are
    /// re-extracted and checked against their recorded mappings first.
    pub async fn apply_state(
        &self,
        prepared: &PreparedState,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<()> {
        self.apply_state_as(prepared, OperationKind::Reconcile, progress, cancel)
            .await
    }

    /// Apply an approved state preview, journaled under an explicit kind.
    ///
    /// A return-to-clean is a reconciliation to the empty desired state, but the
    /// journal must record *why* it happened so recovery and history can tell a
    /// clean restore from an ordinary profile change.
    pub async fn apply_state_as(
        &self,
        prepared: &PreparedState,
        kind: OperationKind,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<()> {
        let _guard = self
            .locks
            .acquire(prepared.plan.desired.local_game_id)
            .await;
        self.apply_state_locked(prepared, kind, Publication::none(), progress, cancel)
            .await
            .result
    }

    /// Apply an approved state preview while the game lock is already held.
    ///
    /// Callers that sequence several steps under one lock — profile activation
    /// acquires it before it starts downloading — use this and keep the guard
    /// themselves. It reports the journaled operation whether or not the apply
    /// succeeded, which is what an activation record needs.
    pub(crate) async fn apply_state_locked(
        &self,
        prepared: &PreparedState,
        kind: OperationKind,
        publication: Publication,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> ReconciliationAttempt {
        let game = prepared.plan.desired.local_game_id;
        let mut needed = std::collections::BTreeSet::new();
        for step in &prepared.plan.steps {
            if let MutationStep::Write { provider, .. } = step {
                if let Some(installation) = provider.provider.installation_id() {
                    needed.insert(installation);
                }
            }
        }
        let mut extracted = std::collections::BTreeMap::new();
        let staged = async {
            for installation in needed {
                let archive = self
                    .db
                    .archive_for_installation(game, installation)
                    .await?
                    .ok_or_else(|| CoreError::NotFound {
                        kind: "retained installation",
                        id: installation.to_string(),
                    })?;
                let staging = self
                    .staging_for(game, onera_core::ids::OperationId::new())
                    .await?;
                extracted.insert(installation, staging.clone());
                let manifest = self
                    .archives
                    .extract(&archive.path, &staging, progress, cancel)
                    .await?;
                if manifest.archive_hash != archive.hash {
                    return Err(CoreError::IntegrityMismatch {
                        path: archive.path.display().to_string(),
                        expected: archive.hash.to_string(),
                        actual: manifest.archive_hash.to_string(),
                    });
                }
                for mapping in prepared
                    .mappings
                    .iter()
                    .filter(|mapping| mapping.installation_id == installation)
                {
                    let actual = manifest.file(&mapping.source).ok_or_else(|| {
                        CoreError::IntegrityMismatch {
                            path: mapping.source.to_string(),
                            expected: mapping.source_hash.to_string(),
                            actual: "missing".into(),
                        }
                    })?;
                    if actual.hash != mapping.source_hash {
                        return Err(CoreError::IntegrityMismatch {
                            path: mapping.source.to_string(),
                            expected: mapping.source_hash.to_string(),
                            actual: actual.hash.to_string(),
                        });
                    }
                }
            }
            Ok(())
        }
        .await;
        let attempt = match staged {
            Err(error) => ReconciliationAttempt {
                operation: None,
                rolled_back: false,
                result: Err(error),
            },
            Ok(()) => {
                self.reconciler
                    .attempt(
                        &prepared.plan,
                        &prepared.mappings,
                        &extracted,
                        &prepared.roots,
                        kind,
                        publication,
                        progress,
                        cancel,
                    )
                    .await
            }
        };
        for staging in extracted.values() {
            let _ = tokio::fs::remove_dir_all(staging).await;
        }
        attempt
    }

    /// Enable a retained artifact through the shared reconciler.
    pub async fn enable(
        &self,
        game: LocalGameId,
        installation: InstallationId,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<()> {
        let mut desired = self.db.active_installations(game).await?;
        let lineage = self
            .db
            .mod_for_installation(game, installation)
            .await?
            .ok_or_else(|| CoreError::NotFound {
                kind: "retained installation",
                id: installation.to_string(),
            })?;
        let mut retained = Vec::with_capacity(desired.len() + 1);
        for active in desired.drain(..) {
            if self.db.mod_for_installation(game, active).await? != Some(lineage) {
                retained.push(active);
            }
        }
        desired = retained;
        if !desired.contains(&installation) {
            desired.push(installation);
        }
        let prepared = self.plan_state(game, desired).await?;
        self.apply_state(&prepared, progress, cancel).await
    }

    /// Disable an artifact while retaining its archive and mappings.
    pub async fn disable(
        &self,
        game: LocalGameId,
        installation: InstallationId,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<()> {
        let desired = self
            .db
            .active_installations(game)
            .await?
            .into_iter()
            .filter(|candidate| *candidate != installation)
            .collect();
        let prepared = self.plan_state(game, desired).await?;
        self.apply_state(&prepared, progress, cancel).await
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
        let kind = self
            .db
            .get(operation)
            .await?
            .ok_or_else(|| CoreError::NotFound {
                kind: "operation",
                id: operation.to_string(),
            })?
            .kind;
        match kind {
            OperationKind::Reconcile | OperationKind::CleanRestore => {
                self.reconciler.rollback(operation).await
            }
            OperationKind::Install | OperationKind::Remove | OperationKind::Repair => {
                self.installer.rollback(operation, progress).await
            }
        }
    }
}

async fn cleanup_expired_staging(root: &std::path::Path) -> Result<u64> {
    let mut entries = match tokio::fs::read_dir(root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(CoreError::fs(root, error)),
    };
    let mut removed = 0;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| CoreError::fs(root, error))?
    {
        let path = entry.path();
        let result = if entry
            .file_type()
            .await
            .map_err(|error| CoreError::fs(&path, error))?
            .is_dir()
        {
            tokio::fs::remove_dir_all(&path).await
        } else {
            tokio::fs::remove_file(&path).await
        };
        result.map_err(|error| CoreError::fs(&path, error))?;
        removed += 1;
    }
    Ok(removed)
}

fn trusted_provider_hash(hash: Option<&FileHash>) -> Option<&FileHash> {
    hash.filter(|value| value.algorithm == onera_core::hash::HashAlgorithm::Blake3)
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
    /// Size published by the provider, when known.
    pub expected_size: Option<u64>,
    /// Hash to check against, when the provider published one.
    pub expected_hash: Option<FileHash>,
    /// A download the provider's website authorised, when the request came
    /// from a browser holding one. Passed straight through to the download.
    pub grant: Option<DownloadGrant>,
}

/// Provider file to download independently of an installation plan.
#[derive(Debug, Clone)]
pub struct DownloadRequest {
    /// Provider game slug.
    pub game_slug: String,
    /// Provider mod identifier.
    pub provider_mod_id: ProviderModId,
    /// Provider file identifier.
    pub provider_file_id: ProviderFileId,
    /// Display filename.
    pub filename: String,
    /// Expected size, when published.
    pub expected_size: Option<u64>,
    /// Provider hash metadata. Only trusted algorithms drive integrity checks.
    pub expected_hash: Option<FileHash>,
    /// A download the provider's website authorised, when the request came
    /// from a browser holding one.
    ///
    /// It is never persisted with the job: a grant outlives its click by
    /// minutes, so a job resumed after a restart resolves afresh and says so if
    /// the provider refuses.
    pub grant: Option<DownloadGrant>,
}

/// Result of a completed, inspected download.
#[derive(Debug, Clone)]
pub struct DownloadedArchive {
    /// Stored archive identity.
    pub archive_id: ArchiveId,
    /// Content-addressed path.
    pub path: PathBuf,
    /// Computed BLAKE3 hash.
    pub hash: FileHash,
    /// Total archive bytes.
    pub bytes: u64,
    /// Whether existing stored content avoided a network transfer.
    pub deduplicated: bool,
}

/// Installed-mod row used by desktop and CLI list/update views.
#[derive(Debug, Clone, serde::Serialize)]
pub struct InstalledModInfo {
    /// Concrete installation identity.
    pub installation_id: InstallationId,
    /// Mod lineage identity.
    pub mod_id: ModId,
    /// Cached display name.
    pub name: String,
    /// Cached author.
    pub author: Option<String>,
    /// Installed version exactly as published.
    pub version: String,
    /// Installation timestamp.
    pub installed_at: chrono::DateTime<chrono::Utc>,
    /// When the provider published the installed version, if it said.
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Whether a newer publication is available.
    pub update_available: bool,
    /// Newest available version, verbatim.
    pub latest_version: Option<String>,
    /// When the newest available version was published.
    pub latest_published_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Provider game slug, for building a page address.
    pub game_slug: String,
    /// Provider mod identifier, for building a page address.
    pub provider_mod_id: ProviderModId,
    /// Provider artwork address, when the provider offers one.
    pub thumbnail_url: Option<String>,
}

impl InstalledModInfo {
    /// Build a row from a stored installation and, when one was fetched, the
    /// newest release the provider currently offers.
    ///
    /// Ordering is by publication date and never by parsing a version string:
    /// two releases of the same mod are comparable only through the dates the
    /// provider recorded for them.
    #[must_use]
    pub fn from_record(
        record: onera_db::catalog::InstalledModRecord,
        latest: Option<&Release>,
    ) -> Self {
        let latest_published_at = latest.and_then(|release| release.published_at);
        let update_available = match (record.published_at, latest_published_at) {
            (Some(installed), Some(available)) => available > installed,
            _ => false,
        };
        Self {
            installation_id: record.installation_id,
            mod_id: record.mod_id,
            name: record.name,
            author: record.author,
            version: record.version,
            installed_at: record.installed_at,
            published_at: record.published_at,
            update_available,
            latest_version: if update_available {
                latest.map(|release| release.version.clone())
            } else {
                None
            },
            latest_published_at,
            game_slug: record.game_slug,
            provider_mod_id: record.provider_mod_id,
            thumbnail_url: record.thumbnail_url,
        }
    }
}

/// The newest release a provider reported, ignoring any it could not date.
///
/// An undated release cannot be placed in a lineage, so it can neither be "the
/// newest" nor evidence that something newer exists.
#[must_use]
fn newest_release(releases: &[Release]) -> Option<&Release> {
    releases
        .iter()
        .filter(|release| release.published_at.is_some())
        .max_by_key(|release| release.published_at)
}

/// One action the browser extension asked the desktop to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserAction {
    /// What to do.
    pub kind: InboxRequestKind,
    /// Provider game slug from the page URL.
    pub game_slug: String,
    /// Provider mod identifier from the page URL.
    pub provider_mod_id: ProviderModId,
    /// The file the host resolved, when it could resolve one unambiguously.
    pub provider_file_id: Option<ProviderFileId>,
    /// A download the provider's website authorised for that file, when the
    /// request came from a "Mod manager download" rather than Onera's button.
    pub download_grant: Option<DownloadGrant>,
    /// The page the user was on, recorded so the mod can be reopened later.
    pub page_url: Option<String>,
    /// Whether the user asked for this to happen, rather than merely be noted.
    pub auto_run: bool,
}

/// Provider artwork, cached locally and ready to hand to a view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModArtwork {
    /// Encoded image bytes.
    pub bytes: Vec<u8>,
    /// MIME type of [`ModArtwork::bytes`].
    pub content_type: String,
}

/// What Onera already knows about a mod the user is looking at.
///
/// The browser extension asks this before it draws its buttons, so the answer
/// has to be complete enough to choose between "add", "already installed" and
/// "update available" without a second round trip.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModStateInfo {
    /// Provider game slug the question was asked about.
    pub game_slug: String,
    /// Provider mod identifier the question was asked about.
    pub provider_mod_id: ProviderModId,
    /// Whether an adapter claims this game *and* the user has registered it.
    pub game_registered: bool,
    /// The local game a request for this mod would target, when unambiguous.
    pub local_game_id: Option<LocalGameId>,
    /// Cached display name, if Onera has ever fetched this mod.
    pub name: Option<String>,
    /// Whether an archive for one of this mod's files is already stored.
    pub downloaded: bool,
    /// Every current installation of this mod, newest first.
    pub installations: Vec<InstalledModInfo>,
    /// Newest version the provider offers, when it was asked.
    pub latest_version: Option<String>,
    /// When that version was published.
    pub latest_published_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Whether an installed copy is older than the newest publication.
    pub update_available: bool,
}

impl ModStateInfo {
    /// Whether at least one copy of this mod is installed.
    #[must_use]
    pub fn is_installed(&self) -> bool {
        !self.installations.is_empty()
    }
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
    /// Whether the game's baseline still describes the installed build.
    ///
    /// Read before any work starts, so the preview can warn that the clean
    /// state Onera would compare against is out of date. Never `Fresh` when the
    /// store exposes no comparable identity.
    pub baseline_freshness: onera_core::domain::baseline::BaselineFreshness,
}

/// A dry-run desired-state plan together with the retained mappings needed to
/// prepare its writes later.
#[derive(Debug)]
pub struct PreparedState {
    /// Deterministic preview shown before apply.
    pub plan: MutationPlan,
    /// Stable artifact mappings used for archive-backed reactivation.
    pub mappings: Vec<InstallationMapping>,
    /// Resolved deployment roots.
    pub roots: RootMap,
}

/// A profile together with its priority-ordered member table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProfileDetails {
    /// Profile card data.
    pub profile: Profile,
    /// Desired members, lowest priority first.
    pub members: Vec<ProfileMember>,
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
            provider_version_id: None,
            provider_download_id: None,
            provider_file_group_id: None,
            position: None,
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

    #[tokio::test]
    async fn startup_expires_abandoned_preparation_directories() {
        let directory = tempfile::tempdir().unwrap();
        let paths = crate::Paths::rooted_at(directory.path().to_path_buf());
        paths.ensure().await.unwrap();
        tokio::fs::create_dir_all(paths.staging().join("old-plan"))
            .await
            .unwrap();
        tokio::fs::write(paths.staging().join("orphan"), b"stale")
            .await
            .unwrap();

        assert_eq!(cleanup_expired_staging(&paths.staging()).await.unwrap(), 2);
        assert!(tokio::fs::read_dir(paths.staging())
            .await
            .unwrap()
            .next_entry()
            .await
            .unwrap()
            .is_none());
    }
}
