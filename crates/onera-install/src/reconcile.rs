//! Journaled desired-state reconciliation.

use crate::planner::RootMap;
use onera_core::domain::operation::OperationState;
use onera_core::domain::reconcile::{InstallationMapping, MutationPlan, MutationStep};
use onera_core::ids::InstallationId;
use onera_core::ports::{
    BackupStore, DeploymentStore, FileSystem, JournalEntry, JournalStatus, OperationJournal,
};
use onera_core::progress::{CancelToken, ProgressSink};
use onera_core::{CoreError, FileHash, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Applies one complete desired-state plan as a single journaled operation.
pub struct ReconciliationEngine {
    fs: Arc<dyn FileSystem>,
    journal: Arc<dyn OperationJournal>,
    deployments: Arc<dyn DeploymentStore>,
    backups: Arc<dyn BackupStore>,
}

impl ReconciliationEngine {
    /// Assemble the engine from the same ports as the single-install engine.
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

    /// Apply a ready plan. `extracted` maps each retained artifact to a fresh,
    /// validated extraction directory containing its recorded source paths.
    pub async fn apply(
        &self,
        plan: &MutationPlan,
        mappings: &[InstallationMapping],
        extracted: &BTreeMap<InstallationId, PathBuf>,
        roots: &RootMap,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<()> {
        if !plan.is_ready() {
            return Err(CoreError::DecisionRequired(
                "reconciliation has unresolved cross-mod conflicts".into(),
            ));
        }
        let operation = self.journal.begin_reconciliation(plan).await?;
        let mut entries = Vec::new();
        for (seq, step) in plan.steps.iter().enumerate() {
            cancel.check()?;
            let target = match step {
                MutationStep::Write { target, .. } | MutationStep::Delete { target } => target,
            };
            let root = roots.get(&target.root_key).ok_or_else(|| {
                CoreError::InvalidInput(format!("no deployment root named {:?}", target.root_key))
            })?;
            let absolute_path = target.path.resolve_under(root);
            let prior = self.fs.stat_hash(&absolute_path).await?;
            let mut entry = JournalEntry {
                seq: seq as u32,
                target: target.clone(),
                absolute_path: absolute_path.clone(),
                source_hash: prior
                    .as_ref()
                    .map(|(h, _)| h.clone())
                    .unwrap_or_else(|| FileHash::blake3_of(b"")),
                previous_hash: prior.as_ref().map(|(h, _)| h.clone()),
                backup_id: None,
                temp_path: None,
                status: JournalStatus::Pending,
            };
            if let Some((hash, size)) = prior {
                entry.backup_id = Some(
                    self.backups
                        .create(
                            plan.desired.local_game_id,
                            target,
                            &absolute_path,
                            &hash,
                            size,
                        )
                        .await?,
                );
            }
            if let MutationStep::Write { provider, .. } = step {
                let installation = provider
                    .provider
                    .installation_id()
                    .expect("write providers are installations");
                let mapping = mappings
                    .iter()
                    .find(|m| {
                        m.installation_id == installation
                            && m.target == *target
                            && m.source_hash == provider.hash
                    })
                    .ok_or_else(|| {
                        CoreError::Conflict(format!("no recorded mapping for {target}"))
                    })?;
                let dir = extracted
                    .get(&installation)
                    .ok_or_else(|| CoreError::NotFound {
                        kind: "extracted artifact",
                        id: installation.to_string(),
                    })?;
                let source = mapping.source.resolve_under(dir);
                verify(&*self.fs, &source, &provider.hash).await?;
                let temp = self.fs.write_temp_adjacent(&absolute_path, &source).await?;
                verify(&*self.fs, &temp, &provider.hash).await?;
                entry.source_hash = provider.hash.clone();
                entry.temp_path = Some(temp);
                entry.status = JournalStatus::Staged;
            } else {
                entry.status = JournalStatus::Staged;
            }
            self.journal.put_entry(operation.id, &entry).await?;
            entries.push((step, entry));
        }
        self.journal
            .set_state(operation.id, OperationState::Prepared, None)
            .await?;
        self.journal
            .set_state(operation.id, OperationState::Committing, None)
            .await?;
        for (step, entry) in &entries {
            match step {
                MutationStep::Write { provider, .. } => {
                    self.fs
                        .rename(
                            entry.temp_path.as_ref().expect("staged temp"),
                            &entry.absolute_path,
                        )
                        .await?;
                    verify(&*self.fs, &entry.absolute_path, &provider.hash).await?;
                }
                MutationStep::Delete { .. } => self.fs.remove_file(&entry.absolute_path).await?,
            }
            self.journal
                .put_entry(
                    operation.id,
                    &JournalEntry {
                        temp_path: None,
                        status: JournalStatus::Committed,
                        ..entry.clone()
                    },
                )
                .await?;
        }
        for (target, stack) in &plan.final_stacks {
            self.deployments
                .put_stack(plan.desired.local_game_id, target, stack)
                .await?;
        }
        let wanted: BTreeSet<_> = plan.desired.installations.iter().copied().collect();
        for installation in wanted {
            self.deployments.activate_installation(installation).await?;
        }
        self.journal
            .set_state(operation.id, OperationState::Complete, None)
            .await?;
        let _ = progress;
        Ok(())
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
