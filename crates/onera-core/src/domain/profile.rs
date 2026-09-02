//! Profiles: a game-scoped, reusable selection of mods.
//!
//! A profile is *desired* state. Adding a member, pinning it or reordering it
//! changes nothing on disk; the game directory only moves when the profile is
//! activated and the resulting [`DesiredGameState`] is reconciled, previewed and
//! applied through the journaled mutation engine.
//!
//! Terminology note: Onera says **profile** for its own local selection because
//! Nexus already has a feature called Collections. Using one word for both would
//! make API and UI behaviour ambiguous.
//!
//! Invariants modelled here rather than left to the database:
//!
//! * A profile belongs to exactly one [`LocalGameId`], not to a game title. Two
//!   copies of a game can be on different builds with different deploy roots.
//! * Exactly one profile per local game is active. See
//!   [`validate_profile_set`].
//! * Membership priority is explicit and orders the provider stack; it never
//!   bypasses the conflict preview.
//! * A member may name an artifact that has not been downloaded yet. That is a
//!   *missing* member, reported by [`desired_state`], not a silent omission.

use crate::domain::reconcile::DesiredGameState;
use crate::ids::{
    InstallationId, LocalGameId, ModId, OperationId, ProfileId, ProfileMemberId,
    ProviderFileGroupId, ProviderFileId, ProviderId, ProviderModId, ProviderVersionId,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Name of the profile created automatically when a game is confirmed.
pub const DEFAULT_PROFILE_NAME: &str = "Default";

/// A named, game-scoped selection of mods.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// Onera's identifier.
    pub id: ProfileId,
    /// The concrete installation this profile applies to.
    pub local_game_id: LocalGameId,
    /// Display name, unique per local game.
    pub name: String,
    /// Optional free-form note shown in the profile card.
    pub description: Option<String>,
    /// Whether this is the one active profile for its local game.
    pub is_active: bool,
    /// When the profile was created.
    pub created_at: DateTime<Utc>,
    /// When the profile or one of its members last changed.
    pub updated_at: DateTime<Utc>,
}

impl Profile {
    /// Whether this profile is the automatically created default.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.name == DEFAULT_PROFILE_NAME
    }
}

/// Explicit ordering of a member within its profile.
///
/// Lower priority is deployed first, so a higher priority sits nearer the top of
/// the provider stack. The value is a plain integer rather than a list position
/// so that inserting between two members does not renumber the whole profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MemberPriority(pub i32);

impl MemberPriority {
    /// Priority assigned to a member the user has not ordered explicitly.
    pub const DEFAULT: Self = Self(0);
    /// Lowest representable priority: deployed first, covered by everything.
    pub const LOWEST: Self = Self(i32::MIN);
    /// Highest representable priority: deployed last, covers everything.
    pub const HIGHEST: Self = Self(i32::MAX);

    /// The next priority above this one, saturating at [`MemberPriority::HIGHEST`].
    #[must_use]
    pub const fn above(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// The next priority below this one, saturating at [`MemberPriority::LOWEST`].
    #[must_use]
    pub const fn below(self) -> Self {
        Self(self.0.saturating_sub(1))
    }
}

impl Default for MemberPriority {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Whether the user wants a member deployed.
///
/// This is desired state only. A member can be `Enabled` while its artifact is
/// still being downloaded, and `Disabled` while its files are still on disk
/// because the profile has not been applied yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesiredModState {
    /// Deploy this member when the profile is applied.
    Enabled,
    /// Keep the member and its artifact, but deploy nothing.
    Disabled,
}

impl DesiredModState {
    /// Whether the member should be part of the deployed set.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

/// Which provider artifact a member wants.
///
/// Every field is provider-neutral and opaque. `provider_version_id` and
/// `provider_file_group_id` are what the dependency solver reasons about;
/// nothing here is ever parsed or ordered as a version string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberSelection {
    /// Provider the artifact comes from.
    pub provider: ProviderId,
    /// Provider's opaque mod identifier.
    pub provider_mod_id: ProviderModId,
    /// Selected file, when the user or the solver has chosen one.
    pub provider_file_id: Option<ProviderFileId>,
    /// Provider version identity of that file, when the provider exposes one.
    pub provider_version_id: Option<ProviderVersionId>,
    /// Group of mutually superseding files this selection belongs to.
    pub provider_file_group_id: Option<ProviderFileGroupId>,
}

impl MemberSelection {
    /// A selection that names a mod but no particular file yet.
    #[must_use]
    pub fn unresolved(provider: ProviderId, provider_mod_id: ProviderModId) -> Self {
        Self {
            provider,
            provider_mod_id,
            provider_file_id: None,
            provider_version_id: None,
            provider_file_group_id: None,
        }
    }

