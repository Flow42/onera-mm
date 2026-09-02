//! The Steam [`GameStore`] adapter, and the manifest boundary reserved for later.
//!
//! [`SteamGameStore`] answers "which build of this game is installed?" for a
//! registered [`LocalGameInstall`], using nothing but the `appmanifest` Steam
//! wrote next to the game. It never launches Steam, never talks to a Steam
//! service, and never asks the user for Steam credentials.
//!
//! [`SteamManifestProvider`] is the other half of that honesty: Steam documents
//! depot manifests as carrying file paths, sizes, flags and SHA-1 hashes, but
//! publishes no supported consumer API for retrieving them. The port is
//! implemented so the wiring exists, and it reports
//! [`ManifestAvailability::Unsupported`] because that is the true answer today.

use crate::identity;
use async_trait::async_trait;
use onera_core::domain::baseline::{StoreBuildIdentity, StoreDlc};
use onera_core::domain::game::{InstallSource, LocalGameInstall};
use onera_core::ports::{GameManifestProvider, GameStore, ManifestAvailability, StoreCapability};
use onera_core::progress::CancelToken;
use onera_core::Result;
use std::path::{Path, PathBuf};

/// Stable slug of the Steam store adapter.
pub const STEAM_STORE_ID: &str = "steam";

/// Build identity for Steam-managed installations.
///
/// Stateless: every call re-reads the manifest, because the point of the type is
/// to notice when Steam has changed it.
#[derive(Debug, Clone, Copy, Default)]
pub struct SteamGameStore;

impl SteamGameStore {
    /// A new adapter.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// Locate the `appmanifest_<appid>.acf` that describes `install_root`.
///
/// The search is *layout-relative*, not rooted at a Steam installation: a game
/// always sits at `<library>/steamapps/common/<installdir>`, so the manifest
/// directory is two levels up. That is what makes native Steam, Flatpak Steam
/// and a library on a second drive one code path instead of three — only the
/// prefix in front of `steamapps` differs, and this never looks at it.
///
/// Returns `None` when the directory is not shaped like a Steam library entry,
/// or when no manifest in that library claims this `installdir`.
#[must_use]
pub fn locate_app_manifest(install_root: &Path) -> Option<PathBuf> {
    let install_dir = install_root.file_name()?.to_str()?;
    let common = install_root.parent()?;
    if common.file_name()? != "common" {
        return None;
    }
    let steamapps = common.parent()?;
    if steamapps.file_name()? != "steamapps" {
        return None;
    }

    let mut fallback = None;
    for entry in std::fs::read_dir(steamapps).ok()?.flatten() {
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        if !identity::is_app_manifest_name(file_name) {
            continue;
        }
        let Some(manifest) = identity::read_app_manifest(&entry.path()) else {
            continue;
        };
        if manifest.install_dir == install_dir {
            return Some(manifest.path);
        }
        // Steam's `installdir` is written by the publisher and the directory on
        // disk is created from it, so an exact match is the norm. A
        // case-insensitive hit is kept only as a fallback, and only when nothing
        // matches exactly, so a case-only clash can never silently win.
        if manifest.install_dir.eq_ignore_ascii_case(install_dir) && fallback.is_none() {
            fallback = Some(manifest.path);
        }
    }
    fallback
}

/// Read the Steam build identity for one installation, if there is one to read.
///
/// Separate from the trait so callers that are not holding a `LocalGameInstall`
/// — discovery, diagnostics, tests — can use the same rules.
#[must_use]
pub fn build_identity_at(install_root: &Path) -> Option<StoreBuildIdentity> {
    let path = locate_app_manifest(install_root)?;
    let manifest = identity::read_app_manifest(&path)?;
    Some(manifest.identity.to_store_identity(chrono::Utc::now()))
}

#[async_trait]
impl GameStore for SteamGameStore {
    fn id(&self) -> &str {
        STEAM_STORE_ID
    }

    async fn build_identity(
        &self,
        install: &LocalGameInstall,
    ) -> Result<StoreCapability<StoreBuildIdentity>> {
        if install.source == InstallSource::Manual {
            // A manually added path may happen to sit inside a Steam library,
            // but Onera did not learn it from Steam and will not assert a Steam
            // identity for it. `Unknown` is the honest answer and keeps the
            // baseline visibly a local snapshot.
            return Ok(StoreCapability::unknown(
                "this installation was added manually, so Steam has no record of its build",
            ));
        }

        let Some(path) = locate_app_manifest(&install.install_root) else {
            return Ok(StoreCapability::unknown(format!(
                "no Steam app manifest describes {}",
                install.install_root.display()
            )));
        };
        let Some(manifest) = identity::read_app_manifest(&path) else {
            return Ok(StoreCapability::unknown(format!(
                "the Steam app manifest at {} could not be read",
                path.display()
            )));
        };
        Ok(StoreCapability::known(
            manifest.identity.to_store_identity(chrono::Utc::now()),
        ))
    }

    async fn owned_dlc(
        &self,
        _install: &LocalGameInstall,
    ) -> Result<StoreCapability<Vec<StoreDlc>>> {
        // The appmanifest lists depots that are *installed*, which is neither
        // ownership nor a complete list, and the ownership APIs Steam does
        // publish need credentials Onera will not ask for. Reporting an empty
        // list here would let a solver conclude the user owns no DLC.
        Ok(StoreCapability::unknown(
            "Steam publishes no credential-free API for DLC ownership",
        ))
    }
}

/// The Steam manifest boundary — deliberately unsupported for now.
///
/// This exists so that the day Steam publishes a consumer API for depot
/// manifests, an authoritative expected-file set can replace local baseline
/// capture by changing this one type. It does not exist to imply that such a
/// manifest is available: every call reports
/// [`ManifestAvailability::Unsupported`], which the baseline domain already
/// distinguishes from "we asked and it failed".
#[derive(Debug, Clone, Copy, Default)]
pub struct SteamManifestProvider;

impl SteamManifestProvider {
    /// A new manifest provider.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl GameManifestProvider for SteamManifestProvider {
    fn id(&self) -> &str {
        STEAM_STORE_ID
    }

    async fn expected_manifest(
        &self,
        _install: &LocalGameInstall,
        _identity: &StoreBuildIdentity,
        _cancel: &CancelToken,
    ) -> Result<ManifestAvailability> {
        Ok(ManifestAvailability::Unsupported)
    }
}
