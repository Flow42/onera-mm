//! Modular plugin and extension framework for Onera.
//!
//! Onera decouples external mod sources (e.g. Nexus Mods, direct HTTP/GitHub)
//! and target project layouts (e.g. Cyberpunk 2077, Skyrim SE, generic folders)
//! behind stable plugin traits managed by [`PluginRegistry`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod generic_folder;
pub mod http_provider;
pub mod registry;

pub use generic_folder::GenericFolderProfile;
pub use http_provider::HttpProviderPlugin;
pub use registry::PluginRegistry;

use async_trait::async_trait;
use onera_core::domain::archive::ArchiveManifest;
use onera_core::domain::baseline::{BaselineExclusion, BaselineRoot};
use onera_core::domain::game::{DeployRoot, InstallValidation, LocalGameInstall};
use onera_core::domain::project::Project;
use onera_core::ids::OperationId;
use onera_core::plan::{InstallPlan, TargetLocation};
use onera_core::ports::{DownloadTarget, GameAdapter, LayoutResolution, ModProvider};
use onera_core::progress::CancelToken;
use onera_core::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use url::Url;

/// Descriptive metadata for any registered plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginMetadata {
    /// Unique stable slug for this plugin (e.g. `"nexus"`, `"cyberpunk2077"`, `"generic_folder"`).
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Semantic version of the plugin.
    pub version: String,
    /// Short summary of what this plugin provides.
    pub description: String,
}

/// Result of inspecting a directory to determine if a profile plugin matches it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileDetection {
    /// Plugin ID that matched.
    pub plugin_id: String,
    /// Confidence score (higher wins when multiple plugins match; generic folder is 0).
    pub priority: u32,
    /// Validation findings from the profile.
    pub validation: InstallValidation,
}

/// A plugin that resolves and fetches downloadable artifacts from a URI or catalog.
#[async_trait]
pub trait ProviderPlugin: Send + Sync {
    /// Descriptive metadata for this provider.
    fn metadata(&self) -> PluginMetadata;

    /// Supported URI schemes or host patterns (for display and routing).
    fn supported_schemes(&self) -> &[&str];

    /// Whether this provider can handle the given URI (e.g., `nxm://`, `https://...`).
    fn can_handle_uri(&self, uri: &Url) -> bool;

    /// Resolve a URI into a concrete [`DownloadTarget`] ready for streaming.
    async fn resolve_uri(&self, uri: &Url, cancel: &CancelToken) -> Result<DownloadTarget>;

    /// Optional underlying [`ModProvider`] catalog interface, if this plugin
    /// exposes a full mod catalog API (like Nexus Mods).
    fn as_mod_provider(&self) -> Option<Arc<dyn ModProvider>> {
        None
    }
}

/// A plugin defining how a project folder is validated, mapped, and scanned.
pub trait ProjectProfilePlugin: Send + Sync {
    /// Descriptive metadata for this profile.
    fn metadata(&self) -> PluginMetadata;

    /// Detect whether `path` matches this profile (e.g. contains `bin/x64/Cyberpunk2077.exe`).
    fn detect(&self, path: &Path) -> ProfileDetection;

    /// Derive deployment roots for a [`Project`].
    fn deploy_roots(&self, project: &Project) -> Result<Vec<DeployRoot>>;

    /// Derive baseline scan roots for a [`Project`].
    fn baseline_roots(&self, project: &Project) -> Result<Vec<BaselineRoot>>;

    /// Exclusion patterns for initial baseline / drift scanning.
    fn baseline_exclusions(&self) -> Vec<BaselineExclusion>;

    /// Map an extracted archive's files onto deployment roots.
    fn resolve_layout(&self, manifest: &ArchiveManifest) -> Result<LayoutResolution>;

    /// Validate that a deployment target path is safe/legal for this project.
    fn validate_target(&self, target: &TargetLocation) -> Result<()>;

    /// Optional underlying [`GameAdapter`] interface for backward compatibility.
    fn as_game_adapter(&self) -> Option<&dyn GameAdapter> {
        None
    }
}

