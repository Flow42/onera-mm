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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
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

// ---------------------------------------------------------------------------
// Dependency wire types
// ---------------------------------------------------------------------------

/// A response that may or may not be wrapped in a `data` envelope.
///
/// The v3 specification wraps some payloads and returns others bare — the batch
/// dependency endpoints wrap, the raw per-version dependency endpoint does not.
/// Accepting either shape means a future change of mind on one endpoint cannot
/// turn a working dependency check into an "unavailable".
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum MaybeEnvelope<T> {
    /// `{ "data": … }`.
    Wrapped {
        /// The payload.
        data: T,
    },
    /// The payload itself.
    Bare(T),
}

impl<T> MaybeEnvelope<T> {
    /// The payload, however it arrived.
    pub fn into_inner(self) -> T {
        match self {
            Self::Wrapped { data } | Self::Bare(data) => data,
        }
    }
}

/// A paged batch response: `{ "data": …, "meta": { … } }`.
#[derive(Debug, Clone, Deserialize)]
pub struct PagedEnvelope<T> {
    /// The payload.
    pub data: T,
    /// Pagination metadata. Absent metadata stops pagination rather than
    /// looping forever.
    #[serde(default)]
    pub meta: Option<WirePagination>,
}

/// The effective visibility status of a mod.
///
/// Anything unfamiliar deserializes as [`WireModStatus::Unknown`], which maps to
/// a non-selectable candidate: a status this build cannot interpret is never
/// assumed to be installable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WireModStatus {
    /// Visible and downloadable.
    Published,
    /// Not yet published by the author.
    NotPublished,
    /// Hidden by the author.
    Hidden,
    /// Under moderation.
    UnderModeration,
    /// Removed by the author.
    Removed,
    /// Removed by Nexus staff.
    RemovedByStaff,
    /// A status this build does not know about.
    #[serde(other)]
    #[default]
    Unknown,
}

/// A minimal game, as the dependency endpoints embed it.
#[derive(Debug, Clone, Deserialize)]
pub struct WireMinimalGame {
    /// Identifier.
    #[serde(default)]
    pub id: Option<String>,
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
    /// URL slug. This is what Onera keys candidates on: a candidate for another
    /// game is never a valid selection.
    #[serde(default)]
    pub domain_name: Option<String>,
}

/// A minimal mod, as the dependency endpoints embed it.
#[derive(Debug, Clone, Deserialize)]
pub struct WireMinimalMod {
    /// Global identifier.
    #[serde(default)]
    pub id: Option<String>,
    /// Game-scoped identifier — the number in the mod's URL, and the id every
    /// other Onera call uses.
    #[serde(default)]
    pub game_scoped_id: Option<String>,
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
    /// The game the mod belongs to.
    #[serde(default)]
    pub game: Option<WireMinimalGame>,
    /// Visibility status, when the endpoint reports one.
    #[serde(default)]
    pub status: Option<WireModStatus>,
}

/// A mod file together with the mod that owns it.
#[derive(Debug, Clone, Deserialize)]
pub struct WireModFileWithMod {
    /// Mod file (update chain) identifier.
    pub id: String,
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
    /// The owning mod. `mod` is a Rust keyword, hence the rename.
    #[serde(rename = "mod", default)]
    pub owning_mod: Option<WireMinimalMod>,
}

/// One authored version range inside a dependency definition.
#[derive(Debug, Clone, Deserialize)]
pub struct WireDependencyRange {
    /// Range identifier.
    #[serde(default)]
    pub id: Option<String>,
    /// The mod file the range points at.
    #[serde(default)]
    pub target_mod_file: Option<WireModFileWithMod>,
    /// Inclusive lower bound. Never parsed: it is only ever used to name the
    /// requirement, because the materialized endpoint resolves the bounds.
    #[serde(default)]
    pub min_version: Option<WireModFileVersion>,
    /// Inclusive upper bound, absent when the range is open-ended.
    #[serde(default)]
    pub max_version: Option<WireModFileVersion>,
}

/// One authored dependency definition: an AND term with OR ranges inside it.
#[derive(Debug, Clone, Deserialize)]
pub struct WireDependencyDefinition {
    /// Definition identifier. Materialized candidate rows carry it back.
    pub id: String,
    /// The authored ranges.
    #[serde(default)]
    pub ranges: Vec<WireDependencyRange>,
}

/// One DLC alternative inside a DLC dependency definition.
#[derive(Debug, Clone, Deserialize)]
pub struct WireDlcTarget {
    /// Target identifier.
    #[serde(default)]
    pub id: Option<String>,
    /// The DLC identifier, scoped to the game.
    pub dlc_id: String,
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
}

