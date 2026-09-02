//! Return-to-clean: undo everything Onera did, and report everything it will not.
//!
//! The flow is deliberately narrow about what it is allowed to touch:
//!
//! 1. reconcile to the empty desired state, which restores Onera's
//!    bottom-of-stack unmanaged backups and deletes only files Onera itself
//!    introduced;
//! 2. hash the baseline scope again;
//! 3. report baseline files that are still missing or modified and that Onera
//!    has no trusted backup for — **as needing the store's own repair**, never
//!    by synthesizing content; and
//! 4. report unknown extras separately, and never delete one, with or without
//!    confirmation.
//!
//! Steps 3 and 4 are the whole point. A mod manager that "cleans" a directory by
//! deleting what it does not recognize destroys the user's own files; one that
//! rewrites a damaged game file from nowhere invents bytes it never had. Onera
//! hands both back and says so.

use crate::flow::Onera;
use onera_core::domain::baseline::ScanPurpose;
use onera_core::domain::baseline::{
    BaselineFinding, BaselineVerification, FileClassification, GameBaseline,
};
use onera_core::domain::operation::OperationKind;
use onera_core::domain::reconcile::MutationPlan;
use onera_core::ids::LocalGameId;
use onera_core::progress::{CancelToken, ProgressSink};
use onera_core::{CoreError, RelPath, Result};
use serde::{Deserialize, Serialize};

/// Where restored bytes come from.
///
/// One variant today, and an enum rather than a boolean because "Onera has a
/// backup of this" and "the store could supply this" are different promises and
/// must never collapse into one another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreSource {
    /// Bytes Onera set aside before it overwrote them.
    Backup,
}

/// A baseline file the restore can put back itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestorableFile {
    /// Root the path lives under.
    pub root_key: String,
    /// Path relative to that root.
    pub path: RelPath,
    /// Where the bytes come from.
    pub from: RestoreSource,
}

/// A baseline file only the store can repair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreRepair {
    /// Root the path lives under.
    pub root_key: String,
    /// Path relative to that root.
    pub path: RelPath,
    /// Why it cannot be used as it is.
    pub classification: FileClassification,
}

/// A file nobody claims, which is never deleted by this flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnknownExtra {
    /// Root the path lives under.
    pub root_key: String,
    /// Path relative to that root.
    pub path: RelPath,
}

/// The preview of a return-to-clean.
///
/// Serializes as the `plan_return_to_clean` payload in
/// `docs/frontend-contracts.md`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanRestorePreview {
    /// The Milestone 1 mutation plan, unchanged.
    pub plan: MutationPlan,
    /// Baseline files Onera can put back from its own backups.
    pub restorable: Vec<RestorableFile>,
    /// Baseline files that need the store's own repair.
    pub needs_store_repair: Vec<StoreRepair>,
    /// Files Onera did not deploy and will not delete.
    pub unknown_extras: Vec<UnknownExtra>,
}

/// The result of a return-to-clean that has been applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanRestoreReport {
    /// The plan that was applied.
    pub plan: MutationPlan,
    /// Baseline files the restore put back.
    pub restored: Vec<RestorableFile>,
    /// Baseline files that still need the store's own repair.
    pub needs_store_repair: Vec<StoreRepair>,
    /// Files Onera did not delete and never will without an explicit decision.
    pub unknown_extras: Vec<UnknownExtra>,
    /// The confirming scan, hashed after the restore.
    pub verification: BaselineVerification,
    /// Whether the installation now matches its baseline byte for byte.
    ///
    /// False whenever anything above is non-empty — including unknown extras,
    /// which are differences Onera reports rather than resolves.
    pub clean: bool,
}

