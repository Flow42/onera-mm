//! Managed folder workspaces and projects.
//!
//! A [`Project`] represents any directory on disk that Onera tracks:
//! - a game installation directory,
//! - a creative asset or plugin folder,
//! - or an arbitrary application directory.
//!
//! Every project owns an initial baseline, snapshot history, file-provider stack,
//! and provenance records for downloaded or deployed files.

use crate::hash::FileHash;
use crate::ids::{ArtifactId, GameId, LocalGameId, ProjectId, ProviderId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use url::Url;

/// Strategy used when deploying files from staging to target locations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentStrategy {
    /// Deploy via symbolic links from target to staging (recommended default on Linux).
    #[default]
    Symlink,
    /// Deploy via hardlinks on the same filesystem (avoids duplication without symlinks).
    Hardlink,
    /// Deploy via Copy-on-Write block cloning (reflinks on Btrfs/XFS/ZFS).
    Reflink,
    /// Fallback: physical file copy.
    Copy,
}

impl DeploymentStrategy {
    /// Whether this strategy creates symbolic links.
    #[must_use]
    pub const fn is_symlink(&self) -> bool {
        matches!(self, Self::Symlink)
    }

    /// Whether this strategy links rather than copying bytes.
    #[must_use]
    pub const fn is_linking(&self) -> bool {
        matches!(self, Self::Symlink | Self::Hardlink | Self::Reflink)
    }
}

/// The kind of project being managed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectKind {
    /// A game managed by an adapter (e.g. Cyberpunk, Skyrim).
    Game {
        /// Associated catalogue game identifier, if known.
        game_id: Option<GameId>,
        /// Associated local game install identifier, if known.
        local_game_id: Option<LocalGameId>,
        /// Adapter handling game-specific layout (e.g. "cyberpunk2077").
        adapter_id: String,
    },
    /// A generic directory tracked by Onera without special game layout rules.
    Folder,
    /// A custom profile or workspace layout.
    Custom {
        /// Identifier of the custom profile / plugin.
        profile_id: String,
    },
}

/// A managed folder workspace / project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// Unique identifier for this project.
    pub id: ProjectId,
    /// Human-readable name.
    pub name: String,
    /// Root path of the managed folder on disk.
    pub root_path: PathBuf,
    /// Specialization kind.
    pub kind: ProjectKind,
    /// File deployment strategy (defaults to Symlink).
    pub deployment_strategy: DeploymentStrategy,
    /// Custom staging root, if overriding default.
    pub staging_root: Option<PathBuf>,
    /// When this project was initialized.
    pub created_at: DateTime<Utc>,
}

impl Project {
    /// Create a generic folder project.
    #[must_use]
    pub fn new_folder(name: impl Into<String>, root_path: PathBuf) -> Self {
        Self {
            id: ProjectId::new(),
            name: name.into(),
            root_path,
            kind: ProjectKind::Folder,
            deployment_strategy: DeploymentStrategy::default(),
            staging_root: None,
            created_at: Utc::now(),
        }
    }

    /// Create a game project from a local game install.
    #[must_use]
    pub fn from_game_install(
        local_game_id: LocalGameId,
        game_id: GameId,
        name: impl Into<String>,
        root_path: PathBuf,
        adapter_id: impl Into<String>,
    ) -> Self {
        Self {
            id: ProjectId::from(local_game_id),
            name: name.into(),
            root_path,
            kind: ProjectKind::Game {
                game_id: Some(game_id),
                local_game_id: Some(local_game_id),
                adapter_id: adapter_id.into(),
            },
            deployment_strategy: DeploymentStrategy::default(),
            staging_root: None,
            created_at: Utc::now(),
        }
    }
}

/// Origin and provenance information for a downloaded or imported file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceUri {
    /// Stable URI the file was obtained from (`https://...`, `nxm://...`, `file://...`).
    pub uri: Url,
    /// Provider that served the download, if known.
    pub provider: Option<ProviderId>,
    /// Web page URL where the download originated.
    pub referrer_url: Option<String>,
    /// HTTP ETag at download time, if exposed.
    pub etag: Option<String>,
    /// Content hash of the downloaded payload.
    pub content_hash: FileHash,
    /// When the download or import completed.
    pub downloaded_at: DateTime<Utc>,
}

/// A tracked artifact/package within a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectArtifact {
    /// Onera's unique artifact identifier.
    pub id: ArtifactId,
    /// Project this artifact belongs to.
    pub project_id: ProjectId,
    /// Display name (e.g. mod name or package name).
    pub name: String,
    /// Version label, if known.
    pub version: Option<String>,
    /// Provenance URI details, if downloaded from an external URI.
    pub provenance: Option<ProvenanceUri>,
    /// BLAKE3 hash of the source archive or file.
    pub archive_hash: FileHash,
    /// When this artifact was registered.
    pub imported_at: DateTime<Utc>,
}
