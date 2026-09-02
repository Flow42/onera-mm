//! Dependency ingestion.
//!
//! Nexus answers "what does this version need?" across three endpoints, and the
//! difference between them is the whole point of this module:
//!
//! | Endpoint | What it is authoritative about |
//! | --- | --- |
//! | `GET /mod-file-versions/{id}/dependencies` | whether the version declares anything at all |
//! | `POST …/dependencies/ranges/materialized/batch` | which concrete versions currently satisfy a declaration |
//! | `POST /mod-file-versions/batch` | display identity of a candidate |
//!
//! The materialized batch is the efficient one — one request covers a whole
//! profile — but it is **not** authoritative about absence: the specification
//! says a source with no resolvable candidates simply contributes no rows. A
//! missing row therefore means "no candidate, or no declaration, or the resolver
//! had nothing to say", which is exactly the ambiguity Onera refuses to collapse.
//! So the raw endpoint is asked first, per source, and it decides between:
//!
//! - it declares nothing → `Fetched` with no groups, and
//!   `declares_no_dependencies()` is true;
//! - it declares something the batch could not resolve → a group with no
//!   candidates, which the solver reports as unsatisfiable; and
//! - we could not ask → `Unavailable`, never an empty group list.
//!
//! Nothing here parses a version string. Ordering within an update chain comes
//! from the provider's own decimal `position`, and the four Nexus identifiers —
//! version id, mod file (update chain) id, source version id and position — are
//! carried into distinct domain fields.

use crate::client::NexusClient;
use crate::models::{
    parse_position, MaybeEnvelope, PagedEnvelope, WireCandidatesBatchRequest,
    WireCandidatesBatchResponse, WireCategory, WireDependenciesResponse, WireDependencyCandidate,
    WireModFileVersionDetail, WireModStatus, WireVersionDetailsBatchRequest,
    WireVersionDetailsBatchResponse,
};
use chrono::{DateTime, Utc};
use onera_core::domain::dependency::{
    CandidateStatus, DependencyCandidate, DependencyGroup, DependencySnapshot, DependencySource,
    DlcRequirement, RequirementKind,
};
use onera_core::ids::{
    DependencyGroupId, ProviderFileGroupId, ProviderFileId, ProviderId, ProviderModId,
    ProviderVersionId, StoreDlcId,
};
use onera_core::progress::CancelToken;
use onera_core::{CoreError, Result};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Path of the non-deprecated materialized batch endpoint.
///
/// The deprecated twin at `/mod-file-versions/dependencies/materialized/batch`
/// takes the same request and returns the same rows; Onera does not call it.
pub const MATERIALIZED_BATCH_PATH: &str =
    "/mod-file-versions/dependencies/ranges/materialized/batch";
/// Path of the version-detail batch endpoint used to hydrate candidates.
pub const VERSION_DETAILS_BATCH_PATH: &str = "/mod-file-versions/batch";

/// Whether a failure must abort the whole call rather than mark one source.
///
/// The port draws this line: a lost credential or a cancelled operation is not a
/// property of one mod, so reporting it as "this mod's dependencies are
/// unavailable" for every source in the profile would be a lie that also hides
/// the real problem.
fn is_fatal(error: &CoreError) -> bool {
    matches!(
        error,
        CoreError::Unauthenticated { .. } | CoreError::Cancelled
    )
}

/// What the target of a declaration is, joined in from the raw definitions.
///
/// The materialized rows identify a candidate's mod by a composite id and say
/// nothing about its game. The raw definitions name both, keyed by the same mod
/// file id, so this is where a candidate's game slug and page id come from. A
/// candidate whose game cannot be established stays [`CandidateStatus::Unknown`]
/// rather than being assumed to belong to the source's game.
#[derive(Debug, Clone, Default)]
struct TargetInfo {
    game_slug: Option<String>,
    provider_mod_id: Option<String>,
    mod_name: Option<String>,
    file_name: Option<String>,
}

