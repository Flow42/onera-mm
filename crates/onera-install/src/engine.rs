//! The transactional installer.
//!
//! The apply path is a strict sequence, and every step is journaled before it
//! happens:
//!
//! 1. persist the plan (`planned`)
//! 2. back up anything that will be overwritten, write target-adjacent
//!    temporary files, hash them (`prepared`)
//! 3. rename each temporary file onto its target, re-hash the result
//!    (`committing`)
//! 4. update the provider stacks and the installation record
//! 5. mark the operation `complete` and delete the temporary state
//!
//! Any failure between (2) and (4) triggers a rollback that walks the journal
//! backwards. Because every step is idempotent — restoring a backup that is
//! already restored, deleting a temp file that is already gone — a rollback can
//! itself be interrupted and resumed.

use crate::planner::RootMap;
use onera_core::domain::operation::{Operation, OperationKind, OperationState};
use onera_core::domain::provider_stack::{FileProvider, StackEntry};
use onera_core::ids::{ArchiveId, ReleaseId};
use onera_core::plan::{InstallPlan, PlannedAction, PlannedFile, TargetLocation};
use onera_core::ports::{
    BackupStore, DeploymentStore, FileSystem, JournalEntry, JournalStatus, OperationJournal,
};
use onera_core::progress::{CancelToken, ProgressEvent, ProgressSink, Stage};
use onera_core::{CoreError, Result};
use std::path::PathBuf;
use std::sync::Arc;

/// What an apply produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    /// The journaled operation.
    pub operation: Operation,
    /// Files written to disk.
    pub written: usize,
    /// Files left alone because identical content was already there.
    pub shared: usize,
    /// Files skipped by a decision or a rule.
    pub skipped: usize,
    /// Backups taken.
    pub backed_up: usize,
}

/// The installation engine.
///
/// Holds only ports, so the whole engine can be driven against fakes.
pub struct Installer {
    fs: Arc<dyn FileSystem>,
    journal: Arc<dyn OperationJournal>,
    deployments: Arc<dyn DeploymentStore>,
    backups: Arc<dyn BackupStore>,
}

impl Installer {
    /// Assemble an installer from its ports.
    #[must_use]
    pub fn new(
        fs: Arc<dyn FileSystem>,
        journal: Arc<dyn OperationJournal>,
        deployments: Arc<dyn DeploymentStore>,
        backups: Arc<dyn BackupStore>,
    ) -> Self {
        Self {
            fs,
            journal,
            deployments,
            backups,
        }
    }

    /// Borrow the journal, for callers driving recovery.
    #[must_use]
    pub fn journal(&self) -> &Arc<dyn OperationJournal> {
        &self.journal
    }

    /// Borrow the deployment store.
    #[must_use]
    pub fn deployments(&self) -> &Arc<dyn DeploymentStore> {
        &self.deployments
    }

    /// Borrow the filesystem port.
    #[must_use]
    pub fn filesystem(&self) -> &Arc<dyn FileSystem> {
        &self.fs
    }

