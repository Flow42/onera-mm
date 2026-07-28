//! Games, local installations and deployment roots.
//!
//! The four location kinds are modelled separately because on Linux they are
//! genuinely different directories: the install root lives in a Steam library,
//! the compatibility prefix under `steamapps/compatdata/<appid>/pfx`, user data
//! inside that prefix's `drive_c/users/steamuser`, and a deployment root may be
//! any of those or a subdirectory of one.

use crate::ids::{GameId, LocalGameId, ProviderId};
use crate::paths::DeployRootKind;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A game Onera knows about, as advertised by a provider's catalogue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Game {
    /// Onera's identifier for the game.
    pub id: GameId,
    /// Provider that supplied the catalogue entry.
    pub provider: ProviderId,
    /// Provider's opaque slug for the game, e.g. a Nexus domain name.
    ///
    /// Stored as a plain string precisely so the installation domain can carry
    /// it without depending on any provider's identifier type.
    pub provider_slug: String,
    /// Human-readable name.
    pub name: String,
    /// Steam application id, when the game is distributed on Steam.
    pub steam_app_id: Option<u32>,
}

/// Where the platform found a game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallSource {
    /// A native Steam installation.
    SteamNative,
    /// A Steam installation inside the Flatpak sandbox.
    SteamFlatpak,
    /// A path the user typed in.
    Manual,
}

/// A concrete game installation on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalGameInstall {
    /// Onera's identifier for this installation.
    pub id: LocalGameId,
    /// The catalogue game this installation is matched to.
    pub game_id: GameId,
    /// Slug of the adapter that manages it, e.g. `cyberpunk2077`.
    pub adapter_id: String,
    /// How it was found.
    pub source: InstallSource,
    /// The game's own installation directory.
    pub install_root: PathBuf,
    /// Proton/Wine prefix root, when the game runs under compatibility tooling.
    pub compat_prefix: Option<PathBuf>,
    /// User-data directories (saves, per-user configuration).
    pub user_data_roots: Vec<PathBuf>,
    /// Whether the user has confirmed this detection.
    pub confirmed: bool,
}

/// A directory mods are deployed into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployRoot {
    /// Stable adapter-defined key, e.g. `game` or `redmod`.
    pub key: String,
    /// Which class of location this root belongs to.
    pub kind: DeployRootKind,
    /// Absolute path on this machine.
    pub path: PathBuf,
}

/// The result of validating a game installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallValidation {
    /// Whether the directory really is this game.
    pub valid: bool,
    /// Version reported by the game, verbatim, when the adapter can read one.
    pub reported_version: Option<String>,
    /// Human-readable findings, safe to display.
    pub findings: Vec<String>,
}

impl InstallValidation {
    /// A successful validation with no remarks.
    #[must_use]
    pub fn ok() -> Self {
        Self {
            valid: true,
            reported_version: None,
            findings: Vec::new(),
        }
    }

    /// A failed validation with one reason.
    #[must_use]
    pub fn invalid(reason: impl Into<String>) -> Self {
        Self {
            valid: false,
            reported_version: None,
            findings: vec![reason.into()],
        }
    }
}