impl TargetInfo {
    fn label(&self) -> Option<String> {
        match (&self.mod_name, &self.file_name) {
            (Some(m), Some(f)) if m != f => Some(format!("{m} — {f}")),
            (Some(m), _) => Some(m.clone()),
            (None, Some(f)) => Some(f.clone()),
            (None, None) => None,
        }
    }
}

/// One source's raw declaration, plus the JSON it came in.
struct RawDeclaration {
    parsed: WireDependenciesResponse,
    json: serde_json::Value,
}

/// Everything gathered for one source version id.
enum SourceData {
    /// Raw declaration read; candidate rows resolved (possibly none).
    Resolved {
        raw: RawDeclaration,
        rows: Vec<WireDependencyCandidate>,
    },
    /// Nothing believable to report for this source.
    Unavailable(String),
}

impl NexusClient {
    /// Provider-neutral dependency definitions for a set of versions.
    ///
    /// Returns exactly one snapshot per requested source, in request order.
    ///
    /// # Errors
    /// Fails only on authentication failure and cancellation; every other
    /// failure becomes an `Unavailable` snapshot for the affected sources.
    pub(crate) async fn fetch_dependencies(
        &self,
        sources: &[DependencySource],
        cancel: &CancelToken,
    ) -> Result<Vec<DependencySnapshot>> {
        let limits = self.dependency_limits();
        let fetched_at = Utc::now();

        // Distinct source version ids, in request order: a profile can hold the
        // same version twice, and asking twice would double the request budget.
        let mut version_ids: Vec<String> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for source in sources {
            if let Some(id) = &source.provider_version_id {
                if seen.insert(id.as_str()) {
                    version_ids.push(id.as_str().to_owned());
                }
            }
        }

        // Step 1: the authoritative per-source declaration.
        let mut declarations: HashMap<String, std::result::Result<RawDeclaration, String>> =
            HashMap::new();
        for id in &version_ids {
            cancel.check()?;
            match self.raw_declaration(id, cancel).await {
                Ok(raw) => {
                    declarations.insert(id.clone(), Ok(raw));
                }
                Err(error) if is_fatal(&error) => return Err(error),
                Err(error) => {
                    declarations.insert(id.clone(), Err(format!("{error}")));
                }
            }
        }

        // Step 2: materialize, but only for sources that actually declared
        // something. A source that declared nothing needs no candidates, and
        // asking for them would spend request budget to learn nothing.
        let needs_candidates: Vec<String> = version_ids
            .iter()
            .filter(|id| {
                declarations
                    .get(*id)
                    .and_then(|d| d.as_ref().ok())
                    .is_some_and(|raw| !raw.parsed.dependency_definitions.is_empty())
            })
            .cloned()
            .collect();

        let mut rows_by_source: HashMap<String, Vec<WireDependencyCandidate>> = HashMap::new();
        let mut chunk_failures: HashMap<String, String> = HashMap::new();
        for chunk in needs_candidates.chunks(limits.max_sources_per_request) {
            cancel.check()?;
            match self.materialized_chunk(chunk, cancel).await {
                Ok(rows) => {
                    for row in rows {
                        rows_by_source
                            .entry(row.source_version_id.clone())
                            .or_default()
                            .push(row);
                    }
                }
                Err(error) if is_fatal(&error) => return Err(error),
                Err(error) => {
                    let reason = format!("{error}");
                    for id in chunk {
                        chunk_failures.insert(id.clone(), reason.clone());
                    }
                }
            }
        }

        // Step 3: hydrate candidate identities. Purely cosmetic, so a failure
        // degrades the labels instead of the answer.
        let details = self
            .hydrate_candidates(&rows_by_source, &needs_candidates, cancel)
            .await?;

        // Step 4: join the raw declarations, the rows and the hydration.
        let targets = target_index(&declarations);
        let mut per_source: HashMap<&str, SourceData> = HashMap::new();
        for id in &version_ids {
            let data = match declarations.remove(id) {
                Some(Err(reason)) => SourceData::Unavailable(reason),
                Some(Ok(raw)) => match chunk_failures.get(id) {
                    Some(reason) => SourceData::Unavailable(reason.clone()),
                    None => SourceData::Resolved {
                        raw,
                        rows: rows_by_source.remove(id).unwrap_or_default(),
                    },
                },
                None => SourceData::Unavailable("no dependency data was requested".to_owned()),
            };
            per_source.insert(id.as_str(), data);
        }

        Ok(sources
            .iter()
            .map(|source| {
                let Some(version) = source.provider_version_id.as_ref() else {
                    return DependencySnapshot::unavailable(
                        source.clone(),
                        "Nexus resolves dependencies against a mod file version id, which this member does not have yet",
                        fetched_at,
                    );
                };
                match per_source.get(version.as_str()) {
                    Some(SourceData::Resolved { raw, rows }) => {
                        build_snapshot(source, raw, rows, &targets, &details, fetched_at)
                    }
                    Some(SourceData::Unavailable(reason)) => {
                        DependencySnapshot::unavailable(source.clone(), reason.clone(), fetched_at)
                    }
                    None => DependencySnapshot::unavailable(
                        source.clone(),
                        "no dependency data was returned for this version",
                        fetched_at,
                    ),
                }
            })
            .collect())
    }

