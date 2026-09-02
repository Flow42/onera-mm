//! The pure dependency solver.
//!
//! **Status: scaffolding.** The types below are the contract Milestone 4 will
//! implement against; [`solve`] itself is not implemented yet and says so.
//!
//! This crate exists as its own crate to make its central constraint structural
//! rather than a matter of discipline: it depends on `onera-core` and `serde`
//! and nothing else. There is no `sqlx`, no `reqwest`, no `tokio` and no
//! filesystem here, so a solver that reached for the network or the database
//! would not compile. Everything it reasons about arrives in a
//! [`ResolutionRequest`] and everything it concludes leaves in a
//! [`ResolutionResult`].
//!
//! ## Rules the implementation must keep
//!
//! Hard constraints:
//!
//! 1. one selected version per enabled mod and per provider file group;
//! 2. every non-ignored blocking group has at least one selected candidate;
//! 3. pinned members never change version;
//! 4. candidates must target the same game and be selectable
//!    ([`DependencyCandidate::is_selectable_for`]);
//! 5. DLC that is known to be missing is never treated as satisfied, and DLC
//!    whose ownership is unknown produces [`ResolutionOutcome::Unknown`] rather
//!    than an assumption.
//!
//! Preference order, applied in sequence:
//!
//! 1. keep the version that is already selected, when it is compatible;
//! 2. avoid disabling a mod;
//! 3. minimise changed mods, then downloads;
//! 4. prefer a higher provider [`DependencyCandidate::position`] within a file
//!    group;
//! 5. break remaining ties on the provider's stable identifiers, so the same
//!    input always produces the same output.
//!
//! Dependency relationships form a graph, not a tree. The implementation must
//! traverse it with an explicit visited set: a cycle whose members can all be
//! selected at once is valid, and one that cannot must contribute to an
//! explanation rather than a stack overflow.
//!
//! Never compare free-form version strings. Ordering comes from the provider's
//! `position`; identity comes from [`ProviderVersionId`].
//!
//! [`DependencyCandidate::is_selectable_for`]: onera_core::domain::dependency::DependencyCandidate::is_selectable_for
//! [`DependencyCandidate::position`]: onera_core::domain::dependency::DependencyCandidate::position
//! [`ProviderVersionId`]: onera_core::ids::ProviderVersionId

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use onera_core::domain::dependency::{
    CandidateStatus, DependencyOverride, DependencySnapshot, DlcOwnership, ResolutionEvidence,
    ResolutionOutcome, ResolutionResult,
};
use onera_core::domain::profile::{DesiredModState, MemberPin, MemberSelection};
use onera_core::ids::{
    ModId, ProfileMemberId, ProviderFileGroupId, ProviderFileId, ProviderId, ProviderModId,
    ProviderVersionId, StoreDlcId,
};
use serde::{Deserialize, Serialize};

/// One profile member the solver must account for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberRequest {
    /// Membership being solved for.
    pub profile_member_id: ProfileMemberId,
    /// Mod lineage.
    pub mod_id: ModId,
    /// The version the member currently wants.
    pub selection: MemberSelection,
    /// Whether the version may be changed.
    pub pin: MemberPin,
    /// Whether the user wants it deployed.
    pub desired: DesiredModState,
}

/// A version the solver may choose.
///
/// The union of what is already downloaded and what the provider offers. The
/// solver prefers installed candidates when everything else is equal, because
/// choosing one avoids a download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailableVersion {
    /// Provider hosting the version.
    pub provider: ProviderId,
    /// Provider slug of the game it targets.
    pub game_slug: String,
    /// Mod it belongs to.
    pub provider_mod_id: ProviderModId,
    /// File to deploy.
    pub provider_file_id: ProviderFileId,
    /// Version identity, when the provider exposes one.
    pub provider_version_id: Option<ProviderVersionId>,
    /// Group of mutually superseding files it belongs to.
    pub provider_file_group_id: Option<ProviderFileGroupId>,
    /// Provider's own ordering position within that group. Higher is newer.
    pub position: Option<i64>,
    /// Whether it can be selected at all.
    pub status: CandidateStatus,
    /// Whether the artifact is already in local storage.
    pub installed: bool,
}

/// Known ownership of one store extra.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DlcOwnershipFact {
    /// Store's opaque identifier.
    pub id: StoreDlcId,
    /// What is known about it. [`DlcOwnership::Unknown`] is a legitimate entry
    /// and must not be omitted: an absent fact and a known non-ownership are
    /// different inputs.
    pub ownership: DlcOwnership,
}

