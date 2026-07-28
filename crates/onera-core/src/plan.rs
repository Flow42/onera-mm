//! Installation planning: classification, conflicts and dry-run previews.
//!
//! Planning is a pure function of (archive manifest, layout mapping, recorded
//! state, on-disk state). It writes nothing. The [`InstallPlan`] it produces is
//! what the UI previews, what the user approves, and what gets persisted to the
//! journal before a single byte moves — so what is approved and what is applied
//! cannot diverge.

use crate::domain::provider_stack::ProviderStack;
use crate::hash::FileHash;
use crate::ids::{InstallationId, LocalGameId, ModId, OperationId};
use crate::paths::RelPath;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

/// Where a file will be written: a deployment-root key plus a relative path.
///
/// The root key is resolved to an absolute directory by the game adapter, so
/// plans stay portable across machines and can be persisted and replayed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TargetLocation {
    /// Adapter-defined deployment-root key.
    pub root_key: String,
    /// Path relative to that root.
    pub path: RelPath,
}

impl std::fmt::Display for TargetLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.root_key, self.path)
    }
}

/// What is currently true about a target path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TargetState {
    /// Hash and size of the file currently on disk, if it exists.
    pub on_disk: Option<(FileHash, u64)>,
    /// Providers Onera has recorded for this path.
    pub stack: ProviderStack,
}

impl TargetState {
    /// Nothing on disk and nothing recorded.
    #[must_use]
    pub fn absent() -> Self {
        Self::default()
    }

    /// A file Onera has never seen before.
    #[must_use]
    pub fn unmanaged(hash: FileHash, size: u64) -> Self {
        Self {
            on_disk: Some((hash, size)),
            stack: ProviderStack::new(),
        }
    }

    fn on_disk_hash(&self) -> Option<&FileHash> {
        self.on_disk.as_ref().map(|(h, _)| h)
    }

    /// Whether the file on disk differs from what Onera recorded for it.
    #[must_use]
    pub fn is_externally_modified(&self) -> bool {
        match (self.stack.top(), self.on_disk_hash()) {
            (Some(top), Some(actual)) => &top.hash != actual,
            // Recorded but gone: the user (or another tool) deleted it.
            (Some(_), None) => true,
            _ => false,
        }
    }
}

/// How an incoming file relates to what is already at its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileClassification {
    /// Nothing is there; write it.
    Create,
    /// The exact same bytes are already there; register shared ownership and do
    /// not rewrite the file.
    Identical,
    /// A previous release of this same mod owns it; replace in place.
    ReplacePreviousRelease,
    /// A different mod owns it; the user must decide.
    ConflictWithOtherMod,
    /// A file Onera has never managed is there; the user must decide.
    UnmanagedExisting,
    /// A file Onera manages has been changed behind its back; the user must
    /// decide.
    ExternallyModified,
    /// The target is not a legal path for this game; refuse it.
    InvalidTarget,
    /// A remembered or per-operation rule says to skip it.
    SkippedByRule,
}

impl FileClassification {
    /// Whether this classification blocks the plan until the user decides.
    ///
    /// The three "always ask" rules live here: another mod's file, an unmanaged
    /// file, and an externally modified file are never resolved automatically.
    #[must_use]
    pub const fn needs_decision(self) -> bool {
        matches!(
            self,
            Self::ConflictWithOtherMod | Self::UnmanagedExisting | Self::ExternallyModified
        )
    }

    /// Whether applying this entry writes to the target.
    #[must_use]
    pub const fn writes_target(self) -> bool {
        matches!(self, Self::Create | Self::ReplacePreviousRelease)
    }
}

/// How a conflict is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictChoice {
    /// Leave the existing file alone; this mod does not provide this path.
    KeepExisting,
    /// Back the existing file up, then write the new one on top.
    ReplaceAfterBackup,
    /// Record the existing file as if this mod had provided it, without writing.
    ///
    /// Used when a user has already installed a mod by hand and wants Onera to
    /// take over management of the file it finds.
    AdoptExisting,
    /// Cancel the whole operation.
    Abort,
}