/// One DLC dependency definition: an AND term with OR alternatives inside it.
#[derive(Debug, Clone, Deserialize)]
pub struct WireDlcDependencyDefinition {
    /// Definition identifier.
    pub id: String,
    /// The DLCs that satisfy the definition.
    #[serde(default)]
    pub dlc_targets: Vec<WireDlcTarget>,
}

/// `GET /mod-file-versions/{id}/dependencies`
///
/// The authoritative answer to "does this version declare anything at all?".
/// Both arrays present and empty is a positive statement that it declares
/// nothing; the materialized batch alone can never say that, because a source
/// with no resolvable candidates simply contributes no rows.
#[derive(Debug, Clone, Deserialize)]
pub struct WireDependenciesResponse {
    /// Authored version-range definitions.
    #[serde(default)]
    pub dependency_definitions: Vec<WireDependencyDefinition>,
    /// Authored DLC definitions.
    #[serde(default)]
    pub dlc_dependency_definitions: Vec<WireDlcDependencyDefinition>,
}

/// `POST /mod-file-versions/dependencies/ranges/materialized/batch` request.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WireCandidatesBatchRequest<'a> {
    /// Source mod file version ids, already chunked to the documented limit.
    pub version_ids: &'a [String],
    /// 1-indexed page.
    pub page: u32,
    /// Rows per page.
    pub page_size: u32,
}

/// One materialized dependency candidate row.
///
/// The four identifiers are deliberately kept apart: `version_id` is the
/// candidate's version identity, `mod_file_id` is its update chain,
/// `source_version_id` is the version that asked, and `position` orders versions
/// *within* a chain. Collapsing any two of them would make the solver select the
/// wrong artifact.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct WireDependencyCandidate {
    /// The requesting (installed/enabled) mod file version id.
    pub source_version_id: String,
    /// The dependency definition. Rows sharing it are OR-alternatives.
    pub definition_id: String,
    /// The mod file (update group/chain) the candidate belongs to.
    pub mod_file_id: String,
    /// The candidate mod file version id.
    pub version_id: String,
    /// Position within the mod file, as a decimal string. Higher is newer.
    #[serde(default)]
    pub position: Option<String>,
    /// The candidate's file category.
    #[serde(default)]
    pub category: WireCategory,
    /// The owning mod's visibility status.
    #[serde(default)]
    pub mod_status: WireModStatus,
    /// The owning mod's composite identifier.
    #[serde(default)]
    pub mod_id: Option<String>,
}

/// `POST …/materialized/batch` response payload.
#[derive(Debug, Clone, Deserialize)]
pub struct WireCandidatesBatchResponse {
    /// Candidate rows for this page.
    #[serde(default)]
    pub candidates: Vec<WireDependencyCandidate>,
}

/// `POST /mod-file-versions/batch` request.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WireVersionDetailsBatchRequest<'a> {
    /// Version ids to hydrate, already chunked to the documented limit.
    pub version_ids: &'a [String],
}

/// One hydrated version identity.
#[derive(Debug, Clone, Deserialize)]
pub struct WireModFileVersionDetail {
    /// The mod file version id.
    pub id: String,
    /// The owning mod's composite identifier.
    #[serde(default)]
    pub mod_id: Option<String>,
    /// The owning mod file (update chain).
    #[serde(default)]
    pub mod_file_id: Option<String>,
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
    /// Version string, stored verbatim and never parsed.
    #[serde(default)]
    pub version: Option<String>,
    /// Position within the mod file, as a decimal string.
    #[serde(default)]
    pub position: Option<String>,
}

/// `POST /mod-file-versions/batch` response payload.
#[derive(Debug, Clone, Deserialize)]
pub struct WireVersionDetailsBatchResponse {
    /// Hydrated rows. Unknown ids simply contribute none.
    #[serde(default)]
    pub versions: Vec<WireModFileVersionDetail>,
}

/// Longest decimal position string Onera will look at.
///
/// A megabyte of digits is not a position; refusing to parse one keeps a hostile
/// response from turning into an unbounded allocation.
const MAX_POSITION_LEN: usize = 40;

/// Scale applied to a decimal position so it fits [`i64`] ordering.
const POSITION_SCALE: i128 = 1_000_000;

