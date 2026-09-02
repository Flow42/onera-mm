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
use crate::domain::game::{DeployRoot, Game, InstallValidation, LocalGameInstall};
use crate::domain::release::{Mod, ProviderFile, Release};
use crate::hash::FileHash;
use crate::ids::{ProviderFileId, ProviderId, ProviderModId};
use crate::paths::RelPath;
use crate::plan::TargetLocation;
use crate::progress::{CancelToken, ProgressSink};
use crate::redact::Secret;
use crate::Result;
use async_trait::async_trait;
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
    }
}

// ---------------------------------------------------------------------------
// Persistence ports
// ---------------------------------------------------------------------------

use crate::domain::operation::{Operation, OperationKind, OperationState};
use crate::domain::provider_stack::{ProviderStack, StackEntry};
use crate::domain::reconcile::InstallationMapping;
use crate::domain::reconcile::MutationPlan;
use crate::ids::{ArchiveId, BackupId, InstallationId, LocalGameId, ModId, OperationId, ReleaseId};
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
    async fn begin_reconciliation(&self, plan: &MutationPlan) -> Result<Operation>;

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
