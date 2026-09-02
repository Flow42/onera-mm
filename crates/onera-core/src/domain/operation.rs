//! The journaled operation state machine.
//!
//! Every mutation of a game directory is a journaled *operation*. The journal is
//! the single source of truth for crash recovery: on startup Onera reads back
//! any operation that is not in a terminal state and decides, purely from the
//! recorded state, whether it can be continued or must be rolled back.
//!
//! ```text
//!            ┌──────────┐
//!            │ Planned  │  plan persisted, nothing on disk
//!            └────┬─────┘
//!                 │ prepare
//!            ┌────▼─────┐
//!            │ Prepared │  backups written, temp files staged
//!            └────┬─────┘
//!                 │ commit
//!            ┌────▼─────┐
//!            │Committing│  renames in progress — the only risky window
//!            └────┬─────┘
//!                 │ verify + record
//!            ┌────▼─────┐
//!            │ Complete │  terminal
//!            └──────────┘
//!
//!   Planned ──abort──► RolledBack (terminal, nothing to undo)
//!   Prepared ─abort──► RollingBack ──► RolledBack
//!   Committing ─fail─► RollingBack ──► RolledBack
//!   RollingBack ─fail► Failed (terminal, needs the user)
//! ```
//!
//! `Committing` is deliberately the only state in which target files may be
//! mid-change. Recovery from it is safe because each individual file transition
//! is an atomic rename and the journal records per-file completion.

use crate::ids::{LocalGameId, OperationId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// What an operation is trying to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    /// Deploy a release's files into a game.
    Install,
    /// Remove an installation and restore what it covered.
    Remove,
    /// Re-deploy files that verification found wrong.
    Repair,
    /// Reconcile several retained artifacts into one desired deployment state.
    Reconcile,
    /// Reconcile to no active mods, then verify the trusted game baseline.
    CleanRestore,
}

/// Where an operation is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    /// The full plan is persisted. Nothing has touched the game directory.
    Planned,
    /// Backups exist and temporary files are staged next to their targets.
    Prepared,
    /// Renames are in flight. The only state where targets can be mid-change.
    Committing,
    /// Everything is deployed, verified and recorded. Terminal.
    Complete,
    /// Undo is in progress.
    RollingBack,
    /// Undo finished; the game directory is back to its pre-operation state.
    /// Terminal.
    RolledBack,
    /// Undo itself failed. Terminal; requires the user to inspect. Onera will
    /// not automatically retry, because a failed rollback means the recorded
    /// state and the disk state disagree.
    Failed,
}

impl OperationState {
    /// Whether no further automatic work is possible.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::RolledBack | Self::Failed)
    }

    /// Whether finding this state at startup means an operation was interrupted.
    #[must_use]
    pub const fn is_interrupted(self) -> bool {
        !self.is_terminal()
    }

    /// What recovery should offer for an operation found in this state.
    #[must_use]
    pub const fn recovery(self) -> Recovery {
        match self {
            // Nothing was written, so discarding the plan is free and safe.
            Self::Planned => Recovery::DiscardPlan,
            // Backups and temp files exist but no target changed: either
            // finishing or undoing is safe.
            Self::Prepared => Recovery::ContinueOrRollBack,
            // Targets may be half-swapped. The journal says which files are
            // done, so both directions remain available, but the user chooses.
            Self::Committing => Recovery::ContinueOrRollBack,
            Self::RollingBack => Recovery::ResumeRollback,
            Self::Complete | Self::RolledBack | Self::Failed => Recovery::None,
        }
    }

    /// Whether a transition to `next` is allowed.
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Planned, Self::Prepared)
                | (Self::Planned, Self::RolledBack)
                | (Self::Prepared, Self::Committing)
                | (Self::Prepared, Self::RollingBack)
                | (Self::Committing, Self::Complete)
                | (Self::Committing, Self::RollingBack)
                | (Self::RollingBack, Self::RolledBack)
                | (Self::RollingBack, Self::Failed)
        )
    }
}

impl fmt::Display for OperationState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Planned => "planned",
            Self::Prepared => "prepared",
            Self::Committing => "committing",
            Self::Complete => "complete",
            Self::RollingBack => "rolling_back",
            Self::RolledBack => "rolled_back",
            Self::Failed => "failed",
        };
        f.write_str(s)
    }
}

/// What Onera can offer for an interrupted operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recovery {
    /// Nothing to recover.
    None,
    /// Throw the plan away; the disk was never touched.
    DiscardPlan,
    /// Offer the user both finishing and undoing.
    ContinueOrRollBack,
    /// Finish the rollback that was already underway.
    ResumeRollback,
}

/// A journaled operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    /// Identifier, also used to name the staging and backup directories.
    pub id: OperationId,
    /// Game installation being mutated. Deployments are serialized per value of
    /// this field.
    pub local_game_id: LocalGameId,
    /// What the operation does.
    pub kind: OperationKind,
    /// Current state.
    pub state: OperationState,
    /// When the operation was first journaled.
    pub created_at: DateTime<Utc>,
    /// When the state last changed.
    pub updated_at: DateTime<Utc>,
    /// Redacted failure message, when the operation failed.
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [OperationState; 7] = [
        OperationState::Planned,
        OperationState::Prepared,
        OperationState::Committing,
        OperationState::Complete,
        OperationState::RollingBack,
        OperationState::RolledBack,
        OperationState::Failed,
    ];

    #[test]
    fn happy_path_transitions_are_allowed() {
        let path = [
            OperationState::Planned,
            OperationState::Prepared,
            OperationState::Committing,
            OperationState::Complete,
        ];
        for pair in path.windows(2) {
            assert!(
                pair[0].can_transition_to(pair[1]),
                "{:?} -> {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn terminal_states_have_no_outgoing_transitions() {
        for from in ALL.into_iter().filter(|s| s.is_terminal()) {
            for to in ALL {
                assert!(!from.can_transition_to(to), "{from:?} must be terminal");
            }
            assert_eq!(from.recovery(), Recovery::None);
        }
    }

    #[test]
    fn skipping_states_is_rejected() {
        assert!(!OperationState::Planned.can_transition_to(OperationState::Committing));
        assert!(!OperationState::Planned.can_transition_to(OperationState::Complete));
        assert!(!OperationState::Prepared.can_transition_to(OperationState::Complete));
        // Rolling back cannot silently turn into a success.
        assert!(!OperationState::RollingBack.can_transition_to(OperationState::Complete));
    }

    #[test]
    fn every_non_terminal_state_offers_a_recovery() {
        for state in ALL.into_iter().filter(|s| s.is_interrupted()) {
            assert_ne!(
                state.recovery(),
                Recovery::None,
                "{state:?} needs a recovery path"
            );
        }
    }

    #[test]
    fn a_plan_that_never_ran_is_discarded_not_rolled_back() {
        assert_eq!(OperationState::Planned.recovery(), Recovery::DiscardPlan);
    }

    #[test]
    fn state_display_matches_storage_encoding() {
        for state in ALL {
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(json, format!("\"{state}\""));
        }
    }
}
