//! Ports: the traits every adapter implements.
//!
//! The core depends on these traits and never on a concrete implementation.
//! Tauri, the CLI and the Native Messaging host are drivers on one side;
//! SQLite, `reqwest`, `7zz`, Secret Service and the game adapters are driven
//! adapters on the other.
//!
//! Two rules keep the boundaries honest, and both are checked by tests:
//!
//! * No trait here mentions a provider-specific type. [`ModProvider`] speaks in
//!   [`ProviderModId`]/[`ProviderFileId`], which are opaque strings.
//! * No trait here takes a `std::path::Path` for a location *inside* a game or
//!   a staging directory; those are always [`RelPath`] plus a root.

use crate::domain::archive::{ArchiveInspection, ArchiveManifest};
use crate::domain::baseline::{
    BaselineExclusion, BaselineFile, BaselineFinding, BaselineRoot, BaselineScanRun, GameBaseline,
    StoreBuildIdentity, StoreDlc,
};
use crate::domain::dependency::{
    DependencyCapability, DependencyOverride, DependencySnapshot, DependencySource,
};
use crate::domain::game::{DeployRoot, Game, InstallValidation, LocalGameInstall};
use crate::domain::profile::{Profile, ProfileActivation, ProfileMember};
use crate::domain::release::{Mod, ProviderFile, Release};
use crate::hash::FileHash;
use crate::ids::{ProviderFileId, ProviderId, ProviderModId};
use crate::paths::DeployRootKind;
use crate::paths::RelPath;
use crate::plan::TargetLocation;
use crate::progress::{CancelToken, ProgressSink};
use crate::redact::Secret;
use crate::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A page of results from a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// Items on this page.
    pub items: Vec<T>,
    /// Opaque cursor for the next page, or `None` when exhausted.
    pub next: Option<String>,
    /// Total item count across all pages, when the provider reports one.
    pub total: Option<u64>,
}

impl<T> Page<T> {
    /// A single, final page.
    pub fn single(items: Vec<T>) -> Self {
        let total = items.len() as u64;
        Self {
            items,
            next: None,
            total: Some(total),
        }
    }
}

/// A resolved, time-limited download location.
#[derive(Debug, Clone)]
pub struct DownloadTarget {
    /// URL to fetch. Treat as a secret: it usually carries a signature.
    pub url: url::Url,
    /// Extra headers the provider requires.
    pub headers: Vec<(String, Secret)>,
    /// Expected size in bytes, when the provider states one.
    pub expected_size: Option<u64>,
    /// Suggested filename.
    pub filename: String,
}

/// Identity of the account a credential belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountInfo {
    /// Provider's opaque user identifier.
    pub provider_user_id: String,
    /// Display name.
    pub username: String,
    /// Whether the account has a paid tier, when the provider exposes it.
    pub premium: Option<bool>,
    /// Email, when the provider exposes it and the user consented.
    pub email: Option<String>,
}

/// How Onera proves who it is to a provider.
///
/// Personal API keys are the initial mechanism. An OAuth/SSO flow is a second
/// implementation of this same trait: [`ModProvider`] only ever asks for a
/// ready-to-use credential, so swapping the mechanism does not touch the client
/// or the core.
#[async_trait]
pub trait AuthProvider: Send + Sync {
    /// Which provider this authenticates against.
    fn provider_id(&self) -> ProviderId;

    /// Whether a usable credential is currently available.
    async fn is_authenticated(&self) -> Result<bool>;

    /// Fetch the credential to attach to outgoing requests.
    ///
    /// # Errors
    /// Returns [`crate::CoreError::Unauthenticated`] when nothing is stored.
    async fn credential(&self) -> Result<Credential>;

    /// Validate a credential against the provider and return the account.
    ///
    /// # Errors
    /// Returns [`crate::CoreError::Unauthenticated`] if the provider rejects it.
    async fn validate(&self, credential: &Credential) -> Result<AccountInfo>;

    /// Validate and then persist a credential, replacing any existing one.
    ///
    /// Implementations must store the secret only in the platform secret store
    /// and must never write it to the database, a config file or a log.
    ///
    /// # Errors
    /// Fails if validation fails or if secret storage is unavailable. There is
    /// no plaintext fallback.
    async fn store(&self, credential: Credential) -> Result<AccountInfo>;

    /// Delete the stored credential.
    ///
    /// # Errors
    /// Fails if the secret store is unavailable. Succeeds if nothing was stored.
    async fn forget(&self) -> Result<()>;
}

/// A credential in a form the transport can attach to a request.
#[derive(Debug, Clone)]
pub enum Credential {
    /// A user-supplied personal API key, sent in a provider-specific header.
    ApiKey(Secret),
    /// A bearer token from an SSO/OAuth flow.
    Bearer(Secret),
}

