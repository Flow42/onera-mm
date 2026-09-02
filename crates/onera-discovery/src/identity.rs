//! Steam build identity read from a local `appmanifest_<appid>.acf`.
//!
//! Steam writes one manifest per installed app into `steamapps/`. It is the only
//! machine-local record of *which build* of a game is on disk, and reading it
//! needs no credentials, no running client and no undocumented service. That is
//! the whole of Onera's Steam identity story: everything here comes out of a
//! file Steam itself wrote next to the game.
//!
//! What that buys, and what it does not, is spelled out in
//! `docs/steam-baseline-assumptions.md`. In short: the AppID and the manifest
//! path are trustworthy, the BuildID, branch and depot identifiers are
//! best-effort, and none of it is an attestation that the bytes on disk match
//! the build named here — only Steam's own *Verify Installed Files* can say
//! that, and only at the moment the user runs it.
//!
//! Two rules are enforced by every parser in this module:
//!
//! * **A field that is absent, empty or malformed becomes `None`.** Never a
//!   default, never a placeholder, never a guess. A `StoreBuildIdentity` with a
//!   missing BuildID compares as
//!   [`BuildIdentityMatch::Unknown`](onera_core::domain::baseline::BuildIdentityMatch::Unknown),
//!   which is the correct answer — "we could not tell" is not "unchanged".
//! * **Identifiers stay opaque strings.** They are compared for equality and
//!   nothing else. Nothing here parses a version, and nothing orders a BuildID.

use crate::vdf;
use chrono::{DateTime, Utc};
use onera_core::domain::baseline::{DepotIdentity, GameStoreKind, StoreBuildIdentity};
use std::path::{Path, PathBuf};

/// Build identity as one `appmanifest_<appid>.acf` records it.
///
/// The `app_id` and `manifest_path` are always present — a manifest that cannot
/// supply them is not parsed at all. Everything else is optional because Steam
/// genuinely omits these keys in real installations: a manifest written before
/// the first successful update has no usable BuildID, a game on the default
/// branch has no beta key, and a depot list can be absent entirely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamBuildIdentity {
    /// Steam application id, cross-checked against the manifest's file name.
    pub app_id: u32,
    /// Absolute path of the `appmanifest_<appid>.acf` the identity came from.
    pub manifest_path: PathBuf,
    /// Steam's `buildid`, verbatim, when it is present and well-formed.
    pub build_id: Option<String>,
    /// Beta/branch key, when the installation is not on the default branch.
    pub branch: Option<String>,
    /// Installed depots that reported both a depot id and a manifest id.
    ///
    /// Sorted and deduplicated so that two reads of the same manifest compare
    /// equal regardless of the order Steam happened to write the entries in.
    pub depots: Vec<DepotIdentity>,
}

impl SteamBuildIdentity {
    /// Convert into the provider-neutral identity the baseline domain stores.
    ///
    /// `observed_at` is supplied by the caller rather than read from the clock
    /// here, so a test can pin it and so one discovery pass stamps every app it
    /// found with the same instant.
    #[must_use]
    pub fn to_store_identity(&self, observed_at: DateTime<Utc>) -> StoreBuildIdentity {
        StoreBuildIdentity {
            store: GameStoreKind::Steam,
            app_id: Some(self.app_id.to_string()),
            build_id: self.build_id.clone(),
            branch: self.branch.clone(),
            depots: self.depots.clone(),
            manifest_path: Some(self.manifest_path.clone()),
            observed_at,
        }
    }

    /// Whether enough was recovered to detect a later build change.
    ///
    /// Mirrors [`StoreBuildIdentity::is_comparable`]: with neither a BuildID nor
    /// a single depot manifest, a future comparison can only answer `Unknown`.
    #[must_use]
    pub fn is_comparable(&self) -> bool {
        self.build_id.is_some() || !self.depots.is_empty()
    }
}

/// The `AppState` object of a parsed manifest, plus where it was read from.
#[derive(Debug, Clone)]
pub struct AppManifest {
    /// Absolute path of the file.
    pub path: PathBuf,
    /// Steam application id, agreed on by the file name and the `appid` key.
    pub app_id: u32,
    /// `installdir`, the game's directory name under `steamapps/common`.
    pub install_dir: String,
    /// `name`, when Steam recorded one.
    pub name: Option<String>,
    /// Build identity recovered from the same file.
    pub identity: SteamBuildIdentity,
}

/// Whether a directory entry is named like an app manifest.
#[must_use]
pub fn is_app_manifest_name(file_name: &str) -> bool {
    app_id_from_file_name(file_name).is_some()
}

/// The AppID encoded in an `appmanifest_<appid>.acf` file name.
///
/// The file name is Steam's own index into `steamapps/`, so it is treated as
/// authoritative and any manifest whose body disagrees is rejected outright
/// rather than guessed at.
#[must_use]
pub fn app_id_from_file_name(file_name: &str) -> Option<u32> {
    let rest = file_name.strip_prefix("appmanifest_")?;
    let digits = rest.strip_suffix(".acf")?;
    parse_app_id(digits)
}

