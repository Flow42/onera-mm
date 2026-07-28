//! Mods, releases and provider files.
//!
//! A [`Mod`] is a lineage. A [`Release`] is one published version of it, and a
//! [`ProviderFile`] is one downloadable artifact belonging to a release.
//!
//! Version handling rule: `version` is whatever the provider reported, stored
//! byte for byte. Onera orders releases by `published_at` and by provider file
//! id, never by parsing the version string, and never compares versions across
//! two different [`Mod`]s. Mod authors use mutually incompatible schemes and a
//! cross-mod comparison is meaningless.

use crate::hash::FileHash;
use crate::ids::{ModId, ProviderFileId, ProviderId, ProviderModId, ReleaseId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A mod lineage as tracked by Onera.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mod {
    /// Onera's identifier.
    pub id: ModId,
    /// Provider the mod came from.
    pub provider: ProviderId,
    /// Provider's opaque mod identifier.
    pub provider_mod_id: ProviderModId,
    /// Provider slug of the game this mod targets.
    pub game_slug: String,
    /// Display name at the time we last refreshed metadata.
    pub name: String,
    /// Author as reported by the provider.
    pub author: Option<String>,
}

/// One published version of a [`Mod`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// Onera's identifier.
    pub id: ReleaseId,
    /// The mod lineage this release belongs to.
    pub mod_id: ModId,
    /// The version string exactly as the provider reported it.
    ///
    /// Never parsed, never normalized, never compared across mods.
    pub version: String,
    /// Publication timestamp reported by the provider, the only ordering key
    /// Onera trusts.
    pub published_at: Option<DateTime<Utc>>,
    /// Provider-specific metadata, opaque to the installation domain.
    pub metadata: serde_json::Value,
}

impl Release {
    /// Order two releases *of the same mod*.
    ///
    /// # Panics
    /// Panics if the two releases belong to different mods. Comparing versions
    /// across mods is a programming error, not a runtime condition.
    #[must_use]
    pub fn is_newer_than(&self, other: &Release) -> bool {
        assert_eq!(
            self.mod_id, other.mod_id,
            "refusing to compare releases of different mods; version strings are not comparable across mods"
        );
        match (self.published_at, other.published_at) {
            (Some(a), Some(b)) => a > b,
            // Without timestamps we cannot order, and we will not guess by
            // parsing the version string.
            _ => false,
        }
    }
}

/// Category a provider assigns to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileCategory {
    /// The main download.
    Main,
    /// An update patch on top of a main file.
    Update,
    /// An optional extra.
    Optional,
    /// A superseded version.
    OldVersion,
    /// Anything else the provider offers.
    Miscellaneous,
    /// Category the provider reported but Onera does not model.
    Unknown,
}

/// A downloadable artifact belonging to a [`Release`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderFile {
    /// Provider that hosts the file.
    pub provider: ProviderId,
    /// Provider's opaque file identifier.
    pub provider_file_id: ProviderFileId,
    /// Release this file belongs to.
    pub release_id: ReleaseId,
    /// Filename as published, used for display and for archive-format sniffing.
    pub name: String,
    /// Size in bytes as reported by the provider, when known.
    pub size_bytes: Option<u64>,
    /// Category assigned by the provider.
    pub category: FileCategory,
    /// Digest published by the provider, when there is one.
    ///
    /// Advisory only: Onera always computes its own BLAKE3 hash and uses that
    /// for every integrity decision.
    pub published_hash: Option<FileHash>,
    /// Publication timestamp reported by the provider.
    pub uploaded_at: Option<DateTime<Utc>>,
    /// Whether the provider marks this as the default download.
    pub is_primary: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(mod_id: ModId, version: &str, at: Option<i64>) -> Release {
        Release {
            id: ReleaseId::new(),
            mod_id,
            version: version.to_owned(),
            published_at: at.and_then(|s| DateTime::from_timestamp(s, 0)),
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn orders_releases_by_publication_date() {
        let m = ModId::new();
        let old = release(m, "1.0", Some(1_000));
        let new = release(m, "0.9-beta", Some(2_000));
        // Publication date wins even though the version string looks older.
        assert!(new.is_newer_than(&old));
        assert!(!old.is_newer_than(&new));
    }

    #[test]
    fn refuses_to_order_without_timestamps() {
        let m = ModId::new();
        let a = release(m, "2.0", None);
        let b = release(m, "1.0", Some(1_000));
        assert!(!a.is_newer_than(&b));
        assert!(!b.is_newer_than(&a));
    }

    #[test]
    #[should_panic(expected = "different mods")]
    fn panics_on_cross_mod_comparison() {
        let a = release(ModId::new(), "1.0", Some(1));
        let b = release(ModId::new(), "2.0", Some(2));
        let _ = a.is_newer_than(&b);
    }

    #[test]
    fn version_strings_are_stored_verbatim() {
        let r = release(ModId::new(), "  v1.0-RC1 (final) ", Some(1));
        assert_eq!(r.version, "  v1.0-RC1 (final) ");
    }
}
