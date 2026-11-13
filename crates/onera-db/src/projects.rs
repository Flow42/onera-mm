//! Persistence for generalized projects, provenance URIs, file change events,
//! and artifact dependencies.

use crate::convert::{from_timestamp, to_timestamp};
use crate::{db_err, Database};
use chrono::{DateTime, Utc};
use onera_core::domain::project::{DeploymentStrategy, Project, ProjectKind, ProvenanceUri};
use onera_core::hash::{FileHash, HashAlgorithm};
use onera_core::ids::{ArtifactId, ProjectId, ProviderId};
use onera_core::{CoreError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use url::Url;
use uuid::Uuid;

/// Persisted record of a filesystem change in a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChangeRecord {
    /// Relative path inside the project root.
    pub path: String,
    /// `"created"`, `"modified"`, `"deleted"`, or `"renamed"`.
    pub event_type: String,
    /// `"managed"` or `"external_drift"`.
    pub source: String,
    /// BLAKE3 hash prior to the change.
    pub previous_hash: Option<FileHash>,
    /// BLAKE3 hash after the change.
    pub new_hash: Option<FileHash>,
    /// Timestamp when observed.
    pub observed_at: DateTime<Utc>,
}

/// Dependency relationship kind between two artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactDependencyKind {
    /// Hard requirement.
    Required,
    /// Soft / optional recommendation.
    Recommended,
    /// Mutually exclusive conflict.
    Conflicts,
}

impl ArtifactDependencyKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Recommended => "recommended",
            Self::Conflicts => "conflicts",
        }
    }

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "required" => Ok(Self::Required),
            "recommended" => Ok(Self::Recommended),
            "conflicts" => Ok(Self::Conflicts),
            other => Err(CoreError::Database(format!(
                "invalid artifact dependency kind '{other}'"
            ))),
        }
    }
}

/// Persisted dependency edge between two artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactDependencyRecord {
    /// Artifact declaring the dependency.
    pub artifact_id: ArtifactId,
    /// Target artifact it depends on or conflicts with.
    pub depends_on_artifact: ArtifactId,
    /// Optional version constraint expression.
    pub version_constraint: Option<String>,
    /// Relationship kind.
    pub kind: ArtifactDependencyKind,
}

fn strategy_to_str(s: DeploymentStrategy) -> &'static str {
    match s {
        DeploymentStrategy::Symlink => "symlink",
        DeploymentStrategy::Hardlink => "hardlink",
        DeploymentStrategy::Reflink => "reflink",
        DeploymentStrategy::Copy => "copy",
    }
}

fn str_to_strategy(s: &str) -> Result<DeploymentStrategy> {
    match s {
        "symlink" => Ok(DeploymentStrategy::Symlink),
        "hardlink" => Ok(DeploymentStrategy::Hardlink),
        "reflink" => Ok(DeploymentStrategy::Reflink),
        "copy" => Ok(DeploymentStrategy::Copy),
        other => Err(CoreError::Database(format!(
            "invalid deployment strategy '{other}'"
        ))),
    }
}

impl Database {
    /// Insert or update a [`Project`].
    pub async fn upsert_project(&self, project: &Project) -> Result<()> {
        let (kind_str, kind_meta) = match &project.kind {
            ProjectKind::Folder => ("folder", None),
            ProjectKind::Game { .. } => (
                "game",
                Some(serde_json::to_string(&project.kind).map_err(|e| {
                    CoreError::Database(format!("failed to serialize project kind: {e}"))
                })?),
            ),
            ProjectKind::Custom { .. } => (
                "custom",
                Some(serde_json::to_string(&project.kind).map_err(|e| {
                    CoreError::Database(format!("failed to serialize project kind: {e}"))
                })?),
            ),
        };
        let root_str = project.root_path.to_string_lossy().into_owned();
        let staging_str = project
            .staging_root
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned());

