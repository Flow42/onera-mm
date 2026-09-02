//! Baseline status, capture and verification.
//!
//! The sequencing layer between the read-only scanner in [`onera_install`], the
//! immutable records in [`onera_db`], and the store adapter that says which
//! build is installed. Nothing here touches the filesystem itself.
//!
//! Four rules are enforced here rather than left to the drivers:
//!
//! * **Capture requires an empty active mod set.** A baseline captured over
//!   Onera's own deployments would record modded files as clean.
//! * **A store-verified capture requires the user's explicit confirmation** that
//!   they ran the store's own file verification. Onera cannot check that, so it
//!   asks and records the answer in [`BaselineSource`].
//! * **A manual installation always gets a clearly labelled local snapshot.**
//!   Onera did not learn the path from a store and will not stamp a store
//!   identity on it.
//! * **Unknown is never Fresh.** A missing or incomparable build identity
//!   produces [`BaselineFreshness::Unknown`], which the UI must render as
//!   "cannot be verified".

use crate::flow::Onera;
use onera_core::domain::baseline::{
    BaselineExclusion, BaselineFreshness, BaselineRoot, BaselineScanRun, BaselineSource,
    BaselineStatus, BaselineVerification, GameBaseline, ScanPurpose, ScanState, StoreBuildIdentity,
};
use onera_core::domain::game::{InstallSource, LocalGameInstall};
use onera_core::ids::{BaselineId, LocalGameId};
use onera_core::plan::TargetLocation;
use onera_core::ports::{BaselineStore, DeploymentStore, GameAdapter, GameStore, StoreCapability};
use onera_core::progress::{CancelToken, ProgressSink};
use onera_core::{CoreError, Result};
use onera_discovery::store::SteamGameStore;
use onera_install::baseline::{
    capture_baseline as scan_capture, quick_verify_baseline, verify_baseline as scan_verify,
    BaselineVerificationRequest,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Everything the Game Integrity panel needs in one call.
///
/// Serializes exactly as `docs/frontend-contracts.md` documents the
/// `baseline_status` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineStatusReport {
    /// The current baseline, or `null` when the game has never been captured.
    pub baseline: Option<GameBaseline>,
    /// Whether that baseline still describes the installed build.
    pub freshness: BaselineFreshness,
    /// Build identity read from the store *now*, when it exposes one.
    pub observed_build_identity: Option<StoreBuildIdentity>,
    /// Active Onera mods, which capture requires to be zero.
    pub active_mod_count: u64,
    /// Why capture cannot start, or `null` when it can.
    pub capture_blocked_reason: Option<String>,
}

/// What a capture would scan, shown before a long hash run starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineCapturePreview {
    /// Store-managed roots the adapter declares.
    pub roots: Vec<BaselineRoot>,
    /// Paths inside those roots that are never part of a baseline.
    pub exclusions: Vec<BaselineExclusion>,
    /// Files the scan expects to hash.
    pub estimated_files: u64,
    /// Bytes the scan expects to read.
    pub estimated_bytes: u64,
    /// The source a capture started now would record.
    pub source: BaselineSource,
    /// Whether the user must confirm store verification before capturing.
    pub requires_store_verification: bool,
    /// Why capture cannot start, or `null` when it can.
    pub capture_blocked_reason: Option<String>,
}

impl Onera {
    /// Read the store's build identity for a registered installation.
    ///
    /// Returns `None` when the store exposes nothing comparable — which the
    /// freshness rules turn into `Unknown`, never `Fresh`.
    ///
    /// # Errors
    /// Fails if the game is not registered.
    pub async fn observed_build_identity(
        &self,
        game: LocalGameId,
    ) -> Result<Option<StoreBuildIdentity>> {
        let install = self.local_install(game).await?;
        Ok(observed_identity(&install).await)
    }

