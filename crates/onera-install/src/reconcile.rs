//! Journaled desired-state reconciliation.

use crate::planner::RootMap;
use onera_core::domain::operation::{OperationKind, OperationState};
use onera_core::domain::reconcile::{InstallationMapping, MutationPlan, MutationStep};
use onera_core::ids::{InstallationId, OperationId, ProfileId};
use onera_core::ports::{
    BackupStore, FileSystem, JournalEntry, JournalStatus, OperationJournal, ReconciliationStore,
};
use onera_core::progress::{CancelToken, ProgressEvent, ProgressSink, Stage};
use onera_core::{CoreError, FileHash, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What a completed reconciliation must publish besides the deployment itself.
///
/// Everything named here commits in the *same* database transaction as the
/// completed operation, so a crash can never leave one half visible without
/// the other.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Publication {
    /// Profile to make active once the filesystem has been verified.
    pub activate_profile: Option<ProfileId>,
}

impl Publication {
    /// Publish nothing beyond the deployment.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            activate_profile: None,
        }
    }

    /// Publish a profile switch alongside the deployment.
    #[must_use]
    pub const fn activating(profile: ProfileId) -> Self {
        Self {
            activate_profile: Some(profile),
        }
    }
}

/// The outcome of one apply attempt, including the operation it journaled.
///
/// Callers that keep their own record of an attempt — profile activation does —
/// need the operation identifier and whether the rollback succeeded even when
/// the apply itself failed. Returning only `Result<()>` would force them to
/// guess both.
#[derive(Debug)]
pub struct ReconciliationAttempt {
    /// The journaled operation, once one was begun.
    pub operation: Option<OperationId>,
    /// Whether a failure was fully undone.
    ///
    /// `false` after a failure means recovery is required; it is never `true`
    /// for an attempt that succeeded.
    pub rolled_back: bool,
    /// What the apply itself concluded.
    pub result: Result<()>,
}

/// Applies one complete desired-state plan as a single journaled operation.
pub struct ReconciliationEngine {
    fs: Arc<dyn FileSystem>,
    journal: Arc<dyn OperationJournal>,
    backups: Arc<dyn BackupStore>,
    state: Arc<dyn ReconciliationStore>,
}

impl ReconciliationEngine {
    /// Assemble the engine from persistence and filesystem ports.
    #[must_use]
    pub fn new(
        fs: Arc<dyn FileSystem>,
        journal: Arc<dyn OperationJournal>,
        backups: Arc<dyn BackupStore>,
        state: Arc<dyn ReconciliationStore>,
    ) -> Self {
        Self {
            fs,
            journal,
            backups,
            state,
        }
    }

