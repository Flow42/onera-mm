//! Generic folder profile plugin.
//!
//! Treats any directory on disk as a managed workspace project, mapping archive
//! entries 1:1 relative to the project root without game-specific layout rules.

use crate::{PluginMetadata, ProfileDetection, ProjectProfilePlugin};
use onera_core::domain::archive::ArchiveManifest;
use onera_core::domain::baseline::{
    BaselineExclusion, BaselineRoot, ExclusionPattern, ExclusionReason,
};
use onera_core::domain::game::{DeployRoot, InstallValidation, LocalGameInstall};
use onera_core::domain::project::Project;
use onera_core::paths::{DeployRootKind, RelPath};
use onera_core::plan::TargetLocation;
use onera_core::ports::{GameAdapter, LayoutResolution};
use onera_core::{CoreError, Result};
use std::path::Path;

/// Built-in profile for arbitrary folder projects.
#[derive(Debug, Default, Clone, Copy)]
pub struct GenericFolderProfile;

impl GenericFolderProfile {
    /// Stable plugin identifier.
    pub const ID: &'static str = "generic_folder";

    /// Build a `BaselineExclusion` list shared by both trait implementations.
    fn default_exclusions() -> Vec<BaselineExclusion> {
        vec![
            BaselineExclusion {
                root_key: Some("root".to_owned()),
                pattern: ExclusionPattern::Prefix {
                    path: RelPath::normalize(".git").expect("valid relpath"),
                },
                reason: ExclusionReason::Other,
                note: Some("Git repository metadata".to_owned()),
            },
            BaselineExclusion {
                root_key: Some("root".to_owned()),
                pattern: ExclusionPattern::Prefix {
                    path: RelPath::normalize(".onera").expect("valid relpath"),
                },
                reason: ExclusionReason::Other,
                note: Some("Onera project metadata".to_owned()),
            },
        ]
    }
}

impl GameAdapter for GenericFolderProfile {
    fn id(&self) -> &str {
        Self::ID
    }

    fn display_name(&self) -> &str {
        "Generic Folder Project"
    }

    fn provider_slugs(&self) -> &[&str] {
        &[]
    }

    fn steam_app_ids(&self) -> &[u32] {
        &[]
    }

    fn validate_install(&self, install_root: &Path) -> InstallValidation {
        if install_root.is_dir() {
            InstallValidation::ok()
        } else {
            InstallValidation::invalid(format!(
                "{} is not an existing directory",
                install_root.display()
            ))
        }
    }

    fn deploy_roots(&self, install: &LocalGameInstall) -> Result<Vec<DeployRoot>> {
        Ok(vec![DeployRoot {
            key: "root".to_owned(),
            kind: DeployRootKind::GameInstall,
            path: install.install_root.clone(),
        }])
    }

    fn resolve_layout(&self, manifest: &ArchiveManifest) -> Result<LayoutResolution> {
        let mappings = manifest
            .files
            .iter()
            .map(|e| {
                let target = TargetLocation {
                    root_key: "root".to_owned(),
                    path: e.path.clone(),
                };
                (e.path.clone(), target)
            })
            .collect();

        Ok(LayoutResolution {
            mappings,
            rationale: "1:1 mapping into project root".to_owned(),
            ignored: Vec::new(),
        })
    }

    fn validate_target(&self, target: &TargetLocation) -> Result<()> {
        if target.root_key != "root" {
            return Err(CoreError::InvalidInput(format!(
                "unknown root key '{}' for generic folder project",
                target.root_key
            )));
        }
        Ok(())
    }

    fn baseline_exclusions(&self) -> Vec<BaselineExclusion> {
        Self::default_exclusions()
    }
}

impl ProjectProfilePlugin for GenericFolderProfile {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: Self::ID.to_owned(),
            name: "Generic Folder Project".to_owned(),
            version: "1.0.0".to_owned(),
            description: "Tracks any directory with 1:1 file mapping and baseline change tracking"
                .to_owned(),
        }
    }

    fn detect(&self, path: &Path) -> ProfileDetection {
        let validation = self.validate_install(path);
        ProfileDetection {
            plugin_id: Self::ID.to_owned(),
            // Lowest priority fallback: specialized profiles (e.g. games) override it.
            priority: if validation.valid { 1 } else { 0 },
            validation,
        }
    }

    fn deploy_roots(&self, project: &Project) -> Result<Vec<DeployRoot>> {
        Ok(vec![DeployRoot {
            key: "root".to_owned(),
            kind: DeployRootKind::GameInstall,
            path: project.root_path.clone(),
        }])
    }

    fn baseline_roots(&self, project: &Project) -> Result<Vec<BaselineRoot>> {
        Ok(vec![BaselineRoot {
            key: "root".to_owned(),
            kind: DeployRootKind::GameInstall,
            path: project.root_path.clone(),
        }])
    }

    fn baseline_exclusions(&self) -> Vec<BaselineExclusion> {
        Self::default_exclusions()
    }

    fn resolve_layout(&self, manifest: &ArchiveManifest) -> Result<LayoutResolution> {
        GameAdapter::resolve_layout(self, manifest)
    }

    fn validate_target(&self, target: &TargetLocation) -> Result<()> {
        GameAdapter::validate_target(self, target)
    }

    fn as_game_adapter(&self) -> Option<&dyn GameAdapter> {
        Some(self)
    }
}
