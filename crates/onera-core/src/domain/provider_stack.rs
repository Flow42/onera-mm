//! The file-provider stack.
//!
//! Every deployed relative path under a deployment root owns a *stack* of
//! providers rather than a single owner. The top of the stack is what is
//! currently on disk; everything beneath it is what would come back if the top
//! were removed.
//!
//! This single structure gives Onera all three restoration behaviours the
//! design requires:
//!
//! * removing a mod that overrode another mod's file restores that other mod's
//!   file, because the other mod's entry is still on the stack;
//! * removing a mod that overrode a pre-existing, unmanaged file restores the
//!   backup, because [`FileProvider::UnmanagedBackup`] sits at the bottom;
//! * downgrading a mod restores the previous release of the *same* mod, because
//!   each installation is a distinct entry.
//!
//! Shared ownership of identical content falls out of the same model: when two
//! installations provide byte-identical content for a path, both are on the
//! stack, and removing either one leaves the bytes on disk untouched because
//! the new top has the same hash.

use crate::hash::FileHash;
use crate::ids::{BackupId, InstallationId};
use serde::{Deserialize, Serialize};

/// One entry in a path's provider stack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileProvider {
    /// A file that existed before Onera touched this path, saved aside.
    UnmanagedBackup {
        /// Backup that holds the original bytes.
        backup_id: BackupId,
    },
    /// A file deployed by one of Onera's installations.
    Installation {
        /// The installation that deployed it.
        installation_id: InstallationId,
    },
}

impl FileProvider {
    /// The installation that owns this entry, if any.
    #[must_use]
    pub fn installation_id(&self) -> Option<InstallationId> {
        match self {
            Self::Installation { installation_id } => Some(*installation_id),
            Self::UnmanagedBackup { .. } => None,
        }
    }

    /// Whether this entry is a pre-existing unmanaged file.
    #[must_use]
    pub fn is_unmanaged(&self) -> bool {
        matches!(self, Self::UnmanagedBackup { .. })
    }
}

/// A provider together with the content it supplies for the path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackEntry {
    /// Who supplies the content.
    pub provider: FileProvider,
    /// BLAKE3 hash of the content this provider supplies.
    pub hash: FileHash,
    /// Size in bytes of that content.
    pub size: u64,
}

/// The ordered set of providers for a single deployed path.
///
/// Index `0` is the bottom of the stack (the oldest provider); the last element
/// is the top (what is currently deployed).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderStack {
    entries: Vec<StackEntry>,
}

/// What the installer must do to disk after a stack mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreAction {
    /// Nothing on disk changes; the new top has identical content.
    ///
    /// This is the shared-identical-file case.
    Nothing,
    /// Write the given entry's content back to the path.
    Restore(StackEntry),
    /// No provider remains; delete the path.
    Delete,
}

impl ProviderStack {
    /// An empty stack, meaning Onera has never deployed to this path.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a stack from bottom-to-top entries.
    #[must_use]
    pub fn from_entries(entries: Vec<StackEntry>) -> Self {
        Self { entries }
    }

    /// Bottom-to-top view of the stack.
    #[must_use]
    pub fn entries(&self) -> &[StackEntry] {
        &self.entries
    }

    /// The entry currently deployed at this path.
    #[must_use]
    pub fn top(&self) -> Option<&StackEntry> {
        self.entries.last()
    }

    /// Whether the stack has no providers at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of providers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether a pre-existing unmanaged file is recorded at the bottom.
    #[must_use]
    pub fn has_unmanaged_original(&self) -> bool {
        self.entries
            .first()
            .is_some_and(|e| e.provider.is_unmanaged())
    }

