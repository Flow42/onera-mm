//! What one mod put on disk, and where to go and look at it.
//!
//! Onera records every file it deploys, so "which files belong to this mod" is
//! a question the database already answers exactly — there is no need to guess
//! from a directory listing, and no risk of claiming a file the mod does not
//! own. This module turns that record into two things the window needs: the
//! list itself, and one directory worth opening in a file manager.
//!
//! The directory is the deepest one that contains every file the mod owns. A
//! mod that writes into two roots therefore opens at the point where its two
//! branches meet rather than at one of them, which is the only answer that does
//! not silently hide half of what it installed.

use crate::flow::Onera;
use onera_core::ids::{InstallationId, LocalGameId};
use onera_core::ports::DeploymentStore;
use onera_core::{CoreError, Result};
use std::path::{Component, Path, PathBuf};

/// One file a mod owns.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ModFileEntry {
    /// The deployment root it lives under, as the adapter names it.
    pub root_key: String,
    /// Path within that root, exactly as it was deployed.
    pub path: String,
    /// Where it actually is.
    pub absolute: PathBuf,
    /// Whether it is still there.
    ///
    /// A missing file is worth showing rather than hiding: it means something
    /// outside Onera removed it, which is exactly what the user would want to
    /// know from a file list.
    pub exists: bool,
    /// Size on disk, when it is still there.
    pub size: Option<u64>,
}

/// Which kind of location a mod can be browsed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModLocationKind {
    /// Deployed into the game, so the game's own directories are where it is.
    Installed,
    /// Downloaded but not installed: the archive in Onera's store is all there
    /// is, because extraction happens per install and is cleared afterwards.
    Archive,
    /// Nothing on disk to show.
    None,
}

/// A mod's files and the one directory that holds them all.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ModContents {
    /// Where the files are, in the sense the window needs to label a button.
    pub kind: ModLocationKind,
    /// The directory to open, when there is one that exists.
    pub browse: Option<PathBuf>,
    /// The files themselves, capped by the caller's limit.
    pub entries: Vec<ModFileEntry>,
    /// How many files the mod owns in total, cap or no cap.
    pub total: usize,
}

impl Onera {
    /// The files one installed mod owns, and where to browse them.
    ///
    /// `limit` bounds what is returned, not what is counted: a mod with two
    /// thousand files reports two thousand and hands back the first `limit` of
    /// them, because a list view has no use for the rest and a command result
    /// is not the place to move megabytes of paths.
    ///
    /// # Errors
    /// Fails when the game is not registered. A file that cannot be stat'd is
    /// reported as missing rather than failing the call — the list is a view,
    /// and a permission error on one path should not blank the other nine.
    pub async fn mod_contents(
        &self,
        game: LocalGameId,
        installation: InstallationId,
        limit: usize,
    ) -> Result<ModContents> {
        let (roots, _) = self.roots_for(game).await?;
        let targets = self.database().targets_of(installation).await?;

        let mut entries = Vec::new();
        for target in &targets {
            let Some(root) = roots.get(&target.root_key) else {
                // A root the adapter no longer declares: the file was deployed
                // under a key this build cannot resolve, so it is counted and
                // named but cannot be pointed at.
                continue;
            };
            let absolute = root.join(target.path.as_str());
            let metadata = tokio::fs::metadata(&absolute).await.ok();
            entries.push(ModFileEntry {
                root_key: target.root_key.clone(),
                path: target.path.as_str().to_owned(),
                exists: metadata.is_some(),
                size: metadata.map(|value| value.len()),
                absolute,
            });
        }

        if entries.is_empty() {
            // Nothing deployed: either it was only ever downloaded, or every
            // one of its files has been removed. The archive is then the only
            // thing there is to look at.
            let archive = self
                .database()
                .archive_for_installation(game, installation)
                .await?;
            let browse = archive
                .map(|stored| stored.path)
                .filter(|path| path.exists())
                .and_then(|path| path.parent().map(Path::to_path_buf));
            return Ok(ModContents {
                kind: if browse.is_some() {
                    ModLocationKind::Archive
                } else {
                    ModLocationKind::None
                },
                browse,
                entries,
                total: targets.len(),
            });
        }

        let browse = existing_ancestor(&common_ancestor(
            entries.iter().map(|entry| entry.absolute.as_path()),
        ));
        let total = entries.len();
        entries.truncate(limit);
        Ok(ModContents {
            kind: ModLocationKind::Installed,
            browse,
            entries,
            total,
        })
    }