/// Everything the solver is allowed to look at.
///
/// Assembling this is the application layer's job. The solver never fetches,
/// reads or caches anything itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionRequest {
    /// Provider slug of the game being solved for.
    ///
    /// Candidates targeting any other game are rejected outright.
    pub game_slug: String,
    /// The profile's members, enabled and disabled.
    pub members: Vec<MemberRequest>,
    /// Dependency definitions, including unavailable and unsupported ones.
    ///
    /// The availability of each snapshot is part of the input: a missing answer
    /// must reach the solver as such, never as an empty requirement list.
    pub snapshots: Vec<DependencySnapshot>,
    /// Versions the solver may choose from.
    pub available: Vec<AvailableVersion>,
    /// What is known about DLC ownership.
    pub dlc_ownership: Vec<DlcOwnershipFact>,
    /// Risks the user has already accepted.
    pub overrides: Vec<DependencyOverride>,
}

impl ResolutionRequest {
    /// Tally how trustworthy this request's dependency data is.
    ///
    /// Pure bookkeeping, and correct before the solver exists: it is what tells
    /// the UI whether a result rests on stale or missing data.
    #[must_use]
    pub fn evidence(&self) -> ResolutionEvidence {
        let mut evidence = ResolutionEvidence::default();
        for snapshot in &self.snapshots {
            evidence.observe(&snapshot.availability);
        }
        evidence.unknown_dlc = self
            .dlc_ownership
            .iter()
            .filter(|fact| fact.ownership == DlcOwnership::Unknown)
            .count()
            .try_into()
            .unwrap_or(u32::MAX);
        evidence
    }
}

/// Solve a profile's dependency constraints.
///
/// Not implemented yet. It returns [`ResolutionOutcome::Unknown`], which is a
/// modelled outcome rather than a failure: callers already have to treat
/// `Unknown` as "cannot be shown to be compatible", so a caller wired up against
/// this scaffold blocks and explains instead of silently applying an unchecked
/// plan. It deliberately never returns [`ResolutionOutcome::Compatible`].
#[must_use]
pub fn solve(request: &ResolutionRequest) -> ResolutionResult {
    ResolutionResult {
        outcome: ResolutionOutcome::Unknown {
            reason: "dependency solving is not implemented yet".to_owned(),
        },
        health: Vec::new(),
        evidence: request.evidence(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    use onera_core::domain::dependency::{DependencyAvailability, DependencySource};

    fn source(id: &str) -> DependencySource {
        DependencySource {
            provider: ProviderId::nexus(),
            game_slug: "cyberpunk2077".into(),
            provider_mod_id: ProviderModId::new(id),
            provider_file_id: None,
            provider_version_id: None,
        }
    }

    fn request(
        snapshots: Vec<DependencySnapshot>,
        dlc: Vec<DlcOwnershipFact>,
    ) -> ResolutionRequest {
        ResolutionRequest {
            game_slug: "cyberpunk2077".into(),
            members: vec![],
            snapshots,
            available: vec![],
            dlc_ownership: dlc,
            overrides: vec![],
        }
    }

    fn now() -> DateTime<chrono::Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).expect("valid timestamp")
    }

    #[test]
    fn the_scaffold_never_claims_a_set_is_compatible() {
        let result = solve(&request(vec![], vec![]));
        assert!(!result.outcome.is_apply_ready());
        assert!(!result.outcome.offers_a_plan());
        assert!(matches!(result.outcome, ResolutionOutcome::Unknown { .. }));
    }

    #[test]
    fn evidence_separates_fresh_stale_and_missing_inputs() {
        let mut cached = DependencySnapshot::fetched(source("2"), vec![], vec![], now());
        cached.availability = DependencyAvailability::Cached {
            fetched_at: now(),
            stale: true,
        };
        let evidence = request(
            vec![
                DependencySnapshot::fetched(source("1"), vec![], vec![], now()),
                cached,
                DependencySnapshot::unavailable(source("3"), "offline", now()),
                DependencySnapshot::unsupported(source("4"), now()),
            ],
            vec![DlcOwnershipFact {
                id: StoreDlcId::new("1091501"),
                ownership: DlcOwnership::Unknown,
            }],
        )
        .evidence();

        assert_eq!(evidence.fresh, 1);
        assert_eq!(evidence.stale, 1);
        assert_eq!(evidence.unavailable, 1);
        assert_eq!(evidence.unsupported, 1);
        assert_eq!(evidence.unknown_dlc, 1);
        assert!(!evidence.is_complete_and_current());

        let clean = request(
            vec![DependencySnapshot::fetched(
                source("1"),
                vec![],
                vec![],
                now(),
            )],
            vec![DlcOwnershipFact {
                id: StoreDlcId::new("1091501"),
                ownership: DlcOwnership::Owned,
            }],
        )
        .evidence();
        assert!(clean.is_complete_and_current());
    }

    #[test]
    fn a_request_round_trips_through_json() {
        let original = request(
            vec![DependencySnapshot::unavailable(
                source("1"),
                "offline",
                now(),
            )],
            vec![],
        );
        let json = serde_json::to_string(&original).unwrap();
        assert_eq!(
            serde_json::from_str::<ResolutionRequest>(&json).unwrap(),
            original
        );
    }
}