    /// Installations that currently claim this path, bottom to top.
    pub fn claiming_installations(&self) -> impl Iterator<Item = InstallationId> + '_ {
        self.entries
            .iter()
            .filter_map(|e| e.provider.installation_id())
    }

    /// Push a provider on top, or update it in place if it is already present.
    ///
    /// Re-installing the same installation must not create a duplicate entry,
    /// so an existing entry for the same provider is updated and moved to the
    /// top. Returns whether disk content needs to change.
    pub fn push(&mut self, entry: StackEntry) -> RestoreAction {
        let previous_top_hash = self.top().map(|e| e.hash.clone());
        self.entries.retain(|e| e.provider != entry.provider);
        self.entries.push(entry);
        let new_top = self.entries.last().expect("just pushed");
        if previous_top_hash.as_ref() == Some(&new_top.hash) {
            RestoreAction::Nothing
        } else {
            RestoreAction::Restore(new_top.clone())
        }
    }

    /// Remove an installation's claim and report what disk should look like.
    ///
    /// Removing a provider that is not on top only rewrites history: the file on
    /// disk keeps belonging to whoever is still on top, so the action is
    /// [`RestoreAction::Nothing`].
    pub fn remove_installation(&mut self, installation_id: InstallationId) -> RestoreAction {
        let was_top =
            self.top().and_then(|e| e.provider.installation_id()) == Some(installation_id);
        let before = self.entries.len();
        let old_top_hash = self.top().map(|e| e.hash.clone());
        self.entries
            .retain(|e| e.provider.installation_id() != Some(installation_id));
        if self.entries.len() == before {
            return RestoreAction::Nothing;
        }
        if !was_top {
            return RestoreAction::Nothing;
        }
        match self.entries.last() {
            None => RestoreAction::Delete,
            Some(next) if Some(&next.hash) == old_top_hash.as_ref() => RestoreAction::Nothing,
            Some(next) => RestoreAction::Restore(next.clone()),
        }
    }

    /// Remove the unmanaged original from the bottom of the stack.
    ///
    /// Used when the user chooses to *adopt* a pre-existing file permanently, so
    /// its backup can be discarded.
    pub fn forget_unmanaged_original(&mut self) -> Option<BackupId> {
        let first = self.entries.first()?;
        let FileProvider::UnmanagedBackup { backup_id } = first.provider else {
            return None;
        };
        self.entries.remove(0);
        Some(backup_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(provider: FileProvider, content: &[u8]) -> StackEntry {
        StackEntry {
            provider,
            hash: FileHash::blake3_of(content),
            size: content.len() as u64,
        }
    }

    fn install(id: InstallationId, content: &[u8]) -> StackEntry {
        entry(
            FileProvider::Installation {
                installation_id: id,
            },
            content,
        )
    }

    fn unmanaged(id: BackupId, content: &[u8]) -> StackEntry {
        entry(FileProvider::UnmanagedBackup { backup_id: id }, content)
    }

    #[test]
    fn empty_stack_has_no_top() {
        let s = ProviderStack::new();
        assert!(s.is_empty());
        assert_eq!(s.top(), None);
        assert!(!s.has_unmanaged_original());
    }

    #[test]
    fn removing_a_mod_restores_the_mod_underneath() {
        let (a, b) = (InstallationId::new(), InstallationId::new());
        let mut s = ProviderStack::new();
        s.push(install(a, b"from mod a"));
        assert_eq!(
            s.push(install(b, b"from mod b")),
            RestoreAction::Restore(install(b, b"from mod b"))
        );

        // Removing the overriding mod brings mod A's file back.
        assert_eq!(
            s.remove_installation(b),
            RestoreAction::Restore(install(a, b"from mod a"))
        );
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn removing_the_last_mod_deletes_the_file() {
        let a = InstallationId::new();
        let mut s = ProviderStack::new();
        s.push(install(a, b"x"));
        assert_eq!(s.remove_installation(a), RestoreAction::Delete);
        assert!(s.is_empty());
    }

    #[test]
    fn removing_a_mod_restores_the_unmanaged_original() {
        let backup = BackupId::new();
        let a = InstallationId::new();
        let mut s = ProviderStack::new();
        s.push(unmanaged(backup, b"vanilla"));
        s.push(install(a, b"modded"));

        assert_eq!(
            s.remove_installation(a),
            RestoreAction::Restore(unmanaged(backup, b"vanilla"))
        );
        assert!(s.has_unmanaged_original());
    }

    #[test]
    fn identical_content_is_shared_and_survives_removal() {
        let (a, b) = (InstallationId::new(), InstallationId::new());
        let mut s = ProviderStack::new();
        s.push(install(a, b"same bytes"));
        // The second mod ships an identical file: nothing to write.
        assert_eq!(s.push(install(b, b"same bytes")), RestoreAction::Nothing);

        // Removing either leaves the bytes in place, because the remaining
        // provider supplies the same content.
        assert_eq!(s.remove_installation(b), RestoreAction::Nothing);
        assert_eq!(s.len(), 1);
        assert_eq!(s.remove_installation(a), RestoreAction::Delete);
    }

    #[test]
    fn same_mod_upgrade_replaces_its_own_entry_without_duplicating() {
        let a = InstallationId::new();
        let mut s = ProviderStack::new();
        s.push(install(a, b"v1"));
        assert_eq!(
            s.push(install(a, b"v2")),
            RestoreAction::Restore(install(a, b"v2"))
        );
        assert_eq!(
            s.len(),
            1,
            "re-install must not stack a second entry for the same installation"
        );
        assert_eq!(s.top().unwrap().hash, FileHash::blake3_of(b"v2"));
    }

    #[test]
    fn downgrade_restores_the_previous_release_of_the_same_mod() {
        // Each release installs as its own installation, so a downgrade is just
        // a new entry whose content is the older bytes.
        let (v2, v1) = (InstallationId::new(), InstallationId::new());
        let mut s = ProviderStack::new();
        s.push(install(v2, b"v2 bytes"));
        s.push(install(v1, b"v1 bytes"));
        assert_eq!(s.top().unwrap().hash, FileHash::blake3_of(b"v1 bytes"));
        assert_eq!(
            s.remove_installation(v1),
            RestoreAction::Restore(install(v2, b"v2 bytes"))
        );
    }

    #[test]
    fn removing_a_buried_provider_does_not_touch_disk() {
        let (a, b) = (InstallationId::new(), InstallationId::new());
        let mut s = ProviderStack::new();
        s.push(install(a, b"a"));
        s.push(install(b, b"b"));
        // A is buried under B; removing it must not change what is deployed.
        assert_eq!(s.remove_installation(a), RestoreAction::Nothing);
        assert_eq!(s.top().unwrap().hash, FileHash::blake3_of(b"b"));
    }

    #[test]
    fn removing_an_absent_installation_is_a_no_op() {
        let mut s = ProviderStack::new();
        s.push(install(InstallationId::new(), b"a"));
        assert_eq!(
            s.remove_installation(InstallationId::new()),
            RestoreAction::Nothing
        );
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn adopting_an_unmanaged_file_drops_its_backup() {
        let backup = BackupId::new();
        let mut s = ProviderStack::new();
        s.push(unmanaged(backup, b"vanilla"));
        s.push(install(InstallationId::new(), b"modded"));
        assert_eq!(s.forget_unmanaged_original(), Some(backup));
        assert!(!s.has_unmanaged_original());
        assert_eq!(s.forget_unmanaged_original(), None);
    }

    #[test]
    fn deep_stacks_unwind_in_order() {
        let ids: Vec<_> = (0..5).map(|_| InstallationId::new()).collect();
        let mut s = ProviderStack::new();
        for (i, id) in ids.iter().enumerate() {
            s.push(install(*id, format!("content {i}").as_bytes()));
        }
        for i in (1..5).rev() {
            assert_eq!(
                s.remove_installation(ids[i]),
                RestoreAction::Restore(install(
                    ids[i - 1],
                    format!("content {}", i - 1).as_bytes()
                ))
            );
        }
        assert_eq!(s.remove_installation(ids[0]), RestoreAction::Delete);
    }
}