    /// The authoritative raw declaration for one version.
    ///
    /// Kept as JSON as well as parsed: the snapshot carries the provider's own
    /// bytes for diagnostics, and the database stream needs something it can
    /// store without this crate's types.
    async fn raw_declaration(
        &self,
        version_id: &str,
        cancel: &CancelToken,
    ) -> Result<RawDeclaration> {
        let url = self.v3_url(&format!(
            "/mod-file-versions/{}/dependencies",
            crate::client::urlencode(version_id)
        ));
        let json: serde_json::Value = self.get_json(&url, cancel).await?;
        let parsed: MaybeEnvelope<WireDependenciesResponse> = serde_json::from_value(json.clone())
            .map_err(|e| CoreError::Provider(format!("unreadable dependency declaration: {e}")))?;
        Ok(RawDeclaration {
            parsed: parsed.into_inner(),
            json,
        })
    }

    /// Every candidate row for one chunk of source version ids.
    ///
    /// Pagination stops on an empty page, on the reported total, or on a short
    /// page; it never trusts the server to eventually say "no more", which is
    /// what the page and row ceilings are for.
    async fn materialized_chunk(
        &self,
        chunk: &[String],
        cancel: &CancelToken,
    ) -> Result<Vec<WireDependencyCandidate>> {
        let limits = self.dependency_limits();
        let url = self.v3_url(MATERIALIZED_BATCH_PATH);
        let mut rows: Vec<WireDependencyCandidate> = Vec::new();
        let mut page = 1_u32;
        loop {
            cancel.check()?;
            let request = WireCandidatesBatchRequest {
                version_ids: chunk,
                page,
                page_size: limits.page_size,
            };
            let response: PagedEnvelope<WireCandidatesBatchResponse> =
                self.post_json(&url, &request, cancel).await?;
            let returned = response.data.candidates.len();
            if rows.len().saturating_add(returned) > limits.max_rows {
                return Err(CoreError::Unsupported(format!(
                    "Nexus returned more than {} dependency candidate rows for {} versions",
                    limits.max_rows,
                    chunk.len()
                )));
            }
            rows.extend(response.data.candidates);
            if returned == 0 {
                return Ok(rows);
            }
            let more = response.meta.map_or_else(
                || u32::try_from(returned).unwrap_or(u32::MAX) >= limits.page_size,
                |meta| meta.has_more(),
            );
            if !more {
                return Ok(rows);
            }
            page = page.saturating_add(1);
            if page > limits.max_pages {
                return Err(CoreError::Unsupported(format!(
                    "Nexus kept offering dependency pages past the {}-page limit",
                    limits.max_pages
                )));
            }
        }
    }

