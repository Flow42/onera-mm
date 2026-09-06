//! Where a game's archives are extracted while an install is in flight.
//!
//! Staging is a working area, not a store. An install extracts its archive into
//! a directory of its own under the game's staging root, deploys out of it, and
//! deletes it; anything still there at startup belonged to an operation that
//! never finished and is swept. **Everything under a staging root is Onera's to
//! delete**, which is the single fact that shapes every rule below.
//!
//! One root under `$XDG_STATE_HOME` serves every game by default. The reason to
//! move one is physical: extracting on one filesystem and deploying to another
//! copies every byte twice, so a game on a second disk is better staged on that
//! disk. That is a preference about how Onera works, not a fact about the game,
//! which is why it lives in its own table rather than on the installation row.

use crate::flow::Onera;
use onera_core::ids::LocalGameId;
use onera_core::{CoreError, Result};
use std::path::{Path, PathBuf};

/// What a game's staging root is, and what is currently in it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StagingInfo {
    /// The directory extractions happen under.
    pub root: PathBuf,
    /// Whether that is Onera's own default rather than a chosen directory.
    pub is_default: bool,
    /// Entries currently in it — always work in progress or leftovers.
    pub entries: usize,
}

/// What changing a staging root did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StagingChange {
    /// The new root.
    pub root: PathBuf,
    /// Entries carried over from the old root.
    pub moved: usize,
    /// The root that was left behind.
    pub previous: PathBuf,
}

impl Onera {
    /// The directory this game's archives are extracted under.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn staging_root(&self, game: LocalGameId) -> Result<PathBuf> {
        Ok(self
            .database()
            .staging_root(game)
            .await?
            .unwrap_or_else(|| self.paths.staging()))
    }

    /// A staging directory for one operation of one game.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn staging_for(
        &self,
        game: LocalGameId,
        operation: onera_core::ids::OperationId,
    ) -> Result<PathBuf> {
        Ok(self.staging_root(game).await?.join(operation.to_string()))
    }

    /// The staging root and what it holds, for display.
    ///
    /// # Errors
    /// Propagates database errors. A root that cannot be read reports no
    /// entries rather than failing: it is a directory Onera creates on demand,
    /// and not having been created yet is not an error.
    pub async fn staging_info(&self, game: LocalGameId) -> Result<StagingInfo> {
        let custom = self.database().staging_root(game).await?;
        let root = custom.clone().unwrap_or_else(|| self.paths.staging());
        Ok(StagingInfo {
            entries: count_entries(&root).await,
            is_default: custom.is_none(),
            root,
        })
    }

    /// Point a game's extractions at another directory.
    ///
    /// The directory must be empty, because Onera deletes what it finds in a
    /// staging root; anything already in the *old* root is an unfinished
    /// operation, so it is carried across rather than abandoned.
    ///
    /// # Errors
    /// Refuses a relative path, a path that is not a directory, a directory
    /// with anything in it, and any directory that holds something Onera or the
    /// user cares about — a game, Onera's own data, or a home directory.
    /// Propagates database and filesystem errors.
    pub async fn set_staging_root(
        &self,
        game: LocalGameId,
        chosen: &Path,
    ) -> Result<StagingChange> {
        let previous = self.staging_root(game).await?;
        let root = self.vet_staging_root(chosen).await?;
        if root == previous {
            return Ok(StagingChange {
                moved: 0,
                previous,
                root,
            });
        }

        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|error| CoreError::fs(&root, error))?;
        let moved = move_entries(&previous, &root).await?;

        // Recorded last: a database row pointing at a directory the move never
        // reached would send the next extraction somewhere half-prepared.
        self.database()
            .set_staging_root(game, Some(&root))
            .await?;
        Ok(StagingChange {
            root,
            moved,
            previous,
        })
    }

    /// Return a game to Onera's own staging root, carrying its work across.
    ///
    /// # Errors
    /// As [`Onera::set_staging_root`].
    pub async fn reset_staging_root(&self, game: LocalGameId) -> Result<StagingChange> {
        let previous = self.staging_root(game).await?;
        let root = self.paths.staging();
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|error| CoreError::fs(&root, error))?;
        let moved = if root == previous {
            0
        } else {
            move_entries(&previous, &root).await?
        };
        self.database().set_staging_root(game, None).await?;
        Ok(StagingChange {
            root,
            moved,
            previous,
        })
    }

    /// Check a candidate directory, and answer with the path to record.
    ///
    /// The refusals are not paperwork. A staging root is swept wholesale at
    /// startup, so accepting a directory that holds anything — or that contains
    /// a game, Onera's own state, or a home directory — would turn a settings
    /// change into data loss on the next launch.
    async fn vet_staging_root(&self, chosen: &Path) -> Result<PathBuf> {
        if !chosen.is_absolute() {
            return Err(CoreError::InvalidInput(
                "the staging directory must be an absolute path".to_owned(),
            ));
        }
        // Resolved through the filesystem where it exists, so that a symlink or
        // a `..` cannot be used to name a protected directory in disguise.
        let root = match tokio::fs::canonicalize(chosen).await {
            Ok(resolved) => resolved,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => chosen.to_path_buf(),
            Err(error) => return Err(CoreError::fs(chosen, error)),
        };

        match tokio::fs::metadata(&root).await {
            Ok(metadata) if !metadata.is_dir() => {
                return Err(CoreError::InvalidInput(format!(
                    "{} is not a directory",
                    root.display()
                )))
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(CoreError::fs(&root, error)),
        }
        if count_entries(&root).await > 0 {
            return Err(CoreError::InvalidInput(format!(
                "{} is not empty. Onera deletes everything in a staging directory when it starts, \
                 so it will only use one that has nothing in it",
                root.display()
            )));
        }

        // Two different dangers, and conflating them would either allow one or
        // refuse half the sensible answers.
        for protected in self.must_not_be_swallowed().await? {
            if root == protected || protected.starts_with(&root) {
                return Err(CoreError::InvalidInput(format!(
                    "{} is or contains {} — Onera clears a staging directory when it starts, so \
                     it will not use one that holds anything you would miss",
                    root.display(),
                    protected.display()
                )));
            }
        }
        for game_directory in self.game_directories().await? {
            if root.starts_with(&game_directory) {
                return Err(CoreError::InvalidInput(format!(
                    "{} is inside {} — staging inside a game would make an extraction in progress \
                     look like a modified game file",
                    root.display(),
                    game_directory.display()
                )));
            }
        }
        Ok(root)
    }

    /// Directories a staging root must not be, or contain.
    ///
    /// Being *inside* one of these is a different question: a staging directory
    /// in the user's home is an ordinary choice, and refusing it would rule out
    /// the obvious answer. What must never happen is a staging root that has
    /// one of them underneath it, because the startup sweep would take it.
    async fn must_not_be_swallowed(&self) -> Result<Vec<PathBuf>> {
        let mut protected = vec![
            self.paths.data.clone(),
            self.paths.config.clone(),
            self.paths.cache.clone(),
            self.paths.logs(),
        ];
        if let Some(home) = dirs::home_dir() {
            protected.push(home);
        }
        protected.extend(self.game_directories().await?);
        Ok(protected)
    }

    /// Every directory that belongs to a registered game.
    ///
    /// A staging root inside one of these would put half-extracted files where
    /// verification and the baseline look for the game's own, so it is refused
    /// even when the directory itself is empty and safe to clear.
    async fn game_directories(&self) -> Result<Vec<PathBuf>> {
        let mut directories = Vec::new();
        for install in self.database().local_installs().await? {
            directories.push(install.install_root.clone());
            directories.extend(install.user_data_roots.iter().cloned());
            directories.extend(install.compat_prefix.clone());
        }
        Ok(directories)
    }
}