    /// Whether a concrete file has been chosen.
    #[must_use]
    pub fn is_resolved(&self) -> bool {
        self.provider_file_id.is_some()
    }
}

/// Whether the user has frozen a member's version.
///
/// A pinned member is a hard constraint on the solver: it may not be upgraded,
/// downgraded or silently replaced to satisfy someone else's dependency.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemberPin {
    /// The solver may change this member's version.
    #[default]
    Unpinned,
    /// The selected version is frozen.
    Pinned {
        /// When the pin was set.
        pinned_at: DateTime<Utc>,
        /// The user's stated reason, shown when a plan is blocked by the pin.
        reason: Option<String>,
    },
}

impl MemberPin {
    /// Whether the version may not be changed.
    #[must_use]
    pub const fn is_pinned(&self) -> bool {
        matches!(self, Self::Pinned { .. })
    }
}

/// One mod's membership in a profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileMember {
    /// Onera's identifier for the membership itself.
    ///
    /// Dependency overrides are scoped to this identifier, so removing and
    /// re-adding a mod deliberately drops any risk the user had accepted.
    pub id: ProfileMemberId,
    /// Profile this membership belongs to.
    pub profile_id: ProfileId,
    /// Mod lineage.
    pub mod_id: ModId,
    /// Artifact the member wants.
    pub selection: MemberSelection,
    /// Retained artifact that satisfies the selection, once one exists.
    ///
    /// `None` means the profile references something not downloaded yet;
    /// activation must include the download in its preview.
    pub installation_id: Option<InstallationId>,
    /// Whether the user wants it deployed.
    pub desired: DesiredModState,
    /// Whether the version is frozen.
    pub pin: MemberPin,
    /// Provider-stack ordering within the profile.
    pub priority: MemberPriority,
    /// When the mod was added to the profile.
    pub added_at: DateTime<Utc>,
}

impl ProfileMember {
    /// Whether this member contributes files to the next apply.
    ///
    /// Enabled *and* backed by a retained artifact. An enabled member with no
    /// artifact is a download requirement, not a deployable one.
    #[must_use]
    pub fn is_deployable(&self) -> bool {
        self.desired.is_enabled() && self.installation_id.is_some()
    }
}

/// How far a profile activation got.
///
/// The target profile is only marked active in [`ProfileActivationState::Applied`],
/// which is reached after filesystem verification succeeds. Every earlier state
/// leaves the previous profile active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileActivationState {
    /// Resolving versions, downloads and a mutation plan. Nothing written.
    Preparing,
    /// A journaled operation is staging and committing files.
    Applying,
    /// Verified on disk; the target profile is now the active one.
    Applied,
    /// Failed and fully undone; the source profile is still active.
    RolledBack,
    /// Failed and could not be undone automatically; recovery is required.
    Failed,
}

impl ProfileActivationState {
    /// Whether the target profile may be reported as active.
    #[must_use]
    pub const fn target_is_active(self) -> bool {
        matches!(self, Self::Applied)
    }

    /// Whether no further transition is possible.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Applied | Self::RolledBack | Self::Failed)
    }
}

/// One recorded attempt to switch profiles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileActivation {
    /// Profile that was active before the attempt, if any.
    pub from_profile_id: Option<ProfileId>,
    /// Profile being activated.
    pub to_profile_id: ProfileId,
    /// The journaled operation carrying the filesystem half, once one exists.
    pub operation_id: Option<OperationId>,
    /// How far it got.
    pub state: ProfileActivationState,
    /// When the attempt began.
    pub started_at: DateTime<Utc>,
    /// When it reached a terminal state.
    pub finished_at: Option<DateTime<Utc>>,
    /// Displayable failure reason, when it did not succeed.
    pub error: Option<String>,
}