impl ConflictChoice {
    /// Whether choosing this aborts the entire plan.
    #[must_use]
    pub const fn is_abort(self) -> bool {
        matches!(self, Self::Abort)
    }
}

/// How widely a decision applies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum DecisionScope {
    /// Just this one file in this one operation.
    ThisFile,
    /// Every file in this operation with the same classification.
    ///
    /// This is the "apply to equivalent conflicts" action.
    EquivalentInThisOperation {
        /// Classification the decision applies to.
        classification: FileClassification,
    },
    /// A remembered rule, deliberately narrow: one mod, one deployment root, one
    /// path prefix. Onera does not offer a global "always replace" rule.
    RememberedRule {
        /// Mod the rule is scoped to.
        mod_id: ModId,
        /// Deployment root the rule is scoped to.
        root_key: String,
        /// Path prefix the rule is scoped to.
        path_prefix: String,
    },
}

/// A decision the user made about one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    /// What to do.
    pub choice: ConflictChoice,
    /// How far it reaches.
    pub scope: DecisionScope,
}

/// A remembered, narrowly scoped rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedRule {
    /// Mod the rule applies to.
    pub mod_id: ModId,
    /// Deployment root the rule applies to.
    pub root_key: String,
    /// Path prefix the rule applies to.
    pub path_prefix: String,
    /// What to do when it matches.
    pub choice: ConflictChoice,
}

impl ScopedRule {
    /// Whether this rule covers the given mod and target.
    #[must_use]
    pub fn matches(&self, mod_id: ModId, target: &TargetLocation) -> bool {
        self.mod_id == mod_id
            && self.root_key == target.root_key
            && target.path.as_str().starts_with(&self.path_prefix)
    }
}

/// One file in a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedFile {
    /// Path of the file inside the extracted staging directory.
    pub source: RelPath,
    /// Where it will be deployed.
    pub target: TargetLocation,
    /// BLAKE3 hash of the source bytes.
    pub source_hash: FileHash,
    /// Size of the source bytes.
    pub source_size: u64,
    /// How it relates to what is already there.
    pub classification: FileClassification,
    /// Hash currently on disk at the target, if anything is there.
    pub existing_hash: Option<FileHash>,
    /// The decision that resolves it, if one has been made.
    pub decision: Option<ConflictChoice>,
    /// Human-readable remarks, safe to display.
    pub notes: Vec<String>,
}

impl PlannedFile {
    /// Whether this entry still needs the user.
    #[must_use]
    pub fn is_unresolved(&self) -> bool {
        self.classification.needs_decision() && self.decision.is_none()
    }

    /// The effective action once decisions are applied.
    #[must_use]
    pub fn effective_action(&self) -> PlannedAction {
        match (self.classification, self.decision) {
            (_, Some(ConflictChoice::Abort)) => PlannedAction::Abort,
            (_, Some(ConflictChoice::KeepExisting)) => PlannedAction::Skip,
            (_, Some(ConflictChoice::AdoptExisting)) => PlannedAction::Adopt,
            (_, Some(ConflictChoice::ReplaceAfterBackup)) => PlannedAction::BackupAndWrite,
            (FileClassification::Create, None) => PlannedAction::Write,
            (FileClassification::ReplacePreviousRelease, None) => PlannedAction::Write,
            (FileClassification::Identical, None) => PlannedAction::RegisterShared,
            (FileClassification::SkippedByRule, None) => PlannedAction::Skip,
            (FileClassification::InvalidTarget, None) => PlannedAction::Reject,
            // Unresolved "always ask" classifications never reach apply; the
            // installer refuses a plan that still has them.
            (c, None) => {
                debug_assert!(c.needs_decision());
                PlannedAction::Blocked
            }
        }
    }
}