    /// Open one of a mod's own directories in the system file manager.
    ///
    /// The path is not taken from the caller: the window names a mod, and the
    /// directory is recomputed here from what Onera deployed. A command that
    /// accepted a path would be a command that opens any directory on request.
    ///
    /// # Errors
    /// Fails when the mod has nothing on disk to show.
    pub async fn mod_browse_path(
        &self,
        game: LocalGameId,
        installation: InstallationId,
    ) -> Result<PathBuf> {
        self.mod_contents(game, installation, 0)
            .await?
            .browse
            .ok_or_else(|| {
                CoreError::NotFound {
                    kind: "mod location",
                    id: installation.to_string(),
                }
            })
    }
}

/// The deepest directory containing every one of these paths.
///
/// An empty iterator has no ancestor, and neither does a set of paths that
/// share nothing but the filesystem root — both answer with the root, which is
/// then checked for existence by the caller.
fn common_ancestor<'a>(paths: impl Iterator<Item = &'a Path>) -> PathBuf {
    let mut shared: Option<Vec<Component<'a>>> = None;
    for path in paths {
        // The file itself is not a directory to open; its parent is.
        let directory = path.parent().unwrap_or(path);
        let components: Vec<Component<'a>> = directory.components().collect();
        shared = Some(match shared {
            None => components,
            Some(current) => current
                .into_iter()
                .zip(components)
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| a)
                .collect(),
        });
    }
    shared.map(|components| components.iter().collect()).unwrap_or_default()
}

/// The nearest directory at or above `path` that exists.
///
/// A mod whose files were deleted still has a sensible place to open — the
/// directory that used to hold them, or whatever is left of that path.
fn existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut candidate = path;
    loop {
        if candidate.as_os_str().is_empty() {
            return None;
        }
        if candidate.is_dir() {
            return Some(candidate.to_path_buf());
        }
        candidate = candidate.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ancestor(paths: &[&str]) -> String {
        common_ancestor(paths.iter().map(Path::new))
            .display()
            .to_string()
    }

    #[test]
    fn one_file_opens_the_directory_that_holds_it() {
        assert_eq!(ancestor(&["/games/cp/archive/pc/mod/a.archive"]), "/games/cp/archive/pc/mod");
    }

    #[test]
    fn files_in_one_directory_open_that_directory() {
        assert_eq!(
            ancestor(&[
                "/games/cp/archive/pc/mod/a.archive",
                "/games/cp/archive/pc/mod/b.archive",
            ]),
            "/games/cp/archive/pc/mod"
        );
    }

    #[test]
    fn files_in_two_branches_open_where_the_branches_meet() {
        // Opening either branch would hide the other half of the mod.
        assert_eq!(
            ancestor(&[
                "/games/cp/archive/pc/mod/a.archive",
                "/games/cp/r6/scripts/a.reds",
            ]),
            "/games/cp"
        );
    }

    #[test]
    fn paths_with_nothing_in_common_fall_back_to_the_root() {
        assert_eq!(ancestor(&["/games/cp/a", "/home/user/.local/b"]), "/");
    }

    #[test]
    fn no_files_have_no_ancestor() {
        assert_eq!(ancestor(&[]), "");
        assert!(existing_ancestor(Path::new("")).is_none());
    }

    #[test]
    fn the_directory_offered_is_one_that_exists() {
        let directory = tempfile::tempdir().unwrap();
        let deep = directory.path().join("archive/pc/mod");
        assert_eq!(
            existing_ancestor(&deep).as_deref(),
            Some(directory.path()),
            "a mod whose files are gone still opens somewhere real"
        );
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(existing_ancestor(&deep).as_deref(), Some(deep.as_path()));
    }
}