/// How many entries a directory holds. A missing directory holds none.
async fn count_entries(root: &Path) -> usize {
    let Ok(mut entries) = tokio::fs::read_dir(root).await else {
        return 0;
    };
    let mut count = 0;
    while let Ok(Some(_)) = entries.next_entry().await {
        count += 1;
    }
    count
}

/// Move everything in `from` into `to`, preferring a rename.
///
/// A rename is one syscall and keeps a half-extracted archive intact; across a
/// filesystem boundary — the very case that makes a user move a staging root —
/// it fails, and the entry is copied and then removed instead.
async fn move_entries(from: &Path, to: &Path) -> Result<usize> {
    let Ok(mut entries) = tokio::fs::read_dir(from).await else {
        return Ok(0);
    };
    let mut moved = 0;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| CoreError::fs(from, error))?
    {
        let source = entry.path();
        let target = to.join(entry.file_name());
        if tokio::fs::rename(&source, &target).await.is_err() {
            copy_recursively(&source, &target).await?;
            remove_recursively(&source).await?;
        }
        moved += 1;
    }
    Ok(moved)
}

/// Copy one file or directory tree.
///
/// Boxed because it recurses: an `async fn` that awaits itself needs its future
/// on the heap, and a staging tree is as deep as the archive that made it.
fn copy_recursively<'a>(
    source: &'a Path,
    target: &'a Path,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
        let metadata = tokio::fs::symlink_metadata(source)
            .await
            .map_err(|error| CoreError::fs(source, error))?;
        if metadata.is_dir() {
            tokio::fs::create_dir_all(target)
                .await
                .map_err(|error| CoreError::fs(target, error))?;
            let mut entries = tokio::fs::read_dir(source)
                .await
                .map_err(|error| CoreError::fs(source, error))?;
            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|error| CoreError::fs(source, error))?
            {
                copy_recursively(&entry.path(), &target.join(entry.file_name())).await?;
            }
            return Ok(());
        }
        // Symlinks are copied as links rather than followed: a staged tree can
        // contain one, and following it would copy something outside the tree.
        if metadata.is_symlink() {
            let destination = tokio::fs::read_link(source)
                .await
                .map_err(|error| CoreError::fs(source, error))?;
            return tokio::fs::symlink(&destination, target)
                .await
                .map_err(|error| CoreError::fs(target, error));
        }
        tokio::fs::copy(source, target)
            .await
            .map(|_| ())
            .map_err(|error| CoreError::fs(target, error))
    })
}

/// Delete one file or directory tree.
async fn remove_recursively(path: &Path) -> Result<()> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|error| CoreError::fs(path, error))?;
    let result = if metadata.is_dir() {
        tokio::fs::remove_dir_all(path).await
    } else {
        tokio::fs::remove_file(path).await
    };
    result.map_err(|error| CoreError::fs(path, error))
}