/// Read and parse one `appmanifest_<appid>.acf`.
///
/// Returns `None` for anything Onera cannot trust: an unreadable file, invalid
/// KeyValues, a missing `AppState`, a missing or non-numeric `appid`, an `appid`
/// that contradicts the file name, or a missing `installdir`. Those are the
/// fields Onera *acts* on, so a manifest that cannot supply them is skipped
/// entirely; the optional identity fields are never a reason to skip.
#[must_use]
pub fn read_app_manifest(path: &Path) -> Option<AppManifest> {
    let text = std::fs::read_to_string(path).ok()?;
    let file_name = path.file_name()?.to_str()?;
    parse_app_manifest(path, file_name, &text)
}

/// Parse manifest text that was already read from `path`.
///
/// Split out from [`read_app_manifest`] so the parsing rules can be exercised
/// against fixture strings without a filesystem.
#[must_use]
pub fn parse_app_manifest(path: &Path, file_name: &str, text: &str) -> Option<AppManifest> {
    let named_app_id = app_id_from_file_name(file_name)?;
    let state = vdf::parse(text).ok()?;
    let state = state.get("AppState")?;

    let app_id = parse_app_id(state.string("appid")?)?;
    if app_id != named_app_id {
        // Steam indexes `steamapps/` by file name. A body that names a different
        // app is corrupt or hand-edited; acting on either value would attach a
        // build identity to the wrong game.
        tracing::warn!(
            path = %path.display(),
            named_app_id,
            body_app_id = app_id,
            "appmanifest file name and appid disagree; skipping"
        );
        return None;
    }

    let install_dir = non_empty(state.string("installdir")?)?.to_owned();

    Some(AppManifest {
        path: path.to_path_buf(),
        app_id,
        install_dir,
        name: state.string("name").and_then(non_empty).map(str::to_owned),
        identity: SteamBuildIdentity {
            app_id,
            manifest_path: path.to_path_buf(),
            build_id: parse_build_id(state),
            branch: parse_branch(state),
            depots: parse_depots(state),
        },
    })
}

/// `buildid`, or `None` when it is absent, empty, non-numeric or a placeholder.
///
/// Steam writes `"buildid" "0"` into a manifest it has created but not yet
/// filled in. Keeping that value would be worse than dropping it: two unrelated
/// half-written installations would compare `Same`. It is therefore treated as
/// "not known yet", which is what it means.
fn parse_build_id(state: &vdf::Value) -> Option<String> {
    numeric_identifier(state.string("buildid")?)
}

/// The beta/branch key, preferring the branch whose content is actually mounted.
///
/// Steam records a beta key in up to three places. `MountedConfig` describes the
/// content currently on disk, `UserConfig` describes what the user asked for —
/// and after switching branches but before downloading, those disagree. A
/// baseline describes bytes on disk, so the mounted value wins.
///
/// An empty key means the default branch, which Onera records as "no branch"
/// rather than inventing the string `public`.
fn parse_branch(state: &vdf::Value) -> Option<String> {
    ["MountedConfig", "UserConfig"]
        .iter()
        .filter_map(|section| state.get(section))
        .find_map(|section| section.string("betakey").and_then(non_empty))
        .or_else(|| state.string("betakey").and_then(non_empty))
        .map(str::to_owned)
}

/// Installed depots that reported both identifiers, sorted and deduplicated.
///
/// A depot entry Onera cannot fully read is dropped rather than half-recorded:
/// a `DepotIdentity` with a fabricated manifest id would compare equal to
/// another fabricated one, which is exactly the false "unchanged" this module
/// exists to prevent. Dropping entries can only ever weaken the identity into
/// `Unknown`, never strengthen it into a wrong `Same`.
fn parse_depots(state: &vdf::Value) -> Vec<DepotIdentity> {
    let Some(installed) = state.get("InstalledDepots") else {
        return Vec::new();
    };
    let mut depots: Vec<DepotIdentity> = installed
        .entries()
        .filter_map(|(depot_id, entry)| {
            Some(DepotIdentity {
                depot_id: numeric_identifier(depot_id)?,
                manifest_id: numeric_identifier(entry.string("manifest")?)?,
            })
        })
        .collect();
    depots.sort();
    depots.dedup();
    depots
}

/// An opaque numeric identifier: trimmed, non-empty, all digits, not a placeholder.
///
/// The digit check is a well-formedness test, not a comparison: the value is
/// kept verbatim as a string and only ever compared for equality.
fn numeric_identifier(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "0" || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(trimmed.to_owned())
}

/// Parse an AppID, which Onera does use as a number to match against adapters.
fn parse_app_id(raw: &str) -> Option<u32> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    trimmed.parse().ok()
}

/// A trimmed string, or `None` when there is nothing left.
fn non_empty(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}