/// A source of mods.
///
/// Nothing in this trait is Nexus-specific. A second provider implements the
/// same methods with its own identifier space.
#[async_trait]
pub trait ModProvider: Send + Sync {
    /// Stable slug of this provider.
    fn id(&self) -> ProviderId;

    /// Games this provider supports, paginated.
    ///
    /// # Errors
    /// Fails on transport, authentication or malformed-response errors.
    async fn games(&self, cursor: Option<&str>, cancel: &CancelToken) -> Result<Page<Game>>;

    /// Metadata for one mod.
    ///
    /// # Errors
    /// Returns [`crate::CoreError::NotFound`] if the mod does not exist.
    async fn mod_metadata(
        &self,
        game_slug: &str,
        mod_id: &ProviderModId,
        cancel: &CancelToken,
    ) -> Result<(Mod, Vec<Release>)>;

    /// Files available for a mod.
    ///
    /// # Errors
    /// Fails on transport, authentication or malformed-response errors.
    async fn files(
        &self,
        game_slug: &str,
        mod_id: &ProviderModId,
        cursor: Option<&str>,
        cancel: &CancelToken,
    ) -> Result<Page<ProviderFile>>;

    /// Resolve a file into something the downloader can fetch.
    ///
    /// # Errors
    /// Fails if the provider refuses the download (e.g. it requires a paid
    /// tier or an interactive confirmation).
    async fn resolve_download(
        &self,
        game_slug: &str,
        mod_id: &ProviderModId,
        file_id: &ProviderFileId,
        cancel: &CancelToken,
    ) -> Result<DownloadTarget>;

    /// What this provider can say about dependencies, before anything is asked.
    ///
    /// Lets the UI tell "this source has no such concept" apart from "we asked
    /// and it failed". The default is
    /// [`DependencyCapability::Unsupported`], so a provider that models no
    /// dependencies needs no code at all and can never be mistaken for one that
    /// reported none.
    fn dependency_capability(&self) -> DependencyCapability {
        DependencyCapability::Unsupported
    }

    /// Provider-neutral dependency definitions for a set of versions.
    ///
    /// Implementations return exactly one [`DependencySnapshot`] per requested
    /// source, in the order requested. A source the provider could not answer
    /// for gets a snapshot with
    /// [`crate::domain::dependency::DependencyAvailability::Unavailable`] — an
    /// empty group list must never be used to mean "we do not know". Returning
    /// `Err` is reserved for failures that abort the whole request, such as
    /// cancellation or a lost credential.
    ///
    /// # Errors
    /// Fails on authentication errors and cancellation.
    async fn dependencies(
        &self,
        sources: &[DependencySource],
        cancel: &CancelToken,
    ) -> Result<Vec<DependencySnapshot>> {
        let _ = cancel;
        let now = chrono::Utc::now();
        Ok(sources
            .iter()
            .map(|source| DependencySnapshot::unsupported(source.clone(), now))
            .collect())
    }
}

/// A game-specific adapter.
///
/// Adapters know about one game. They never touch the filesystem beyond reading
/// for validation, and they never decide *whether* to write anything — that is
/// the planner's job.
pub trait GameAdapter: Send + Sync {
    /// Stable slug, e.g. `cyberpunk2077`.
    fn id(&self) -> &str;

    /// Human-readable name.
    fn display_name(&self) -> &str;

    /// Provider game slugs this adapter claims, e.g. Nexus domain names.
    fn provider_slugs(&self) -> &[&str];

    /// Steam application ids this adapter claims.
    fn steam_app_ids(&self) -> &[u32];

    /// Check that a directory really is this game.
    fn validate_install(&self, install_root: &Path) -> InstallValidation;

    /// Deployment roots for a validated installation.
    ///
    /// # Errors
    /// Fails if a required root cannot be derived from the installation.
    fn deploy_roots(&self, install: &LocalGameInstall) -> Result<Vec<DeployRoot>>;

    /// Map the contents of an extracted archive onto deployment roots.
    ///
    /// # Errors
    /// Returns [`crate::CoreError::AmbiguousLayout`] when more than one mapping
    /// is plausible. The caller then asks the user rather than guessing.
    fn resolve_layout(&self, manifest: &ArchiveManifest) -> Result<LayoutResolution>;

    /// Reject targets this game must never have written.
    ///
    /// # Errors
    /// Returns [`crate::CoreError::InvalidInput`] with a displayable reason.
    fn validate_target(&self, target: &TargetLocation) -> Result<()>;

    /// Directories a baseline capture may scan.
    ///
    /// Only store-managed locations belong here. The default derives them from
    /// [`GameAdapter::deploy_roots`] by keeping the install directory and the
    /// adapter's auxiliary roots and dropping user-data and compatibility-prefix
    /// roots, which is the documented default: saves, per-user configuration and
    /// prefix internals are not part of what "clean" means and change constantly
    /// on their own.
    ///
    /// # Errors
    /// Fails if a required root cannot be derived from the installation.
    fn baseline_roots(&self, install: &LocalGameInstall) -> Result<Vec<BaselineRoot>> {
        Ok(self
            .deploy_roots(install)?
            .into_iter()
            .filter(|root| {
                matches!(
                    root.kind,
                    DeployRootKind::GameInstall | DeployRootKind::Auxiliary
                )
            })
            .map(|root| BaselineRoot {
                key: root.key,
                kind: root.kind,
                path: root.path,
            })
            .collect())
    }