    /// The whole Game Integrity panel model for one installation.
    ///
    /// # Errors
    /// Fails if the game is not registered, or on database errors.
    pub async fn baseline_status(&self, game: LocalGameId) -> Result<BaselineStatusReport> {
        let install = self.local_install(game).await?;
        let baseline = BaselineStore::current_baseline(self.database(), game).await?;
        let observed = observed_identity(&install).await;
        let freshness = freshness_of(baseline.as_ref(), observed.as_ref());
        let active = self.database().active_installations(game).await?;
        Ok(BaselineStatusReport {
            baseline,
            freshness,
            observed_build_identity: observed,
            active_mod_count: active.len() as u64,
            capture_blocked_reason: capture_blocked_reason(active.len()),
        })
    }

    /// Whether the current baseline still describes the installed build.
    ///
    /// # Errors
    /// Fails if the game is not registered, or on database errors.
    pub async fn baseline_freshness(&self, game: LocalGameId) -> Result<BaselineFreshness> {
        let install = self.local_install(game).await?;
        let baseline = BaselineStore::current_baseline(self.database(), game).await?;
        let observed = observed_identity(&install).await;
        Ok(freshness_of(baseline.as_ref(), observed.as_ref()))
    }

    /// Every baseline recorded for a game, newest first.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn baseline_history(&self, game: LocalGameId) -> Result<Vec<GameBaseline>> {
        BaselineStore::baselines(self.database(), game).await
    }

    /// Describe the scan a capture would run, without running it.
    ///
    /// The estimate walks directory entries but reads no file contents, so it is
    /// cheap next to the capture itself.
    ///
    /// # Errors
    /// Fails if the game or its adapter is unknown, or a declared root is
    /// missing.
    pub async fn plan_baseline_capture(
        &self,
        game: LocalGameId,
        requested_source: Option<BaselineSource>,
    ) -> Result<BaselineCapturePreview> {
        let install = self.local_install(game).await?;
        let adapter = adapter_for(&install)?;
        let roots = adapter.baseline_roots(&install)?;
        let exclusions = adapter.baseline_exclusions();
        let source = effective_source(&install, requested_source);
        let (estimated_files, estimated_bytes) = estimate_scope(&roots, &exclusions);
        let active = self.database().active_installations(game).await?;
        Ok(BaselineCapturePreview {
            roots,
            exclusions,
            estimated_files,
            estimated_bytes,
            source,
            requires_store_verification: source.is_store_verified(),
            capture_blocked_reason: capture_blocked_reason(active.len()),
        })
    }

    /// Hash the adapter-declared scope and record it as the current baseline.
    ///
    /// The scan is read-only: nothing under a baseline root is created, moved or
    /// modified, and symlinks are rejected rather than followed. A cancelled or
    /// failed scan is still persisted — as a scan run with its terminal state
    /// and no baseline — so an abandoned capture cannot be mistaken for one that
    /// finished.
    ///
    /// # Errors
    /// Returns [`CoreError::Conflict`] while Onera mods are active,
    /// [`CoreError::DecisionRequired`] when a store-verified capture was asked
    /// for without the user's confirmation, and [`CoreError::Cancelled`] when
    /// the scan was interrupted.
    pub async fn capture_baseline(
        &self,
        game: LocalGameId,
        requested_source: Option<BaselineSource>,
        store_verification_confirmed: bool,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<GameBaseline> {
        let install = self.local_install(game).await?;
        let adapter = adapter_for(&install)?;
        let source = effective_source(&install, requested_source);

        let active = self.database().active_installations(game).await?;
        if let Some(reason) = capture_blocked_reason(active.len()) {
            return Err(CoreError::Conflict(reason));
        }
        if source.is_store_verified() && !store_verification_confirmed {
            return Err(CoreError::DecisionRequired(
                "run the store's own file verification, then confirm it finished before capturing \
                 a baseline"
                    .to_owned(),
            ));
        }

        let roots = adapter.baseline_roots(&install)?;
        let exclusions = adapter.baseline_exclusions();
        let capture = scan_capture(game, &roots, &exclusions, progress, cancel).await?;

        // The run is persisted whatever happened to it, including its findings:
        // an abandoned capture that left no trace would be indistinguishable
        // from one that was never started.
        self.persist_run(&capture.run, &capture.findings).await?;
        if capture.run.state != ScanState::Completed {
            return Err(match capture.run.state {
                ScanState::Cancelled => CoreError::Cancelled,
                state => CoreError::InvalidInput(format!(
                    "the baseline scan of {} ended {state:?} and recorded no baseline",
                    install.install_root.display()
                )),
            });
        }

        let baseline = GameBaseline {
            id: BaselineId::new(),
            local_game_id: game,
            source,
            build_identity: observed_identity(&install).await,
            adapter_id: adapter.id().to_owned(),
            reported_version: adapter
                .validate_install(&install.install_root)
                .reported_version,
            status: BaselineStatus::Current,
            captured_at: capture.run.finished_at.unwrap_or_else(chrono::Utc::now),
            scope_fingerprint: capture.scope_fingerprint,
            file_count: capture.files.len() as u64,
            total_bytes: capture.files.iter().map(|file| file.size).sum(),
        };
        // Writing the baseline supersedes the previous current one; neither it
        // nor its file records are deleted.
        BaselineStore::put_baseline(self.database(), &baseline, &capture.files).await?;

        let mut run = capture.run;
        run.baseline_id = Some(baseline.id);
        BaselineStore::put_scan_run(self.database(), &run).await?;
        Ok(baseline)
    }

    /// Compare the installation with its current baseline.
    ///
    /// `quick` trades evidence for responsiveness: it compares sizes and modes
    /// without reading contents and returns
    /// [`onera_core::domain::baseline::ScanEvidence::MetadataOnly`], which
    /// [`BaselineVerification::is_clean`] refuses. It can prove that something
    /// changed; it can never prove that nothing did.
    ///
    /// # Errors
    /// Returns [`CoreError::NotFound`] when the game has no baseline, and
    /// propagates scan and database errors.
    pub async fn verify_baseline(
        &self,
        game: LocalGameId,
        quick: bool,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<BaselineVerification> {
        self.verify_baseline_for(game, quick, ScanPurpose::Verify, progress, cancel)
            .await
    }

    /// As [`Onera::verify_baseline`], recording why the scan ran.
    ///
    /// The confirmation pass after a return-to-clean is the same comparison
    /// asking a different question, and the persisted run says which.
    ///
    /// # Errors
    /// As [`Onera::verify_baseline`].
    pub(crate) async fn verify_baseline_for(
        &self,
        game: LocalGameId,
        quick: bool,
        purpose: ScanPurpose,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<BaselineVerification> {
        let install = self.local_install(game).await?;
        let adapter = adapter_for(&install)?;
        let baseline = BaselineStore::current_baseline(self.database(), game)
            .await?
            .ok_or_else(|| CoreError::NotFound {
                kind: "baseline",
                id: game.to_string(),
            })?;
        let files = BaselineStore::baseline_files(self.database(), baseline.id).await?;
        let managed: BTreeSet<TargetLocation> = DeploymentStore::all_targets(self.database(), game)
            .await?
            .into_iter()
            .collect();
        let roots = adapter.baseline_roots(&install)?;
        let exclusions = adapter.baseline_exclusions();

        let request = BaselineVerificationRequest {
            game,
            baseline: &baseline,
            baseline_files: &files,
            roots: &roots,
            exclusions: &exclusions,
            managed_targets: &managed,
        };
        let mut scan = if quick {
            quick_verify_baseline(request, progress, cancel).await?
        } else {
            scan_verify(request, progress, cancel).await?
        };
        scan.run.purpose = purpose;

        self.persist_run(&scan.run, &scan.verification.findings)
            .await?;
        Ok(scan.verification)
    }

    /// A scan run and its findings, recorded together.
    pub(crate) async fn persist_run(
        &self,
        run: &BaselineScanRun,
        findings: &[onera_core::domain::baseline::BaselineFinding],
    ) -> Result<()> {
        BaselineStore::put_scan_run(self.database(), run).await?;
        BaselineStore::put_findings(self.database(), run.id, findings).await
    }

    /// One registered installation by id.
    pub(crate) async fn local_install(&self, game: LocalGameId) -> Result<LocalGameInstall> {
        self.database()
            .local_installs()
            .await?
            .into_iter()
            .find(|install| install.id == game)
            .ok_or_else(|| CoreError::NotFound {
                kind: "game installation",
                id: game.to_string(),
            })
    }
}

/// Read the store's identity, degrading to `None` rather than guessing.
async fn observed_identity(install: &LocalGameInstall) -> Option<StoreBuildIdentity> {
    match SteamGameStore::new().build_identity(install).await {
        Ok(StoreCapability::Known { value }) => Some(value),
        // Both "the store publishes nothing" and "the read failed" are the same
        // answer here: not enough to compare. Neither may become `Fresh`.
        Ok(StoreCapability::Unknown { .. }) | Err(_) => None,
    }
}

/// The warning a stale or unverifiable baseline earns before an install.
///
/// Neither blocks the install: Onera reports what it knows and lets the user
/// decide. `None` and `Fresh` are silent — the first because there is nothing
/// to be stale, the second because nothing changed.
#[must_use]
pub fn freshness_warning(freshness: &BaselineFreshness) -> Option<String> {
    match freshness {
        BaselineFreshness::None | BaselineFreshness::Fresh => None,
        BaselineFreshness::Stale { .. } => Some(
            "the game's build changed since its baseline was captured, so the baseline is stale. \
             Run the store's file verification and recapture it."
                .to_owned(),
        ),
        BaselineFreshness::Unknown { reason } => Some(format!(
            "the baseline's freshness cannot be determined: {reason}"
        )),
    }
}

/// Evaluate freshness, distinguishing "never captured" from "cannot tell".
fn freshness_of(
    baseline: Option<&GameBaseline>,
    observed: Option<&StoreBuildIdentity>,
) -> BaselineFreshness {
    baseline.map_or(BaselineFreshness::None, |baseline| {
        BaselineFreshness::evaluate(baseline.build_identity.as_ref(), observed)
    })
}

/// The one reason a capture can currently be blocked.
fn capture_blocked_reason(active_mods: usize) -> Option<String> {
    (active_mods > 0).then(|| {
        format!(
            "{active_mods} Onera mod(s) are active; reconcile to an empty desired state before \
             capturing a baseline"
        )
    })
}

/// The source a capture would record, given what the user asked for.
///
/// A manual installation is always a local snapshot: Onera did not learn its
/// path from a store, so no amount of user confirmation makes it store-verified.
fn effective_source(
    install: &LocalGameInstall,
    requested: Option<BaselineSource>,
) -> BaselineSource {
    if install.source == InstallSource::Manual {
        return BaselineSource::LocalSnapshot;
    }
    match requested {
        // Nothing produces a store manifest yet; asking for one would record a
        // stronger claim than the capture supports.
        Some(BaselineSource::StoreManifest) | None => BaselineSource::StoreVerifiedCapture,
        Some(source) => source,
    }
}

fn adapter_for(install: &LocalGameInstall) -> Result<&'static dyn GameAdapter> {
    onera_games::adapter_by_id(&install.adapter_id)
        .ok_or_else(|| CoreError::Unsupported(format!("no adapter named {:?}", install.adapter_id)))
}

/// Count entries and bytes a capture would cover, without reading any contents.
fn estimate_scope(roots: &[BaselineRoot], exclusions: &[BaselineExclusion]) -> (u64, u64) {
    use onera_core::domain::baseline::excluded_by;
    let mut files = 0_u64;
    let mut bytes = 0_u64;
    for root in roots {
        for entry in walkdir::WalkDir::new(&root.path)
            .follow_links(false)
            .into_iter()
            .filter_map(std::result::Result::ok)
        {
            if entry.depth() == 0 || !entry.file_type().is_file() {
                continue;
            }
            let Ok(relative) = entry.path().strip_prefix(&root.path) else {
                continue;
            };
            let Ok(path) = onera_core::RelPath::normalize(&relative.to_string_lossy()) else {
                continue;
            };
            if excluded_by(exclusions, &root.key, &path).is_some() {
                continue;
            }
            files += 1;
            bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    (files, bytes)
}
