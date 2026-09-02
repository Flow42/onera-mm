//! Game discovery.
//!
//! Discovery reads platform metadata rather than scanning disks, matches what it
//! finds against the game adapters Onera ships and the provider's game
//! catalogue, and returns *candidates*. Nothing is registered until the user
//! confirms it — a wrong match would point deployments at the wrong directory.
//!
//! The same metadata answers a second question: *which build* is installed.
//! [`identity`] parses that out of Steam's own `appmanifest`, and [`store`]
//! exposes it through [`onera_core::ports::GameStore`] so a baseline can be
//! stamped with the build it was captured from. No Steam credential, running
//! client or undocumented service is involved in either.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod identity;
pub mod steam;
pub mod store;
pub mod vdf;

use onera_core::domain::game::{Game, InstallSource, InstallValidation};
use onera_core::ports::GameAdapter;
use onera_core::Result;
use std::path::{Path, PathBuf};

/// A detected game awaiting the user's confirmation.
///
/// Serializable because it crosses the Tauri bridge: the frontend shows the
/// candidate and hands the same value back when the user confirms it, so the
/// backend never has to re-scan to find what was clicked.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiscoveredGame {
    /// Adapter that claims it.
    pub adapter_id: String,
    /// Provider catalogue entry it matched, when one was found.
    pub provider_slug: Option<String>,
    /// Name to show the user.
    pub name: String,
    /// The game's directory.
    pub install_root: PathBuf,
    /// Proton prefix, if any.
    pub compat_prefix: Option<PathBuf>,
    /// User-data directories.
    pub user_data_roots: Vec<PathBuf>,
    /// How it was found.
    pub source: InstallSource,
    /// What the adapter said about the directory.
    pub validation: InstallValidation,
}

impl DiscoveredGame {
    /// Whether this candidate is safe to offer.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.validation.valid
    }
}

/// Scan for games under a given home directory.
///
/// `catalogue` is the provider's list of supported games; a detected game with
/// no catalogue match is still reported, because the user may want to manage it
/// with a manually added path.
///
/// # Errors
/// Propagates errors from reading Steam's metadata.
pub fn discover(
    home: &Path,
    adapters: &[&dyn GameAdapter],
    catalogue: &[Game],
) -> Result<Vec<DiscoveredGame>> {
    let mut found = Vec::new();
    for install in steam::find_steam_installs(home) {
        for app in steam::installed_apps(&install)? {
            let Some(adapter) = adapters
                .iter()
                .find(|a| a.steam_app_ids().contains(&app.app_id))
            else {
                continue;
            };
            let validation = adapter.validate_install(&app.install_root);
            let provider_slug = catalogue
                .iter()
                .find(|g| {
                    adapter.provider_slugs().contains(&g.provider_slug.as_str())
                        || g.steam_app_id == Some(app.app_id)
                })
                .map(|g| g.provider_slug.clone());

            found.push(DiscoveredGame {
                adapter_id: adapter.id().to_owned(),
                provider_slug,
                name: app.name,
                install_root: app.install_root,
                compat_prefix: app.compat_prefix,
                user_data_roots: app.user_data_roots,
                source: app.source,
                validation,
            });
        }
    }
    Ok(found)
}

/// Validate a path the user typed in.
///
/// Manual paths bypass Steam entirely, which is how a GOG, Epic or Heroic
/// installation is supported without Onera needing to understand those
/// launchers.
///
/// # Errors
/// Returns [`onera_core::CoreError::InvalidGameInstall`] if no adapter
/// recognizes the directory.
pub fn add_manual(install_root: &Path, adapters: &[&dyn GameAdapter]) -> Result<DiscoveredGame> {
    for adapter in adapters {
        let validation = adapter.validate_install(install_root);
        if validation.valid {
            return Ok(DiscoveredGame {
                adapter_id: adapter.id().to_owned(),
                provider_slug: adapter.provider_slugs().first().map(|s| (*s).to_owned()),
                name: adapter.display_name().to_owned(),
                install_root: install_root.to_path_buf(),
                compat_prefix: None,
                user_data_roots: Vec::new(),
                source: InstallSource::Manual,
                validation,
            });
        }
    }
    Err(onera_core::CoreError::InvalidGameInstall(format!(
        "no game adapter recognizes {}",
        install_root.display()
    )))
}
