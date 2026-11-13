//! Direct HTTP/HTTPS and local `file://` URI provider plugin.
//!
//! Enables Onera to download or import files from arbitrary URLs (such as GitHub
//! releases, direct file links, or local paths) while recording full URI
//! provenance.

use crate::{PluginMetadata, ProviderPlugin};
use async_trait::async_trait;
use onera_core::ports::DownloadTarget;
use onera_core::progress::CancelToken;
use onera_core::{CoreError, Result};
use url::Url;

/// Built-in provider for direct `http://`, `https://`, and `file://` URIs.
#[derive(Debug, Default, Clone, Copy)]
pub struct HttpProviderPlugin;

impl HttpProviderPlugin {
    /// Stable plugin identifier.
    pub const ID: &'static str = "direct_uri";
}

#[async_trait]
impl ProviderPlugin for HttpProviderPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: Self::ID.to_owned(),
            name: "Direct URI / HTTP".to_owned(),
            version: "1.0.0".to_owned(),
            description: "Resolves direct HTTP, HTTPS, and local file:// URIs".to_owned(),
        }
    }

    fn supported_schemes(&self) -> &[&str] {
        &["https", "http", "file"]
    }

    fn can_handle_uri(&self, uri: &Url) -> bool {
        matches!(uri.scheme(), "https" | "http" | "file")
    }

    async fn resolve_uri(&self, uri: &Url, cancel: &CancelToken) -> Result<DownloadTarget> {
        cancel.check()?;
        if !self.can_handle_uri(uri) {
            return Err(CoreError::InvalidInput(format!(
                "unsupported URI scheme '{}' for direct provider",
                uri.scheme()
            )));
        }

        let filename = uri
            .path_segments()
            .and_then(|mut segments| segments.rfind(|s| !s.is_empty()))
            .unwrap_or("artifact.bin")
            .to_owned();

        Ok(DownloadTarget {
            url: uri.clone(),
            headers: Vec::new(),
            expected_size: None,
            filename,
        })
    }
}
