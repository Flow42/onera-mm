//! Desired-state reconciliation.
//!
//! A reconciler is deliberately pure: it receives the current provider stacks,
//! the mappings available for each retained artifact, and an ordered desired
//! set. It returns the final stacks and the disk changes required to reach
//! them. Database, archive, and filesystem work happens only after its result
//! has been previewed and explicitly approved.

use crate::domain::provider_stack::{FileProvider, ProviderStack, StackEntry};
use crate::hash::FileHash;
use crate::ids::{InstallationId, LocalGameId};
use crate::paths::RelPath;
use crate::plan::TargetLocation;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The enabled artifacts Onera should deploy for one local game.
///
/// Installations are ordered from lowest to highest priority. If two selected
/// artifacts intentionally provide identical bytes for one path, the latter is
/// still represented in the stack but no disk write is needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesiredGameState {
    /// Game whose deployment roots will be reconciled.
    pub local_game_id: LocalGameId,
    /// Enabled artifacts, ordered from lowest to highest priority.
    pub installations: Vec<InstallationId>,
}

impl DesiredGameState {
    /// Construct a desired state, rejecting duplicate installations.
    pub fn new(local_game_id: LocalGameId, installations: Vec<InstallationId>) -> Self {
        let mut seen = BTreeSet::new();
        let installations = installations
            .into_iter()
            .filter(|id| seen.insert(*id))
            .collect();
        Self {
            local_game_id,
            installations,
        }
    }
}

/// One stable source-to-target mapping retained with an acquired artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallationMapping {
    /// Artifact providing this file.
    pub installation_id: InstallationId,
    /// Relative source path inside the extracted artifact.
    pub source: RelPath,
    /// File's location in the deployment roots.
    pub target: TargetLocation,
    /// BLAKE3 of the source bytes recorded after extraction.
    pub source_hash: FileHash,
    /// Source byte count.
    pub source_size: u64,
}

/// A disk mutation required by a reconciled final stack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MutationStep {
    /// Put these bytes at the target. The caller resolves the installation's
    /// retained archive mapping before staging the write.
    Write {
        /// Target to update.
        target: TargetLocation,
        /// Provider whose bytes must be staged.
        provider: StackEntry,
        /// Hash expected on disk when this preview was built.
        expected_previous: Option<FileHash>,
    },
    /// No provider remains for this target.
    Delete {
        /// Target to delete, subject to the existing external-modification
        /// safeguards in the mutation engine.
        target: TargetLocation,
        /// Hash expected on disk when this preview was built.
        expected_previous: FileHash,
    },
}

/// A newly introduced collision that must be explicitly resolved before apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossModConflict {
    /// Path targeted by incompatible selected artifacts.
    pub target: TargetLocation,
    /// Distinct installation providers, in desired priority order.
    pub providers: Vec<InstallationId>,
}

/// Previewable result of a desired-state reconciliation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationPlan {
    /// State being requested.
    pub desired: DesiredGameState,
    /// Final provider stacks, including paths whose bytes do not change.
    #[serde(with = "target_map")]
    pub final_stacks: BTreeMap<TargetLocation, ProviderStack>,
    /// Filesystem changes needed before the final stacks can be persisted.
    pub steps: Vec<MutationStep>,
    /// Expected current top hash for every affected target, including paths
    /// whose ownership changes without requiring a byte rewrite.
    #[serde(with = "target_map")]
    pub expected_files: BTreeMap<TargetLocation, Option<FileHash>>,
    /// Cross-mod collisions that are not safe to choose automatically.
    pub conflicts: Vec<CrossModConflict>,
    /// Explicit winner selected for each resolved cross-mod collision.
    #[serde(with = "target_map")]
    pub conflict_decisions: BTreeMap<TargetLocation, InstallationId>,
}

// JSON object keys can only be strings. Persist target-keyed maps as ordered
// pairs so the complete, typed target survives journal serialization.
mod target_map {
    use super::TargetLocation;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<S, V>(
        map: &BTreeMap<TargetLocation, V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        V: Serialize,
    {
        map.iter().collect::<Vec<_>>().serialize(serializer)
    }

    pub fn deserialize<'de, D, V>(deserializer: D) -> Result<BTreeMap<TargetLocation, V>, D::Error>
    where
        D: Deserializer<'de>,
        V: Deserialize<'de>,
    {
        let entries = Vec::<(TargetLocation, V)>::deserialize(deserializer)?;
        Ok(entries.into_iter().collect())
    }
}

impl MutationPlan {
    /// Whether this exact preview can be applied without more user decisions.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.conflicts.is_empty()
    }
}