        sqlx::query(
            "INSERT INTO projects (id, name, root_path, kind, kind_metadata, deployment_strategy, staging_root, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                root_path = excluded.root_path,
                kind = excluded.kind,
                kind_metadata = excluded.kind_metadata,
                deployment_strategy = excluded.deployment_strategy,
                staging_root = excluded.staging_root",
        )
        .bind(project.id.to_string())
        .bind(&project.name)
        .bind(root_str)
        .bind(kind_str)
        .bind(kind_meta)
        .bind(strategy_to_str(project.deployment_strategy))
        .bind(staging_str)
        .bind(to_timestamp(project.created_at))
        .execute(&self.pool)
        .await
        .map_err(db_err)?;

        Ok(())
    }

    /// Fetch a [`Project`] by ID.
    pub async fn get_project(&self, id: ProjectId) -> Result<Option<Project>> {
        let row: Option<(
            String,
            String,
            String,
            String,
            Option<String>,
            String,
            Option<String>,
            String,
        )> = sqlx::query_as(
            "SELECT id, name, root_path, kind, kind_metadata, deployment_strategy, staging_root, created_at
             FROM projects WHERE id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;

        row.map(row_to_project).transpose()
    }

    /// List all registered [`Project`]s ordered by creation time.
    pub async fn list_projects(&self) -> Result<Vec<Project>> {
        let rows: Vec<(
            String,
            String,
            String,
            String,
            Option<String>,
            String,
            Option<String>,
            String,
        )> = sqlx::query_as(
            "SELECT id, name, root_path, kind, kind_metadata, deployment_strategy, staging_root, created_at
             FROM projects ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.into_iter().map(row_to_project).collect()
    }

    /// Record a download provenance URI for an artifact.
    pub async fn record_provenance_uri(
        &self,
        artifact_id: ArtifactId,
        provenance: &ProvenanceUri,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO provenance_uris (id, artifact_id, uri, provider_id, referrer_url, etag, content_hash, downloaded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(artifact_id.to_string())
        .bind(provenance.uri.as_str())
        .bind(provenance.provider.as_ref().map(|p| p.as_str().to_owned()))
        .bind(&provenance.referrer_url)
        .bind(&provenance.etag)
        .bind(&provenance.content_hash.hex)
        .bind(to_timestamp(provenance.downloaded_at))
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    /// Fetch the most recent [`ProvenanceUri`] for an artifact.
    pub async fn provenance_for_artifact(
        &self,
        artifact_id: ArtifactId,
    ) -> Result<Option<ProvenanceUri>> {
        let row: Option<(String, Option<String>, Option<String>, Option<String>, String, String)> =
            sqlx::query_as(
                "SELECT uri, provider_id, referrer_url, etag, content_hash, downloaded_at
                 FROM provenance_uris WHERE artifact_id = ?1
                 ORDER BY downloaded_at DESC LIMIT 1",
            )
            .bind(artifact_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;

        let Some((uri_str, provider_id, referrer_url, etag, content_hash, downloaded_at)) = row
        else {
            return Ok(None);
        };

        let uri = Url::parse(&uri_str)
            .map_err(|e| CoreError::Database(format!("invalid stored URI '{uri_str}': {e}")))?;

        Ok(Some(ProvenanceUri {
            uri,
            provider: provider_id.map(ProviderId::new),
            referrer_url,
            etag,
            content_hash: FileHash {
                algorithm: HashAlgorithm::Blake3,
                hex: content_hash,
            },
            downloaded_at: from_timestamp(&downloaded_at)?,
        }))
    }

    /// Append detected file change events to the project's audit ledger.
    pub async fn record_file_changes(
        &self,
        project_id: ProjectId,
        changes: &[FileChangeRecord],
    ) -> Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        for change in changes {
            sqlx::query(
                "INSERT INTO file_change_events (project_id, path, event_type, source, previous_hash, new_hash, observed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .bind(project_id.to_string())
            .bind(&change.path)
            .bind(&change.event_type)
            .bind(&change.source)
            .bind(change.previous_hash.as_ref().map(|h| h.hex.clone()))
            .bind(change.new_hash.as_ref().map(|h| h.hex.clone()))
            .bind(to_timestamp(change.observed_at))
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    /// Fetch recorded file change events for a project, ordered chronologically.
    pub async fn file_changes_for_project(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<FileChangeRecord>> {
        let rows: Vec<(String, String, String, Option<String>, Option<String>, String)> =
            sqlx::query_as(
                "SELECT path, event_type, source, previous_hash, new_hash, observed_at
                 FROM file_change_events WHERE project_id = ?1
                 ORDER BY id ASC",
            )
            .bind(project_id.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)?;

        rows.into_iter()
            .map(
                |(path, event_type, source, prev, new, observed_at)| -> Result<FileChangeRecord> {
                    Ok(FileChangeRecord {
                        path,
                        event_type,
                        source,
                        previous_hash: prev.map(|hex| FileHash {
                            algorithm: HashAlgorithm::Blake3,
                            hex,
                        }),
                        new_hash: new.map(|hex| FileHash {
                            algorithm: HashAlgorithm::Blake3,
                            hex,
                        }),
                        observed_at: from_timestamp(&observed_at)?,
                    })
                },
            )
            .collect()
    }

    /// Define or update a dependency edge between two downloaded/imported artifacts.
    pub async fn upsert_artifact_dependency(&self, dep: &ArtifactDependencyRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO artifact_dependencies (artifact_id, depends_on_artifact, version_constraint, kind)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(artifact_id, depends_on_artifact) DO UPDATE SET
                version_constraint = excluded.version_constraint,
                kind = excluded.kind",
        )
        .bind(dep.artifact_id.to_string())
        .bind(dep.depends_on_artifact.to_string())
        .bind(&dep.version_constraint)
        .bind(dep.kind.as_str())
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    /// List all dependencies declared for an artifact.
    pub async fn dependencies_for_artifact(
        &self,
        artifact_id: ArtifactId,
    ) -> Result<Vec<ArtifactDependencyRecord>> {
        let rows: Vec<(String, String, Option<String>, String)> = sqlx::query_as(
            "SELECT artifact_id, depends_on_artifact, version_constraint, kind
             FROM artifact_dependencies WHERE artifact_id = ?1",
        )
        .bind(artifact_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.into_iter()
            .map(|(a, b, constraint, kind)| {
                Ok(ArtifactDependencyRecord {
                    artifact_id: a.parse().map_err(|e| {
                        CoreError::Database(format!("invalid artifact_id '{a}': {e}"))
                    })?,
                    depends_on_artifact: b.parse().map_err(|e| {
                        CoreError::Database(format!("invalid depends_on_artifact '{b}': {e}"))
                    })?,
                    version_constraint: constraint,
                    kind: ArtifactDependencyKind::from_str(&kind)?,
                })
            })
            .collect()
    }
}

type ProjectRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    String,
);

fn row_to_project(row: ProjectRow) -> Result<Project> {
    let (id, name, root_path, kind_str, kind_meta, strategy, staging_root, created_at) = row;
    let id: ProjectId = id
        .parse()
        .map_err(|e| CoreError::Database(format!("invalid project id '{id}': {e}")))?;
    let kind = match kind_str.as_str() {
        "folder" => ProjectKind::Folder,
        "game" | "custom" => match kind_meta {
            Some(meta) => serde_json::from_str(&meta).unwrap_or(ProjectKind::Folder),
            None => ProjectKind::Folder,
        },
        other => {
            return Err(CoreError::Database(format!(
                "unknown project kind '{other}'"
            )))
        }
    };
    Ok(Project {
        id,
        name,
        root_path: PathBuf::from(root_path),
        kind,
        deployment_strategy: str_to_strategy(&strategy)?,
        staging_root: staging_root.map(PathBuf::from),
        created_at: from_timestamp(&created_at)?,
    })
}