    /// Paths inside the baseline roots that are never part of a baseline.
    ///
    /// Caches, logs, shader caches and configuration the game rewrites at
    /// runtime. Declaring one here is what stops a routine rewrite from being
    /// reported as a modified game file. The declarations are fingerprinted into
    /// every baseline, so narrowing this list later invalidates the comparison
    /// rather than silently producing an easier "clean".
    fn baseline_exclusions(&self) -> Vec<BaselineExclusion> {
        Vec::new()
    }
}

/// The outcome of mapping an archive onto deployment roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutResolution {
    /// Source path in staging paired with its deployment target.
    pub mappings: Vec<(RelPath, TargetLocation)>,
    /// How the adapter arrived at this mapping, shown in the preview.
    pub rationale: String,
    /// Files the adapter deliberately ignored (readme, screenshots, …).
    pub ignored: Vec<RelPath>,
}

/// Candidate layouts when more than one is plausible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutCandidate {
    /// Short label for the UI.
    pub label: String,
    /// The mapping this candidate would produce.
    pub resolution: LayoutResolution,
}

/// Reads and extracts archives.
#[async_trait]
pub trait ArchiveBackend: Send + Sync {
    /// Whether this backend handles the given archive.
    fn supports(&self, path: &Path) -> bool;

    /// Enumerate and validate entries without writing anything.
    ///
    /// # Errors
    /// Returns [`crate::CoreError::ArchiveRejected`] when a limit or a security
    /// rule is violated.
    async fn inspect(&self, path: &Path, cancel: &CancelToken) -> Result<ArchiveInspection>;

    /// Extract into a *fresh, empty* staging directory and hash the result.
    ///
    /// Implementations must never extract into a game directory, must never
    /// follow or create links, and must enforce the same limits as
    /// [`ArchiveBackend::inspect`] while writing.
    ///
    /// # Errors
    /// Fails on I/O errors, limit violations and cancellation.
    async fn extract(
        &self,
        path: &Path,
        staging: &Path,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<ArchiveManifest>;
}

/// Filesystem operations, behind a trait so the installer can be tested against
/// fault injection without touching a real disk.
#[async_trait]
pub trait FileSystem: Send + Sync {
    /// Whether a path exists.
    async fn exists(&self, path: &Path) -> Result<bool>;

    /// Size and BLAKE3 hash of a file, or `None` if it does not exist.
    ///
    /// # Errors
    /// Fails on I/O errors other than "not found".
    async fn stat_hash(&self, path: &Path) -> Result<Option<(FileHash, u64)>>;

    /// Create a directory and all of its parents.
    async fn create_dir_all(&self, path: &Path) -> Result<()>;

    /// Copy a file, creating parent directories as needed.
    async fn copy_file(&self, from: &Path, to: &Path) -> Result<u64>;

    /// Write bytes to a temporary file adjacent to `final_path` and return it.
    ///
    /// Adjacency matters: the temporary file must be on the same filesystem so
    /// the later rename is atomic.
    async fn write_temp_adjacent(&self, final_path: &Path, from: &Path) -> Result<PathBuf>;

    /// Atomically move `from` onto `to`, replacing it.
    async fn rename(&self, from: &Path, to: &Path) -> Result<()>;

    /// Remove a file. Succeeds if it is already gone.
    async fn remove_file(&self, path: &Path) -> Result<()>;

    /// Remove a directory only if it is empty. Succeeds if already gone.
    async fn remove_dir_if_empty(&self, path: &Path) -> Result<bool>;

    /// Flush a directory entry so a rename survives a crash.
    async fn sync_dir(&self, path: &Path) -> Result<()>;
}

/// Stores credentials in the platform secret store.
///
/// The only implementation Onera ships talks to the Linux Secret Service via
/// `keyring`. There is deliberately no file-backed implementation: if the
/// secret store is unavailable, authentication fails.
#[async_trait]
pub trait SecretStore: Send + Sync {
    /// Read a secret.
    ///
    /// # Errors
    /// Fails if the store is unavailable. Returns `Ok(None)` if the key is
    /// simply absent.
    async fn get(&self, key: &str) -> Result<Option<Secret>>;

    /// Write a secret, replacing any existing value.
    ///
    /// # Errors
    /// Fails if the store is unavailable. Must never fall back to disk.
    async fn set(&self, key: &str, value: &Secret) -> Result<()>;

    /// Delete a secret. Succeeds if it was already absent.
    async fn delete(&self, key: &str) -> Result<()>;