/// The concrete thing the installer will do with a planned file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlannedAction {
    /// Write the file; no backup needed.
    Write,
    /// Back the existing file up, then write.
    BackupAndWrite,
    /// Do not write; add this installation to the path's provider stack.
    RegisterShared,
    /// Do not write; record the existing bytes as provided by this
    /// installation.
    Adopt,
    /// Do nothing at all for this path.
    Skip,
    /// Refuse the file.
    Reject,
    /// Cancel the whole operation.
    Abort,
    /// Still waiting on the user.
    Blocked,
}

/// A dry-run installation plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallPlan {
    /// The operation this plan will be journaled under.
    pub operation_id: OperationId,
    /// Game installation being written to.
    pub local_game_id: LocalGameId,
    /// The installation record this plan creates.
    pub installation_id: InstallationId,
    /// The mod lineage being installed.
    pub mod_id: ModId,
    /// Every file, sorted by target for a stable preview.
    pub files: Vec<PlannedFile>,
}

impl InstallPlan {
    /// Sort files so previews and journal entries are deterministic.
    pub fn sort(&mut self) {
        self.files.sort_by(|a, b| a.target.cmp(&b.target));
    }

    /// Files that still need a decision.
    pub fn unresolved(&self) -> impl Iterator<Item = &PlannedFile> {
        self.files.iter().filter(|f| f.is_unresolved())
    }

    /// Whether the plan can be applied as it stands.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.unresolved().next().is_none()
            && !self
                .files
                .iter()
                .any(|f| f.effective_action() == PlannedAction::Abort)
    }

    /// Count of each classification, for the preview header.
    #[must_use]
    pub fn summary(&self) -> BTreeMap<FileClassification, usize> {
        let mut out = BTreeMap::new();
        for f in &self.files {
            *out.entry(f.classification).or_insert(0) += 1;
        }
        out
    }

    /// Total bytes that will actually be written.
    #[must_use]
    pub fn bytes_to_write(&self) -> u64 {
        self.files
            .iter()
            .filter(|f| {
                matches!(
                    f.effective_action(),
                    PlannedAction::Write | PlannedAction::BackupAndWrite
                )
            })
            .map(|f| f.source_size)
            .sum()
    }

    /// Apply a decision, honouring its scope.
    ///
    /// Returns how many files were affected.
    pub fn apply_decision(&mut self, target: &TargetLocation, decision: &Decision) -> usize {
        match &decision.scope {
            DecisionScope::ThisFile => {
                let mut n = 0;
                for f in self.files.iter_mut().filter(|f| &f.target == target) {
                    f.decision = Some(decision.choice);
                    n += 1;
                }
                n
            }
            DecisionScope::EquivalentInThisOperation { classification } => {
                let mut n = 0;
                for f in self
                    .files
                    .iter_mut()
                    .filter(|f| f.classification == *classification && f.decision.is_none())
                {
                    f.decision = Some(decision.choice);
                    n += 1;
                }
                n
            }
            DecisionScope::RememberedRule {
                mod_id,
                root_key,
                path_prefix,
            } => {
                let rule = ScopedRule {
                    mod_id: *mod_id,
                    root_key: root_key.clone(),
                    path_prefix: path_prefix.clone(),
                    choice: decision.choice,
                };
                let plan_mod = self.mod_id;
                let mut n = 0;
                for f in self.files.iter_mut().filter(|f| f.decision.is_none()) {
                    if rule.matches(plan_mod, &f.target) && f.classification.needs_decision() {
                        f.decision = Some(decision.choice);
                        n += 1;
                    }
                }
                n
            }
        }
    }
}

