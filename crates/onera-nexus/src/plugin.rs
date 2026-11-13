//! `ProviderPlugin` adapter for Nexus Mods.

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use onera_core::ids::{ProviderFileId, ProviderModId};
use onera_core::ports::{DownloadGrant, DownloadTarget, ModProvider};
use onera_core::progress::CancelToken;
use onera_core::redact::Secret;
use onera_core::{CoreError, Result};
use onera_plugin::{PluginMetadata, ProviderPlugin};
use std::sync::Arc;
use url::Url;

/// Wraps a Nexus [`ModProvider`] (such as [`crate::NexusClient`]) as an
/// [`onera_plugin::ProviderPlugin`].
pub struct NexusProviderPlugin {
    provider: Arc<dyn ModProvider>,
}

impl NexusProviderPlugin {
    /// Wrap a Nexus [`ModProvider`] implementation.
    #[must_use]
    pub fn new(provider: Arc<dyn ModProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl ProviderPlugin for NexusProviderPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: self.provider.id().to_string(),
            name: "Nexus Mods".to_owned(),
            version: "3.0.0".to_owned(),
            description: "Nexus Mods API v3 provider and nxm:// URI resolver".to_owned(),
        }
    }

    fn supported_schemes(&self) -> &[&str] {
        &["nxm", "https"]
    }

    fn can_handle_uri(&self, uri: &Url) -> bool {
        if uri.scheme() == "nxm" {
            return true;
        }
        matches!(uri.scheme(), "https" | "http")
            && uri
                .host_str()
                .is_some_and(|h| h == "nexusmods.com" || h.ends_with(".nexusmods.com"))
    }

    async fn resolve_uri(&self, uri: &Url, cancel: &CancelToken) -> Result<DownloadTarget> {
        cancel.check()?;
        if uri.scheme() != "nxm" {
            return Err(CoreError::InvalidInput(format!(
                "Nexus URI must use nxm:// scheme to resolve a direct file download (got {})",
                uri.scheme()
            )));
        }

        // Format: nxm://<game_domain>/mods/<mod_id>/files/<file_id>?key=<key>&expires=<ts>
        let game_domain = uri.host_str().ok_or_else(|| {
            CoreError::InvalidInput("nxm URI is missing game domain host".to_owned())
        })?;
        let segments: Vec<&str> = uri
            .path_segments()
            .map(|s| s.collect())
            .unwrap_or_default();
        if segments.len() < 4 || segments[0] != "mods" || segments[2] != "files" {
            return Err(CoreError::InvalidInput(format!(
                "unrecognized nxm URI path: {}",
                uri.path()
            )));
        }

        let mod_id = ProviderModId::new(segments[1]);
        let file_id = ProviderFileId::new(segments[3]);

        let mut key: Option<String> = None;
        let mut expires: Option<i64> = None;
        for (k, v) in uri.query_pairs() {
            match k.as_ref() {
                "key" => key = Some(v.into_owned()),
                "expires" => expires = v.parse::<i64>().ok(),
                _ => {}
            }
        }

        let grant = match (key, expires) {
            (Some(k), Some(exp)) => Utc
                .timestamp_opt(exp, 0)
                .single()
                .map(|expires_at| DownloadGrant {
                    key: Secret::new(k),
                    expires_at,
                }),
            _ => None,
        };

        self.provider
            .resolve_download(game_domain, &mod_id, &file_id, grant.as_ref(), cancel)
            .await
    }

    fn as_mod_provider(&self) -> Option<Arc<dyn ModProvider>> {
        Some(Arc::clone(&self.provider))
    }
}