    /// Whether the backing store is reachable right now.
    async fn is_available(&self) -> bool;
}

/// Content-addressed archive storage.
#[async_trait]
pub trait ArchiveStore: Send + Sync {
    /// Absolute path for a stored archive, whether or not it exists yet.
    fn path_for(&self, hash: &FileHash) -> PathBuf;

    /// Whether an archive with this hash is already stored.
    async fn contains(&self, hash: &FileHash) -> Result<bool>;

    /// Move a completed temporary download into storage atomically.
    ///
    /// Returns the final path. If the hash is already present, the temporary
    /// file is discarded and the existing path returned — this is where
    /// download deduplication happens.
    ///
    /// # Errors
    /// Fails on I/O errors.
    async fn promote(&self, temp: &Path, hash: &FileHash) -> Result<PathBuf>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_helpers_behave() {
        let p = Page::single(vec![1, 2, 3]);
        assert_eq!(p.total, Some(3));
        assert!(p.next.is_none());
    }

    /// The ports must stay object-safe: the application layer stores them as
    /// `Arc<dyn Trait>` so adapters can be swapped at runtime and faked in
    /// tests.
    #[test]
    fn ports_are_object_safe() {
        fn assert_object_safe<T: ?Sized>() {}
        assert_object_safe::<dyn ModProvider>();
        assert_object_safe::<dyn GameAdapter>();
        assert_object_safe::<dyn ArchiveBackend>();
        assert_object_safe::<dyn FileSystem>();
        assert_object_safe::<dyn SecretStore>();
        assert_object_safe::<dyn ArchiveStore>();
        assert_object_safe::<dyn ReconciliationStore>();
        assert_object_safe::<dyn AuthProvider>();
        assert_object_safe::<dyn ProgressSink>();
        assert_object_safe::<dyn GameStore>();
        assert_object_safe::<dyn GameManifestProvider>();
        assert_object_safe::<dyn ProfileStore>();
        assert_object_safe::<dyn BaselineStore>();
        assert_object_safe::<dyn DependencyStore>();
    }

    /// A store that cannot answer must be distinguishable from one that answered
    /// "nothing", including after a round trip to the frontend.
    #[test]
    fn an_unknown_store_capability_is_not_an_empty_one() {
        let empty: StoreCapability<Vec<u8>> = StoreCapability::known(vec![]);
        let unknown: StoreCapability<Vec<u8>> = StoreCapability::unknown("Steam is not running");
        assert_ne!(empty, unknown);
        assert!(empty.is_known());
        assert_eq!(empty.value(), Some(&vec![]));
        assert!(!unknown.is_known());
        assert_eq!(unknown.value(), None);

        let json = serde_json::to_string(&unknown).unwrap();
        assert!(json.contains("\"kind\":\"unknown\""), "{json}");
        assert_eq!(
            serde_json::from_str::<StoreCapability<Vec<u8>>>(&json).unwrap(),
            unknown
        );
    }

    /// The default [`ModProvider::dependencies`] must answer for every source it
    /// was given, and must not pass an empty group list off as "no dependencies".
    #[tokio::test]
    async fn the_default_provider_reports_unsupported_rather_than_none() {
        use crate::domain::dependency::DependencyAvailability;

        struct Bare;

        #[async_trait]
        impl ModProvider for Bare {
            fn id(&self) -> ProviderId {
                ProviderId::new("bare")
            }
            async fn games(&self, _: Option<&str>, _: &CancelToken) -> Result<Page<Game>> {
                Ok(Page::single(vec![]))
            }
            async fn mod_metadata(
                &self,
                _: &str,
                _: &ProviderModId,
                _: &CancelToken,
            ) -> Result<(Mod, Vec<Release>)> {
                Err(crate::CoreError::Unsupported("test".into()))
            }
            async fn files(
                &self,
                _: &str,
                _: &ProviderModId,
                _: Option<&str>,
                _: &CancelToken,
            ) -> Result<Page<ProviderFile>> {
                Ok(Page::single(vec![]))
            }
            async fn resolve_download(
                &self,
                _: &str,
                _: &ProviderModId,
                _: &ProviderFileId,
                _: &CancelToken,
            ) -> Result<DownloadTarget> {
                Err(crate::CoreError::Unsupported("test".into()))
            }
        }

        assert!(!Bare.dependency_capability().is_supported());
        let sources = vec![DependencySource {
            provider: ProviderId::new("bare"),
            game_slug: "cyberpunk2077".into(),
            provider_mod_id: ProviderModId::new("1"),
            provider_file_id: None,
            provider_version_id: None,
        }];
        let snapshots = Bare
            .dependencies(&sources, &CancelToken::new())
            .await
            .unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(
            snapshots[0].availability,
            DependencyAvailability::Unsupported
        );
        assert!(!snapshots[0].declares_no_dependencies());
    }
}

// ---------------------------------------------------------------------------
// Persistence ports
// ---------------------------------------------------------------------------

use crate::domain::operation::{Operation, OperationKind, OperationState};
use crate::domain::provider_stack::{ProviderStack, StackEntry};
use crate::domain::reconcile::InstallationMapping;
use crate::domain::reconcile::MutationPlan;
use crate::ids::{
    ArchiveId, BackupId, BaselineId, BaselineScanRunId, DependencyGroupId, InstallationId,
    LocalGameId, ModId, OperationId, ProfileId, ProfileMemberId, ReleaseId,
};
use crate::plan::{InstallPlan, ScopedRule, TargetLocation as PlanTargetLocation};

/// One file's recorded progress inside a journaled operation.
///
/// The installer writes a row per file *before* touching it and updates the row
/// after each atomic step, so recovery can tell exactly which files were
/// already swapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    /// Position in the plan, so recovery replays in the recorded order.
    pub seq: u32,
    /// Where the file goes, as a root key plus a relative path.
    pub target: TargetLocation,
    /// The resolved absolute path of the target.
    ///
    /// Recorded so that crash recovery can act without re-deriving deployment
    /// roots — the game adapter may not even be loadable at recovery time.
    pub absolute_path: PathBuf,
    /// Hash of the content being deployed.
    pub source_hash: FileHash,
    /// Hash that was at the target before, if anything was.
    pub previous_hash: Option<FileHash>,
    /// Backup taken before overwriting, if one was needed.
    pub backup_id: Option<BackupId>,
    /// Temporary file staged next to the target, if one was written.
    pub temp_path: Option<PathBuf>,
    /// How far this file got.
    pub status: JournalStatus,
}