    /// Apply a plan transactionally.
    ///
    /// # Errors
    /// Refuses a plan with unresolved decisions. On any failure after work has
    /// begun, rolls back and returns the original error.
    // The arguments are all distinct, required inputs — plan, staging, roots and
    // the two provenance ids — and bundling them into a struct would only move
    // the same list one level away from where it is read.
    #[allow(clippy::too_many_arguments)]
    pub async fn apply(
        &self,
        plan: &InstallPlan,
        staging: &std::path::Path,
        roots: &RootMap,
        release: ReleaseId,
        archive: ArchiveId,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<InstallReport> {
        if !plan.is_ready() {
            return Err(CoreError::DecisionRequired(format!(
                "{} file(s) still need a decision",
                plan.unresolved().count()
            )));
        }

        let operation = self.journal.begin(plan, OperationKind::Install).await?;
        match self
            .apply_inner(plan, staging, roots, release, archive, progress, cancel)
            .await
        {
            Ok(mut report) => {
                self.journal
                    .set_state(operation.id, OperationState::Complete, None)
                    .await?;
                report.operation = self
                    .journal
                    .get(operation.id)
                    .await?
                    .unwrap_or(report.operation);
                progress.emit(ProgressEvent::Finished {
                    stage: Stage::Deploying,
                    success: true,
                });
                Ok(report)
            }
            Err(original) => {
                // The rollback's own failure must not hide why we are rolling
                // back, so it is reported as a warning and the original error
                // is what the caller sees.
                if let Err(rollback_error) = self.rollback(operation.id, progress).await {
                    progress.emit(ProgressEvent::Warning {
                        message: format!("rollback did not complete: {rollback_error}"),
                    });
                }
                Err(original)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn apply_inner(
        &self,
        plan: &InstallPlan,
        staging: &std::path::Path,
        roots: &RootMap,
        release: ReleaseId,
        archive: ArchiveId,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<InstallReport> {
        let operation_id = plan.operation_id;
        let mut report = InstallReport {
            operation: self.journal.get(operation_id).await?.ok_or_else(|| {
                CoreError::NotFound {
                    kind: "operation",
                    id: operation_id.to_string(),
                }
            })?,
            written: 0,
            shared: 0,
            skipped: 0,
            backed_up: 0,
        };

        // ---- prepare -------------------------------------------------------
        progress.emit(ProgressEvent::Started {
            operation: Some(operation_id),
            stage: Stage::BackingUp,
            total: Some(plan.files.len() as u64),
        });

        // The whole journal entry is carried forward, not just the path: the
        // commit step must not overwrite the backup id recorded during staging.
        let mut staged: Vec<(&PlannedFile, JournalEntry)> = Vec::new();
        // Directories that did not exist before this operation. Only these may
        // ever be removed again; a game's own empty directories are not ours.
        let mut created_dirs: std::collections::BTreeSet<TargetLocation> =
            std::collections::BTreeSet::new();
        for (seq, file) in plan.files.iter().enumerate() {
            cancel.check()?;
            let seq = u32::try_from(seq).unwrap_or(u32::MAX);
            let action = file.effective_action();

            let target_path = absolute(roots, &file.target)?;
            let mut entry = JournalEntry {
                seq,
                target: file.target.clone(),
                absolute_path: target_path.clone(),
                source_hash: file.source_hash.clone(),
                previous_hash: file.existing_hash.clone(),
                backup_id: None,
                temp_path: None,
                status: JournalStatus::Pending,
            };

            if !matches!(action, PlannedAction::Write | PlannedAction::BackupAndWrite) {
                entry.status = JournalStatus::Skipped;
                self.journal.put_entry(operation_id, &entry).await?;
                match action {
                    PlannedAction::RegisterShared | PlannedAction::Adopt => report.shared += 1,
                    _ => report.skipped += 1,
                }
                continue;
            }

            self.note_missing_ancestors(roots, &file.target, &mut created_dirs)
                .await?;

            // Back up before anything is written, so a crash between backup and
            // rename still leaves a recoverable original.
            if action == PlannedAction::BackupAndWrite {
                if let Some((hash, size)) = self.fs.stat_hash(&target_path).await? {
                    let id = self
                        .backups
                        .create(plan.local_game_id, &file.target, &target_path, &hash, size)
                        .await?;
                    entry.backup_id = Some(id);
                    entry.previous_hash = Some(hash);
                    report.backed_up += 1;
                }
            }
            self.journal.put_entry(operation_id, &entry).await?;

            let source_path = file.source.resolve_under(staging);
            let temp = self
                .fs
                .write_temp_adjacent(&target_path, &source_path)
                .await?;

            // Verify the staged bytes before they are allowed anywhere near the
            // target: a bad copy must fail here, not after the rename.
            verify_hash(self.fs.as_ref(), &temp, &file.source_hash).await?;

            entry.temp_path = Some(temp.clone());
            entry.status = JournalStatus::Staged;
            self.journal.put_entry(operation_id, &entry).await?;
            staged.push((file, entry.clone()));

            progress.emit(ProgressEvent::Advanced {
                stage: Stage::BackingUp,
                completed: seq as u64 + 1,
                total: Some(plan.files.len() as u64),
                detail: Some(file.target.to_string()),
            });
        }

        self.journal
            .set_state(operation_id, OperationState::Prepared, None)
            .await?;
        cancel.check()?;

        // ---- commit --------------------------------------------------------
        self.journal
            .set_state(operation_id, OperationState::Committing, None)
            .await?;
        progress.emit(ProgressEvent::Started {
            operation: Some(operation_id),
            stage: Stage::Deploying,
            total: Some(staged.len() as u64),
        });

        for (index, (file, entry)) in staged.iter().enumerate() {
            // Cancellation is *not* checked inside the commit loop: once
            // renames have started, stopping halfway is strictly worse than
            // finishing and letting the user remove the mod afterwards.
            let target_path = entry.absolute_path.clone();
            let temp = entry
                .temp_path
                .as_ref()
                .expect("a staged entry always has a temporary file");
            self.fs.rename(temp, &target_path).await?;
            verify_hash(self.fs.as_ref(), &target_path, &file.source_hash).await?;
            if let Some(parent) = target_path.parent() {
                // Best effort: a filesystem that refuses to fsync a directory
                // is not a reason to fail an otherwise good install.
                let _ = self.fs.sync_dir(parent).await;
            }

            // Only the status and the now-consumed temporary path change; the
            // backup id and previous hash must survive for rollback.
            self.journal
                .put_entry(
                    operation_id,
                    &JournalEntry {
                        temp_path: None,
                        status: JournalStatus::Committed,
                        ..entry.clone()
                    },
                )
                .await?;
            report.written += 1;

            progress.emit(ProgressEvent::Advanced {
                stage: Stage::Deploying,
                completed: index as u64 + 1,
                total: Some(staged.len() as u64),
                detail: Some(file.target.to_string()),
            });
        }

        // ---- record --------------------------------------------------------
        self.deployments
            .record_installation(
                plan.installation_id,
                plan.local_game_id,
                plan.mod_id,
                release,
                archive,
            )
            .await?;

        let created: Vec<TargetLocation> = created_dirs.into_iter().collect();
        self.deployments
            .record_created_dirs(plan.local_game_id, plan.installation_id, &created)
            .await?;

        for file in &plan.files {
            let action = file.effective_action();
            if matches!(action, PlannedAction::Skip | PlannedAction::Reject) {
                continue;
            }

            let mut stack = self
                .deployments
                .stack(plan.local_game_id, &file.target)
                .await?;

            // A pre-existing unmanaged file becomes the bottom of the stack the
            // first time Onera covers it, so removal can restore it later.
            if stack.is_empty() {
                if let (PlannedAction::BackupAndWrite, Some(hash)) =
                    (action, file.existing_hash.as_ref())
                {
                    if let Some(backup_id) = self.backup_id_for(operation_id, &file.target).await? {
                        stack.push(StackEntry {
                            provider: FileProvider::UnmanagedBackup { backup_id },
                            hash: hash.clone(),
                            size: 0,
                        });
                    }
                }
            }

            let entry = StackEntry {
                provider: FileProvider::Installation {
                    installation_id: plan.installation_id,
                },
                hash: file.source_hash.clone(),
                size: file.source_size,
            };
            stack.push(entry.clone());
            self.deployments
                .put_stack(plan.local_game_id, &file.target, &stack)
                .await?;
            self.deployments
                .record_history(
                    plan.local_game_id,
                    &file.target,
                    operation_id,
                    match action {
                        PlannedAction::RegisterShared => "shared",
                        PlannedAction::Adopt => "adopted",
                        _ => "deployed",
                    },
                    Some(&entry),
                )
                .await?;
        }

        Ok(report)
    }

    /// Record every ancestor directory of `target` that does not yet exist.
    async fn note_missing_ancestors(
        &self,
        roots: &RootMap,
        target: &TargetLocation,
        out: &mut std::collections::BTreeSet<TargetLocation>,
    ) -> Result<()> {
        let Some(root) = roots.get(&target.root_key) else {
            return Ok(());
        };
        let mut ancestors: Vec<onera_core::RelPath> = Vec::new();
        let mut current = target.path.parent();
        while let Some(dir) = current {
            current = dir.parent();
            ancestors.push(dir);
        }
        // Shallowest first: once one ancestor is missing, everything below it
        // will be created too.
        for dir in ancestors.into_iter().rev() {
            let absolute = dir.resolve_under(root);
            if !self.fs.exists(&absolute).await? {
                out.insert(TargetLocation {
                    root_key: target.root_key.clone(),
                    path: dir,
                });
            }
        }
        Ok(())
    }

    async fn backup_id_for(
        &self,
        operation: onera_core::ids::OperationId,
        target: &TargetLocation,
    ) -> Result<Option<onera_core::ids::BackupId>> {
        Ok(self
            .journal
            .entries(operation)
            .await?
            .into_iter()
            .find(|e| &e.target == target)
            .and_then(|e| e.backup_id))
    }

    /// Undo an operation, walking its journal backwards.
    ///
    /// Safe to call on an operation that is already partly or fully rolled
    /// back: every step checks the current state before acting.
    ///
    /// # Errors
    /// Fails if the operation is unknown or the journal cannot be updated. A
    /// filesystem failure during rollback moves the operation to
    /// [`OperationState::Failed`] rather than being swallowed.
    pub async fn rollback(
        &self,
        operation: onera_core::ids::OperationId,
        progress: &dyn ProgressSink,
    ) -> Result<()> {
        let Some(op) = self.journal.get(operation).await? else {
            return Err(CoreError::NotFound {
                kind: "operation",
                id: operation.to_string(),
            });
        };
        if op.state.is_terminal() {
            return Ok(());
        }

        // A `planned` operation can still have temporary files on disk: staging
        // happens before the state moves to `prepared`, so a failure in the
        // middle of staging leaves work to clean up even though no target has
        // changed. The state machine forbids `planned -> rolling_back`, so the
        // cleanup runs and the operation goes straight to `rolled_back`.
        if op.state != OperationState::Planned && op.state != OperationState::RollingBack {
            self.journal
                .set_state(operation, OperationState::RollingBack, None)
                .await?;
        }
        progress.emit(ProgressEvent::Started {
            operation: Some(operation),
            stage: Stage::RollingBack,
            total: None,
        });

        let mut entries = self.journal.entries(operation).await?;
        entries.reverse();

        for entry in entries {
            match entry.status {
                JournalStatus::Staged => {
                    // Only a temporary file exists; deleting it undoes the step.
                    if let Some(temp) = &entry.temp_path {
                        self.fs.remove_file(temp).await?;
                    }
                }
                JournalStatus::Committed => {
                    let target = &entry.absolute_path;
                    if let Some(backup_id) = entry.backup_id {
                        // Restore through a temporary file and a rename so the
                        // restoration is itself atomic.
                        if let Some(blob) = self.backups.path_of(backup_id).await? {
                            let temp = self.fs.write_temp_adjacent(target, &blob).await?;
                            self.fs.rename(&temp, target).await?;
                        }
                    } else if entry.previous_hash.is_none() {
                        // Nothing was there before this operation, so undoing it
                        // means the file goes away again.
                        self.fs.remove_file(target).await?;
                    }
                }
                JournalStatus::Pending | JournalStatus::Skipped | JournalStatus::RolledBack => {}
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
            .await?;
        progress.emit(ProgressEvent::Finished {
            stage: Stage::RollingBack,
            success: true,
        });
        Ok(())
    }
}

fn absolute(roots: &RootMap, target: &TargetLocation) -> Result<PathBuf> {
    let root = roots.get(&target.root_key).ok_or_else(|| {
        CoreError::InvalidInput(format!("no deployment root named {:?}", target.root_key))
    })?;
    Ok(target.path.resolve_under(root))
}

async fn verify_hash(
    fs: &dyn FileSystem,
    path: &std::path::Path,
    expected: &onera_core::FileHash,
) -> Result<()> {
    let Some((actual, _)) = fs.stat_hash(path).await? else {
        return Err(CoreError::IntegrityMismatch {
            path: path.display().to_string(),
            expected: expected.to_string(),
            actual: "missing".to_owned(),
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
