//! Removal and restoration.
//!
//! Removing a mod is not "delete the files it installed". Every tracked path is
//! inspected and one of five things happens:
//!
//! | situation                                    | action                          |
//! |----------------------------------------------|---------------------------------|
//! | only this mod provides it, unchanged          | delete it                       |
//! | another mod provides identical bytes          | keep the file, drop the claim   |
//! | another mod is underneath                     | restore that mod's file         |
//! | an unmanaged original is underneath           | restore it from its backup      |
//! | the file changed since Onera deployed it      | ask, never touch it silently    |
//!
//! A file that is already missing is not an error: users delete things, and a
//! removal that refuses to proceed because of that would be useless.

use crate::planner::RootMap;
use onera_core::domain::provider_stack::RestoreAction;
use onera_core::ids::{InstallationId, LocalGameId};
use onera_core::plan::TargetLocation;
use onera_core::ports::{BackupStore, DeploymentStore, FileSystem};
use onera_core::progress::{CancelToken, ProgressEvent, ProgressSink, Stage};
use onera_core::{CoreError, Result};
use std::path::PathBuf;
use std::sync::Arc;

/// What a removal did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemovalReport {
    /// Files deleted because nothing else provided them.
    pub deleted: Vec<TargetLocation>,
    /// Files whose content was restored from a provider underneath.
    pub restored: Vec<TargetLocation>,
    /// Files left in place because another provider supplies identical bytes.
    pub kept_shared: Vec<TargetLocation>,
    /// Files that were already gone.
    pub already_missing: Vec<TargetLocation>,
    /// Files that changed since deployment and were left untouched.
    pub externally_modified: Vec<TargetLocation>,
    /// Directories Onera created and then emptied.
    pub directories_removed: Vec<PathBuf>,
}

impl RemovalReport {
    /// Whether anything needs the user's attention.
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        !self.externally_modified.is_empty()
    }
}

/// How to treat files that changed behind Onera's back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModifiedFilePolicy {
    /// Stop and report them; nothing is touched. The default.
    Ask,
    /// Leave them on disk but drop Onera's claim on them.
    Keep,
    /// Remove them anyway. Only ever set from an explicit user confirmation.
    Force,
}

/// Removes installations and restores what they covered.
pub struct Remover {
    fs: Arc<dyn FileSystem>,
    deployments: Arc<dyn DeploymentStore>,
    backups: Arc<dyn BackupStore>,
}

impl Remover {
    /// Assemble a remover from its ports.
    #[must_use]
    pub fn new(
        fs: Arc<dyn FileSystem>,
        deployments: Arc<dyn DeploymentStore>,
        backups: Arc<dyn BackupStore>,
    ) -> Self {
        Self {
            fs,
            deployments,
            backups,
        }
    }

    /// Plan a removal without changing anything.
    ///
    /// # Errors
    /// Propagates store and filesystem errors.
    pub async fn preview(
        &self,
        game: LocalGameId,
        installation: InstallationId,
        roots: &RootMap,
    ) -> Result<RemovalReport> {
        self.run(
            game,
            installation,
            roots,
            ModifiedFilePolicy::Ask,
            true,
            &onera_core::progress::NullProgress,
            &CancelToken::new(),
        )
        .await
    }

