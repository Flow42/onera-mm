//! Provider-declared dependencies.
//!
//! Dependency definitions are **advisory input, not executable authority**. They
//! can block a plan or raise a warning; they can never write a file, pick a
//! conflict winner, or change desired state on their own.
//!
//! The distinction this module exists to protect:
//!
//! | State                                | Meaning                                    |
//! | ------------------------------------ | ------------------------------------------ |
//! | [`DependencyAvailability::Fetched`] with no groups | the provider says this mod needs nothing |
//! | [`DependencyAvailability::Unsupported`] | the provider does not model dependencies |
//! | [`DependencyAvailability::Unavailable`] | it does, but we could not ask right now  |
//! | [`DependencyAvailability::Cached`]   | answered from a stored snapshot, possibly stale |
//!
//! An empty `Vec<DependencyGroup>` therefore never means "no dependencies" on
//! its own. Always ask [`DependencySnapshot::declares_no_dependencies`].
//!
//! Nothing here parses a version string. Candidates are addressed by
//! [`ProviderVersionId`] and ordered by the provider's own
//! [`DependencyCandidate::position`].

use crate::hash::FileHash;
use crate::ids::{
    DependencyGroupId, DependencySnapshotId, ProfileMemberId, ProviderFileGroupId, ProviderFileId,
    ProviderId, ProviderModId, ProviderVersionId, StoreDlcId,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What a provider can tell Onera about dependencies.
///
/// Reported by [`crate::ports::ModProvider::dependency_capability`] before any
/// request is made, so the UI can distinguish "this provider has no such
/// concept" from "we asked and it failed".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DependencyCapability {
    /// The provider does not model dependencies at all.
    Unsupported,
    /// The provider models dependencies.
    Supported {
        /// Whether several versions can be queried in one request. Whole-profile
        /// checks prefer the batch endpoint when it exists.
        batch: bool,
        /// Whether the provider reports store DLC requirements.
        dlc: bool,
    },
}

impl DependencyCapability {
    /// Whether it is worth issuing a dependency request at all.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::Supported { .. })
    }
}

/// Whether a snapshot's contents can be believed, and how much.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DependencyAvailability {
    /// Fetched successfully and completely. Empty groups mean *no dependencies*.
    Fetched,
    /// Served from a stored snapshot rather than the network.
    Cached {
        /// When the cached data was originally fetched.
        fetched_at: DateTime<Utc>,
        /// Whether the cache is past its freshness window.
        ///
        /// Stale cached data is usable and must be labelled; it is never
        /// presented as current.
        stale: bool,
    },
    /// The provider does not model dependencies.
    Unsupported,
    /// The provider models them but did not answer: offline, an error, or an
    /// experimental endpoint that has disappeared.
    Unavailable {
        /// Displayable reason.
        reason: String,
    },
}

impl DependencyAvailability {
    /// Whether the accompanying groups describe the provider's real answer.
    #[must_use]
    pub const fn is_authoritative(&self) -> bool {
        matches!(self, Self::Fetched | Self::Cached { .. })
    }

    /// Whether the data is known to be out of date.
    #[must_use]
    pub const fn is_stale(&self) -> bool {
        matches!(self, Self::Cached { stale: true, .. })
    }
}

/// The provider version a dependency definition belongs to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DependencySource {
    /// Provider that published the definition.
    pub provider: ProviderId,
    /// Provider slug of the game, so a candidate for another game is rejectable.
    pub game_slug: String,
    /// Mod the definition belongs to.
    pub provider_mod_id: ProviderModId,
    /// File the definition belongs to, when the provider is file-scoped.
    pub provider_file_id: Option<ProviderFileId>,
    /// Version identity of that file, when the provider exposes one.
    pub provider_version_id: Option<ProviderVersionId>,
}

