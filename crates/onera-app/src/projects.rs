//! Project workspace lifecycle: initialization, initial state capture,
//! continuous change tracking, URI provenance, dependency resolution, and
//! snapshot rollback.

use crate::Onera;
use onera_core::domain::project::{DeploymentStrategy, Project, ProjectKind, ProvenanceUri};
use onera_core::ids::{ArtifactId, ProjectId};
use onera_core::paths::RelPath;
use onera_core::ports::FileSystem;
use onera_core::{CoreError, Result};
use onera_db::projects::{ArtifactDependencyKind, ArtifactDependencyRecord, FileChangeRecord};
use onera_install::RealFileSystem;
use onera_resolver::artifact_graph::{
    resolve_artifact_graph, ArtifactEdge, ArtifactEdgeKind, ArtifactGraphResolution,
};
use onera_watcher::{
    capture_folder_snapshot, diff_snapshots, ChangeKind, ChangeSource, DetectedChange,
    FolderSnapshot,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

impl Onera {
    /// Initialize any directory on disk as a managed [`Project`], auto-detecting
    /// the best matching profile plugin and capturing its initial baseline state.
    ///
    /// # Errors
    /// Fails if `root_path` is not an existing directory or database persistence fails.
    pub async fn init_project(
        &self,
        name: impl Into<String>,
        root_path: PathBuf,
        strategy: DeploymentStrategy,
    ) -> Result<(Project, FolderSnapshot)> {
        if !root_path.is_dir() {
            return Err(CoreError::InvalidInput(format!(
                "cannot initialize project: {} is not a directory",
                root_path.display()
            )));
        }

        let detected = self.plugins().detect_profile(&root_path);
        let kind = match detected {
            Some((profile, _)) if profile.metadata().id == "generic_folder" => ProjectKind::Folder,
            Some((profile, _)) => ProjectKind::Custom {
                profile_id: profile.metadata().id,
            },
            None => ProjectKind::Folder,
        };

        let mut project = Project::new_folder(name, root_path);
        project.kind = kind;
        project.deployment_strategy = strategy;

        self.database().upsert_project(&project).await?;
        let initial_snapshot = self.capture_project_snapshot(&project).await?;
        Ok((project, initial_snapshot))
    }

    /// Retrieve a [`Project`] by ID.
    pub async fn get_project(&self, id: ProjectId) -> Result<Option<Project>> {
        self.database().get_project(id).await
    }

    /// List all registered [`Project`]s.
    pub async fn list_projects(&self) -> Result<Vec<Project>> {
        self.database().list_projects().await
    }

    fn project_exclusions(
        &self,
        project: &Project,
    ) -> Vec<onera_core::domain::baseline::BaselineExclusion> {
        match &project.kind {
            ProjectKind::Custom { profile_id } => self
                .plugins()
                .profile(profile_id)
                .map(|p| p.baseline_exclusions())
                .unwrap_or_default(),
            ProjectKind::Game { adapter_id, .. } => self
                .plugins()
                .profile(adapter_id)
                .map(|p| p.baseline_exclusions())
                .unwrap_or_default(),
            ProjectKind::Folder => self
                .plugins()
                .profile(onera_plugin::GenericFolderProfile::ID)
                .map(|p| p.baseline_exclusions())
                .unwrap_or_default(),
        }
    }

    /// Capture a full snapshot of a project's directory state.
    pub async fn capture_project_snapshot(&self, project: &Project) -> Result<FolderSnapshot> {
        let exclusions = self.project_exclusions(project);
        capture_folder_snapshot(project.id, &project.root_path, &exclusions, "root").await
    }

    /// Scan `project.root_path` against `previous`, persist any detected changes
    /// into `file_change_events`, and return the new snapshot + change list.
    pub async fn scan_and_record_changes(
        &self,
        project: &Project,
        previous: &FolderSnapshot,
        managed_paths: &BTreeSet<RelPath>,
    ) -> Result<(FolderSnapshot, Vec<DetectedChange>)> {
        let current = self.capture_project_snapshot(project).await?;
        let changes = diff_snapshots(previous, &current, managed_paths);
        if !changes.is_empty() {
            let records = changes_to_records(&changes);
            self.database()
                .record_file_changes(project.id, &records)
                .await?;
        }
        Ok((current, changes))
    }

    /// Spawn a background watcher for `project` that continuously records
    /// detected filesystem changes into `file_change_events` until `cancel` is tripped.
    pub fn watch_project(
        &self,
        project: &Project,
        initial_snapshot: FolderSnapshot,
        managed_paths: BTreeSet<RelPath>,
        poll_interval: std::time::Duration,
        cancel: onera_core::progress::CancelToken,
    ) -> (
        tokio::sync::mpsc::Receiver<Vec<DetectedChange>>,
        tokio::task::JoinHandle<()>,
    ) {
        let exclusions = self.project_exclusions(project);
        let mut raw_rx = onera_watcher::spawn_project_watcher(
            project.id,
            project.root_path.clone(),
            exclusions,
            "root".to_owned(),
            initial_snapshot,
            managed_paths,
            poll_interval,
            cancel,
        );
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let db = self.database().clone();
        let project_id = project.id;
        let handle = tokio::spawn(async move {
            while let Some(changes) = raw_rx.recv().await {
                if !changes.is_empty() {
                    let records = changes_to_records(&changes);
                    let _ = db.record_file_changes(project_id, &records).await;
                    if tx.send(changes).await.is_err() {
                        break;
                    }
                }
            }
        });
        (rx, handle)
    }

    /// Fetch the chronological change ledger for a project.
    pub async fn project_change_history(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<FileChangeRecord>> {
        self.database().file_changes_for_project(project_id).await
    }

    /// Record the download URI provenance for an artifact.
    pub async fn record_artifact_provenance(
        &self,
        artifact_id: ArtifactId,
        provenance: &ProvenanceUri,
    ) -> Result<()> {
        self.database()
            .record_provenance_uri(artifact_id, provenance)
            .await
    }

    /// Look up the saved URI provenance for an artifact.
    pub async fn artifact_provenance(
        &self,
        artifact_id: ArtifactId,
    ) -> Result<Option<ProvenanceUri>> {
        self.database().provenance_for_artifact(artifact_id).await
    }

    /// Define a dependency or conflict edge between two downloaded/imported artifacts.
    pub async fn define_artifact_dependency(
        &self,
        dep: &ArtifactDependencyRecord,
    ) -> Result<()> {
        self.database().upsert_artifact_dependency(dep).await
    }

    /// Resolve dependencies and topological deployment order for a set of artifacts.
    pub async fn resolve_project_dependencies(
        &self,
        selected: &BTreeSet<ArtifactId>,
    ) -> Result<ArtifactGraphResolution> {
        let mut edges = Vec::new();
        for &artifact_id in selected {
            let deps = self
                .database()
                .dependencies_for_artifact(artifact_id)
                .await?;
            for d in deps {
                edges.push(ArtifactEdge {
                    from: d.artifact_id,
                    to: d.depends_on_artifact,
                    kind: match d.kind {
                        ArtifactDependencyKind::Required => ArtifactEdgeKind::Required,
                        ArtifactDependencyKind::Recommended => ArtifactEdgeKind::Recommended,
                        ArtifactDependencyKind::Conflicts => ArtifactEdgeKind::Conflicts,
                    },
                });
            }
        }
        Ok(resolve_artifact_graph(selected, &edges))
    }

    /// Roll back a project directory to a previous [`FolderSnapshot`].
    ///
    /// - Files added after `target_snapshot` are removed (unlinked/deleted).
    /// - Files present in `target_snapshot` whose links or contents changed are
    ///   restored either by re-linking to their recorded `link_target` or copying
    ///   from `restore_sources`.
    ///
    /// Returns the number of filesystem entries restored or removed.
    pub async fn rollback_to_snapshot(
        &self,
        project: &Project,
        target_snapshot: &FolderSnapshot,
        current_snapshot: &FolderSnapshot,
        restore_sources: &BTreeMap<RelPath, PathBuf>,
    ) -> Result<usize> {
        let operation_id = onera_core::ids::OperationId::new();
        for hook in self.plugins().hooks() {
            hook.on_pre_rollback(project, operation_id).await?;
        }

        let fs = RealFileSystem;
        let mut operations = 0;

        // 1. Remove files that did not exist in target_snapshot
        for path in current_snapshot.entries.keys() {
            if !target_snapshot.entries.contains_key(path) {
                let abs = path.resolve_under(&project.root_path);
                fs.remove_file(&abs).await?;
                operations += 1;
            }
        }

        // 2. Restore files that existed in target_snapshot
        for (path, target_entry) in &target_snapshot.entries {
            let needs_restore = match current_snapshot.entries.get(path) {
                None => true,
                Some(curr) => {
                    curr.hash != target_entry.hash
                        || curr.is_symlink != target_entry.is_symlink
                        || curr.link_target != target_entry.link_target
                }
            };
            if !needs_restore {
                continue;
            }

            let abs = path.resolve_under(&project.root_path);
            if target_entry.is_symlink {
                if let Some(link_target) = &target_entry.link_target {
                    let temp = fs.link_temp_adjacent(&abs, link_target).await?;
                    fs.rename(&temp, &abs).await?;
                    operations += 1;
                    continue;
                }
            }

            if let Some(source_blob) = restore_sources.get(path) {
                let temp = fs.write_temp_adjacent(&abs, source_blob).await?;
                fs.rename(&temp, &abs).await?;
                operations += 1;
            } else {
                return Err(CoreError::NotFound {
                    kind: "snapshot restore source",
                    id: path.to_string(),
                });
            }
        }

        for hook in self.plugins().hooks() {
            hook.on_post_rollback(project, operation_id).await?;
        }
        Ok(operations)
    }
}

fn changes_to_records(changes: &[DetectedChange]) -> Vec<FileChangeRecord> {
    changes
        .iter()
        .map(|c| FileChangeRecord {
            path: c.path.as_str().to_owned(),
            event_type: match c.kind {
                ChangeKind::Created => "created".to_owned(),
                ChangeKind::Modified => "modified".to_owned(),
                ChangeKind::Deleted => "deleted".to_owned(),
            },
            source: match c.source {
                ChangeSource::Managed => "managed".to_owned(),
                ChangeSource::ExternalDrift => "external_drift".to_owned(),
            },
            previous_hash: c.previous_hash.clone(),
            new_hash: c.new_hash.clone(),
            observed_at: c.observed_at,
        })
        .collect()
}