/// A plugin that reacts to project lifecycle events (deploy, rollback).
#[async_trait]
pub trait LifecycleHookPlugin: Send + Sync {
    /// Descriptive metadata for this hook plugin.
    fn metadata(&self) -> PluginMetadata;

    /// Called before an [`InstallPlan`] begins writing/linking to disk.
    async fn on_pre_deploy(&self, _project: &Project, _plan: &InstallPlan) -> Result<()> {
        Ok(())
    }

    /// Called after an [`InstallPlan`] completes successfully.
    async fn on_post_deploy(&self, _project: &Project, _operation_id: OperationId) -> Result<()> {
        Ok(())
    }

    /// Called before a rollback operation begins.
    async fn on_pre_rollback(&self, _project: &Project, _operation_id: OperationId) -> Result<()> {
        Ok(())
    }

    /// Called after a rollback operation completes.
    async fn on_post_rollback(&self, _project: &Project, _operation_id: OperationId) -> Result<()> {
        Ok(())
    }
}

/// Wraps any [`GameAdapter`] into a [`ProjectProfilePlugin`].
pub struct GameProfileAdapter<A: GameAdapter + 'static> {
    adapter: A,
    priority: u32,
}

impl<A: GameAdapter + 'static> GameProfileAdapter<A> {
    /// Wrap a [`GameAdapter`] with a detection priority (default 100 for specific games).
    #[must_use]
    pub const fn new(adapter: A, priority: u32) -> Self {
        Self { adapter, priority }
    }
}

impl<A: GameAdapter + 'static> ProjectProfilePlugin for GameProfileAdapter<A> {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: self.adapter.id().to_owned(),
            name: self.adapter.display_name().to_owned(),
            version: "1.0.0".to_owned(),
            description: format!("Game profile adapter for {}", self.adapter.display_name()),
        }
    }

    fn detect(&self, path: &Path) -> ProfileDetection {
        let validation = self.adapter.validate_install(path);
        ProfileDetection {
            plugin_id: self.adapter.id().to_owned(),
            priority: if validation.valid { self.priority } else { 0 },
            validation,
        }
    }

    fn deploy_roots(&self, project: &Project) -> Result<Vec<DeployRoot>> {
        let install = project_to_local_install(project, self.adapter.id());
        self.adapter.deploy_roots(&install)
    }

    fn baseline_roots(&self, project: &Project) -> Result<Vec<BaselineRoot>> {
        let install = project_to_local_install(project, self.adapter.id());
        self.adapter.baseline_roots(&install)
    }

    fn baseline_exclusions(&self) -> Vec<BaselineExclusion> {
        self.adapter.baseline_exclusions()
    }

    fn resolve_layout(&self, manifest: &ArchiveManifest) -> Result<LayoutResolution> {
        self.adapter.resolve_layout(manifest)
    }

    fn validate_target(&self, target: &TargetLocation) -> Result<()> {
        self.adapter.validate_target(target)
    }

    fn as_game_adapter(&self) -> Option<&dyn GameAdapter> {
        Some(&self.adapter)
    }
}

fn project_to_local_install(project: &Project, adapter_id: &str) -> LocalGameInstall {
    use onera_core::domain::game::InstallSource;
    use onera_core::domain::project::ProjectKind;
    use onera_core::ids::{GameId, LocalGameId};

    let (game_id, local_game_id) = match &project.kind {
        ProjectKind::Game {
            game_id,
            local_game_id,
            ..
        } => (
            game_id.unwrap_or_else(GameId::new),
            local_game_id.unwrap_or_else(|| LocalGameId::from(project.id)),
        ),
        _ => (GameId::new(), LocalGameId::from(project.id)),
    };

    LocalGameInstall {
        id: local_game_id,
        game_id,
        adapter_id: adapter_id.to_owned(),
        source: InstallSource::Manual,
        install_root: project.root_path.clone(),
        compat_prefix: None,
        user_data_roots: Vec::new(),
        confirmed: true,
    }
}