    /// Display identities for the candidates, keyed by version id.
    ///
    /// Hydration only supplies labels, so a failure here is logged and dropped:
    /// a nameless candidate is still a correct candidate, and refusing the whole
    /// answer over a cosmetic call would be the wrong trade.
    async fn hydrate_candidates(
        &self,
        rows_by_source: &HashMap<String, Vec<WireDependencyCandidate>>,
        source_order: &[String],
        cancel: &CancelToken,
    ) -> Result<HashMap<String, WireModFileVersionDetail>> {
        let limits = self.dependency_limits();
        // Deterministic order, so the same profile always produces the same
        // requests and the same test fixtures match.
        let mut wanted: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for source in source_order {
            for row in rows_by_source.get(source).into_iter().flatten() {
                if seen.insert(row.version_id.clone()) {
                    wanted.push(row.version_id.clone());
                }
            }
        }
        let mut out: HashMap<String, WireModFileVersionDetail> = HashMap::new();
        if wanted.is_empty() {
            return Ok(out);
        }
        let url = self.v3_url(VERSION_DETAILS_BATCH_PATH);
        for chunk in wanted.chunks(limits.max_details_per_request) {
            cancel.check()?;
            let request = WireVersionDetailsBatchRequest { version_ids: chunk };
            let response: Result<MaybeEnvelope<WireVersionDetailsBatchResponse>> =
                self.post_json(&url, &request, cancel).await;
            match response {
                Ok(envelope) => {
                    for detail in envelope.into_inner().versions {
                        out.insert(detail.id.clone(), detail);
                    }
                }
                Err(error) if is_fatal(&error) => return Err(error),
                Err(error) => {
                    tracing::debug!(
                        %error,
                        "nexus candidate hydration failed; candidates keep their identifiers but lose their labels"
                    );
                }
            }
        }
        Ok(out)
    }
}

/// Map each mod file id named by a raw declaration to what is known about it.
fn target_index(
    declarations: &HashMap<String, std::result::Result<RawDeclaration, String>>,
) -> BTreeMap<String, TargetInfo> {
    let mut index: BTreeMap<String, TargetInfo> = BTreeMap::new();
    for raw in declarations.values().filter_map(|d| d.as_ref().ok()) {
        for definition in &raw.parsed.dependency_definitions {
            for range in &definition.ranges {
                let Some(file) = &range.target_mod_file else {
                    continue;
                };
                let entry = index.entry(file.id.clone()).or_default();
                if entry.file_name.is_none() {
                    entry.file_name.clone_from(&file.name);
                }
                if let Some(owner) = &file.owning_mod {
                    if entry.mod_name.is_none() {
                        entry.mod_name.clone_from(&owner.name);
                    }
                    if entry.provider_mod_id.is_none() {
                        entry.provider_mod_id.clone_from(&owner.game_scoped_id);
                    }
                    if entry.game_slug.is_none() {
                        entry.game_slug = owner
                            .game
                            .as_ref()
                            .and_then(|game| game.domain_name.clone());
                    }
                }
            }
        }
    }
    index
}

/// Whether a candidate may be selected.
///
/// Two independent signals have to agree. The mod's visibility decides whether
/// the artifact can be obtained at all; the file category demotes a version the
/// author withdrew or Nexus archived even on a published mod. An unfamiliar
/// value on either — a status from a future API, a candidate whose game could
/// not be established — resolves to [`CandidateStatus::Unknown`], which the
/// solver never selects.
fn candidate_status(
    status: WireModStatus,
    category: WireCategory,
    game_known: bool,
) -> CandidateStatus {
    if !game_known {
        return CandidateStatus::Unknown;
    }
    match status {
        WireModStatus::Published => match category {
            WireCategory::Removed => CandidateStatus::Removed,
            WireCategory::Archived => CandidateStatus::Hidden,
            _ => CandidateStatus::Available,
        },
        WireModStatus::Hidden | WireModStatus::NotPublished | WireModStatus::UnderModeration => {
            CandidateStatus::Hidden
        }
        WireModStatus::Removed | WireModStatus::RemovedByStaff => CandidateStatus::Removed,
        WireModStatus::Unknown => CandidateStatus::Unknown,
    }
}