/// Why a set of profiles is not internally consistent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileSetError {
    /// More than one profile claims to be active for the same local game.
    #[error("{count} profiles are marked active for local game {local_game_id}")]
    MultipleActive {
        /// The game with an ambiguous active profile.
        local_game_id: LocalGameId,
        /// How many profiles claimed to be active.
        count: usize,
    },
    /// A local game has profiles but none of them is active.
    #[error("no profile is marked active for local game {local_game_id}")]
    NoneActive {
        /// The game with no active profile.
        local_game_id: LocalGameId,
    },
    /// Two profiles for one game share a name.
    #[error("duplicate profile name {name:?} for local game {local_game_id}")]
    DuplicateName {
        /// The game with the clashing names.
        local_game_id: LocalGameId,
        /// The name used twice.
        name: String,
    },
}

/// Check the per-game invariants of a set of profiles.
///
/// Profiles for several games may be passed at once; each game is validated
/// independently. An empty set is valid: a game with no profiles yet has not
/// been confirmed.
///
/// # Errors
/// Returns the first violated invariant, in the order profiles were supplied.
pub fn validate_profile_set(profiles: &[Profile]) -> Result<(), ProfileSetError> {
    let games: BTreeSet<_> = profiles.iter().map(|p| p.local_game_id).collect();
    for local_game_id in games {
        let scoped: Vec<&Profile> = profiles
            .iter()
            .filter(|p| p.local_game_id == local_game_id)
            .collect();

        let mut names = BTreeSet::new();
        for profile in &scoped {
            if !names.insert(profile.name.as_str()) {
                return Err(ProfileSetError::DuplicateName {
                    local_game_id,
                    name: profile.name.clone(),
                });
            }
        }

        match scoped.iter().filter(|p| p.is_active).count() {
            1 => {}
            0 => return Err(ProfileSetError::NoneActive { local_game_id }),
            count => {
                return Err(ProfileSetError::MultipleActive {
                    local_game_id,
                    count,
                })
            }
        }
    }
    Ok(())
}

/// The desired game state a profile's members describe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileDesiredState {
    /// Deployable members, ordered from lowest to highest priority.
    pub state: DesiredGameState,
    /// Enabled members with no retained artifact yet.
    ///
    /// These must be acquired before the profile can be fully applied, and an
    /// activation preview has to show them as downloads rather than dropping
    /// them.
    pub missing: Vec<ProfileMemberId>,
}

impl ProfileDesiredState {
    /// Whether every enabled member already has an artifact on disk.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }
}