/// Parse a decimal position string into a scaled integer.
///
/// Nexus reports positions as decimal strings (`"3"`, `"3.5"`) so a version can
/// be inserted between two others. Onera's domain carries an [`i64`], so the
/// value is scaled by a million: ordering within the representable range is
/// preserved, which is the only property the solver uses. Anything that is not a
/// plain decimal — an exponent, a NaN, whitespace, an absurd length — yields
/// `None`, and an unordered candidate is honest about being unordered.
#[must_use]
pub fn parse_position(raw: &str) -> Option<i64> {
    if raw.is_empty() || raw.len() > MAX_POSITION_LEN {
        return None;
    }
    let (negative, digits) = match raw.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, raw.strip_prefix('+').unwrap_or(raw)),
    };
    let (int_part, frac_part) = match digits.split_once('.') {
        Some((i, f)) => (i, f),
        None => (digits, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    if !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    // Saturating rather than wrapping: a position beyond i64 range clamps to the
    // extreme, which keeps ordering monotonic instead of inverting it.
    let mut scaled: i128 = 0;
    for byte in int_part.bytes() {
        scaled = scaled
            .saturating_mul(10)
            .saturating_add(i128::from(byte - b'0'));
        if scaled > i128::from(i64::MAX) {
            scaled = i128::from(i64::MAX);
            break;
        }
    }
    scaled = scaled.saturating_mul(POSITION_SCALE);
    let mut fraction: i128 = 0;
    let mut divisor: i128 = POSITION_SCALE;
    for byte in frac_part.bytes().take(6) {
        divisor /= 10;
        fraction += i128::from(byte - b'0') * divisor;
    }
    scaled = scaled.saturating_add(fraction);
    if negative {
        scaled = -scaled;
    }
    Some(scaled.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
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

    #[test]
    fn an_unknown_mod_status_is_not_a_deserialization_failure() {
        let c: WireDependencyCandidate = serde_json::from_str(
            r#"{"source_version_id":"1","definition_id":"d","mod_file_id":"f",
                "version_id":"v","mod_status":"quarantined_by_the_future"}"#,
        )
        .unwrap();
        assert_eq!(c.mod_status, WireModStatus::Unknown);
        assert_eq!(c.category, WireCategory::Unknown);
        assert_eq!(c.position, None);
    }

    #[test]
    fn a_candidate_row_missing_an_identifier_is_rejected() {
        // Without a version id there is nothing to install; defaulting it would
        // materialize a candidate that cannot exist.
        assert!(serde_json::from_str::<WireDependencyCandidate>(
            r#"{"source_version_id":"1","definition_id":"d","mod_file_id":"f"}"#
        )
        .is_err());
    }

    #[test]
    fn a_payload_is_accepted_wrapped_or_bare() {
        let bare: MaybeEnvelope<WireDependenciesResponse> = serde_json::from_str(
            r#"{"dependency_definitions":[],"dlc_dependency_definitions":[]}"#,
        )
        .unwrap();
        assert!(bare.into_inner().dependency_definitions.is_empty());
        let wrapped: MaybeEnvelope<WireDependenciesResponse> = serde_json::from_str(
            r#"{"data":{"dependency_definitions":[{"id":"d","ranges":[]}],
                "dlc_dependency_definitions":[]}}"#,
        )
        .unwrap();
        assert_eq!(wrapped.into_inner().dependency_definitions.len(), 1);
    }

    #[test]
    fn decimal_positions_keep_their_order() {
        let one = parse_position("1").unwrap();
        let one_and_a_half = parse_position("1.5").unwrap();
        let two = parse_position("2").unwrap();
        assert!(one < one_and_a_half && one_and_a_half < two);
        assert_eq!(parse_position("-1").unwrap(), -one);
        assert_eq!(parse_position("0"), Some(0));
        // Excess precision truncates rather than failing.
        assert_eq!(parse_position("1.0000001"), parse_position("1"));
    }

    #[test]
    fn a_hostile_position_is_refused_rather_than_guessed() {
        for hostile in [
            "",
            " 1",
            "1e309",
            "NaN",
            "inf",
            "1.2.3",
            "--1",
            "0x10",
            ".",
            &"9".repeat(4096),
        ] {
            assert_eq!(parse_position(hostile), None, "{hostile:?}");
        }
        // A position too large for i64 clamps instead of wrapping negative.
        assert_eq!(parse_position(&"9".repeat(30)), Some(i64::MAX));
    }

    #[test]
    fn a_dependency_definition_with_no_ranges_still_deserializes() {
        let d: WireDependencyDefinition = serde_json::from_str(r#"{"id":"d"}"#).unwrap();
        assert!(d.ranges.is_empty());
    }

    #[test]
    fn the_mod_keyword_field_is_read_back() {
        let f: WireModFileWithMod = serde_json::from_str(
            r#"{"id":"7","name":"Main","mod":{"game_scoped_id":"107",
                "game":{"domain_name":"cyberpunk2077"}}}"#,
        )
        .unwrap();
        let owner = f.owning_mod.unwrap();
        assert_eq!(owner.game_scoped_id.as_deref(), Some("107"));
        assert_eq!(
            owner.game.unwrap().domain_name.as_deref(),
            Some("cyberpunk2077")
        );
    }
}