/// How far one journaled file got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalStatus {
    /// Recorded in the plan; nothing done.
    Pending,
    /// Backup taken, temporary file written and hashed.
    Staged,
    /// Renamed into place and re-verified.
    Committed,
    /// Deliberately not applied (a skip, an adopt or a shared identical file).
    Skipped,
    /// Undone.
    RolledBack,
}

/// The operation journal.
///
/// This is the crash-recovery substrate: everything the installer does is
/// written here before it happens and confirmed here after it happens.
#[async_trait]
pub trait OperationJournal: Send + Sync {
    /// Persist a plan and open an operation in
    /// [`OperationState::Planned`].
    async fn begin(&self, plan: &InstallPlan, kind: OperationKind) -> Result<Operation>;

    /// Persist a desired-state reconciliation before it stages any file.
    ///
    /// `kind` distinguishes an ordinary desired-state change from a
    /// return-to-clean, which reaches the same empty stack for a different
    /// reason and must be recoverable as what it was.
    async fn begin_reconciliation(
        &self,
        plan: &MutationPlan,
        kind: OperationKind,
    ) -> Result<Operation>;

    /// Move an operation to a new state.
    ///
    /// # Errors
    /// Returns [`crate::CoreError::Conflict`] if the transition is not legal
    /// for the operation's current state.
    async fn set_state(
        &self,
        id: OperationId,
        state: OperationState,
        error: Option<&str>,
    ) -> Result<()>;

    /// Read an operation back.
    async fn get(&self, id: OperationId) -> Result<Option<Operation>>;

    /// The plan an operation was opened with.
    async fn plan(&self, id: OperationId) -> Result<Option<InstallPlan>>;

    /// Record or update one file's journal row.
    async fn put_entry(&self, id: OperationId, entry: &JournalEntry) -> Result<()>;

    /// All journal rows for an operation, in `seq` order.
    async fn entries(&self, id: OperationId) -> Result<Vec<JournalEntry>>;

    /// Operations that are not in a terminal state.
    ///
    /// Called on startup: anything this returns was interrupted.
    async fn interrupted(&self) -> Result<Vec<Operation>>;
}

/// What is deployed where, and who provides it.
#[async_trait]
pub trait DeploymentStore: Send + Sync {
    /// The provider stack recorded for one target.
    async fn stack(&self, game: LocalGameId, target: &TargetLocation) -> Result<ProviderStack>;

    /// Replace the recorded stack for one target.
    ///
    /// An empty stack removes the row entirely.
    async fn put_stack(
        &self,
        game: LocalGameId,
        target: &TargetLocation,
        stack: &ProviderStack,
    ) -> Result<()>;

    /// Every target an installation currently claims.
    async fn targets_of(&self, installation: InstallationId) -> Result<Vec<TargetLocation>>;

    /// Every target recorded for a game installation.
    async fn all_targets(&self, game: LocalGameId) -> Result<Vec<TargetLocation>>;

    /// Record that an installation exists and what it deployed.
    async fn record_installation(
        &self,
        installation: InstallationId,
        game: LocalGameId,
        mod_id: ModId,
        release: ReleaseId,
        archive: ArchiveId,
    ) -> Result<()>;

