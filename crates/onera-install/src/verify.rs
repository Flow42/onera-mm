//! Verification and repair.
//!
//! Verification re-reads every file an installation claims and compares it with
//! the hash the provider stack recorded. It is deliberately a separate operation
//! from repair: seeing what is wrong and changing it are different decisions,
//! and a user who hand-edited a config file wants the first without the second.

use crate::planner::RootMap;
use onera_core::ids::{InstallationId, LocalGameId};
use onera_core::plan::TargetLocation;
use onera_core::ports::{DeploymentStore, FileSystem};
use onera_core::progress::{CancelToken, ProgressEvent, ProgressSink, Stage};
use onera_core::{CoreError, FileHash, Result};

/// The state of one verified file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyStatus {
    /// On disk and matching what Onera recorded.
    Ok,
    /// On disk but with different content.
    Modified,
    /// Recorded but no longer present.
    Missing,
    /// Present but not something Onera can hash (a link, a device node).
    Unreadable,
}

/// One file's verification result.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VerifiedFile {
    /// Where the file should be.
    pub target: TargetLocation,
    /// What was found.
    pub status: VerifyStatus,
    /// Hash Onera recorded.
    pub expected: FileHash,
    /// Hash found on disk, when one could be computed.
    pub actual: Option<FileHash>,
}

/// The outcome of verifying an installation.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VerifyReport {
    /// Every file that was checked.
    pub files: Vec<VerifiedFile>,
}

impl VerifyReport {
    /// Whether everything matched.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.files.iter().all(|f| f.status == VerifyStatus::Ok)
    }

    /// Files that did not match.
    pub fn problems(&self) -> impl Iterator<Item = &VerifiedFile> {
        self.files.iter().filter(|f| f.status != VerifyStatus::Ok)
    }

    /// Count of each status.
    #[must_use]
    pub fn counts(&self) -> std::collections::BTreeMap<String, usize> {
        let mut out = std::collections::BTreeMap::new();
        for file in &self.files {
            *out.entry(format!("{:?}", file.status)).or_insert(0) += 1;
        }
        out
    }
}

/// Verify every file an installation claims.
///
/// # Errors
/// Fails if a deployment root is unknown or a store call fails. A modified or
/// missing file is a *result*, not an error.
pub async fn verify_installation(
    game: LocalGameId,
    installation: InstallationId,
    roots: &RootMap,
    fs: &dyn FileSystem,
    deployments: &dyn DeploymentStore,
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
) -> Result<VerifyReport> {
    let targets = deployments.targets_of(installation).await?;
    progress.emit(ProgressEvent::Started {
        operation: None,
        stage: Stage::Verifying,
        total: Some(targets.len() as u64),
    });

    let mut report = VerifyReport::default();
    for (index, target) in targets.iter().enumerate() {
        cancel.check()?;
        let root = roots.get(&target.root_key).ok_or_else(|| {
            CoreError::InvalidInput(format!("no deployment root named {:?}", target.root_key))
        })?;
        let path = target.path.resolve_under(root);
        let stack = deployments.stack(game, target).await?;

        // Only the top of the stack is on disk; a buried provider is not
        // expected to match anything.
        let Some(top) = stack.top() else { continue };
        if top.provider.installation_id() != Some(installation) {
            continue;
        }

        let (status, actual) = match fs.stat_hash(&path).await {
            Ok(Some((hash, _))) if hash == top.hash => (VerifyStatus::Ok, Some(hash)),
            Ok(Some((hash, _))) => (VerifyStatus::Modified, Some(hash)),
            Ok(None) => (VerifyStatus::Missing, None),
            // A link or special file where a managed file belongs.
            Err(CoreError::Conflict(_)) => (VerifyStatus::Unreadable, None),
            Err(e) => return Err(e),
        };

        report.files.push(VerifiedFile {
            target: target.clone(),
            status,
            expected: top.hash.clone(),
            actual,
        });
        progress.emit(ProgressEvent::Advanced {
            stage: Stage::Verifying,
            completed: index as u64 + 1,
            total: Some(targets.len() as u64),
            detail: Some(target.to_string()),
        });
    }

    progress.emit(ProgressEvent::Finished {
        stage: Stage::Verifying,
        success: report.is_clean(),
    });
    Ok(report)
}
