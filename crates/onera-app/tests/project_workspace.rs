//! End-to-end integration test for the generalized folder project workflow:
//! initialization, initial snapshot, URI provenance, dependency graph,
//! symlink staging deployment, change tracking, and snapshot rollback.

use chrono::Utc;
use onera_app::flow::Onera;
use onera_app::Paths;
use onera_core::domain::project::{DeploymentStrategy, ProjectKind, ProvenanceUri};
use onera_core::hash::FileHash;
use onera_core::ids::{ArtifactId, ProviderId};
use onera_core::paths::RelPath;
use onera_core::ports::{FileSystem, ModProvider};
use onera_db::projects::{ArtifactDependencyKind, ArtifactDependencyRecord};
use onera_install::RealFileSystem;
use onera_nexus::{ApiKeyAuth, NexusClient, NexusConfig};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use url::Url;

async fn test_onera(dir: &std::path::Path) -> Onera {
    let paths = Paths::rooted_at(dir.to_path_buf());
    let secrets = Arc::new(onera_app::InMemorySecretStore::new());
    let config = NexusConfig {
        v1_base: "http://127.0.0.1:9".to_owned(),
        v3_base: "http://127.0.0.1:9".to_owned(),
        ..NexusConfig::default()
    };
    let auth = Arc::new(
        ApiKeyAuth::new_for_tests(secrets, config.v1_base.clone(), &config.user_agent).unwrap(),
    );
    let provider: Arc<dyn ModProvider> =
        Arc::new(NexusClient::new_for_tests(config, auth.clone()).unwrap());
    Onera::assemble_with(paths, auth, provider, true)
        .await
        .unwrap()
}

#[tokio::test]
async fn full_project_lifecycle_provenance_deps_symlinks_tracking_and_rollback() {
    let tmp = tempfile::tempdir().unwrap();
    let app_dir = tmp.path().join("app");
    let project_dir = tmp.path().join("my_project");
    let staging_dir = tmp.path().join("staging");
    let backup_dir = tmp.path().join("backups");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();
    tokio::fs::create_dir_all(&staging_dir).await.unwrap();
    tokio::fs::create_dir_all(&backup_dir).await.unwrap();

    // Seed initial state in project_dir
    let init_file = project_dir.join("readme.txt");
    tokio::fs::write(&init_file, b"original readme").await.unwrap();
    // Keep a backup blob for restoring original readme if modified during test
    let init_backup_blob = backup_dir.join("readme.txt");
    tokio::fs::write(&init_backup_blob, b"original readme").await.unwrap();

    let onera = test_onera(&app_dir).await;

    // 1. Initialize arbitrary folder as a Project
    let (project, v0_snapshot) = onera
        .init_project("My Workspace", project_dir.clone(), DeploymentStrategy::Symlink)
        .await
        .unwrap();
    assert_eq!(project.kind, ProjectKind::Folder);
    assert_eq!(v0_snapshot.entries.len(), 1);
    assert!(onera.get_project(project.id).await.unwrap().is_some());

    // 2. Record download URI provenance for two artifacts
    let base_artifact = ArtifactId::new();
    let app_artifact = ArtifactId::new();

    let base_uri = Url::parse("https://example.com/packages/core-lib-1.0.zip").unwrap();
    onera
        .record_artifact_provenance(
            base_artifact,
            &ProvenanceUri {
                uri: base_uri.clone(),
                provider: Some(ProviderId::new("direct_uri")),
                referrer_url: Some("https://example.com/packages/".to_owned()),
                etag: Some("\"etag-core-1\"".to_owned()),
                content_hash: FileHash::blake3_of(b"core lib bytes"),
                downloaded_at: Utc::now(),
            },
        )
        .await
        .unwrap();

    let loaded_prov = onera
        .artifact_provenance(base_artifact)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded_prov.uri, base_uri);
    assert_eq!(loaded_prov.etag.as_deref(), Some("\"etag-core-1\""));

    // 3. Define dependency: app_artifact requires base_artifact
    onera
        .define_artifact_dependency(&ArtifactDependencyRecord {
            artifact_id: app_artifact,
            depends_on_artifact: base_artifact,
            version_constraint: Some(">=1.0.0".to_owned()),
            kind: ArtifactDependencyKind::Required,
        })
        .await
        .unwrap();

    let selected: BTreeSet<ArtifactId> = [app_artifact, base_artifact].into_iter().collect();
    let resolution = onera
        .resolve_project_dependencies(&selected)
        .await
        .unwrap();
    assert!(resolution.is_valid());
    assert_eq!(resolution.ordered_install, vec![base_artifact, app_artifact]);

    // 4. Deploy staged files via symlinks
    let staged_plugin = staging_dir.join("plugin.so");
    tokio::fs::write(&staged_plugin, b"compiled plugin").await.unwrap();
    let deployed_link = project_dir.join("plugins/plugin.so");
    RealFileSystem
        .create_symlink(&staged_plugin, &deployed_link)
        .await
        .unwrap();

    // Simulate external drift on readme.txt
    tokio::fs::write(&init_file, b"externally edited readme")
        .await
        .unwrap();

    // 5. Scan and record changes into the project ledger
    let mut managed = BTreeSet::new();
    managed.insert(RelPath::normalize("plugins/plugin.so").unwrap());

    let (v1_snapshot, changes) = onera
        .scan_and_record_changes(&project, &v0_snapshot, &managed)
        .await
        .unwrap();
    assert_eq!(changes.len(), 2);

    let history = onera.project_change_history(project.id).await.unwrap();
    assert_eq!(history.len(), 2);
    assert!(history
        .iter()
        .any(|h| h.path == "plugins/plugin.so" && h.event_type == "created" && h.source == "managed"));
    assert!(history.iter().any(|h| h.path == "readme.txt"
        && h.event_type == "modified"
        && h.source == "external_drift"));

    // 6. Rollback to v0_snapshot
    let mut restore_sources = BTreeMap::new();
    restore_sources.insert(RelPath::normalize("readme.txt").unwrap(), init_backup_blob);

    let restored_count = onera
        .rollback_to_snapshot(&project, &v0_snapshot, &v1_snapshot, &restore_sources)
        .await
        .unwrap();
    assert_eq!(restored_count, 2);

    // Verify deployed symlink was removed (without deleting staged_plugin) and readme.txt is restored
    assert!(!deployed_link.exists());
    assert!(staged_plugin.exists());
    assert_eq!(
        tokio::fs::read(&init_file).await.unwrap(),
        b"original readme"
    );
}