    /// Record an acquired artifact that is deliberately *not* deployed.
    ///
    /// This is what preparing a profile activation produces: the archive is
    /// downloaded, validated and mapped, but nothing in the game directory has
    /// changed and no other artifact in the lineage loses its active slot. The
    /// reconciler decides later, under the journal, whether it becomes active.
    async fn record_retained_installation(
        &self,
        installation: InstallationId,
        game: LocalGameId,
        mod_id: ModId,
        release: ReleaseId,
        archive: ArchiveId,
    ) -> Result<()>;

    /// Forget an installation once every file it owned has been released.
    async fn remove_installation(&self, installation: InstallationId) -> Result<()>;

    /// Retain an acquired artifact while removing all of its active claims.
    async fn deactivate_installation(&self, installation: InstallationId) -> Result<()>;

    /// Mark a retained artifact active after its recorded mappings have been
    /// staged, committed, and verified.
    async fn activate_installation(&self, installation: InstallationId) -> Result<()>;

    /// Persist one resolved source-to-target mapping for future reactivation.
    async fn put_mapping(&self, mapping: &InstallationMapping) -> Result<()>;

    /// Stable mappings recorded for a retained artifact.
    async fn mappings_of(&self, installation: InstallationId) -> Result<Vec<InstallationMapping>>;

    /// Record directories an installation had to create.
    ///
    /// Removal only ever deletes directories from this list. A game's own empty
    /// directory — `archive/pc/mod` in a stock Cyberpunk install — must survive
    /// uninstalling every mod, and the only way to know the difference is to
    /// have recorded which ones Onera itself made.
    async fn record_created_dirs(
        &self,
        game: LocalGameId,
        installation: InstallationId,
        dirs: &[PlanTargetLocation],
    ) -> Result<()>;

    /// Directories an installation created, deepest last.
    async fn created_dirs(&self, installation: InstallationId) -> Result<Vec<PlanTargetLocation>>;

    /// Installations belonging to the same mod lineage in one game.
    async fn installations_of_mod(
        &self,
        game: LocalGameId,
        mod_id: ModId,
    ) -> Result<Vec<InstallationId>>;

    /// Append an entry to a path's audit trail.
    async fn record_history(
        &self,
        game: LocalGameId,
        target: &TargetLocation,
        operation: OperationId,
        event: &str,
        entry: Option<&StackEntry>,
    ) -> Result<()>;

    /// Narrowly scoped rules the user chose to remember.
    async fn rules_for(&self, mod_id: ModId) -> Result<Vec<ScopedRule>>;

    /// Remember a rule.
    async fn put_rule(&self, rule: &ScopedRule) -> Result<()>;
}

/// Atomically publishes the database half of a completed reconciliation.
#[async_trait]
pub trait ReconciliationStore: Send + Sync {
    /// Replace all affected stacks, synchronize activation flags, and mark the
    /// operation complete in one transaction after disk verification succeeds.
    async fn complete_reconciliation(
        &self,
        operation: OperationId,
        plan: &MutationPlan,
    ) -> Result<()> {
        self.complete_reconciliation_publishing(operation, plan, None)
            .await
    }

    /// Publish a completed reconciliation and, in the same transaction, make a
    /// profile the active one.
    ///
    /// The two halves must commit together. A profile marked active in its own
    /// statement could survive a crash that lost the deployment it describes,
    /// which is precisely the lie the activation flow exists to prevent: the
    /// target profile is reported active only once the filesystem matches it.
    ///
    /// Any activation attempt recorded for that profile and still `applying` is
    /// finished in the same transaction.
    async fn complete_reconciliation_publishing(
        &self,
        operation: OperationId,
        plan: &MutationPlan,
        activate_profile: Option<ProfileId>,
    ) -> Result<()>;
}

/// Copies of files Onera was about to overwrite.
#[async_trait]
pub trait BackupStore: Send + Sync {
    /// Copy a file aside and record it.
    async fn create(
        &self,
        game: LocalGameId,
        target: &TargetLocation,
        source: &Path,
        hash: &FileHash,
        size: u64,
    ) -> Result<BackupId>;

    /// Where a backup's bytes live.
    async fn path_of(&self, id: BackupId) -> Result<Option<PathBuf>>;

    /// Find backed-up bytes by content hash.
    ///
    /// Backups are content-addressed, so restoring "whatever used to be here"
    /// only needs the hash the provider stack recorded — no path bookkeeping and
    /// no dependence on which operation took the backup.
    async fn path_of_hash(&self, hash: &FileHash) -> Result<Option<PathBuf>>;