    /// Apply a ready plan. Extracted roots contain validated artifacts.
    ///
    /// # Errors
    /// As [`ReconciliationEngine::apply_as`].
    #[allow(clippy::too_many_arguments)]
    pub async fn apply(
        &self,
        plan: &MutationPlan,
        mappings: &[InstallationMapping],
        extracted: &BTreeMap<InstallationId, PathBuf>,
        roots: &RootMap,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<()> {
        self.apply_as(
            plan,
            mappings,
            extracted,
            roots,
            OperationKind::Reconcile,
            progress,
            cancel,
        )
        .await
    }

    /// Apply a ready plan, reporting the journaled operation either way.
    ///
    /// # Errors
    /// Never returns `Err` itself: every failure is carried in
    /// [`ReconciliationAttempt::result`] alongside the operation it belongs to.
    #[allow(clippy::too_many_arguments)]
    pub async fn attempt(
        &self,
        plan: &MutationPlan,
        mappings: &[InstallationMapping],
        extracted: &BTreeMap<InstallationId, PathBuf>,
        roots: &RootMap,
        kind: OperationKind,
        publication: Publication,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> ReconciliationAttempt {
        if !plan.is_ready() {
            return ReconciliationAttempt {
                operation: None,
                rolled_back: false,
                result: Err(CoreError::DecisionRequired(
                    "reconciliation has unresolved cross-mod conflicts".into(),
                )),
            };
        }
        let operation = match self.journal.begin_reconciliation(plan, kind).await {
            Ok(operation) => operation,
            Err(error) => {
                return ReconciliationAttempt {
                    operation: None,
                    rolled_back: false,
                    result: Err(error),
                }
            }
        };
        progress.emit(ProgressEvent::Started {
            operation: Some(operation.id),
            stage: Stage::Deploying,
            total: Some(plan.steps.len() as u64),
        });
        let started = self
            .apply_started(
                operation.id,
                plan,
                mappings,
                extracted,
                roots,
                publication,
                progress,
                cancel,
            )
            .await;
        let (rolled_back, result) = match started {
            Ok(()) => (false, Ok(())),
            Err(original) => match self.rollback(operation.id).await {
                Ok(()) => (true, Err(original)),
                Err(rollback) => (
                    false,
                    Err(CoreError::Conflict(format!(
                        "{original}; reconciliation rollback also failed: {rollback}"
                    ))),
                ),
            },
        };
        progress.emit(ProgressEvent::Finished {
            stage: Stage::Deploying,
            success: result.is_ok(),
        });
        ReconciliationAttempt {
            operation: Some(operation.id),
            rolled_back,
            result,
        }
    }

    /// Apply a ready plan, journaled under an explicit operation kind.
    ///
    /// A return-to-clean reaches the same empty desired state as any other
    /// reconciliation, but recovery and history must be able to say which one it
    /// was, so the kind is chosen by the caller rather than assumed here.
    ///
    /// # Errors
    /// Returns [`CoreError::DecisionRequired`] when cross-mod conflicts remain
    /// unresolved. Any failure after work begins rolls the whole operation back.
    #[allow(clippy::too_many_arguments)]
    pub async fn apply_as(
        &self,
        plan: &MutationPlan,
        mappings: &[InstallationMapping],
        extracted: &BTreeMap<InstallationId, PathBuf>,
        roots: &RootMap,
        kind: OperationKind,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<()> {
        self.attempt(
            plan,
            mappings,
            extracted,
            roots,
            kind,
            Publication::none(),
            progress,
            cancel,
        )
        .await
        .result
    }

    #[allow(clippy::too_many_arguments)]
    async fn apply_started(
        &self,
        operation: OperationId,
        plan: &MutationPlan,
        mappings: &[InstallationMapping],
        extracted: &BTreeMap<InstallationId, PathBuf>,
        roots: &RootMap,
        publication: Publication,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<()> {
        // Preconditions cover metadata-only stack changes as well as writes.
        // Otherwise enabling an identical provider could publish ownership over
        // a file that was edited after preview without ever inspecting it.
        for (target, expected) in &plan.expected_files {
            cancel.check()?;
            let root = roots.get(&target.root_key).ok_or_else(|| {
                CoreError::InvalidInput(format!("no deployment root named {:?}", target.root_key))
            })?;
            let actual = self.fs.stat_hash(&target.path.resolve_under(root)).await?;
            if actual.as_ref().map(|(hash, _)| hash) != expected.as_ref() {
                return Err(CoreError::Conflict(format!(
                    "{target} changed after the reconciliation preview"
                )));
            }
        }
        let mut entries = Vec::new();
        for (seq, step) in plan.steps.iter().enumerate() {
            cancel.check()?;
            let (target, expected) = match step {
                MutationStep::Write {
                    target,
                    expected_previous,
                    ..
                } => (target, expected_previous.as_ref()),
                MutationStep::Delete {
                    target,
                    expected_previous,
                } => (target, Some(expected_previous)),
            };
            let root = roots.get(&target.root_key).ok_or_else(|| {
                CoreError::InvalidInput(format!("no deployment root named {:?}", target.root_key))
            })?;
            let absolute = target.path.resolve_under(root);
            let prior = self.fs.stat_hash(&absolute).await?;
            if prior.as_ref().map(|(hash, _)| hash) != expected {
                return Err(CoreError::Conflict(format!(
                    "{target} changed after the reconciliation preview"
                )));
            }
            let mut entry = JournalEntry {
                seq: u32::try_from(seq).unwrap_or(u32::MAX),
                target: target.clone(),
                absolute_path: absolute.clone(),
                source_hash: FileHash::blake3_of(b""),
                previous_hash: prior.as_ref().map(|(hash, _)| hash.clone()),
                backup_id: None,
                temp_path: None,
                status: JournalStatus::Pending,
            };
            if let Some((hash, size)) = prior {
                entry.backup_id = Some(
                    self.backups
                        .create(plan.desired.local_game_id, target, &absolute, &hash, size)
                        .await?,
                );
            }
            if let MutationStep::Write { provider, .. } = step {
                let source = match provider.provider {
                    onera_core::domain::provider_stack::FileProvider::Installation {
                        installation_id,
                    } => {
                        let mapping = mappings
                            .iter()
                            .find(|mapping| {
                                mapping.installation_id == installation_id
                                    && mapping.target == *target
                                    && mapping.source_hash == provider.hash
                            })
                            .ok_or_else(|| {
                                CoreError::Conflict(format!("no recorded mapping for {target}"))
                            })?;
                        mapping
                            .source
                            .resolve_under(extracted.get(&installation_id).ok_or_else(|| {
                                CoreError::NotFound {
                                    kind: "extracted artifact",
                                    id: installation_id.to_string(),
                                }
                            })?)
                    }
                    onera_core::domain::provider_stack::FileProvider::UnmanagedBackup {
                        backup_id,
                    } => self.backups.path_of(backup_id).await?.ok_or_else(|| {
                        CoreError::NotFound {
                            kind: "backup",
                            id: backup_id.to_string(),
                        }
                    })?,
                };
                verify(self.fs.as_ref(), &source, &provider.hash).await?;
                let temp = self.fs.write_temp_adjacent(&absolute, &source).await?;
                verify(self.fs.as_ref(), &temp, &provider.hash).await?;
                entry.source_hash = provider.hash.clone();
                entry.temp_path = Some(temp);
            }
            entry.status = JournalStatus::Staged;
            self.journal.put_entry(operation, &entry).await?;
            entries.push((step, entry));
        }
        self.journal
            .set_state(operation, OperationState::Prepared, None)
            .await?;
        cancel.check()?;
        self.journal
            .set_state(operation, OperationState::Committing, None)
            .await?;
        for (step, entry) in &entries {
            match step {
                MutationStep::Write { provider, .. } => {
                    self.fs
                        .rename(
                            entry.temp_path.as_ref().expect("staged write"),
                            &entry.absolute_path,
                        )
                        .await?;
                    verify(self.fs.as_ref(), &entry.absolute_path, &provider.hash).await?;
                }
                MutationStep::Delete { .. } => self.fs.remove_file(&entry.absolute_path).await?,
            }
            self.journal
                .put_entry(
                    operation,
                    &JournalEntry {
                        temp_path: None,
                        status: JournalStatus::Committed,
                        ..entry.clone()
                    },
                )
                .await?;
            progress.emit(ProgressEvent::Advanced {
                stage: Stage::Deploying,
                completed: u64::from(entry.seq) + 1,
                total: Some(entries.len() as u64),
                detail: Some(entry.target.to_string()),
            });
        }
        // Every write has been renamed into place and re-hashed above, so the
        // filesystem now matches the plan. Only here may a profile be published
        // as active, and it commits with the deployment, not after it.
        self.state
            .complete_reconciliation_publishing(operation, plan, publication.activate_profile)
            .await
    }

    /// Restore every staged or committed file. SQLite remains at the old state
    /// until the atomic completion transaction succeeds.
    pub async fn rollback(&self, operation: OperationId) -> Result<()> {
        let result = self.rollback_inner(operation).await;
        if let Err(error) = &result {
            if self
                .journal
                .get(operation)
                .await?
                .is_some_and(|op| op.state == OperationState::RollingBack)
            {
                // Preserve the original rollback error. Marking the operation
                // failed is best-effort because persistence may itself be the
                // reason rollback could not finish.
                let _ = self
                    .journal
                    .set_state(operation, OperationState::Failed, Some(&error.to_string()))
                    .await;
            }
        }
        result
    }

    async fn rollback_inner(&self, operation: OperationId) -> Result<()> {
        let Some(op) = self.journal.get(operation).await? else {
            return Err(CoreError::NotFound {
                kind: "operation",
                id: operation.to_string(),
            });
        };
        if op.state.is_terminal() {
            return Ok(());
        }
        if op.state == OperationState::Planned {
            // Staging happens while the operation is Planned. A later staging
            // failure can therefore leave earlier temporary files journaled,
            // even though no target has been changed yet.
            for entry in self.journal.entries(operation).await? {
                if let Some(temp) = &entry.temp_path {
                    self.fs.remove_file(temp).await?;
                }
                self.journal
                    .put_entry(
                        operation,
                        &JournalEntry {
                            status: JournalStatus::RolledBack,
                            ..entry
                        },
                    )
                    .await?;
            }
            return self
                .journal
                .set_state(operation, OperationState::RolledBack, None)
                .await;
        }
        if op.state != OperationState::RollingBack {
            self.journal
                .set_state(operation, OperationState::RollingBack, None)
                .await?;
        }
        let mut entries = self.journal.entries(operation).await?;
        entries.reverse();
        for entry in entries {
            if entry.status == JournalStatus::RolledBack {
                continue;
            }
            if let Some(temp) = &entry.temp_path {
                self.fs.remove_file(temp).await?;
            }
            // In Committing/RollingBack, a Staged row may already have been
            // renamed when a process died before recording Committed. Restoring
            // every non-rolled-back entry is idempotent and closes that window.
            if matches!(
                op.state,
                OperationState::Committing | OperationState::RollingBack
            ) || entry.status == JournalStatus::Committed
            {
                if let Some(backup_id) = entry.backup_id {
                    let backup = self.backups.path_of(backup_id).await?.ok_or_else(|| {
                        CoreError::Conflict(format!("missing rollback backup for {}", entry.target))
                    })?;
                    let temp = self
                        .fs
                        .write_temp_adjacent(&entry.absolute_path, &backup)
                        .await?;
                    self.fs.rename(&temp, &entry.absolute_path).await?;
                } else if entry.previous_hash.is_none() {
                    self.fs.remove_file(&entry.absolute_path).await?;
                }
            }
            self.journal
                .put_entry(
                    operation,
                    &JournalEntry {
                        status: JournalStatus::RolledBack,
                        ..entry
                    },
                )
                .await?;
        }
        self.journal
            .set_state(operation, OperationState::RolledBack, None)
            .await
    }
}

async fn verify(fs: &dyn FileSystem, path: &Path, expected: &FileHash) -> Result<()> {
    let Some((actual, _)) = fs.stat_hash(path).await? else {
        return Err(CoreError::IntegrityMismatch {
            path: path.display().to_string(),
            expected: expected.to_string(),
            actual: "missing".into(),
        });
    };
    if &actual != expected {
        return Err(CoreError::IntegrityMismatch {
            path: path.display().to_string(),
            expected: expected.to_string(),
            actual: actual.to_string(),
        });
    }
    Ok(())
}