/// Whether a candidate can actually be selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateStatus {
    /// Visible and downloadable.
    Available,
    /// Hidden by the author; not selectable.
    Hidden,
    /// Deleted or archived by the provider; not selectable.
    Removed,
    /// The provider did not say. Not selectable: the solver never guesses that
    /// an unknown candidate would work.
    Unknown,
}

impl CandidateStatus {
    /// Whether the solver may select this candidate.
    #[must_use]
    pub const fn is_selectable(self) -> bool {
        matches!(self, Self::Available)
    }
}

/// A concrete artifact that could satisfy a requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyCandidate {
    /// Provider hosting the candidate.
    pub provider: ProviderId,
    /// Provider slug of the game the candidate targets.
    ///
    /// A candidate for a different game is never a valid selection, however
    /// confidently the provider offered it.
    pub game_slug: String,
    /// Mod the candidate belongs to.
    pub provider_mod_id: ProviderModId,
    /// File to install, when the provider materialized one.
    pub provider_file_id: Option<ProviderFileId>,
    /// Version identity of that file.
    pub provider_version_id: Option<ProviderVersionId>,
    /// Group of mutually superseding files the candidate belongs to.
    ///
    /// The solver selects at most one version per group.
    pub provider_file_group_id: Option<ProviderFileGroupId>,
    /// The provider's own ordering position within its file group.
    ///
    /// Higher is newer, as the *provider* defines newer. This is the only
    /// ordering Onera uses; version strings are never compared.
    pub position: Option<i64>,
    /// Whether the candidate can be selected.
    pub status: CandidateStatus,
    /// Display name for the confirmation view.
    pub display_name: Option<String>,
}

impl DependencyCandidate {
    /// Whether this candidate may be selected for a game.
    ///
    /// Both hard constraints in one place: right game, selectable status.
    #[must_use]
    pub fn is_selectable_for(&self, game_slug: &str) -> bool {
        self.status.is_selectable() && self.game_slug == game_slug
    }

    /// Stable textual form used by [`DependencyFingerprint`].
    fn canonical(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.provider,
            self.game_slug,
            self.provider_mod_id,
            self.provider_file_id
                .as_ref()
                .map_or("", ProviderFileId::as_str),
            self.provider_version_id
                .as_ref()
                .map_or("", ProviderVersionId::as_str),
            self.provider_file_group_id
                .as_ref()
                .map_or("", ProviderFileGroupId::as_str),
        )
    }
}

/// How strongly a requirement is stated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementKind {
    /// Must be satisfied, or the plan is blocked.
    Required,
    /// Advisory. Reported, never blocking.
    Recommended,
    /// The named candidates must *not* be selected together with the source.
    Incompatible,
}

impl RequirementKind {
    /// Whether an unsatisfied group of this kind blocks an apply.
    #[must_use]
    pub const fn is_blocking(self) -> bool {
        matches!(self, Self::Required | Self::Incompatible)
    }
}

/// One independent requirement.
///
/// Groups are joined with AND; the candidates within a group are joined with OR.
/// An empty candidate list is a requirement nothing can satisfy — kept rather
/// than dropped, because silently ignoring it would turn an unsatisfiable state
/// into an apparently compatible one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyGroup {
    /// Onera's identifier, stable for the lifetime of a snapshot.
    pub id: DependencyGroupId,
    /// Provider's own identifier for the group, when it has one.
    ///
    /// Part of the fingerprint, so a provider renumbering its groups invalidates
    /// the overrides that referred to them.
    pub provider_group_key: Option<String>,
    /// Displayable label, e.g. the required mod's name.
    pub label: Option<String>,
    /// How strongly it is stated.
    pub kind: RequirementKind,
    /// Alternatives, any one of which satisfies the group.
    pub candidates: Vec<DependencyCandidate>,
}

impl DependencyGroup {
    /// Whether no candidate could ever satisfy this group for a given game.
    #[must_use]
    pub fn is_unsatisfiable(&self, game_slug: &str) -> bool {
        !self
            .candidates
            .iter()
            .any(|c| c.is_selectable_for(game_slug))
    }