    /// Drop a backup's record and its bytes.
    async fn delete(&self, id: BackupId) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Store identity
// ---------------------------------------------------------------------------

/// Something a store may or may not be able to tell us.
///
/// The whole point of this type is that a missing answer is *not* an empty one.
/// A store that exposes no ownership list returns
/// [`StoreCapability::Unknown`], never `Known(vec![])`, because the second would
/// let a solver conclude that the user owns no DLC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoreCapability<T> {
    /// The store answered.
    Known {
        /// The answer.
        value: T,
    },
    /// The store cannot answer, or was not reachable.
    Unknown {
        /// Displayable reason.
        reason: String,
    },
}

impl<T> StoreCapability<T> {
    /// Wrap a known answer.
    pub fn known(value: T) -> Self {
        Self::Known { value }
    }

    /// Wrap a missing answer.
    pub fn unknown(reason: impl Into<String>) -> Self {
        Self::Unknown {
            reason: reason.into(),
        }
    }

    /// The answer, if there is one.
    ///
    /// Callers that turn this into a default value must say so in the UI; this
    /// method deliberately does not offer `unwrap_or_default`.
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Known { value } => Some(value),
            Self::Unknown { .. } => None,
        }
    }

    /// Whether the store answered.
    pub fn is_known(&self) -> bool {
        matches!(self, Self::Known { .. })
    }
}

/// The store that manages a game installation.
///
/// Steam is the first implementation, reading its own local `appmanifest` file.
/// Nothing here requires store credentials or scraping a client's internals.
#[async_trait]
pub trait GameStore: Send + Sync {
    /// Stable slug of this store adapter, e.g. `steam`.
    fn id(&self) -> &str;

    /// Best-effort build identity for an installation.
    ///
    /// # Errors
    /// Fails only on I/O errors that prevent even attempting the read. A store
    /// that simply does not publish an identity returns
    /// [`StoreCapability::Unknown`].
    async fn build_identity(
        &self,
        install: &LocalGameInstall,
    ) -> Result<StoreCapability<StoreBuildIdentity>>;

    /// Store extras the user owns.
    ///
    /// # Errors
    /// Fails only on I/O errors. Unknown ownership is
    /// [`StoreCapability::Unknown`], never an empty list.
    async fn owned_dlc(&self, install: &LocalGameInstall)
        -> Result<StoreCapability<Vec<StoreDlc>>>;
}

/// One file as an authoritative store manifest describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedFile {
    /// Baseline root the file belongs to.
    pub root_key: String,
    /// Path relative to that root.
    pub path: RelPath,
    /// Size in bytes, when the manifest states one.
    pub size: Option<u64>,
    /// Digest as the store published it.
    pub digest: Option<ManifestDigest>,
}

/// A digest published by a store, in the store's own algorithm.
///
/// Deliberately not a [`FileHash`]: that type is Onera's own integrity currency
/// and stores use algorithms Onera does not compute — Steam documents SHA-1 for
/// depot manifests. Keeping them separate stops a store-supplied digest from
/// being mistaken for a hash Onera verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestDigest {
    /// Algorithm name as the store publishes it, lowercase.
    pub algorithm: String,
    /// Lowercase hex digest.
    pub hex: String,
}

/// The complete expected file set for one build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedManifest {
    /// Build the manifest describes.
    pub build_identity: StoreBuildIdentity,
    /// Every file the store says the build contains.
    pub files: Vec<ExpectedFile>,
}

/// Whether an authoritative manifest could be obtained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ManifestAvailability {
    /// The store publishes no manifest Onera may consume.
    Unsupported,
    /// It does, but this request did not get one.
    Unavailable {
        /// Displayable reason.
        reason: String,
    },
    /// A manifest was obtained.
    Available {
        /// The expected file set.
        manifest: Box<ExpectedManifest>,
    },
}

/// A source of authoritative expected-file manifests.
///
/// Future-facing on purpose, and nothing implements it today. Steam documents
/// depot manifests as carrying file paths, sizes, flags and SHA-1 hashes, but
/// publishes no supported consumer API for retrieving them, so Onera's first
/// release captures a local baseline instead. This port exists so that if such
/// an API appears, a manifest can replace local capture without changing the
/// baseline domain, the scanner or the UI.
///
/// Implementations must never require store credentials from the user or scrape
/// a store client's internal state.
#[async_trait]
pub trait GameManifestProvider: Send + Sync {
    /// Stable slug of this manifest source.
    fn id(&self) -> &str;

    /// Fetch the expected file set for an installed build.
    ///
    /// # Errors
    /// Fails on cancellation. A missing or unsupported manifest is reported as
    /// [`ManifestAvailability`], not as an error.
    async fn expected_manifest(
        &self,
        install: &LocalGameInstall,
        identity: &StoreBuildIdentity,
        cancel: &CancelToken,
    ) -> Result<ManifestAvailability>;
}

// ---------------------------------------------------------------------------
// Profile, baseline and dependency persistence
// ---------------------------------------------------------------------------

/// Profiles and their members.
///
/// Everything here is desired state. No implementation of this port touches the
/// game directory; a profile only reaches disk through a previewed, journaled
/// [`MutationPlan`].
#[async_trait]
pub trait ProfileStore: Send + Sync {
    /// Every profile for a local game.
    async fn profiles(&self, game: LocalGameId) -> Result<Vec<Profile>>;