/// Turn one source's raw declaration and candidate rows into a snapshot.
fn build_snapshot(
    source: &DependencySource,
    raw: &RawDeclaration,
    rows: &[WireDependencyCandidate],
    targets: &BTreeMap<String, TargetInfo>,
    details: &HashMap<String, WireModFileVersionDetail>,
    fetched_at: DateTime<Utc>,
) -> DependencySnapshot {
    let mut by_definition: HashMap<&str, Vec<&WireDependencyCandidate>> = HashMap::new();
    for row in rows {
        by_definition
            .entry(row.definition_id.as_str())
            .or_default()
            .push(row);
    }

    let groups: Vec<DependencyGroup> = raw
        .parsed
        .dependency_definitions
        .iter()
        .map(|definition| {
            let label = definition
                .ranges
                .iter()
                .filter_map(|range| range.target_mod_file.as_ref())
                .find_map(|file| targets.get(&file.id).and_then(TargetInfo::label));
            let mut candidates: Vec<DependencyCandidate> = by_definition
                .get(definition.id.as_str())
                .into_iter()
                .flatten()
                .map(|row| map_candidate(row, targets, details))
                .collect();
            // Deterministic output: chain, then newest position first, then the
            // provider's own version id as a total-order tie-breaker.
            candidates.sort_by(|a, b| {
                a.provider_file_group_id
                    .cmp(&b.provider_file_group_id)
                    .then(b.position.cmp(&a.position))
                    .then(a.provider_version_id.cmp(&b.provider_version_id))
            });
            DependencyGroup {
                id: DependencyGroupId::new(),
                provider_group_key: Some(definition.id.clone()),
                label,
                // Nexus states no strength on a dependency definition. Everything
                // it declares is a requirement; Onera does not invent a
                // "recommended" or "incompatible" edge it was never told about.
                kind: RequirementKind::Required,
                candidates,
            }
        })
        .collect();

    let dlc: Vec<DlcRequirement> = raw
        .parsed
        .dlc_dependency_definitions
        .iter()
        .map(|definition| DlcRequirement {
            id: DependencyGroupId::new(),
            label: definition
                .dlc_targets
                .iter()
                .find_map(|target| target.name.clone()),
            alternatives: definition
                .dlc_targets
                .iter()
                .map(|target| StoreDlcId::new(&target.dlc_id))
                .collect(),
        })
        .collect();

    let mut snapshot = DependencySnapshot::fetched(source.clone(), groups, dlc, fetched_at);
    snapshot.raw = serde_json::json!({
        "endpoints": {
            "declaration": "GET /mod-file-versions/{id}/dependencies",
            "materialized": format!("POST {MATERIALIZED_BATCH_PATH}"),
        },
        "declaration": raw.json,
        "materialized_candidates": rows,
    });
    snapshot
}