    /// Stable textual form used by [`DependencyFingerprint`].
    fn canonical(&self) -> String {
        let mut candidates: Vec<String> = self
            .candidates
            .iter()
            .map(DependencyCandidate::canonical)
            .collect();
        candidates.sort();
        candidates.dedup();
        format!(
            "group\u{1f}{}\u{1f}{:?}\u{1f}{}",
            self.provider_group_key.as_deref().unwrap_or(""),
            self.kind,
            candidates.join("\u{1d}")
        )
    }
}

/// A store-owned extra a mod requires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DlcRequirement {
    /// Onera's identifier for the requirement group.
    pub id: DependencyGroupId,
    /// Displayable label.
    pub label: Option<String>,
    /// Alternatives, any one of which satisfies the requirement.
    pub alternatives: Vec<StoreDlcId>,
}

impl DlcRequirement {
    /// Whether known ownership satisfies this requirement.
    ///
    /// Returns [`DlcOwnership::Unknown`] unless ownership is known for every
    /// alternative or one owned alternative is found. Unknown ownership is never
    /// treated as satisfied.
    #[must_use]
    pub fn evaluate(&self, ownership: &dyn Fn(&StoreDlcId) -> DlcOwnership) -> DlcOwnership {
        let mut saw_unknown = false;
        for id in &self.alternatives {
            match ownership(id) {
                DlcOwnership::Owned => return DlcOwnership::Owned,
                DlcOwnership::Unknown => saw_unknown = true,
                DlcOwnership::NotOwned => {}
            }
        }
        if saw_unknown || self.alternatives.is_empty() {
            DlcOwnership::Unknown
        } else {
            DlcOwnership::NotOwned
        }
    }

    /// Stable textual form used by [`DependencyFingerprint`].
    fn canonical(&self) -> String {
        let mut ids: Vec<&str> = self.alternatives.iter().map(StoreDlcId::as_str).collect();
        ids.sort_unstable();
        ids.dedup();
        format!("dlc\u{1f}{}", ids.join("\u{1d}"))
    }
}

/// Whether the user owns a store extra.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DlcOwnership {
    /// The store confirmed ownership.
    Owned,
    /// The store confirmed the user does not own it.
    NotOwned,
    /// The store exposes no ownership information.
    ///
    /// Never counted as owned: a missing DLC that looks satisfied produces a
    /// silently broken game.
    Unknown,
}

/// A canonical digest of a dependency definition.
///
/// Scoped to *meaning*, not to bytes: candidate and group ordering, duplicates
/// and cosmetic provider fields do not change it, so an unchanged requirement
/// keeps the user's accepted risk. Any change to which artifacts satisfy a
/// requirement does change it, which invalidates the matching
/// [`DependencyOverride`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DependencyFingerprint(String);

impl DependencyFingerprint {
    /// Compute the fingerprint of a set of requirements.
    #[must_use]
    pub fn of(groups: &[DependencyGroup], dlc: &[DlcRequirement]) -> Self {
        let mut lines: Vec<String> = groups
            .iter()
            .map(DependencyGroup::canonical)
            .chain(dlc.iter().map(DlcRequirement::canonical))
            .collect();
        lines.sort();
        lines.dedup();
        Self(FileHash::blake3_of(lines.join("\u{1e}").as_bytes()).hex)
    }

    /// The fingerprint as a hex string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One fetch of a provider's dependency definition for one version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencySnapshot {
    /// Onera's identifier.
    pub id: DependencySnapshotId,
    /// The version the definition belongs to.
    pub source: DependencySource,
    /// Whether the contents can be believed, and how much.
    pub availability: DependencyAvailability,
    /// Independent requirements (AND).
    pub groups: Vec<DependencyGroup>,
    /// Store DLC requirements.
    pub dlc: Vec<DlcRequirement>,
    /// Provider's own revision marker for the definition, when it has one.
    pub provider_revision: Option<String>,
    /// Canonical fingerprint of `groups` and `dlc`.
    pub fingerprint: DependencyFingerprint,
    /// When the snapshot was taken.
    pub fetched_at: DateTime<Utc>,
    /// The provider's raw response, preserved for diagnostics only.
    ///
    /// Nothing outside the originating provider adapter may interpret it.
    pub raw: serde_json::Value,
}

