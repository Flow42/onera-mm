//! Central plugin registry.

use crate::{
    HttpProviderPlugin, LifecycleHookPlugin, PluginMetadata, ProfileDetection,
    ProjectProfilePlugin, ProviderPlugin,
};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use url::Url;

/// Holds all active provider, project profile, and lifecycle hook plugins.
#[derive(Default)]
pub struct PluginRegistry {
    providers: BTreeMap<String, Arc<dyn ProviderPlugin>>,
    profiles: BTreeMap<String, Arc<dyn ProjectProfilePlugin>>,
    hooks: Vec<Arc<dyn LifecycleHookPlugin>>,
}

impl PluginRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a registry pre-populated with built-in generic plugins
    /// (`GenericFolderProfile` and `HttpProviderPlugin`).
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        registry.register_profile(Arc::new(crate::GenericFolderProfile));
        registry.register_provider(Arc::new(HttpProviderPlugin));
        registry
    }

    /// Register a [`ProviderPlugin`].
    pub fn register_provider(&mut self, plugin: Arc<dyn ProviderPlugin>) {
        let id = plugin.metadata().id;
        self.providers.insert(id, plugin);
    }

    /// Register a [`ProjectProfilePlugin`].
    pub fn register_profile(&mut self, plugin: Arc<dyn ProjectProfilePlugin>) {
        let id = plugin.metadata().id;
        self.profiles.insert(id, plugin);
    }

    /// Register a [`LifecycleHookPlugin`].
    pub fn register_hook(&mut self, plugin: Arc<dyn LifecycleHookPlugin>) {
        self.hooks.push(plugin);
    }

    /// Look up a provider plugin by its ID.
    #[must_use]
    pub fn provider(&self, id: &str) -> Option<Arc<dyn ProviderPlugin>> {
        self.providers.get(id).cloned()
    }

    /// Find the best provider plugin capable of handling `uri`.
    ///
    /// Specialized providers (e.g. Nexus) are checked before the generic
    /// [`HttpProviderPlugin`] fallback.
    #[must_use]
    pub fn provider_for_uri(&self, uri: &Url) -> Option<Arc<dyn ProviderPlugin>> {
        let mut fallback = None;
        for (id, provider) in &self.providers {
            if !provider.can_handle_uri(uri) {
                continue;
            }
            if id == HttpProviderPlugin::ID {
                fallback = Some(Arc::clone(provider));
            } else {
                return Some(Arc::clone(provider));
            }
        }
        fallback
    }

    /// Look up a project profile plugin by its ID.
    #[must_use]
    pub fn profile(&self, id: &str) -> Option<Arc<dyn ProjectProfilePlugin>> {
        self.profiles.get(id).cloned()
    }

    /// Detect the highest-priority valid profile for a directory on disk.
    #[must_use]
    pub fn detect_profile(
        &self,
        path: &Path,
    ) -> Option<(Arc<dyn ProjectProfilePlugin>, ProfileDetection)> {
        let mut best: Option<(Arc<dyn ProjectProfilePlugin>, ProfileDetection)> = None;
        for profile in self.profiles.values() {
            let detection = profile.detect(path);
            if !detection.validation.valid {
                continue;
            }
            match &best {
                Some((_, current)) if current.priority >= detection.priority => {}
                _ => best = Some((Arc::clone(profile), detection)),
            }
        }
        best
    }

    /// All registered lifecycle hooks.
    #[must_use]
    pub fn hooks(&self) -> &[Arc<dyn LifecycleHookPlugin>] {
        &self.hooks
    }

    /// Metadata for all registered providers.
    #[must_use]
    pub fn list_providers(&self) -> Vec<PluginMetadata> {
        self.providers.values().map(|p| p.metadata()).collect()
    }

    /// Metadata for all registered project profiles.
    #[must_use]
    pub fn list_profiles(&self) -> Vec<PluginMetadata> {
        self.profiles.values().map(|p| p.metadata()).collect()
    }

    /// Metadata for all registered lifecycle hooks.
    #[must_use]
    pub fn list_hooks(&self) -> Vec<PluginMetadata> {
        self.hooks.iter().map(|h| h.metadata()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GenericFolderProfile;
    use onera_core::domain::archive::{ArchiveFormat, ArchiveManifest, ManifestFile};
    use onera_core::hash::FileHash;
    use onera_core::ids::ArchiveId;
    use onera_core::paths::RelPath;
    use onera_core::progress::CancelToken;

    #[test]
    fn registry_detects_generic_folder_for_existing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let registry = PluginRegistry::with_builtins();

        let (profile, detection) = registry.detect_profile(dir.path()).unwrap();
        assert_eq!(profile.metadata().id, GenericFolderProfile::ID);
        assert!(detection.validation.valid);
        assert_eq!(detection.priority, 1);
    }

    #[test]
    fn generic_folder_maps_archive_files_one_to_one() {
        let profile = GenericFolderProfile;
        let manifest = ArchiveManifest {
            archive_id: ArchiveId::new(),
            archive_hash: FileHash::blake3_of(b"zip"),
            format: ArchiveFormat::Zip,
            files: vec![ManifestFile {
                path: RelPath::normalize("assets/logo.png").unwrap(),
                size: 42,
                hash: FileHash::blake3_of(b"png"),
                executable: false,
            }],
            directories: vec![RelPath::normalize("assets").unwrap()],
        };

        let layout = profile.resolve_layout(&manifest).unwrap();
        assert_eq!(layout.mappings.len(), 1);
        assert_eq!(layout.mappings[0].0.as_str(), "assets/logo.png");
        assert_eq!(layout.mappings[0].1.root_key, "root");
        assert_eq!(layout.mappings[0].1.path.as_str(), "assets/logo.png");
    }

    #[tokio::test]
    async fn http_provider_resolves_direct_uris() {
        let registry = PluginRegistry::with_builtins();
        let url = Url::parse("https://example.com/releases/v1.0/my-package.zip").unwrap();

        let provider = registry.provider_for_uri(&url).unwrap();
        assert_eq!(provider.metadata().id, HttpProviderPlugin::ID);

        let target = provider
            .resolve_uri(&url, &CancelToken::new())
            .await
            .unwrap();
        assert_eq!(target.filename, "my-package.zip");
        assert_eq!(target.url, url);
    }
}
