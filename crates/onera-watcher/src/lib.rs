//! Folder initial-state capture, continuous file change tracking, and drift detection.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use chrono::{DateTime, Utc};
use onera_core::domain::baseline::{excluded_by, BaselineExclusion};
use onera_core::hash::{hash_file_blake3, FileHash};
use onera_core::ids::ProjectId;
use onera_core::paths::RelPath;
use onera_core::progress::CancelToken;
use onera_core::{CoreError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};
use tokio::sync::mpsc;
use walkdir::WalkDir;

/// One file or symlink recorded in a [`FolderSnapshot`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSnapshotEntry {
    /// Relative path inside the project root.
    pub path: RelPath,
    /// BLAKE3 content hash (for symlinks, the hash of the target content).
    pub hash: FileHash,
    /// Size in bytes.
    pub size: u64,
    /// Whether the entry on disk is a symbolic link.
    pub is_symlink: bool,
    /// Symlink target path, if `is_symlink` is true.
    pub link_target: Option<PathBuf>,
    /// Last modification timestamp in seconds since Unix epoch.
    pub mtime_secs: i64,
}

/// Complete state of a managed folder at a point in time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderSnapshot {
    /// Project this snapshot belongs to.
    pub project_id: ProjectId,
    /// When the snapshot was taken.
    pub captured_at: DateTime<Utc>,
    /// All tracked files keyed by normalized relative path.
    pub entries: BTreeMap<RelPath, FileSnapshotEntry>,
}

/// Classification of a detected filesystem change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// A new file or symlink appeared.
    Created,
    /// An existing file's content or link target changed.
    Modified,
    /// A previously tracked file was removed.
    Deleted,
}

/// Origin of a detected filesystem change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeSource {
    /// Matches a target path actively managed by Onera.
    Managed,
    /// External modification outside Onera's deployment stack.
    ExternalDrift,
}

/// A single recorded change between two folder states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedChange {
    /// Relative path that changed.
    pub path: RelPath,
    /// Kind of change.
    pub kind: ChangeKind,
    /// Whether this path is Onera-managed or external drift.
    pub source: ChangeSource,
    /// Hash before the change, if the file existed.
    pub previous_hash: Option<FileHash>,
    /// Hash after the change, if the file still exists.
    pub new_hash: Option<FileHash>,
    /// When the change was observed.
    pub observed_at: DateTime<Utc>,
}

/// Capture the full initial (or current) state of a project folder.
///
/// Follows symlinks to hash their target content while recording `is_symlink`
/// and `link_target` so Onera-deployed links are tracked accurately.
///
/// # Errors
/// Fails if `root_path` does not exist or cannot be traversed.
pub async fn capture_folder_snapshot(
    project_id: ProjectId,
    root_path: &Path,
    exclusions: &[BaselineExclusion],
    root_key: &str,
) -> Result<FolderSnapshot> {
    if !root_path.is_dir() {
        return Err(CoreError::InvalidInput(format!(
            "project root {} is not a directory",
            root_path.display()
        )));
    }

    let mut entries = BTreeMap::new();
    for entry in WalkDir::new(root_path).follow_links(false) {
        let entry = entry.map_err(|e| CoreError::fs(root_path, std::io::Error::other(e)))?;
        let abs_path = entry.path();
        if abs_path == root_path {
            continue;
        }
        let Ok(rel_raw) = abs_path.strip_prefix(root_path) else {
            continue;
        };
        let rel_str = rel_raw.to_string_lossy().replace('\\', "/");
        let Ok(rel_path) = RelPath::normalize(&rel_str) else {
            continue;
        };

        if excluded_by(exclusions, root_key, &rel_path).is_some() {
            continue;
        }

        let is_symlink = entry.file_type().is_symlink();
        if entry.file_type().is_dir() && !is_symlink {
            continue;
        }

        let (link_target, meta) = if is_symlink {
            let target = tokio::fs::read_link(abs_path).await.ok();
            // Follow symlink to inspect target file; skip broken links gracefully.
            let Ok(m) = tokio::fs::metadata(abs_path).await else {
                continue;
            };
            if !m.is_file() {
                continue;
            }
            (target, m)
        } else if entry.file_type().is_file() {
            let m = tokio::fs::metadata(abs_path)
                .await
                .map_err(|e| CoreError::fs(abs_path, e))?;
            (None, m)
        } else {
            continue;
        };

        let hash = hash_file_blake3(abs_path)
            .await
            .map_err(|e| CoreError::fs(abs_path, e))?;
        let mtime_secs = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));

        entries.insert(
            rel_path.clone(),
            FileSnapshotEntry {
                path: rel_path,
                hash,
                size: meta.len(),
                is_symlink,
                link_target,
                mtime_secs,
            },
        );
    }

    Ok(FolderSnapshot {
        project_id,
        captured_at: Utc::now(),
        entries,
    })
}