/// Reconcile the desired artifacts onto current provider stacks.
///
/// The function never privileges an artifact merely because it has a higher
/// priority: different bytes from two selected mods are reported as a conflict.
/// Identical content is safely shared and produces one provider stack.
#[must_use]
pub fn reconcile(
    desired: DesiredGameState,
    current: &BTreeMap<TargetLocation, ProviderStack>,
    mappings: &[InstallationMapping],
) -> MutationPlan {
    reconcile_with_decisions(desired, current, mappings, &BTreeMap::new())
}

/// Reconcile while applying explicit per-path winners for cross-mod collisions.
///
/// A decision is accepted only when its installation actually provides that
/// target. The selected provider is moved to the top of the final stack while
/// all other providers remain underneath it for later enable/disable changes.
#[must_use]
pub fn reconcile_with_decisions(
    desired: DesiredGameState,
    current: &BTreeMap<TargetLocation, ProviderStack>,
    mappings: &[InstallationMapping],
    decisions: &BTreeMap<TargetLocation, InstallationId>,
) -> MutationPlan {
    let selected: BTreeSet<_> = desired.installations.iter().copied().collect();
    let mut by_target: BTreeMap<TargetLocation, BTreeMap<InstallationId, &InstallationMapping>> =
        BTreeMap::new();
    for mapping in mappings {
        if selected.contains(&mapping.installation_id) {
            by_target
                .entry(mapping.target.clone())
                .or_default()
                .insert(mapping.installation_id, mapping);
        }
    }

    let targets: BTreeSet<_> = current.keys().chain(by_target.keys()).cloned().collect();
    let mut final_stacks = BTreeMap::new();
    let mut steps = Vec::new();
    let mut expected_files = BTreeMap::new();
    let mut conflicts = Vec::new();
    let mut conflict_decisions = BTreeMap::new();

    for target in targets {
        let current_stack = current.get(&target).cloned().unwrap_or_default();
        expected_files.insert(
            target.clone(),
            current_stack.top().map(|entry| entry.hash.clone()),
        );
        let retained_unmanaged = current_stack
            .entries()
            .iter()
            .filter(|entry| entry.provider.is_unmanaged())
            .cloned();
        let target_mappings = by_target.get(&target);
        let mut selected_entries: Vec<_> = desired
            .installations
            .iter()
            .filter_map(|installation_id| {
                target_mappings
                    .and_then(|entries| entries.get(installation_id))
                    .map(|mapping| StackEntry {
                        provider: FileProvider::Installation {
                            installation_id: *installation_id,
                        },
                        hash: mapping.source_hash.clone(),
                        size: mapping.source_size,
                    })
            })
            .collect();

        let distinct_hashes: BTreeSet<_> = selected_entries
            .iter()
            .map(|entry| entry.hash.clone())
            .collect();
        if distinct_hashes.len() > 1 {
            if let Some(winner) = decisions.get(&target) {
                if let Some(position) = selected_entries
                    .iter()
                    .position(|entry| entry.provider.installation_id() == Some(*winner))
                {
                    let selected_winner = *winner;
                    let winner_entry = selected_entries.remove(position);
                    selected_entries.push(winner_entry);
                    conflict_decisions.insert(target.clone(), selected_winner);
                } else {
                    conflicts.push(CrossModConflict {
                        target: target.clone(),
                        providers: selected_entries
                            .iter()
                            .filter_map(|entry| entry.provider.installation_id())
                            .collect(),
                    });
                    final_stacks.insert(target, current_stack);
                    continue;
                }
            } else {
                conflicts.push(CrossModConflict {
                    target: target.clone(),
                    providers: selected_entries
                        .iter()
                        .filter_map(|entry| entry.provider.installation_id())
                        .collect(),
                });
                // Preserve the current stack until the caller supplies a conflict
                // decision; no unsafe inferred final state leaks into apply.
                final_stacks.insert(target, current_stack);
                continue;
            }
        }

        let final_stack =
            ProviderStack::from_entries(retained_unmanaged.chain(selected_entries).collect());
        let current_top = current_stack.top().map(|entry| &entry.hash);
        match final_stack.top() {
            Some(top) if current_top != Some(&top.hash) => steps.push(MutationStep::Write {
                target: target.clone(),
                provider: top.clone(),
                expected_previous: current_top.cloned(),
            }),
            None if current_stack.top().is_some() => steps.push(MutationStep::Delete {
                target: target.clone(),
                expected_previous: current_stack.top().expect("checked").hash.clone(),
            }),
            _ => {}
        }
        final_stacks.insert(target, final_stack);
    }

    MutationPlan {
        desired,
        final_stacks,
        steps,
        expected_files,
        conflicts,
        conflict_decisions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::BackupId;
    use crate::paths::RelPath;

    fn target(name: &str) -> TargetLocation {
        TargetLocation {
            root_key: "mods".into(),
            path: RelPath::normalize(name).unwrap(),
        }
    }

    fn mapping(installation_id: InstallationId, name: &str, bytes: &[u8]) -> InstallationMapping {
        InstallationMapping {
            installation_id,
            source: RelPath::normalize(name).unwrap(),
            target: target(name),
            source_hash: FileHash::blake3_of(bytes),
            source_size: bytes.len() as u64,
        }
    }

    #[test]
    fn disabling_an_artifact_restores_the_unmanaged_original() {
        let game = LocalGameId::new();
        let old = InstallationId::new();
        let target = target("a.archive");
        let original = StackEntry {
            provider: FileProvider::UnmanagedBackup {
                backup_id: BackupId::new(),
            },
            hash: FileHash::blake3_of(b"original"),
            size: 8,
        };
        let current = BTreeMap::from([(
            target.clone(),
            ProviderStack::from_entries(vec![
                original.clone(),
                StackEntry {
                    provider: FileProvider::Installation {
                        installation_id: old,
                    },
                    hash: FileHash::blake3_of(b"mod"),
                    size: 3,
                },
            ]),
        )]);
        let plan = reconcile(DesiredGameState::new(game, vec![]), &current, &[]);
        assert!(plan.is_ready());
        assert_eq!(plan.final_stacks[&target].top(), Some(&original));
        assert!(matches!(
            plan.steps.as_slice(),
            [MutationStep::Write { .. }]
        ));
    }

    #[test]
    fn identical_files_are_shared_without_a_write() {
        let game = LocalGameId::new();
        let (a, b) = (InstallationId::new(), InstallationId::new());
        let plan = reconcile(
            DesiredGameState::new(game, vec![a, b]),
            &BTreeMap::from([(
                target("same"),
                ProviderStack::from_entries(vec![StackEntry {
                    provider: FileProvider::Installation { installation_id: a },
                    hash: FileHash::blake3_of(b"same"),
                    size: 4,
                }]),
            )]),
            &[mapping(a, "same", b"same"), mapping(b, "same", b"same")],
        );
        assert!(plan.is_ready());
        assert!(plan.steps.is_empty());
        assert_eq!(plan.final_stacks[&target("same")].len(), 2);
    }

    #[test]
    fn incompatible_cross_mod_files_require_a_decision() {
        let game = LocalGameId::new();
        let (a, b) = (InstallationId::new(), InstallationId::new());
        let plan = reconcile(
            DesiredGameState::new(game, vec![a, b]),
            &BTreeMap::new(),
            &[mapping(a, "same", b"a"), mapping(b, "same", b"b")],
        );
        assert!(!plan.is_ready());
        assert_eq!(plan.conflicts[0].providers, vec![a, b]);
        assert!(plan.steps.is_empty());
    }

    #[test]
    fn an_explicit_cross_mod_winner_is_preserved_in_the_plan() {
        let game = LocalGameId::new();
        let (a, b) = (InstallationId::new(), InstallationId::new());
        let location = target("same");
        let plan = reconcile_with_decisions(
            DesiredGameState::new(game, vec![a, b]),
            &BTreeMap::new(),
            &[mapping(a, "same", b"a"), mapping(b, "same", b"b")],
            &BTreeMap::from([(location.clone(), a)]),
        );
        assert!(plan.is_ready());
        assert_eq!(plan.conflict_decisions[&location], a);
        assert_eq!(plan.final_stacks[&location].len(), 2);
        assert_eq!(
            plan.final_stacks[&location]
                .top()
                .and_then(|entry| entry.provider.installation_id()),
            Some(a)
        );
        assert!(matches!(
            plan.steps.as_slice(),
            [MutationStep::Write { provider, .. }]
                if provider.provider.installation_id() == Some(a)
        ));
    }

    #[test]
    fn duplicate_requested_installations_are_removed_preserving_order() {
        let game = LocalGameId::new();
        let (a, b) = (InstallationId::new(), InstallationId::new());
        assert_eq!(
            DesiredGameState::new(game, vec![a, b, a]).installations,
            vec![a, b]
        );
    }
}