impl DependencySnapshot {
    /// Build a snapshot from fetched requirements, computing the fingerprint.
    #[must_use]
    pub fn fetched(
        source: DependencySource,
        groups: Vec<DependencyGroup>,
        dlc: Vec<DlcRequirement>,
        fetched_at: DateTime<Utc>,
    ) -> Self {
        let fingerprint = DependencyFingerprint::of(&groups, &dlc);
        Self {
            id: DependencySnapshotId::new(),
            source,
            availability: DependencyAvailability::Fetched,
            groups,
            dlc,
            provider_revision: None,
            fingerprint,
            fetched_at,
            raw: serde_json::Value::Null,
        }
    }

    /// A snapshot recording that the provider has no dependency concept.
    #[must_use]
    pub fn unsupported(source: DependencySource, at: DateTime<Utc>) -> Self {
        Self {
            availability: DependencyAvailability::Unsupported,
            ..Self::fetched(source, Vec::new(), Vec::new(), at)
        }
    }

    /// A snapshot recording that the provider could not be asked.
    #[must_use]
    pub fn unavailable(
        source: DependencySource,
        reason: impl Into<String>,
        at: DateTime<Utc>,
    ) -> Self {
        Self {
            availability: DependencyAvailability::Unavailable {
                reason: reason.into(),
            },
            ..Self::fetched(source, Vec::new(), Vec::new(), at)
        }
    }

    /// Whether the provider positively stated that nothing is required.
    ///
    /// The one safe way to read an empty group list. An unavailable or
    /// unsupported snapshot answers `false` however empty it is.
    #[must_use]
    pub fn declares_no_dependencies(&self) -> bool {
        self.availability.is_authoritative() && self.groups.is_empty() && self.dlc.is_empty()
    }

    /// Requirements that block an apply when unsatisfied.
    #[must_use]
    pub fn blocking_groups(&self) -> Vec<&DependencyGroup> {
        self.groups
            .iter()
            .filter(|g| g.kind.is_blocking())
            .collect()
    }
}

/// A user's explicit decision to proceed despite an unsatisfied requirement.
///
/// Scoped to one profile member *and* one dependency-definition fingerprint:
/// changed provider data invalidates the acceptance, and removing and re-adding
/// a mod drops it, because a new [`ProfileMemberId`] is issued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyOverride {
    /// Membership the risk was accepted for.
    pub profile_member_id: ProfileMemberId,
    /// Fingerprint of the definition that was displayed when it was accepted.
    pub fingerprint: DependencyFingerprint,
    /// The specific requirement that was ignored.
    pub group_id: DependencyGroupId,
    /// The user's stated reason, shown wherever the risk is surfaced.
    pub reason: String,
    /// When it was accepted.
    pub created_at: DateTime<Utc>,
}

impl DependencyOverride {
    /// Whether this override still applies to a requirement as it stands now.
    #[must_use]
    pub fn applies_to(
        &self,
        member: ProfileMemberId,
        fingerprint: &DependencyFingerprint,
        group: DependencyGroupId,
    ) -> bool {
        self.profile_member_id == member
            && &self.fingerprint == fingerprint
            && self.group_id == group
    }
}

/// The state of one member's requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyHealth {
    /// Every blocking requirement has a selected candidate.
    Satisfied,
    /// At least one blocking requirement has none.
    Unsatisfied,
    /// Unsatisfied, but the user accepted the risk for this exact definition.
    Ignored,
    /// The provider does not model dependencies, so there is nothing to check.
    NotApplicable,
    /// Requirements could not be evaluated: no data, or unknown DLC ownership.
    ///
    /// Distinct from [`DependencyHealth::Satisfied`]. Never render it as a tick.
    Unknown,
}