    /// One profile by identifier.
    async fn profile(&self, id: ProfileId) -> Result<Option<Profile>>;

    /// The one active profile for a local game, if the game has profiles.
    async fn active_profile(&self, game: LocalGameId) -> Result<Option<Profile>>;

    /// Insert or update a profile.
    ///
    /// # Errors
    /// Returns [`crate::CoreError::Conflict`] if the name is already used by
    /// another profile of the same local game.
    async fn put_profile(&self, profile: &Profile) -> Result<()>;

    /// Delete a profile and its members.
    ///
    /// # Errors
    /// Returns [`crate::CoreError::Conflict`] if the profile is the active one.
    /// Another profile must be activated first, so a game is never left without
    /// an active profile.
    async fn delete_profile(&self, id: ProfileId) -> Result<()>;

    /// Make one profile the active one for its game, atomically.
    ///
    /// Callers apply this only after the filesystem matches the target profile.
    async fn set_active_profile(&self, game: LocalGameId, profile: ProfileId) -> Result<()>;

    /// Members of a profile, in priority order.
    async fn members(&self, profile: ProfileId) -> Result<Vec<ProfileMember>>;

    /// Insert or update one member.
    async fn put_member(&self, member: &ProfileMember) -> Result<()>;

    /// Remove one member.
    ///
    /// Dependency overrides scoped to that membership go with it: a risk the
    /// user accepted for a mod they have removed must not survive re-adding it.
    async fn remove_member(&self, member: ProfileMemberId) -> Result<()>;

    /// Record an activation attempt or update its state.
    async fn record_activation(&self, activation: &ProfileActivation) -> Result<()>;

    /// Recent activation attempts for a game, newest first.
    async fn activation_history(
        &self,
        game: LocalGameId,
        limit: u32,
    ) -> Result<Vec<ProfileActivation>>;
}

/// Captured baselines, their files, and scan runs.
#[async_trait]
pub trait BaselineStore: Send + Sync {
    /// The baseline Onera currently compares against, if any.
    async fn current_baseline(&self, game: LocalGameId) -> Result<Option<GameBaseline>>;

    /// Every baseline recorded for a game, newest first.
    ///
    /// History is retained across game updates so a superseded capture remains
    /// inspectable.
    async fn baselines(&self, game: LocalGameId) -> Result<Vec<GameBaseline>>;

    /// Store a baseline and its file records together.
    ///
    /// A baseline is immutable once written; a recapture is a new record that
    /// supersedes the old one rather than an update in place.
    async fn put_baseline(&self, baseline: &GameBaseline, files: &[BaselineFile]) -> Result<()>;

    /// Files recorded in a baseline.
    async fn baseline_files(&self, baseline: BaselineId) -> Result<Vec<BaselineFile>>;

    /// Mark a baseline superseded, keeping it and its files.
    async fn supersede_baseline(&self, baseline: BaselineId) -> Result<()>;

    /// Insert or update a scan run's progress and result counts.
    async fn put_scan_run(&self, run: &BaselineScanRun) -> Result<()>;

    /// One scan run by identifier.
    async fn scan_run(&self, id: BaselineScanRunId) -> Result<Option<BaselineScanRun>>;

    /// Persist the findings of a scan run.
    async fn put_findings(
        &self,
        run: BaselineScanRunId,
        findings: &[BaselineFinding],
    ) -> Result<()>;

    /// Findings recorded for a scan run.
    async fn findings(&self, run: BaselineScanRunId) -> Result<Vec<BaselineFinding>>;
}

/// Cached provider dependency data and the user's accepted risks.
#[async_trait]
pub trait DependencyStore: Send + Sync {
    /// The most recent snapshot for a provider version, if one is cached.
    ///
    /// Returning `None` means "nothing cached", which callers must not confuse
    /// with a snapshot that declares no dependencies.
    async fn snapshot(&self, source: &DependencySource) -> Result<Option<DependencySnapshot>>;

    /// Cached snapshots for several versions, in the order requested.
    async fn snapshots(
        &self,
        sources: &[DependencySource],
    ) -> Result<Vec<Option<DependencySnapshot>>>;

    /// Store a snapshot, replacing any previous one for the same version.
    async fn put_snapshot(&self, snapshot: &DependencySnapshot) -> Result<()>;

    /// Overrides recorded for a profile's members.
    async fn overrides(&self, profile: ProfileId) -> Result<Vec<DependencyOverride>>;

    /// Record an accepted risk.
    async fn put_override(&self, decision: &DependencyOverride) -> Result<()>;

    /// Withdraw an accepted risk.
    async fn delete_override(
        &self,
        member: ProfileMemberId,
        group: DependencyGroupId,
    ) -> Result<()>;
}