/// Compare two [`FolderSnapshot`]s and return all detected changes.
#[must_use]
pub fn diff_snapshots(
    previous: &FolderSnapshot,
    current: &FolderSnapshot,
    managed_paths: &BTreeSet<RelPath>,
) -> Vec<DetectedChange> {
    let mut changes = Vec::new();
    let observed_at = current.captured_at;

    for (path, prev_entry) in &previous.entries {
        let source = if managed_paths.contains(path) {
            ChangeSource::Managed
        } else {
            ChangeSource::ExternalDrift
        };
        match current.entries.get(path) {
            None => {
                changes.push(DetectedChange {
                    path: path.clone(),
                    kind: ChangeKind::Deleted,
                    source,
                    previous_hash: Some(prev_entry.hash.clone()),
                    new_hash: None,
                    observed_at,
                });
            }
            Some(curr_entry) => {
                if prev_entry.hash != curr_entry.hash
                    || prev_entry.is_symlink != curr_entry.is_symlink
                    || prev_entry.link_target != curr_entry.link_target
                {
                    changes.push(DetectedChange {
                        path: path.clone(),
                        kind: ChangeKind::Modified,
                        source,
                        previous_hash: Some(prev_entry.hash.clone()),
                        new_hash: Some(curr_entry.hash.clone()),
                        observed_at,
                    });
                }
            }
        }
    }

    for (path, curr_entry) in &current.entries {
        if !previous.entries.contains_key(path) {
            let source = if managed_paths.contains(path) {
                ChangeSource::Managed
            } else {
                ChangeSource::ExternalDrift
            };
            changes.push(DetectedChange {
                path: path.clone(),
                kind: ChangeKind::Created,
                source,
                previous_hash: None,
                new_hash: Some(curr_entry.hash.clone()),
                observed_at,
            });
        }
    }

    changes
}

/// Spawn a background watcher task that periodically scans `root_path` and emits
/// [`DetectedChange`] batches whenever files are created, modified, or deleted.
pub fn spawn_project_watcher(
    project_id: ProjectId,
    root_path: PathBuf,
    exclusions: Vec<BaselineExclusion>,
    root_key: String,
    initial_snapshot: FolderSnapshot,
    managed_paths: BTreeSet<RelPath>,
    poll_interval: Duration,
    cancel: CancelToken,
) -> mpsc::Receiver<Vec<DetectedChange>> {
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(async move {
        let mut last = initial_snapshot;
        let mut ticker = tokio::time::interval(poll_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if cancel.is_cancelled() {
                break;
            }
            let Ok(current) =
                capture_folder_snapshot(project_id, &root_path, &exclusions, &root_key).await
            else {
                continue;
            };
            let changes = diff_snapshots(&last, &current, &managed_paths);
            if !changes.is_empty() && tx.send(changes).await.is_err() {
                break;
            }
            last = current;
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn captures_initial_state_and_detects_create_modify_delete() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("init.txt"), b"initial")
            .await
            .unwrap();

        let project_id = ProjectId::new();
        let initial = capture_folder_snapshot(project_id, dir.path(), &[], "root")
            .await
            .unwrap();
        assert_eq!(initial.entries.len(), 1);
        let init_rel = RelPath::normalize("init.txt").unwrap();
        assert_eq!(initial.entries[&init_rel].hash, FileHash::blake3_of(b"initial"));

        // Mutate directory: modify init.txt, add new.txt
        tokio::fs::write(dir.path().join("init.txt"), b"modified")
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("new.txt"), b"added")
            .await
            .unwrap();

        let second = capture_folder_snapshot(project_id, dir.path(), &[], "root")
            .await
            .unwrap();
        let mut managed = BTreeSet::new();
        managed.insert(RelPath::normalize("new.txt").unwrap());

        let changes = diff_snapshots(&initial, &second, &managed);
        assert_eq!(changes.len(), 2);

        let mod_change = changes.iter().find(|c| c.path == init_rel).unwrap();
        assert_eq!(mod_change.kind, ChangeKind::Modified);
        assert_eq!(mod_change.source, ChangeSource::ExternalDrift);
        assert_eq!(mod_change.previous_hash, Some(FileHash::blake3_of(b"initial")));
        assert_eq!(mod_change.new_hash, Some(FileHash::blake3_of(b"modified")));

        let new_rel = RelPath::normalize("new.txt").unwrap();
        let add_change = changes.iter().find(|c| c.path == new_rel).unwrap();
        assert_eq!(add_change.kind, ChangeKind::Created);
        assert_eq!(add_change.source, ChangeSource::Managed);

        // Delete init.txt
        tokio::fs::remove_file(dir.path().join("init.txt"))
            .await
            .unwrap();
        let third = capture_folder_snapshot(project_id, dir.path(), &[], "root")
            .await
            .unwrap();
        let del_changes = diff_snapshots(&second, &third, &managed);
        assert_eq!(del_changes.len(), 1);
        assert_eq!(del_changes[0].kind, ChangeKind::Deleted);
        assert_eq!(del_changes[0].path, init_rel);
    }

    #[tokio::test]
    async fn watcher_emits_changes_on_file_creation() {
        let dir = tempfile::tempdir().unwrap();
        let project_id = ProjectId::new();
        let initial = capture_folder_snapshot(project_id, dir.path(), &[], "root")
            .await
            .unwrap();

        let cancel = CancelToken::new();
        let mut rx = spawn_project_watcher(
            project_id,
            dir.path().to_path_buf(),
            Vec::new(),
            "root".to_owned(),
            initial,
            BTreeSet::new(),
            Duration::from_millis(25),
            cancel.clone(),
        );

        tokio::fs::write(dir.path().join("watched.txt"), b"hello")
            .await
            .unwrap();

        let batch = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].path.as_str(), "watched.txt");
        assert_eq!(batch[0].kind, ChangeKind::Created);
        cancel.cancel();
    }
}