impl DependencyHealth {
    /// Whether this state should stop an apply until the user decides.
    #[must_use]
    pub const fn blocks_apply(self) -> bool {
        matches!(self, Self::Unsatisfied | Self::Unknown)
    }
}

/// One version the solver chose for one mod.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedVersion {
    /// Provider hosting the selection.
    pub provider: ProviderId,
    /// Mod the selection is for.
    pub provider_mod_id: ProviderModId,
    /// File to deploy.
    pub provider_file_id: ProviderFileId,
    /// Version identity of that file, when the provider exposes one.
    pub provider_version_id: Option<ProviderVersionId>,
    /// Group the selection belongs to; at most one version per group is chosen.
    pub provider_file_group_id: Option<ProviderFileGroupId>,
    /// Profile member this selection applies to, when it is already a member.
    pub profile_member_id: Option<ProfileMemberId>,
}

/// Why a requirement could not be satisfied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsatisfiedRequirement {
    /// The version whose definition stated the requirement.
    pub source: DependencySource,
    /// The requirement group.
    pub group_id: DependencyGroupId,
    /// Displayable label of the requirement.
    pub label: Option<String>,
    /// Human-readable explanation, e.g. "pinned to a version that excludes it".
    pub explanation: String,
}

/// What the solver concluded.
///
/// Every variant other than [`ResolutionOutcome::Compatible`] describes work the
/// user must approve. `Unknown` is a first-class outcome, not a failure to
/// report one: it is what offline operation and a missing DLC ownership answer
/// produce, and it must never be shown as compatible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResolutionOutcome {
    /// The enabled set already satisfies every blocking requirement.
    Compatible,
    /// Adding these candidates satisfies everything; nothing changes version.
    InstallMissing {
        /// Candidates to add, in a deterministic order.
        install: Vec<SelectedVersion>,
    },
    /// A compatible set exists but requires changing versions.
    UpdateSet {
        /// Versions to select, including downgrades.
        select: Vec<SelectedVersion>,
        /// New members required by the selection.
        install: Vec<SelectedVersion>,
    },
    /// The remaining members become valid if these are disabled.
    DisableSet {
        /// Minimal set of members to disable, in a deterministic order.
        disable: Vec<ProfileMemberId>,
    },
    /// No solution exists under the current pins and availability.
    Unsatisfied {
        /// What could not be satisfied, and why.
        requirements: Vec<UnsatisfiedRequirement>,
    },
    /// Not enough information to decide.
    Unknown {
        /// Why, safe to display.
        reason: String,
    },
}

impl ResolutionOutcome {
    /// Whether the current desired state may be applied without more decisions.
    #[must_use]
    pub const fn is_apply_ready(&self) -> bool {
        matches!(self, Self::Compatible)
    }

    /// Whether this outcome offers an actionable plan the user can accept.
    #[must_use]
    pub const fn offers_a_plan(&self) -> bool {
        matches!(
            self,
            Self::InstallMissing { .. } | Self::UpdateSet { .. } | Self::DisableSet { .. }
        )
    }
}

/// A solver result together with the evidence it was based on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionResult {
    /// What the solver concluded.
    pub outcome: ResolutionOutcome,
    /// Per-member health, for the member table.
    pub health: Vec<MemberHealth>,
    /// Whether any input snapshot was cached, unavailable or unsupported.
    ///
    /// Drives the "this may be out of date" banner. A result based on stale data
    /// is never presented as current.
    pub evidence: ResolutionEvidence,
}

/// How trustworthy the inputs to a resolution were.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionEvidence {
    /// Snapshots fetched fresh from the provider.
    pub fresh: u32,
    /// Snapshots served from a cache that is still within its window.
    pub cached: u32,
    /// Snapshots served from a cache that is past its window.
    pub stale: u32,
    /// Versions whose dependency data could not be fetched.
    pub unavailable: u32,
    /// Versions whose provider does not model dependencies.
    pub unsupported: u32,
    /// DLC requirements whose ownership is unknown.
    pub unknown_dlc: u32,
}