/// Classify one incoming file against the current state of its target.
///
/// `same_mod_installations` is the set of installation ids that belong to the
/// same mod lineage as the incoming release — that is how "a previous release of
/// the same mod" is recognized without the domain knowing anything about the
/// provider that supplied either release.
#[must_use]
pub fn classify(
    incoming_hash: &FileHash,
    state: &TargetState,
    same_mod_installations: &HashSet<InstallationId>,
) -> (FileClassification, Vec<String>) {
    let mut notes = Vec::new();

    let Some((disk_hash, _)) = &state.on_disk else {
        // Nothing on disk. If Onera recorded a provider for this path, the file
        // was deleted behind our back; creating it is still the right move, but
        // the user should know.
        if !state.stack.is_empty() {
            notes
                .push("previously deployed file is missing from disk; it will be recreated".into());
        }
        return (FileClassification::Create, notes);
    };

    // An unchanged, identical file is never rewritten regardless of who owns it.
    if disk_hash == incoming_hash {
        return (FileClassification::Identical, notes);
    }

    let Some(top) = state.stack.top() else {
        // On disk, but Onera has no record of it.
        return (FileClassification::UnmanagedExisting, notes);
    };

    // Onera has a record, but disk does not match it: something else edited the
    // file. This outranks every other classification — we must never overwrite
    // a user's manual edit on the strength of a stale record.
    if &top.hash != disk_hash {
        notes.push(format!(
            "recorded hash {} does not match the file on disk",
            top.hash.prefix(12)
        ));
        return (FileClassification::ExternallyModified, notes);
    }

    match top.provider.installation_id() {
        Some(owner) if same_mod_installations.contains(&owner) => {
            (FileClassification::ReplacePreviousRelease, notes)
        }
        Some(_) => (FileClassification::ConflictWithOtherMod, notes),
        // The top of the stack is an unmanaged backup: the original file is
        // still what is deployed, so this is an unmanaged conflict.
        None => (FileClassification::UnmanagedExisting, notes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::provider_stack::{FileProvider, StackEntry};
    use crate::ids::BackupId;

    fn hash(b: &[u8]) -> FileHash {
        FileHash::blake3_of(b)
    }

    fn stack_of(entries: Vec<StackEntry>) -> ProviderStack {
        ProviderStack::from_entries(entries)
    }

    fn install_entry(id: InstallationId, content: &[u8]) -> StackEntry {
        StackEntry {
            provider: FileProvider::Installation {
                installation_id: id,
            },
            hash: hash(content),
            size: content.len() as u64,
        }
    }

    fn empty_set() -> HashSet<InstallationId> {
        HashSet::new()
    }

    #[test]
    fn missing_target_is_created() {
        let (c, notes) = classify(&hash(b"new"), &TargetState::absent(), &empty_set());
        assert_eq!(c, FileClassification::Create);
        assert!(notes.is_empty());
    }

    #[test]
    fn missing_but_recorded_target_is_created_with_a_warning() {
        let id = InstallationId::new();
        let state = TargetState {
            on_disk: None,
            stack: stack_of(vec![install_entry(id, b"gone")]),
        };
        let (c, notes) = classify(&hash(b"new"), &state, &empty_set());
        assert_eq!(c, FileClassification::Create);
        assert_eq!(notes.len(), 1, "user must be told the file vanished");
    }

    #[test]
    fn identical_content_is_never_rewritten() {
        let state = TargetState::unmanaged(hash(b"same"), 4);
        let (c, _) = classify(&hash(b"same"), &state, &empty_set());
        assert_eq!(c, FileClassification::Identical);
        assert_eq!(
            PlannedAction::RegisterShared,
            planned(c, None).effective_action(),
            "identical files register shared ownership instead of writing"
        );
    }

    #[test]
    fn identical_content_wins_over_unmanaged_and_conflict() {
        // Even an unmanaged file is "identical" if the bytes match, so a mod
        // that ships a vanilla file causes no prompt.
        let state = TargetState::unmanaged(hash(b"vanilla"), 7);
        assert_eq!(
            classify(&hash(b"vanilla"), &state, &empty_set()).0,
            FileClassification::Identical
        );
    }

    #[test]
    fn same_mod_update_replaces_only_when_the_hash_still_matches() {
        let previous = InstallationId::new();
        let same_mod: HashSet<_> = [previous].into_iter().collect();
        let state = TargetState {
            on_disk: Some((hash(b"v1"), 2)),
            stack: stack_of(vec![install_entry(previous, b"v1")]),
        };
        assert_eq!(
            classify(&hash(b"v2"), &state, &same_mod).0,
            FileClassification::ReplacePreviousRelease
        );
    }

    #[test]
    fn same_mod_update_becomes_external_modification_when_the_hash_drifted() {
        let previous = InstallationId::new();
        let same_mod: HashSet<_> = [previous].into_iter().collect();
        let state = TargetState {
            // Recorded v1, but the user hand-edited the file.
            on_disk: Some((hash(b"hand edited"), 11)),
            stack: stack_of(vec![install_entry(previous, b"v1")]),
        };
        let (c, notes) = classify(&hash(b"v2"), &state, &same_mod);
        assert_eq!(c, FileClassification::ExternallyModified);
        assert!(c.needs_decision());
        assert!(!notes.is_empty());
    }

    #[test]
    fn other_mods_file_is_a_conflict() {
        let other = InstallationId::new();
        let state = TargetState {
            on_disk: Some((hash(b"theirs"), 6)),
            stack: stack_of(vec![install_entry(other, b"theirs")]),
        };
        let (c, _) = classify(&hash(b"mine"), &state, &empty_set());
        assert_eq!(c, FileClassification::ConflictWithOtherMod);
        assert!(c.needs_decision());
    }

    #[test]
    fn unmanaged_existing_file_always_asks() {
        let state = TargetState::unmanaged(hash(b"vanilla"), 7);
        let (c, _) = classify(&hash(b"modded"), &state, &empty_set());
        assert_eq!(c, FileClassification::UnmanagedExisting);
        assert!(c.needs_decision());
    }

    #[test]
    fn an_unmanaged_backup_on_top_still_counts_as_unmanaged() {
        let state = TargetState {
            on_disk: Some((hash(b"vanilla"), 7)),
            stack: stack_of(vec![StackEntry {
                provider: FileProvider::UnmanagedBackup {
                    backup_id: BackupId::new(),
                },
                hash: hash(b"vanilla"),
                size: 7,
            }]),
        };
        assert_eq!(
            classify(&hash(b"modded"), &state, &empty_set()).0,
            FileClassification::UnmanagedExisting
        );
    }

    fn planned(
        classification: FileClassification,
        decision: Option<ConflictChoice>,
    ) -> PlannedFile {
        PlannedFile {
            source: RelPath::normalize("a/b.txt").unwrap(),
            target: TargetLocation {
                root_key: "game".into(),
                path: RelPath::normalize("a/b.txt").unwrap(),
            },
            source_hash: hash(b"x"),
            source_size: 1,
            classification,
            existing_hash: None,
            decision,
            notes: Vec::new(),
        }
    }

    #[test]
    fn decisions_map_to_actions() {
        use ConflictChoice::*;
        use FileClassification::*;
        let cases = [
            (Create, None, PlannedAction::Write),
            (Identical, None, PlannedAction::RegisterShared),
            (ReplacePreviousRelease, None, PlannedAction::Write),
            (SkippedByRule, None, PlannedAction::Skip),
            (InvalidTarget, None, PlannedAction::Reject),
            (
                ConflictWithOtherMod,
                Some(KeepExisting),
                PlannedAction::Skip,
            ),
            (
                ConflictWithOtherMod,
                Some(ReplaceAfterBackup),
                PlannedAction::BackupAndWrite,
            ),
            (UnmanagedExisting, Some(AdoptExisting), PlannedAction::Adopt),
            (UnmanagedExisting, Some(Abort), PlannedAction::Abort),
            (ExternallyModified, None, PlannedAction::Blocked),
        ];
        for (classification, decision, expected) in cases {
            assert_eq!(
                planned(classification, decision).effective_action(),
                expected,
                "{classification:?} + {decision:?}"
            );
        }
    }

    fn plan_with(files: Vec<PlannedFile>) -> InstallPlan {
        InstallPlan {
            operation_id: OperationId::new(),
            local_game_id: LocalGameId::new(),
            installation_id: InstallationId::new(),
            mod_id: ModId::new(),
            files,
        }
    }

    fn planned_at(path: &str, classification: FileClassification) -> PlannedFile {
        PlannedFile {
            source: RelPath::normalize(path).unwrap(),
            target: TargetLocation {
                root_key: "game".into(),
                path: RelPath::normalize(path).unwrap(),
            },
            source_hash: hash(path.as_bytes()),
            source_size: 10,
            classification,
            existing_hash: None,
            decision: None,
            notes: Vec::new(),
        }
    }

    #[test]
    fn a_plan_with_unresolved_conflicts_is_not_ready() {
        let plan = plan_with(vec![
            planned_at("a", FileClassification::Create),
            planned_at("b", FileClassification::ConflictWithOtherMod),
        ]);
        assert!(!plan.is_ready());
        assert_eq!(plan.unresolved().count(), 1);
    }

    #[test]
    fn applying_to_equivalent_conflicts_resolves_them_all_at_once() {
        let mut plan = plan_with(vec![
            planned_at("a", FileClassification::ConflictWithOtherMod),
            planned_at("b", FileClassification::ConflictWithOtherMod),
            planned_at("c", FileClassification::UnmanagedExisting),
        ]);
        let n = plan.apply_decision(
            &plan.files[0].target.clone(),
            &Decision {
                choice: ConflictChoice::KeepExisting,
                scope: DecisionScope::EquivalentInThisOperation {
                    classification: FileClassification::ConflictWithOtherMod,
                },
            },
        );
        assert_eq!(n, 2);
        // The unmanaged conflict is a different class and is untouched.
        assert!(plan.files[2].is_unresolved());
        assert!(!plan.is_ready());
    }

    #[test]
    fn a_remembered_rule_is_scoped_to_one_mod_root_and_prefix() {
        let mut plan = plan_with(vec![
            planned_at(
                "archive/pc/mod/x.archive",
                FileClassification::ConflictWithOtherMod,
            ),
            planned_at(
                "bin/x64/plugin.dll",
                FileClassification::ConflictWithOtherMod,
            ),
        ]);
        let mod_id = plan.mod_id;
        let n = plan.apply_decision(
            &plan.files[0].target.clone(),
            &Decision {
                choice: ConflictChoice::ReplaceAfterBackup,
                scope: DecisionScope::RememberedRule {
                    mod_id,
                    root_key: "game".into(),
                    path_prefix: "archive/".into(),
                },
            },
        );
        assert_eq!(n, 1, "the rule must not reach outside its prefix");
        assert_eq!(
            plan.files[0].decision,
            Some(ConflictChoice::ReplaceAfterBackup)
        );
        assert_eq!(plan.files[1].decision, None);
    }

    #[test]
    fn a_rule_for_another_mod_never_matches() {
        let rule = ScopedRule {
            mod_id: ModId::new(),
            root_key: "game".into(),
            path_prefix: String::new(),
            choice: ConflictChoice::KeepExisting,
        };
        let target = TargetLocation {
            root_key: "game".into(),
            path: RelPath::normalize("anything").unwrap(),
        };
        assert!(!rule.matches(ModId::new(), &target));
        assert!(rule.matches(rule.mod_id, &target));
    }

    #[test]
    fn abort_makes_a_plan_unready_even_when_everything_is_decided() {
        let mut plan = plan_with(vec![planned_at("a", FileClassification::UnmanagedExisting)]);
        plan.apply_decision(
            &plan.files[0].target.clone(),
            &Decision {
                choice: ConflictChoice::Abort,
                scope: DecisionScope::ThisFile,
            },
        );
        assert_eq!(plan.unresolved().count(), 0);
        assert!(!plan.is_ready());
    }

    #[test]
    fn byte_count_only_includes_files_that_are_written() {
        let mut plan = plan_with(vec![
            planned_at("a", FileClassification::Create),
            planned_at("b", FileClassification::Identical),
            planned_at("c", FileClassification::SkippedByRule),
        ]);
        plan.sort();
        assert_eq!(plan.bytes_to_write(), 10);
        assert_eq!(plan.summary().len(), 3);
        assert!(plan.is_ready());
    }
}
