//! Profile activation: turning a desired mod set into a deployed one.
//!
//! Activation is three phases with a hard wall between them:
//!
//! 1. **Preview.** [`Onera::plan_profile_activation`] resolves the profile's
//!    members, reconciles them against what is deployed now, and reports the
//!    downloads, byte totals, dependency evidence, baseline freshness and
//!    blockers in one payload. It writes nothing.
//! 2. **Preparation.** Enabled members whose artifact has never been downloaded
//!    are acquired: fetched, inspected, extracted, mapped and recorded as
//!    *retained* artifacts. **Preparation never touches the game directory** —
//!    it only makes bytes available so the reconciler has something to stage.
//! 3. **Apply.** One journaled reconciliation stages every write, commits, and
//!    re-hashes what it wrote. Only then, and inside the same database
//!    transaction that publishes the deployment, does the target profile become
//!    the active one.
//!
//! Every failure — a refused download, a cross-mod conflict that appeared after
//! the preview, a staging error, a rollback — leaves the previous profile
//! active. That is the invariant the whole module exists to hold:
//! [`ProfileActivationState::Applied`] is reached only after the filesystem has
//! been verified, and it is reached in the same commit that flips the flag.
//!
//! What this module deliberately does **not** do is choose versions. A member
//! names the provider file the user picked; an enabled member that names none
//! is a blocker, not an invitation to guess. Picking a compatible version is
//! the Milestone 4 solver's job, and until it exists an honest refusal beats a
//! silent choice.

use crate::flow::{DownloadRequest, Onera};
use onera_core::domain::dependency::{
    DependencyHealth, MemberHealth, ResolutionEvidence, ResolutionOutcome, ResolutionResult,
};
use onera_core::domain::operation::{OperationKind, OperationState};
use onera_core::domain::profile::{
    desired_state, ActivationDownload, Profile, ProfileActivation, ProfileActivationPreview,
    ProfileActivationState, ProfileMember,
};
use onera_core::domain::reconcile::InstallationMapping;
use onera_core::ids::{InstallationId, LocalGameId, OperationId, ProfileId};
use onera_core::ports::{OperationJournal, ProfileStore};
use onera_core::progress::{CancelToken, ProgressSink};
use onera_core::{CoreError, Result};
use onera_install::Publication;

impl Onera {
    /// Preview everything a switch to `profile` entails, writing nothing.
    ///
    /// # Errors
    /// Returns [`CoreError::NotFound`] for an unknown profile or a member whose
    /// retained artifact has vanished, and [`CoreError::Conflict`] when a
    /// retained artifact has no recorded layout and would have to be
    /// reinstalled.
    pub async fn plan_profile_activation(
        &self,
        profile: ProfileId,
    ) -> Result<ProfileActivationPreview> {
        let target = self.require_profile(profile).await?;
        let game = target.local_game_id;
        let members = self.database().members(profile).await?;
        let from = ProfileStore::active_profile(self.database(), game)
            .await?
            .map(|active| active.id);

        let projected = desired_state(game, &members);
        let prepared = self.plan_state(game, projected.state.installations).await?;

        let mut downloads = Vec::new();
        let mut unresolved = Vec::new();
        for member_id in &projected.missing {
            let member = members
                .iter()
                .find(|member| member.id == *member_id)
                .ok_or_else(|| CoreError::NotFound {
                    kind: "profile member",
                    id: member_id.to_string(),
                })?;
            match self.acquisition_of(member).await? {
                Some(download) => downloads.push(download),
                // No artifact and no chosen file: Onera will not invent one.
                None => unresolved.push(member.id),
            }
        }

        Ok(ProfileActivationPreview::assemble(
            from,
            profile,
            prepared.plan,
            downloads,
            self.dependency_evidence(&members),
            self.baseline_freshness(game).await?,
            &unresolved,
        ))
    }