#[tokio::test]
async fn watch_project_continuously_persists_external_drift() {
    let tmp = tempfile::tempdir().unwrap();
    let app_dir = tmp.path().join("app");
    let project_dir = tmp.path().join("watched_project");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();
    tokio::fs::write(project_dir.join("base.txt"), b"v0")
        .await
        .unwrap();

    let onera = test_onera(&app_dir).await;
    let (project, initial_snapshot) = onera
        .init_project("Watched", project_dir.clone(), DeploymentStrategy::Symlink)
        .await
        .unwrap();

    let cancel = onera_core::progress::CancelToken::new();
    let (mut rx, handle) = onera.watch_project(
        &project,
        initial_snapshot,
        BTreeSet::new(),
        std::time::Duration::from_millis(50),
        cancel.clone(),
    );

    // Mutate folder after watcher starts
    tokio::fs::write(project_dir.join("drift.txt"), b"external")
        .await
        .unwrap();

    let batch = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .expect("watcher timed out")
        .expect("watcher channel closed");
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].path.as_str(), "drift.txt");

    // Verify change was persisted in SQLite change ledger
    let history = onera.project_change_history(project.id).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].path, "drift.txt");
    assert_eq!(history[0].event_type, "created");
    assert_eq!(history[0].source, "external_drift");

    cancel.cancel();
    let _ = handle.await;
}

struct RecordingHook {
    events: Arc<tokio::sync::Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl onera_plugin::LifecycleHookPlugin for RecordingHook {
    fn metadata(&self) -> onera_plugin::PluginMetadata {
        onera_plugin::PluginMetadata {
            id: "recording_hook".into(),
            name: "Recording Hook".into(),
            version: "0.1.0".into(),
            description: "Records lifecycle events".into(),
        }
    }

    async fn on_pre_deploy(
        &self,
        project: &onera_core::domain::project::Project,
        _plan: &onera_core::plan::InstallPlan,
    ) -> onera_core::Result<()> {
        self.events
            .lock()
            .await
            .push(format!("pre_deploy:{}", project.name));
        Ok(())
    }

    async fn on_post_deploy(
        &self,
        project: &onera_core::domain::project::Project,
        _operation_id: onera_core::ids::OperationId,
    ) -> onera_core::Result<()> {
        self.events
            .lock()
            .await
            .push(format!("post_deploy:{}", project.name));
        Ok(())
    }

    async fn on_pre_rollback(
        &self,
        project: &onera_core::domain::project::Project,
        _operation_id: onera_core::ids::OperationId,
    ) -> onera_core::Result<()> {
        self.events
            .lock()
            .await
            .push(format!("pre_rollback:{}", project.name));
        Ok(())
    }

    async fn on_post_rollback(
        &self,
        project: &onera_core::domain::project::Project,
        _operation_id: onera_core::ids::OperationId,
    ) -> onera_core::Result<()> {
        self.events
            .lock()
            .await
            .push(format!("post_rollback:{}", project.name));
        Ok(())
    }
}

#[tokio::test]
async fn lifecycle_hooks_fire_on_rollback() {
    let tmp = tempfile::tempdir().unwrap();
    let app_dir = tmp.path().join("app");
    let project_dir = tmp.path().join("hooked_project");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();
    tokio::fs::write(project_dir.join("keep.txt"), b"v0")
        .await
        .unwrap();

    let mut onera = test_onera(&app_dir).await;
    let events = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    onera.plugins_mut().register_hook(Arc::new(RecordingHook {
        events: events.clone(),
    }));

    let (project, v0) = onera
        .init_project("Hooked", project_dir.clone(), DeploymentStrategy::Symlink)
        .await
        .unwrap();

    tokio::fs::write(project_dir.join("extra.txt"), b"v1")
        .await
        .unwrap();
    let (v1, _) = onera
        .scan_and_record_changes(&project, &v0, &BTreeSet::new())
        .await
        .unwrap();

    let restored = onera
        .rollback_to_snapshot(&project, &v0, &v1, &BTreeMap::new())
        .await
        .unwrap();
    assert_eq!(restored, 1);

    let recorded = events.lock().await.clone();
    assert_eq!(
        recorded,
        vec![
            "pre_rollback:Hooked".to_owned(),
            "post_rollback:Hooked".to_owned()
        ]
    );
}