    /// Remove an installation.
    ///
    /// # Errors
    /// Returns [`CoreError::DecisionRequired`] when files changed since
    /// deployment and the policy is [`ModifiedFilePolicy::Ask`].
    pub async fn remove(
        &self,
        game: LocalGameId,
        installation: InstallationId,
        roots: &RootMap,
        policy: ModifiedFilePolicy,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<RemovalReport> {
        let report = self
            .run(game, installation, roots, policy, false, progress, cancel)
            .await?;
        if policy == ModifiedFilePolicy::Ask && report.needs_attention() {
            return Err(CoreError::DecisionRequired(format!(
                "{} file(s) changed since they were installed",
                report.externally_modified.len()
            )));
        }
        // A user-facing removal is a deactivation: the artifact archive and
        // its stable mappings remain available for a later desired-state
        // activation. Permanent deletion is a separate, explicit purge
        // operation so disabling never turns into data loss.
        self.deployments
            .deactivate_installation(installation)
            .await?;
        Ok(report)
    }

    #[allow(clippy::too_many_arguments)]
    async fn run(
        &self,
        game: LocalGameId,
        installation: InstallationId,
        roots: &RootMap,
        policy: ModifiedFilePolicy,
        dry_run: bool,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<RemovalReport> {
        let targets = self.deployments.targets_of(installation).await?;
        progress.emit(ProgressEvent::Started {
            operation: None,
            stage: Stage::Removing,
            total: Some(targets.len() as u64),
        });

        let mut report = RemovalReport::default();

        for (index, target) in targets.iter().enumerate() {
            cancel.check()?;
            let Some(root) = roots.get(&target.root_key) else {
                return Err(CoreError::InvalidInput(format!(
                    "no deployment root named {:?}",
                    target.root_key
                )));
            };
            let path = target.path.resolve_under(root);
            let mut stack = self.deployments.stack(game, target).await?;
            let expected = stack.top().map(|e| e.hash.clone());
            let on_disk = self.fs.stat_hash(&path).await?;

            // A file that changed since Onera wrote it is never removed on the
            // strength of a stale record.
            let modified = match (&expected, &on_disk) {
                (Some(recorded), Some((actual, _))) => recorded != actual,
                _ => false,
            };
            if modified && policy == ModifiedFilePolicy::Ask {
                report.externally_modified.push(target.clone());
                continue;
            }
            if modified && policy == ModifiedFilePolicy::Keep {
                report.externally_modified.push(target.clone());
                if !dry_run {
                    stack.remove_installation(installation);
                    self.deployments.put_stack(game, target, &stack).await?;
                }
                continue;
            }

            let action = stack.remove_installation(installation);
            match action {
                RestoreAction::Nothing => {
                    if on_disk.is_none() {
                        report.already_missing.push(target.clone());
                    } else if stack.is_empty() {
                        // Nothing left claims it, but the stack said otherwise;
                        // this only happens for a buried removal.
                        report.kept_shared.push(target.clone());
                    } else {
                        report.kept_shared.push(target.clone());
                    }
                }
                RestoreAction::Delete => {
                    if on_disk.is_none() {
                        report.already_missing.push(target.clone());
                    } else {
                        if !dry_run {
                            self.fs.remove_file(&path).await?;
                        }
                        report.deleted.push(target.clone());
                    }
                }
                RestoreAction::Restore(entry) => {
                    if !dry_run {
                        let source = match entry.provider {
                            onera_core::domain::provider_stack::FileProvider::UnmanagedBackup {
                                backup_id,
                            } => self.backups.path_of(backup_id).await?,
                            // Restoring another mod's file needs that mod's
                            // bytes; they are recoverable from the backup taken
                            // when this mod covered the file.
                            onera_core::domain::provider_stack::FileProvider::Installation {
                                ..
                            } => self.backup_for_hash(game, target, &entry.hash).await?,
                        };
                        let Some(source) = source else {
                            return Err(CoreError::Conflict(format!(
                                "cannot restore {target}: the previous provider's bytes are not in backup storage"
                            )));
                        };
                        let temp = self.fs.write_temp_adjacent(&path, &source).await?;
                        self.fs.rename(&temp, &path).await?;
                    }
                    report.restored.push(target.clone());
                }
            }

            if !dry_run {
                self.deployments.put_stack(game, target, &stack).await?;
            }
            progress.emit(ProgressEvent::Advanced {
                stage: Stage::Removing,
                completed: index as u64 + 1,
                total: Some(targets.len() as u64),
                detail: Some(target.to_string()),
            });
        }

        // Only directories this installation created are candidates, and only
        // if they are now empty. A directory that shipped with the game, or that
        // still holds the user's own files, is left alone.
        if !dry_run {
            let created = self.deployments.created_dirs(installation).await?;
            let mut candidates: Vec<PathBuf> = created
                .iter()
                .filter_map(|dir| {
                    roots
                        .get(&dir.root_key)
                        .map(|root| dir.path.resolve_under(root))
                })
                .collect();
            // Deepest first, so a parent becomes empty before it is considered.
            candidates.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
            for dir in candidates {
                if self.fs.remove_dir_if_empty(&dir).await? {
                    report.directories_removed.push(dir);
                }
            }
        }

        progress.emit(ProgressEvent::Finished {
            stage: Stage::Removing,
            success: true,
        });
        Ok(report)
    }

    /// Find backed-up bytes matching a hash for this path.
    async fn backup_for_hash(
        &self,
        _game: LocalGameId,
        _target: &TargetLocation,
        hash: &onera_core::FileHash,
    ) -> Result<Option<PathBuf>> {
        // Backups are content-addressed, so a hash is enough to find the bytes.
        // The lookup goes through the store rather than reconstructing a path
        // here, so the storage layout stays private to the adapter.
        self.backups.path_of_hash(hash).await
    }
}