    /// Switch the game to `profile`, acquiring anything missing on the way.
    ///
    /// `expected_fingerprint` is the digest of the preview the user approved.
    /// Supplying it turns a desired-state change made behind the user's back —
    /// another window enabling a mod, a finished install — into a
    /// [`CoreError::Conflict`] asking for a fresh preview, rather than silently
    /// applying a different plan than the one that was shown.
    ///
    /// # Errors
    /// Returns [`CoreError::DecisionRequired`] when the preview is not ready,
    /// [`CoreError::Conflict`] for a stale preview or a plan whose
    /// preconditions moved, and propagates download, archive and filesystem
    /// errors. The previous profile stays active in every one of those cases.
    pub async fn activate_profile(
        &self,
        profile: ProfileId,
        expected_fingerprint: Option<&str>,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<ProfileActivation> {
        let target = self.require_profile(profile).await?;
        let game = target.local_game_id;

        // Held across preparation *and* apply: acquiring artifacts for a switch
        // and then applying it is one operation from the game's point of view.
        let _guard = self.locks.acquire(game).await;

        let preview = self.plan_profile_activation(profile).await?;
        if !preview.matches_fingerprint(expected_fingerprint) {
            return Err(CoreError::Conflict(
                "this profile or its deployment changed since the preview was built; \
                 plan the activation again"
                    .into(),
            ));
        }
        if !preview.ready {
            return Err(CoreError::DecisionRequired(format!(
                "the activation has {} unresolved blocker(s)",
                preview.blockers.len()
            )));
        }

        let mut activation = ProfileActivation {
            from_profile_id: preview.from_profile_id,
            to_profile_id: profile,
            operation_id: None,
            state: ProfileActivationState::Preparing,
            started_at: micros_now(),
            finished_at: None,
            error: None,
        };
        self.database().record_activation(&activation).await?;

        let mut undone = false;
        match self
            .run_activation(
                &target,
                &preview,
                &mut activation,
                &mut undone,
                progress,
                cancel,
            )
            .await
        {
            Ok(()) => {
                // The completion transaction published both the deployment and
                // the profile switch, so the authoritative record is the stored
                // one rather than anything this function still holds.
                self.stored_activation(game, &activation).await
            }
            Err(error) => {
                // Nothing was journaled, or the journal undid everything: the
                // attempt is fully reversed. Anything else needs a human.
                activation.state = if activation.operation_id.is_none() || undone {
                    ProfileActivationState::RolledBack
                } else {
                    ProfileActivationState::Failed
                };
                activation.finished_at = Some(micros_now());
                activation.error = Some(error.to_string());
                self.database().record_activation(&activation).await?;
                Err(error)
            }
        }
    }

    /// Recent activation attempts for a game, newest first.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn profile_activation_history(
        &self,
        game: LocalGameId,
        limit: u32,
    ) -> Result<Vec<ProfileActivation>> {
        self.database().activation_history(game, limit).await
    }

    /// Report the dependency health currently known for a profile.
    ///
    /// Milestone 3 exposes the complete profile contract even though dependency
    /// ingestion and solving belong to Milestone 4. Providers without that
    /// capability are reported as unsupported; providers that advertise it but
    /// have not supplied definitions are reported as unknown. Neither state is
    /// fabricated as a successful dependency check.
    ///
    /// # Errors
    /// Returns [`CoreError::NotFound`] for an unknown profile and propagates
    /// database errors while loading its members.
    pub async fn resolve_profile_dependencies(
        &self,
        profile: ProfileId,
    ) -> Result<ResolutionResult> {
        self.require_profile(profile).await?;
        let members = self.database().members(profile).await?;
        Ok(self.dependency_evidence(&members))
    }

    /// Finish activation records left behind by a process that died mid switch.
    ///
    /// Called on startup, *after* journal recovery: an activation whose
    /// operation is still non-terminal is left alone so the operation can be
    /// rolled back first. Nothing here can make a target profile active — the
    /// only code path that does is the completion transaction, which by
    /// definition did not run for any row this returns.
    ///
    /// # Errors
    /// Propagates database and journal errors.
    pub async fn recover_profile_activations(&self) -> Result<Vec<ProfileActivation>> {
        let mut finalized = Vec::new();
        for mut activation in self.database().interrupted_activations().await? {
            let operation = match activation.operation_id {
                None => None,
                Some(id) => OperationJournal::get(self.database(), id).await?,
            };
            let (state, error) = match operation {
                // Nothing was journaled, so nothing was written.
                None => (
                    ProfileActivationState::RolledBack,
                    "interrupted before any file was staged",
                ),
                Some(operation) if !operation.state.is_terminal() => continue,
                Some(operation) if operation.state == OperationState::RolledBack => (
                    ProfileActivationState::RolledBack,
                    "interrupted and rolled back",
                ),
                // A completed operation whose activation was never published
                // means the two halves came apart. Say so rather than claiming
                // the switch worked.
                Some(operation) if operation.state == OperationState::Complete => (
                    ProfileActivationState::Failed,
                    "the deployment completed but the profile switch did not; verify the game",
                ),
                Some(_) => (
                    ProfileActivationState::Failed,
                    "interrupted and could not be undone automatically",
                ),
            };
            activation.state = state;
            activation.finished_at = Some(micros_now());
            activation.error = Some(error.to_owned());
            self.database().record_activation(&activation).await?;
            finalized.push(activation);
        }
        Ok(finalized)
    }

    // -----------------------------------------------------------------------
    // Internals
    // -----------------------------------------------------------------------