/// Project a profile's members onto a desired game state.
///
/// Pure and total: it sorts by [`MemberPriority`] (ties broken by
/// [`ProfileMemberId`] so the result is deterministic), keeps only enabled
/// members, and reports enabled members that have no artifact instead of
/// silently omitting them. It makes no decision about downloads, conflicts or
/// writes; those belong to the reconciler and the engine.
#[must_use]
pub fn desired_state(local_game_id: LocalGameId, members: &[ProfileMember]) -> ProfileDesiredState {
    let mut enabled: Vec<&ProfileMember> =
        members.iter().filter(|m| m.desired.is_enabled()).collect();
    enabled.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));

    let mut installations = Vec::new();
    let mut missing = Vec::new();
    for member in enabled {
        match member.installation_id {
            Some(installation_id) => installations.push(installation_id),
            None => missing.push(member.id),
        }
    }

    ProfileDesiredState {
        state: DesiredGameState::new(local_game_id, installations),
        missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ProviderId;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).expect("valid timestamp")
    }

    fn profile(game: LocalGameId, name: &str, active: bool) -> Profile {
        Profile {
            id: ProfileId::new(),
            local_game_id: game,
            name: name.to_owned(),
            description: None,
            is_active: active,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn member(
        profile_id: ProfileId,
        priority: i32,
        desired: DesiredModState,
        installation: Option<InstallationId>,
    ) -> ProfileMember {
        ProfileMember {
            id: ProfileMemberId::new(),
            profile_id,
            mod_id: ModId::new(),
            selection: MemberSelection::unresolved(ProviderId::nexus(), ProviderModId::new("42")),
            installation_id: installation,
            desired,
            pin: MemberPin::Unpinned,
            priority: MemberPriority(priority),
            added_at: now(),
        }
    }

    #[test]
    fn exactly_one_profile_per_game_may_be_active() {
        let game = LocalGameId::new();
        let other = LocalGameId::new();
        let ok = vec![
            profile(game, DEFAULT_PROFILE_NAME, true),
            profile(game, "Modded", false),
            profile(other, DEFAULT_PROFILE_NAME, true),
        ];
        assert!(validate_profile_set(&ok).is_ok());
        // An empty set is a game that has not been confirmed yet.
        assert!(validate_profile_set(&[]).is_ok());

        let two = vec![
            profile(game, DEFAULT_PROFILE_NAME, true),
            profile(game, "Modded", true),
        ];
        assert!(matches!(
            validate_profile_set(&two),
            Err(ProfileSetError::MultipleActive { count: 2, .. })
        ));

        let none = vec![profile(game, DEFAULT_PROFILE_NAME, false)];
        assert!(matches!(
            validate_profile_set(&none),
            Err(ProfileSetError::NoneActive { .. })
        ));
    }

    #[test]
    fn profile_names_are_unique_per_game_but_not_across_games() {
        let (a, b) = (LocalGameId::new(), LocalGameId::new());
        let across = vec![
            profile(a, DEFAULT_PROFILE_NAME, true),
            profile(b, DEFAULT_PROFILE_NAME, true),
        ];
        assert!(validate_profile_set(&across).is_ok());

        let within = vec![profile(a, "Modded", true), profile(a, "Modded", false)];
        assert!(matches!(
            validate_profile_set(&within),
            Err(ProfileSetError::DuplicateName { .. })
        ));
    }

    #[test]
    fn desired_state_orders_enabled_members_by_priority() {
        let game = LocalGameId::new();
        let p = ProfileId::new();
        let (low, high) = (InstallationId::new(), InstallationId::new());
        let members = vec![
            member(p, 10, DesiredModState::Enabled, Some(high)),
            member(p, -5, DesiredModState::Enabled, Some(low)),
        ];
        let projected = desired_state(game, &members);
        assert_eq!(projected.state.installations, vec![low, high]);
        assert!(projected.is_complete());
    }

    #[test]
    fn disabled_members_are_excluded_and_undownloaded_ones_are_reported() {
        let game = LocalGameId::new();
        let p = ProfileId::new();
        let deployable = InstallationId::new();
        let members = vec![
            member(p, 0, DesiredModState::Enabled, Some(deployable)),
            member(p, 1, DesiredModState::Disabled, Some(InstallationId::new())),
            member(p, 2, DesiredModState::Enabled, None),
        ];
        let projected = desired_state(game, &members);
        assert_eq!(projected.state.installations, vec![deployable]);
        // An enabled member with no artifact is a download requirement, never a
        // silent omission.
        assert_eq!(projected.missing, vec![members[2].id]);
        assert!(!projected.is_complete());
    }

    #[test]
    fn equal_priorities_order_deterministically() {
        let game = LocalGameId::new();
        let p = ProfileId::new();
        let mut members = vec![
            member(p, 0, DesiredModState::Enabled, Some(InstallationId::new())),
            member(p, 0, DesiredModState::Enabled, Some(InstallationId::new())),
        ];
        let forward = desired_state(game, &members);
        members.reverse();
        assert_eq!(desired_state(game, &members).state, forward.state);
    }

    #[test]
    fn priority_helpers_saturate() {
        assert_eq!(MemberPriority::HIGHEST.above(), MemberPriority::HIGHEST);
        assert_eq!(MemberPriority::LOWEST.below(), MemberPriority::LOWEST);
        assert!(MemberPriority::DEFAULT.below() < MemberPriority::DEFAULT);
    }

    #[test]
    fn a_pinned_member_is_distinguishable_after_a_round_trip() {
        let m = ProfileMember {
            pin: MemberPin::Pinned {
                pinned_at: now(),
                reason: Some("known-good with my save".into()),
            },
            ..member(ProfileId::new(), 0, DesiredModState::Enabled, None)
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: ProfileMember = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
        assert!(back.pin.is_pinned());
        assert!(!back.is_deployable());
    }

    #[test]
    fn the_target_profile_is_active_only_after_verification() {
        for state in [
            ProfileActivationState::Preparing,
            ProfileActivationState::Applying,
            ProfileActivationState::RolledBack,
            ProfileActivationState::Failed,
        ] {
            assert!(!state.target_is_active(), "{state:?} claimed the target");
        }
        assert!(ProfileActivationState::Applied.target_is_active());
        assert!(!ProfileActivationState::Preparing.is_terminal());
        assert!(ProfileActivationState::RolledBack.is_terminal());
    }
}