impl ResolutionEvidence {
    /// Whether anything about the inputs must be disclosed to the user.
    #[must_use]
    pub const fn is_complete_and_current(&self) -> bool {
        self.stale == 0 && self.unavailable == 0 && self.unknown_dlc == 0
    }

    /// Count one snapshot's availability.
    pub fn observe(&mut self, availability: &DependencyAvailability) {
        match availability {
            DependencyAvailability::Fetched => self.fresh += 1,
            DependencyAvailability::Cached { stale: true, .. } => self.stale += 1,
            DependencyAvailability::Cached { stale: false, .. } => self.cached += 1,
            DependencyAvailability::Unavailable { .. } => self.unavailable += 1,
            DependencyAvailability::Unsupported => self.unsupported += 1,
        }
    }
}

/// One member's dependency state, for the profile member table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberHealth {
    /// Membership this describes.
    pub profile_member_id: ProfileMemberId,
    /// Its state.
    pub health: DependencyHealth,
    /// Requirements that are unsatisfied or unknown, for the detail view.
    pub unsatisfied: Vec<UnsatisfiedRequirement>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ProviderId;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).expect("valid timestamp")
    }

    fn source() -> DependencySource {
        DependencySource {
            provider: ProviderId::nexus(),
            game_slug: "cyberpunk2077".into(),
            provider_mod_id: ProviderModId::new("107"),
            provider_file_id: Some(ProviderFileId::new("9001")),
            provider_version_id: Some(ProviderVersionId::new("v-9001")),
        }
    }

    fn candidate(mod_id: &str, file: &str, status: CandidateStatus) -> DependencyCandidate {
        DependencyCandidate {
            provider: ProviderId::nexus(),
            game_slug: "cyberpunk2077".into(),
            provider_mod_id: ProviderModId::new(mod_id),
            provider_file_id: Some(ProviderFileId::new(file)),
            provider_version_id: Some(ProviderVersionId::new(format!("v-{file}"))),
            provider_file_group_id: Some(ProviderFileGroupId::new(format!("g-{mod_id}"))),
            position: Some(1),
            status,
            display_name: None,
        }
    }

    fn group(candidates: Vec<DependencyCandidate>) -> DependencyGroup {
        DependencyGroup {
            id: DependencyGroupId::new(),
            provider_group_key: Some("req-1".into()),
            label: Some("Cyber Engine Tweaks".into()),
            kind: RequirementKind::Required,
            candidates,
        }
    }

    #[test]
    fn no_dependencies_is_distinct_from_unavailable_and_unsupported() {
        let fetched = DependencySnapshot::fetched(source(), vec![], vec![], now());
        assert!(fetched.declares_no_dependencies());

        let unavailable = DependencySnapshot::unavailable(source(), "endpoint 503", now());
        assert!(!unavailable.declares_no_dependencies());
        assert!(!unavailable.availability.is_authoritative());

        let unsupported = DependencySnapshot::unsupported(source(), now());
        assert!(!unsupported.declares_no_dependencies());

        // All three carry an empty group list; only the availability separates
        // them, and they are not equal to one another.
        assert!(fetched.groups.is_empty());
        assert_ne!(fetched.availability, unavailable.availability);
        assert_ne!(fetched.availability, unsupported.availability);
    }

    #[test]
    fn cached_data_is_authoritative_but_can_be_labelled_stale() {
        let mut snapshot = DependencySnapshot::fetched(source(), vec![], vec![], now());
        snapshot.availability = DependencyAvailability::Cached {
            fetched_at: now(),
            stale: true,
        };
        assert!(snapshot.declares_no_dependencies());
        assert!(snapshot.availability.is_stale());

        let mut evidence = ResolutionEvidence::default();
        evidence.observe(&snapshot.availability);
        assert_eq!(evidence.stale, 1);
        assert!(!evidence.is_complete_and_current());
    }

    #[test]
    fn the_fingerprint_ignores_ordering_but_not_content() {
        let a = candidate("1", "10", CandidateStatus::Available);
        let b = candidate("2", "20", CandidateStatus::Available);
        let forward = DependencyFingerprint::of(&[group(vec![a.clone(), b.clone()])], &[]);
        let reversed = DependencyFingerprint::of(&[group(vec![b.clone(), a.clone()])], &[]);
        assert_eq!(forward, reversed);

        // A duplicated alternative states nothing new.
        let duplicated =
            DependencyFingerprint::of(&[group(vec![a.clone(), b.clone(), a.clone()])], &[]);
        assert_eq!(duplicated, forward);

        // Dropping an alternative narrows what satisfies the requirement.
        assert_ne!(
            DependencyFingerprint::of(&[group(vec![a.clone()])], &[]),
            forward
        );

        // So does changing the kind of the requirement.
        let recommended = DependencyGroup {
            kind: RequirementKind::Recommended,
            ..group(vec![a.clone(), b.clone()])
        };
        assert_ne!(DependencyFingerprint::of(&[recommended], &[]), forward);

        // Adding a DLC requirement changes it too.
        let dlc = DlcRequirement {
            id: DependencyGroupId::new(),
            label: None,
            alternatives: vec![StoreDlcId::new("1091501")],
        };
        assert_ne!(
            DependencyFingerprint::of(&[group(vec![a, b])], &[dlc]),
            forward
        );
        assert_eq!(forward.as_str().len(), 64);
    }

    #[test]
    fn a_changed_definition_invalidates_an_override() {
        let member = ProfileMemberId::new();
        let g = group(vec![candidate("1", "10", CandidateStatus::Available)]);
        let original = DependencyFingerprint::of(std::slice::from_ref(&g), &[]);
        let accepted = DependencyOverride {
            profile_member_id: member,
            fingerprint: original.clone(),
            group_id: g.id,
            reason: "I have it installed manually".into(),
            created_at: now(),
        };
        assert!(accepted.applies_to(member, &original, g.id));

        // The author changed which files satisfy the requirement.
        let changed = DependencyGroup {
            candidates: vec![candidate("1", "11", CandidateStatus::Available)],
            ..g.clone()
        };
        let new_fingerprint = DependencyFingerprint::of(&[changed], &[]);
        assert!(!accepted.applies_to(member, &new_fingerprint, g.id));

        // And the acceptance does not leak to another member or another group.
        assert!(!accepted.applies_to(ProfileMemberId::new(), &original, g.id));
        assert!(!accepted.applies_to(member, &original, DependencyGroupId::new()));
    }

    #[test]
    fn only_available_candidates_for_the_right_game_are_selectable() {
        for status in [
            CandidateStatus::Hidden,
            CandidateStatus::Removed,
            CandidateStatus::Unknown,
        ] {
            assert!(
                !candidate("1", "10", status).is_selectable_for("cyberpunk2077"),
                "{status:?} was selectable"
            );
        }
        let ok = candidate("1", "10", CandidateStatus::Available);
        assert!(ok.is_selectable_for("cyberpunk2077"));
        assert!(!ok.is_selectable_for("skyrimspecialedition"));

        // An empty group and a group of unusable candidates are both blocked.
        assert!(group(vec![]).is_unsatisfiable("cyberpunk2077"));
        assert!(group(vec![candidate("1", "10", CandidateStatus::Removed)])
            .is_unsatisfiable("cyberpunk2077"));
        assert!(!group(vec![ok]).is_unsatisfiable("cyberpunk2077"));
    }

    #[test]
    fn unknown_dlc_ownership_is_never_treated_as_owned() {
        let owned = StoreDlcId::new("owned");
        let unowned = StoreDlcId::new("unowned");
        let unknown = StoreDlcId::new("unknown");
        let lookup = |id: &StoreDlcId| match id.as_str() {
            "owned" => DlcOwnership::Owned,
            "unowned" => DlcOwnership::NotOwned,
            _ => DlcOwnership::Unknown,
        };
        let req = |alternatives: Vec<StoreDlcId>| DlcRequirement {
            id: DependencyGroupId::new(),
            label: None,
            alternatives,
        };

        assert_eq!(
            req(vec![owned.clone()]).evaluate(&lookup),
            DlcOwnership::Owned
        );
        assert_eq!(
            req(vec![unowned.clone()]).evaluate(&lookup),
            DlcOwnership::NotOwned
        );
        assert_eq!(
            req(vec![unknown.clone()]).evaluate(&lookup),
            DlcOwnership::Unknown
        );
        // One owned alternative satisfies an OR group even beside an unknown.
        assert_eq!(
            req(vec![unknown.clone(), owned]).evaluate(&lookup),
            DlcOwnership::Owned
        );
        // But an unknown beside a not-owned stays unknown, never not-owned.
        assert_eq!(
            req(vec![unowned, unknown]).evaluate(&lookup),
            DlcOwnership::Unknown
        );
        // A requirement with no alternatives cannot be shown to be satisfied.
        assert_eq!(req(vec![]).evaluate(&lookup), DlcOwnership::Unknown);
    }

    #[test]
    fn unknown_health_and_outcomes_never_read_as_apply_ready() {
        assert!(DependencyHealth::Unknown.blocks_apply());
        assert!(DependencyHealth::Unsatisfied.blocks_apply());
        assert!(!DependencyHealth::Satisfied.blocks_apply());
        assert!(!DependencyHealth::Ignored.blocks_apply());
        assert!(!DependencyHealth::NotApplicable.blocks_apply());

        assert!(ResolutionOutcome::Compatible.is_apply_ready());
        for outcome in [
            ResolutionOutcome::Unknown {
                reason: "offline".into(),
            },
            ResolutionOutcome::Unsatisfied {
                requirements: vec![],
            },
            ResolutionOutcome::DisableSet { disable: vec![] },
        ] {
            assert!(!outcome.is_apply_ready(), "{outcome:?} claimed readiness");
        }
        assert!(ResolutionOutcome::DisableSet { disable: vec![] }.offers_a_plan());
        assert!(!ResolutionOutcome::Unknown {
            reason: String::new()
        }
        .offers_a_plan());
    }

    #[test]
    fn recommended_requirements_do_not_block() {
        assert!(RequirementKind::Required.is_blocking());
        assert!(RequirementKind::Incompatible.is_blocking());
        assert!(!RequirementKind::Recommended.is_blocking());

        let blocking = group(vec![]);
        let advisory = DependencyGroup {
            kind: RequirementKind::Recommended,
            ..group(vec![])
        };
        let mut snapshot = DependencySnapshot::fetched(source(), vec![], vec![], now());
        snapshot.groups = vec![blocking.clone(), advisory];
        assert_eq!(snapshot.blocking_groups().len(), 1);
        assert_eq!(snapshot.blocking_groups()[0].id, blocking.id);
        assert!(!snapshot.declares_no_dependencies());
    }

    #[test]
    fn a_snapshot_round_trips_through_json_with_stable_tags() {
        let snapshot = DependencySnapshot::unavailable(source(), "offline", now());
        let json = serde_json::to_string(&snapshot).unwrap();
        assert_eq!(
            serde_json::from_str::<DependencySnapshot>(&json).unwrap(),
            snapshot
        );
        assert!(json.contains("\"kind\":\"unavailable\""), "{json}");

        let outcome = ResolutionOutcome::InstallMissing { install: vec![] };
        let json = serde_json::to_string(&outcome).unwrap();
        assert!(json.contains("\"kind\":\"install_missing\""), "{json}");
        assert_eq!(
            serde_json::from_str::<ResolutionOutcome>(&json).unwrap(),
            outcome
        );
    }
}
