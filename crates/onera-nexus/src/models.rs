//! Wire types for the Nexus Mods API.
//!
//! Every field that the specification does not mark `required` is optional here,
//! and every enum has an `Unknown` fallback. The API is versioned and additive,
//! so a deserializer that panics on an unfamiliar `category` would break Onera
//! the day Nexus ships a new one.
//!
//! These types are wire representations only. [`crate::client`] converts them
//! into the domain types in [`onera_core::domain`]; they never escape this crate.

use serde::Deserialize;

/// v3 wraps single resources in a `data` envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct Envelope<T> {
    /// The payload.
    pub data: T,
}

/// A game, as the catalogue reports it.
#[derive(Debug, Clone, Deserialize)]
pub struct WireGame {
    /// Numeric or string identifier.
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
    /// URL slug, e.g. `cyberpunk2077`. This is what Onera keys on.
    #[serde(alias = "domain_name")]
    pub domain: String,
}

/// A mod.
#[derive(Debug, Clone, Deserialize)]
pub struct WireMod {
    /// Global identifier.
    pub id: String,
    /// Game-scoped identifier — the number in the mod's URL.
    #[serde(default)]
    pub game_scoped_id: Option<String>,
    /// Name, absent when the mod is hidden or unavailable.
    #[serde(default)]
    pub name: Option<String>,
    /// Author, when the endpoint provides one.
    #[serde(default)]
    pub author: Option<String>,
}

/// A mod file: the persistent slot on a mod page whose versions change.
#[derive(Debug, Clone, Deserialize)]
pub struct WireModFile {
    /// Identifier.
    pub id: String,
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
}

/// One version of a mod file: the thing that is actually downloaded.
#[derive(Debug, Clone, Deserialize)]
pub struct WireModFileVersion {
    /// Identifier.
    pub id: String,
    /// The mod file this version belongs to.
    #[serde(default)]
    pub file: Option<WireModFile>,
    /// Game-scoped identifier.
    #[serde(default)]
    pub game_scoped_id: Option<String>,
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
    /// Version string, stored verbatim and never parsed.
    #[serde(default)]
    pub version: Option<String>,
    /// Category the author assigned.
    #[serde(default)]
    pub category: WireCategory,
    /// Upload timestamp — the only ordering key Onera trusts.
    #[serde(default)]
    pub uploaded_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Size in bytes, when the API reports one.
    #[serde(default, alias = "size_in_bytes", alias = "file_size")]
    pub size: Option<u64>,
    /// Whether this is the mod page's default download.
    #[serde(default)]
    pub is_primary: Option<bool>,
    /// MD5 digest, advisory only.
    #[serde(default, alias = "md5")]
    pub md5_hash: Option<String>,
}

/// File category.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireCategory {
    /// The main download.
    Main,
    /// A patch on top of a main file.
    Update,
    /// An optional extra.
    Optional,
    /// A superseded version.
    OldVersion,
    /// Anything else.
    Miscellaneous,
    /// Withdrawn by the author.
    Removed,
    /// Archived by Nexus.
    Archived,
    /// A category this build does not know about.
    ///
    /// The `other` fallback is what keeps a new Nexus category from breaking
    /// every mod listing.
    #[serde(other)]
    #[default]
    Unknown,
}

/// `GET /mods/{id}/files`
#[derive(Debug, Clone, Deserialize)]
pub struct WireModFilesResponse {
    /// The mod's file slots.
    #[serde(default)]
    pub mod_files: Vec<WireModFile>,
}

/// `GET /mod-files/{id}/versions`
#[derive(Debug, Clone, Deserialize)]
pub struct WireVersionsResponse {
    /// Versions of one mod file.
    #[serde(default)]
    pub versions: Vec<WireModFileVersion>,
}

/// Pagination metadata, when an endpoint provides it.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct WirePagination {
    /// 1-indexed current page.
    #[serde(default)]
    pub page: Option<u32>,
    /// Items per page.
    #[serde(default)]
    pub page_size: Option<u32>,
    /// Items across all pages.
    #[serde(default)]
    pub total_count: Option<u64>,
}

impl WirePagination {
    /// Whether another page exists after this one.
    #[must_use]
    pub fn has_more(&self) -> bool {
        let (Some(page), Some(size), Some(total)) = (self.page, self.page_size, self.total_count)
        else {
            return false;
        };
        if size == 0 {
            return false;
        }
        u64::from(page) * u64::from(size) < total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_category_does_not_break_deserialization() {
        let v: WireModFileVersion =
            serde_json::from_str(r#"{"id":"1","category":"some_category_from_the_future"}"#)
                .unwrap();
        assert_eq!(v.category, WireCategory::Unknown);
    }

    #[test]
    fn every_optional_field_may_be_absent() {
        let v: WireModFileVersion = serde_json::from_str(r#"{"id":"1"}"#).unwrap();
        assert_eq!(v.version, None);
        assert_eq!(v.size, None);
        assert_eq!(v.category, WireCategory::Unknown);
        assert_eq!(v.is_primary, None);
    }

    #[test]
    fn size_accepts_the_documented_aliases() {
        for body in [r#"{"id":"1","size":5}"#, r#"{"id":"1","size_in_bytes":5}"#] {
            let v: WireModFileVersion = serde_json::from_str(body).unwrap();
            assert_eq!(v.size, Some(5), "{body}");
        }
    }

    #[test]
    fn a_missing_required_field_is_an_error_not_a_default() {
        // `id` is required; silently defaulting it would produce a mod file
        // Onera could never download.
        assert!(serde_json::from_str::<WireModFileVersion>(r#"{"version":"1.0"}"#).is_err());
    }

    #[test]
    fn games_accept_both_domain_spellings() {
        for body in [
            r#"{"domain":"cyberpunk2077"}"#,
            r#"{"domain_name":"cyberpunk2077"}"#,
        ] {
            let g: WireGame = serde_json::from_str(body).unwrap();
            assert_eq!(g.domain, "cyberpunk2077");
        }
    }

    #[test]
    fn pagination_detects_the_last_page() {
        let more = WirePagination {
            page: Some(1),
            page_size: Some(20),
            total_count: Some(45),
        };
        assert!(more.has_more());
        let last = WirePagination {
            page: Some(3),
            page_size: Some(20),
            total_count: Some(45),
        };
        assert!(!last.has_more());
        // Missing metadata means "assume there is nothing more" rather than
        // looping forever.
        let unknown = WirePagination {
            page: None,
            page_size: None,
            total_count: None,
        };
        assert!(!unknown.has_more());
        let degenerate = WirePagination {
            page: Some(1),
            page_size: Some(0),
            total_count: Some(9),
        };
        assert!(!degenerate.has_more());
    }

    #[test]
    fn an_envelope_unwraps_its_payload() {
        let e: Envelope<WireMod> =
            serde_json::from_str(r#"{"data":{"id":"7","name":"A mod"}}"#).unwrap();
        assert_eq!(e.data.id, "7");
    }

    #[test]
    fn a_hidden_mod_with_no_name_still_deserializes() {
        let m: WireMod = serde_json::from_str(r#"{"id":"7"}"#).unwrap();
        assert_eq!(m.name, None);
    }
}