impl Onera {
    /// Preview reconciling to an empty active mod set, with baseline context.
    ///
    /// # Errors
    /// Returns [`CoreError::NotFound`] when the game has no baseline: without
    /// one there is nothing to define "clean", and reporting empty repair and
    /// extras lists would present "we did not look" as "nothing is wrong".
    pub async fn plan_return_to_clean(
        &self,
        game: LocalGameId,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<CleanRestorePreview> {
        // Refuse early rather than reporting empty repair and extras lists,
        // which would read as "nothing is wrong" when nothing was checked.
        self.require_baseline(game).await?;
        let prepared = self.plan_state(game, Vec::new()).await?;
        let verification = self
            .verify_baseline_for(game, false, ScanPurpose::Verify, progress, cancel)
            .await?;
        let context = self.clean_context(&verification.findings).await?;
        Ok(CleanRestorePreview {
            plan: prepared.plan,
            restorable: context.restorable,
            needs_store_repair: context.needs_store_repair,
            unknown_extras: context.unknown_extras,
        })
    }

    /// Apply a return-to-clean and confirm the result by hashing.
    ///
    /// The plan is rebuilt here rather than carried over from a preview the user
    /// may have been looking at for a while: the reconciliation engine refuses a
    /// plan whose preconditions no longer hold, and re-planning is cheaper than
    /// explaining a stale one.
    ///
    /// # Errors
    /// As [`Onera::plan_return_to_clean`], plus any failure from the journaled
    /// mutation — which rolls the whole operation back before returning.
    pub async fn apply_return_to_clean(
        &self,
        game: LocalGameId,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<CleanRestoreReport> {
        let baseline = self.require_baseline(game).await?;
        let prepared = self.plan_state(game, Vec::new()).await?;
        let before = self
            .verify_baseline_for(game, false, ScanPurpose::Verify, progress, cancel)
            .await?;
        let restored = self.clean_context(&before.findings).await?.restorable;

        self.apply_state_as(&prepared, OperationKind::CleanRestore, progress, cancel)
            .await?;

        // Hash the scope again. A restore that reports success without
        // re-reading the disk is reporting its own intentions.
        let verification = self
            .verify_baseline_for(game, false, ScanPurpose::CleanRestore, progress, cancel)
            .await?;
        let after = self.clean_context(&verification.findings).await?;
        Ok(CleanRestoreReport {
            plan: prepared.plan,
            restored,
            needs_store_repair: after.needs_store_repair,
            unknown_extras: after.unknown_extras,
            clean: verification.is_clean(&baseline),
            verification,
        })
    }

    /// The current baseline, or an error saying there is nothing to restore to.
    async fn require_baseline(&self, game: LocalGameId) -> Result<GameBaseline> {
        use onera_core::ports::BaselineStore as _;
        self.database()
            .current_baseline(game)
            .await?
            .ok_or_else(|| CoreError::NotFound {
                kind: "baseline",
                id: game.to_string(),
            })
    }

    /// Sort a scan's findings into what Onera will fix, what the store must fix,
    /// and what nobody may touch without the user.
    async fn clean_context(&self, findings: &[BaselineFinding]) -> Result<CleanContext> {
        let mut context = CleanContext::default();
        for finding in findings {
            match finding.classification {
                // Already correct, or Onera's own deployment, which the
                // reconciliation to an empty desired state removes.
                FileClassification::Matching | FileClassification::ExtraManaged => {}
                FileClassification::Modified | FileClassification::Missing => {
                    // Restorable only when Onera actually holds the recorded
                    // bytes. Backups are content-addressed, so the baseline's
                    // own hash is the whole lookup.
                    let held = match &finding.expected {
                        Some(hash) => self.backups().path_of_hash(hash).await?.is_some(),
                        None => false,
                    };
                    if held {
                        context.restorable.push(RestorableFile {
                            root_key: finding.root_key.clone(),
                            path: finding.path.clone(),
                            from: RestoreSource::Backup,
                        });
                    } else {
                        context.needs_store_repair.push(repair(finding));
                    }
                }
                // A link or an unreadable entry where a baseline file belongs is
                // damage Onera will not paper over; anywhere else it is an extra
                // that needs an individual decision.
                FileClassification::Unreadable | FileClassification::SpecialFile => {
                    if finding.expected.is_some() {
                        context.needs_store_repair.push(repair(finding));
                    } else {
                        context.unknown_extras.push(extra(finding));
                    }
                }
                FileClassification::ExtraUnknown => context.unknown_extras.push(extra(finding)),
            }
        }
        Ok(context)
    }
}

#[derive(Default)]
struct CleanContext {
    restorable: Vec<RestorableFile>,
    needs_store_repair: Vec<StoreRepair>,
    unknown_extras: Vec<UnknownExtra>,
}

fn repair(finding: &BaselineFinding) -> StoreRepair {
    StoreRepair {
        root_key: finding.root_key.clone(),
        path: finding.path.clone(),
        classification: finding.classification,
    }
}

fn extra(finding: &BaselineFinding) -> UnknownExtra {
    UnknownExtra {
        root_key: finding.root_key.clone(),
        path: finding.path.clone(),
    }
}