    /// Preparation and apply, with the game lock already held.
    async fn run_activation(
        &self,
        target: &Profile,
        preview: &ProfileActivationPreview,
        activation: &mut ProfileActivation,
        undone: &mut bool,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<()> {
        let game = target.local_game_id;

        // ---- prepare: acquire artifacts, never touching the game ----------
        for download in &preview.downloads {
            cancel.check()?;
            let member = self
                .database()
                .profile_member(download.member_id)
                .await?
                .ok_or_else(|| CoreError::NotFound {
                    kind: "profile member",
                    id: download.member_id.to_string(),
                })?;
            self.acquire_member_artifact(game, &member, progress, cancel)
                .await?;
        }
        cancel.check()?;

        // ---- re-plan: acquisition can introduce a new cross-mod conflict --
        let members = self.database().members(target.id).await?;
        let projected = desired_state(game, &members);
        if !projected.missing.is_empty() {
            return Err(CoreError::Conflict(format!(
                "{} enabled member(s) still have no artifact after preparation",
                projected.missing.len()
            )));
        }
        let prepared = self.plan_state(game, projected.state.installations).await?;
        if !prepared.plan.is_ready() {
            return Err(CoreError::DecisionRequired(format!(
                "{} cross-mod conflict(s) appeared once the missing artifacts were acquired",
                prepared.plan.conflicts.len()
            )));
        }

        // ---- apply: one journaled operation, published atomically ---------
        activation.state = ProfileActivationState::Applying;
        self.database().record_activation(activation).await?;

        let attempt = self
            .apply_state_locked(
                &prepared,
                OperationKind::Reconcile,
                Publication::activating(target.id),
                progress,
                cancel,
            )
            .await;
        activation.operation_id = attempt.operation;
        *undone = attempt.rolled_back;
        attempt.result
    }

    /// Download, validate, map and retain one member's artifact.
    ///
    /// Everything this writes is content-addressed storage and database rows.
    /// The game directory is not opened, let alone modified: the artifact is
    /// recorded as retained-but-inactive and the reconciler decides later,
    /// under the journal, whether any of its bytes reach the game.
    async fn acquire_member_artifact(
        &self,
        game: LocalGameId,
        member: &ProfileMember,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<InstallationId> {
        let (_, adapter) = self.roots_for(game).await?;
        let the_mod = self
            .database()
            .mod_by_id(member.mod_id)
            .await?
            .ok_or_else(|| CoreError::NotFound {
                kind: "mod",
                id: member.mod_id.to_string(),
            })?;
        let provider_file_id = member.selection.provider_file_id.clone().ok_or_else(|| {
            CoreError::DecisionRequired(format!(
                "profile member {} names no provider file to download",
                member.id
            ))
        })?;

        // Refresh the catalogue only when the chosen file is not cached: the
        // member's explicit choice is authoritative and is never replaced by
        // whatever the provider currently calls newest.
        let mut file = self
            .database()
            .provider_file(&member.selection.provider, &provider_file_id)
            .await?;
        if file.is_none() {
            self.fetch_mod(&the_mod.game_slug, &the_mod.provider_mod_id, cancel)
                .await?;
            file = self
                .database()
                .provider_file(&member.selection.provider, &provider_file_id)
                .await?;
        }
        let file = file.ok_or_else(|| CoreError::NotFound {
            kind: "provider file",
            id: provider_file_id.to_string(),
        })?;

        let outcome = self
            .download(
                &DownloadRequest {
                    game_slug: the_mod.game_slug.clone(),
                    provider_mod_id: the_mod.provider_mod_id.clone(),
                    provider_file_id: provider_file_id.clone(),
                    filename: file.name.clone(),
                    expected_size: file.size_bytes,
                    expected_hash: file.published_hash.clone(),
                },
                progress,
                cancel,
            )
            .await?;

        // Inspect before extracting, exactly as a single install does: an
        // archive acquired for a profile gets no weaker safety treatment.
        self.archives.inspect(&outcome.path, cancel).await?;
        let staging = self.paths.staging_for(OperationId::new());
        let extracted = self
            .archives
            .extract(&outcome.path, &staging, progress, cancel)
            .await;
        let result = async {
            let manifest = extracted?;
            self.database()
                .record_archive_entries(outcome.archive_id, &manifest)
                .await?;
            let layout = adapter.resolve_layout(&manifest)?;

            let installation = InstallationId::new();
            let mut mappings = Vec::with_capacity(layout.mappings.len());
            for (source, target) in &layout.mappings {
                adapter.validate_target(target)?;
                let entry = manifest.file(source).ok_or_else(|| {
                    CoreError::Conflict(format!(
                        "the adapter mapped {source}, which the archive does not contain"
                    ))
                })?;
                mappings.push(InstallationMapping {
                    installation_id: installation,
                    source: source.clone(),
                    target: target.clone(),
                    source_hash: entry.hash.clone(),
                    source_size: entry.size,
                });
            }

            use onera_core::ports::DeploymentStore as _;
            self.database()
                .record_retained_installation(
                    installation,
                    game,
                    member.mod_id,
                    file.release_id,
                    outcome.archive_id,
                )
                .await?;
            for mapping in &mappings {
                self.database().put_mapping(mapping).await?;
            }
            self.database()
                .put_member(&ProfileMember {
                    installation_id: Some(installation),
                    ..member.clone()
                })
                .await?;
            Ok::<_, CoreError>(installation)
        }
        .await;

        // The extracted tree is disposable: reactivation re-extracts from the
        // retained archive and revalidates against the mappings recorded above.
        let _ = tokio::fs::remove_dir_all(&staging).await;
        result
    }

    /// What acquiring one enabled, artifact-less member would involve.
    ///
    /// `None` means the member names no provider file, which is a blocker
    /// rather than a download.
    async fn acquisition_of(&self, member: &ProfileMember) -> Result<Option<ActivationDownload>> {
        let Some(provider_file_id) = member.selection.provider_file_id.as_ref() else {
            return Ok(None);
        };
        let cached = self
            .database()
            .provider_file(&member.selection.provider, provider_file_id)
            .await?;
        let name = match &cached {
            Some(file) => file.name.clone(),
            None => self
                .database()
                .mod_by_id(member.mod_id)
                .await?
                .map_or_else(|| provider_file_id.to_string(), |the_mod| the_mod.name),
        };
        Ok(Some(ActivationDownload {
            member_id: member.id,
            mod_id: member.mod_id,
            name,
            // `None` is "the provider did not say", which a byte total must not
            // silently read as zero.
            bytes: cached.and_then(|file| file.size_bytes),
        }))
    }

    /// Dependency state for a profile's enabled members.
    ///
    /// Onera has no dependency ingestion yet, so this reports what it actually
    /// knows and nothing more. A provider that models no dependencies gives
    /// [`DependencyHealth::NotApplicable`] — there is nothing to check. A
    /// provider that *does* model them gives [`DependencyHealth::Unknown`],
    /// because the requirements exist and have not been read; that blocks an
    /// apply, which is the intended, conservative answer. Neither is ever
    /// reported as satisfied.
    fn dependency_evidence(&self, members: &[ProfileMember]) -> ResolutionResult {
        let supported = self.provider.dependency_capability().is_supported();
        let mut evidence = ResolutionEvidence::default();
        let mut health = Vec::new();
        for member in members.iter().filter(|member| member.desired.is_enabled()) {
            if supported {
                evidence.unavailable += 1;
            } else {
                evidence.unsupported += 1;
            }
            health.push(MemberHealth {
                profile_member_id: member.id,
                health: if supported {
                    DependencyHealth::Unknown
                } else {
                    DependencyHealth::NotApplicable
                },
                unsatisfied: Vec::new(),
            });
        }
        let outcome = if health
            .iter()
            .any(|member| member.health == DependencyHealth::Unknown)
        {
            ResolutionOutcome::Unknown {
                reason: "dependency definitions have not been retrieved for this provider".into(),
            }
        } else {
            ResolutionOutcome::Compatible
        };
        ResolutionResult {
            outcome,
            health,
            evidence,
        }
    }

    async fn require_profile(&self, profile: ProfileId) -> Result<Profile> {
        ProfileStore::profile(self.database(), profile)
            .await?
            .ok_or_else(|| CoreError::NotFound {
                kind: "profile",
                id: profile.to_string(),
            })
    }

    /// Re-read an attempt from the history it was recorded in.
    async fn stored_activation(
        &self,
        game: LocalGameId,
        attempt: &ProfileActivation,
    ) -> Result<ProfileActivation> {
        self.database()
            .activation_history(game, ACTIVATION_HISTORY_LOOKBACK)
            .await?
            .into_iter()
            .find(|recorded| {
                recorded.to_profile_id == attempt.to_profile_id
                    && recorded.started_at == attempt.started_at
            })
            .ok_or_else(|| CoreError::NotFound {
                kind: "profile activation",
                id: attempt.to_profile_id.to_string(),
            })
    }
}

/// How far back [`Onera::activate_profile`] looks for its own record.
const ACTIVATION_HISTORY_LOOKBACK: u32 = 32;

/// Now, truncated to the precision the database stores.
///
/// The activation history is keyed by `(profile, started_at)`, so an in-memory
/// timestamp that does not survive a round trip would fail to find its own row.
fn micros_now() -> chrono::DateTime<chrono::Utc> {
    let now = chrono::Utc::now();
    chrono::DateTime::from_timestamp_micros(now.timestamp_micros()).unwrap_or(now)
}