/// Map one materialized row onto a provider-neutral candidate.
fn map_candidate(
    row: &WireDependencyCandidate,
    targets: &BTreeMap<String, TargetInfo>,
    details: &HashMap<String, WireModFileVersionDetail>,
) -> DependencyCandidate {
    let target = targets.get(&row.mod_file_id);
    let game_slug = target.and_then(|t| t.game_slug.clone());
    let detail = details.get(&row.version_id);
    let display_name = detail
        .and_then(|d| match (&d.name, &d.version) {
            (Some(name), Some(version))
                if !version.is_empty() && !name.contains(version.as_str()) =>
            {
                Some(format!("{name} ({version})"))
            }
            (Some(name), _) => Some(name.clone()),
            (None, Some(version)) => Some(version.clone()),
            (None, None) => None,
        })
        .or_else(|| target.and_then(TargetInfo::label));
    // The row's position wins; hydration only fills in what the row omitted.
    let position = row
        .position
        .as_deref()
        .and_then(parse_position)
        .or_else(|| {
            detail
                .and_then(|d| d.position.as_deref())
                .and_then(parse_position)
        });
    DependencyCandidate {
        provider: ProviderId::nexus(),
        // An unknown game is stored as an empty slug and paired with
        // `CandidateStatus::Unknown`: `is_selectable_for` then rejects it for
        // every game, which is the safe reading of "we could not tell".
        game_slug: game_slug.clone().unwrap_or_default(),
        provider_mod_id: ProviderModId::new(
            target
                .and_then(|t| t.provider_mod_id.clone())
                .or_else(|| row.mod_id.clone())
                .or_else(|| detail.and_then(|d| d.mod_id.clone()))
                .unwrap_or_default(),
        ),
        // In this adapter a downloadable file *is* a mod file version, so both
        // fields carry the same Nexus id — but they stay separate fields,
        // because a provider where they differ must not have to change the core.
        provider_file_id: Some(ProviderFileId::new(&row.version_id)),
        provider_version_id: Some(ProviderVersionId::new(&row.version_id)),
        provider_file_group_id: Some(ProviderFileGroupId::new(&row.mod_file_id)),
        position,
        status: candidate_status(row.mod_status, row.category, game_slug.is_some()),
        display_name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_published_mod_with_a_live_category_is_selectable() {
        assert_eq!(
            candidate_status(WireModStatus::Published, WireCategory::Main, true),
            CandidateStatus::Available
        );
        // A withdrawn or archived file on a published mod is not an option.
        assert_eq!(
            candidate_status(WireModStatus::Published, WireCategory::Removed, true),
            CandidateStatus::Removed
        );
        assert_eq!(
            candidate_status(WireModStatus::Published, WireCategory::Archived, true),
            CandidateStatus::Hidden
        );
        for hidden in [
            WireModStatus::Hidden,
            WireModStatus::NotPublished,
            WireModStatus::UnderModeration,
        ] {
            assert_eq!(
                candidate_status(hidden, WireCategory::Main, true),
                CandidateStatus::Hidden
            );
        }
        for removed in [WireModStatus::Removed, WireModStatus::RemovedByStaff] {
            assert_eq!(
                candidate_status(removed, WireCategory::Main, true),
                CandidateStatus::Removed
            );
        }
    }

    #[test]
    fn an_unreadable_status_or_game_is_never_guessed_to_be_installable() {
        assert_eq!(
            candidate_status(WireModStatus::Unknown, WireCategory::Main, true),
            CandidateStatus::Unknown
        );
        // Even a perfectly published candidate is unselectable while we cannot
        // tell which game it targets.
        assert_eq!(
            candidate_status(WireModStatus::Published, WireCategory::Main, false),
            CandidateStatus::Unknown
        );
        // An unfamiliar category on a published mod is not a demotion by itself.
        assert_eq!(
            candidate_status(WireModStatus::Published, WireCategory::Unknown, true),
            CandidateStatus::Available
        );
    }

    #[test]
    fn only_credentials_and_cancellation_abort_the_whole_call() {
        assert!(is_fatal(&CoreError::Cancelled));
        assert!(is_fatal(&CoreError::Unauthenticated {
            provider: "nexus".to_owned()
        }));
        for per_source in [
            CoreError::Provider("boom".to_owned()),
            CoreError::NotFound {
                kind: "nexus resource",
                id: "1".to_owned(),
            },
            CoreError::RateLimited {
                provider: "nexus".to_owned(),
                retry_after_secs: 1,
            },
        ] {
            assert!(!is_fatal(&per_source), "{per_source:?}");
        }
    }

    #[test]
    fn a_label_prefers_the_mod_name_and_falls_back_to_the_file() {
        let both = TargetInfo {
            mod_name: Some("SKSE".to_owned()),
            file_name: Some("Main".to_owned()),
            ..TargetInfo::default()
        };
        assert_eq!(both.label().as_deref(), Some("SKSE — Main"));
        let same = TargetInfo {
            mod_name: Some("SKSE".to_owned()),
            file_name: Some("SKSE".to_owned()),
            ..TargetInfo::default()
        };
        assert_eq!(same.label().as_deref(), Some("SKSE"));
        assert_eq!(TargetInfo::default().label(), None);
    }
}
