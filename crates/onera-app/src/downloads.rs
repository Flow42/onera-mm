//! Where downloaded archives are kept.
//!
//! Onera stores an archive by its content hash, so the directory it sits in is
//! not part of its identity — which is exactly what makes this configurable at
//! all. Three answers, most specific first:
//!
//! 1. the directory chosen for **this game**;
//! 2. the directory chosen for **everything**;
//! 3. Onera's own, under `$XDG_DATA_HOME`.
//!
//! The reasons to move it are ordinary: a mod collection is tens of gigabytes
//! and a system disk often is not, and someone who keeps one game on a second
//! drive usually wants that game's downloads there too.
//!
//! Two things follow from archives being *finished* files rather than work in
//! progress, and both are the opposite of how a staging directory behaves:
//! a directory with other things in it is perfectly acceptable, because nothing
//! here is ever swept; and moving one has to rewrite `archives.stored_path` for
//! every file it moves, because the database points at them by absolute path.

use crate::flow::Onera;
use onera_core::ids::LocalGameId;
use onera_core::ports::ArchiveStore as _;
use onera_core::{CoreError, Result};
use onera_db::catalog::StoredArchive;
use onera_download::ContentAddressedStore;
use std::path::{Path, PathBuf};

/// Which setting a download directory came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadScope {
    /// Onera's own directory: nothing has been chosen.
    Default,
    /// The directory chosen for every game.
    Global,
    /// The directory chosen for this game, overriding the shared one.
    Game,
}

/// A download directory and what is in it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DownloadDirInfo {
    /// The directory archives are written to.
    pub root: PathBuf,
    /// Which setting decided that.
    pub scope: DownloadScope,
    /// The directory that would be used if this game's override were removed.
    ///
    /// `None` when asking about the global setting itself.
    pub inherited: Option<PathBuf>,
    /// Archives Onera has recorded in it.
    pub archives: usize,
    /// What they occupy, as the catalogue recorded their sizes.
    pub bytes: u64,
}

/// What moving a download directory did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DownloadDirChange {
    /// The directory now in use.
    pub root: PathBuf,
    /// Archives moved into it.
    pub moved: usize,
    /// Bytes those archives occupy.
    pub bytes: u64,
    /// The directory that was in use before.
    pub previous: PathBuf,
}

impl Onera {
    /// Where a download for this game is written.
    ///
    /// `None` means a download that belongs to no registered game — the CLI
    /// asking for a file of a game Onera does not manage — which uses whatever
    /// the shared setting says.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn download_root(&self, game: Option<LocalGameId>) -> Result<PathBuf> {
        if let Some(game) = game {
            if let Some(root) = self.database().game_download_root(game).await? {
                return Ok(root);
            }
        }
        Ok(self
            .database()
            .download_root()
            .await?
            .unwrap_or_else(|| self.paths.archives()))
    }

    /// The same answer, for a provider game slug rather than a registered game.
    ///
    /// A download names the game it is for by the provider's slug; the setting
    /// belongs to a registered installation. A slug that matches exactly one
    /// confirmed installation is that installation — and one that matches two
    /// is not a coin toss, so it falls back to the shared setting.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn download_root_for_slug(&self, game_slug: &str) -> Result<PathBuf> {
        let game = self.local_game_for_slug(game_slug).await?;
        self.download_root(game).await
    }

    /// A download directory and what it holds, for display.
    ///
    /// # Errors
    /// Propagates database errors.
    pub async fn download_dir_info(&self, game: Option<LocalGameId>) -> Result<DownloadDirInfo> {
        let global = self.database().download_root().await?;
        let default = self.paths.archives();
        let shared = global.clone().unwrap_or_else(|| default.clone());

        let (root, scope, inherited) = match game {
            Some(game) => match self.database().game_download_root(game).await? {
                Some(root) => (root, DownloadScope::Game, Some(shared)),
                None if global.is_some() => (shared.clone(), DownloadScope::Global, Some(shared)),
                None => (default, DownloadScope::Default, Some(shared)),
            },
            None if global.is_some() => (shared, DownloadScope::Global, None),
            None => (default, DownloadScope::Default, None),
        };

        // Counted from the catalogue rather than from the directory: what is on
        // disk under a shared root may belong to another game, and a file Onera
        // has no row for is not something it can claim to be holding.
        let mut archives = 0;
        let mut bytes = 0;
        for stored in self.database().archives().await? {
            if stored.path.starts_with(&root) {
                archives += 1;
                bytes += stored.size;
            }
        }
        Ok(DownloadDirInfo {
            root,
            scope,
            inherited,
            archives,
            bytes,
        })
    }

    /// Move downloads for every game that has not chosen its own directory.
    ///
    /// # Errors
    /// As [`Onera::set_game_download_root`].
    pub async fn set_download_root(&self, chosen: &Path) -> Result<DownloadDirChange> {
        let previous = self.download_root(None).await?;
        let root = self.vet_download_root(chosen).await?;
        if root == previous {
            return Ok(DownloadDirChange {
                moved: 0,
                bytes: 0,
                previous,
                root,
            });
        }

        // Everything currently under the shared directory: an archive that a
        // game's own setting already moved elsewhere is not under it, so it is
        // left exactly where that setting put it.
        let moving: Vec<_> = self
            .database()
            .archives()
            .await?
            .into_iter()
            .filter(|stored| stored.path.starts_with(&previous))
            .collect();
        let (moved, bytes) = self.relocate(&moving, &root).await?;

        self.database().set_download_root(Some(&root)).await?;
        Ok(DownloadDirChange {
            root,
            moved,
            bytes,
            previous,
        })
    }

    /// Move one game's downloads to a directory of its own.
    ///
    /// # Errors
    /// Refuses a relative path, a path that is not a directory, and any
    /// directory inside a registered game or inside a staging root — the first
    /// would make an archive look like a modified game file, the second would
    /// see it deleted on the next startup. Propagates database and filesystem
    /// errors; a move that fails partway leaves every file it did move
    /// correctly recorded.
    pub async fn set_game_download_root(
        &self,
        game: LocalGameId,
        chosen: &Path,
    ) -> Result<DownloadDirChange> {
        let previous = self.download_root(Some(game)).await?;
        let root = self.vet_download_root(chosen).await?;
        if root == previous {
            return Ok(DownloadDirChange {
                moved: 0,
                bytes: 0,
                previous,
                root,
            });
        }

        let moving = self.archives_of_game(game).await?;
        let (moved, bytes) = self.relocate(&moving, &root).await?;

        self.database()
            .set_game_download_root(game, Some(&root))
            .await?;
        Ok(DownloadDirChange {
            root,
            moved,
            bytes,
            previous,
        })
    }

    /// Return the shared setting to Onera's own directory, moving what it holds.
    ///
    /// # Errors
    /// As [`Onera::set_download_root`].
    pub async fn reset_download_root(&self) -> Result<DownloadDirChange> {
        let previous = self.download_root(None).await?;
        let root = self.paths.archives();
        let moving: Vec<_> = self
            .database()
            .archives()
            .await?
            .into_iter()
            .filter(|stored| stored.path.starts_with(&previous))
            .collect();
        let (moved, bytes) = if root == previous {
            (0, 0)
        } else {
            self.relocate(&moving, &root).await?
        };
        self.database().set_download_root(None).await?;
        Ok(DownloadDirChange {
            root,
            moved,
            bytes,
            previous,
        })
    }

    /// Drop one game's own directory, moving its downloads to the shared one.
    ///
    /// # Errors
    /// As [`Onera::set_game_download_root`].
    pub async fn reset_game_download_root(&self, game: LocalGameId) -> Result<DownloadDirChange> {
        let previous = self.download_root(Some(game)).await?;
        let root = self.download_root(None).await?;
        let moving = self.archives_of_game(game).await?;
        let (moved, bytes) = if root == previous {
            (0, 0)
        } else {
            self.relocate(&moving, &root).await?
        };
        self.database().set_game_download_root(game, None).await?;
        Ok(DownloadDirChange {
            root,
            moved,
            bytes,
            previous,
        })
    }

    /// The archives belonging to one registered game's mods.
    async fn archives_of_game(&self, game: LocalGameId) -> Result<Vec<StoredArchive>> {
        let install = self
            .database()
            .local_installs()
            .await?
            .into_iter()
            .find(|candidate| candidate.id == game)
            .ok_or_else(|| CoreError::NotFound {
                kind: "game installation",
                id: game.to_string(),
            })?;
        let adapter = onera_games::adapter_by_id(&install.adapter_id)
            .ok_or_else(|| CoreError::Unsupported(format!("no adapter named {:?}", install.adapter_id)))?;

        let mut archives = Vec::new();
        for slug in adapter.provider_slugs() {
            archives.extend(self.database().archives_for_game_slug(slug).await?);
        }
        // An adapter can claim more than one slug, and one archive can be
        // reached through more than one of them.
        archives.sort_by_key(|stored| stored.id.to_string());
        archives.dedup_by_key(|stored| stored.id);
        Ok(archives)
    }

    /// Move archives into `root`, keeping the catalogue pointing at them.
    ///
    /// The database row is rewritten immediately after each file lands, not at
    /// the end: a failure halfway through then leaves a directory that is half
    /// moved and a catalogue that is entirely correct, which is recoverable by
    /// running the change again.
    async fn relocate(&self, archives: &[StoredArchive], root: &Path) -> Result<(usize, u64)> {
        tokio::fs::create_dir_all(root)
            .await
            .map_err(|error| CoreError::fs(root, error))?;

        let store = ContentAddressedStore::new(root.to_path_buf());
        let mut moved = 0;
        let mut bytes = 0;
        for stored in archives {
            let current = &stored.path;
            let target = store.path_for(&stored.hash);
            if target == *current {
                continue;
            }
            match tokio::fs::metadata(current).await {
                // The file is not where the catalogue says. Moving it is not
                // possible and inventing a new location would be a lie, so it
                // is left for verification to report.
                Err(_) => continue,
                Ok(metadata) => bytes += metadata.len(),
            }
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|error| CoreError::fs(parent, error))?;
            }
            if tokio::fs::rename(current, &target).await.is_err() {
                // Across a filesystem boundary — the usual reason to move a
                // download directory at all — a rename cannot work.
                tokio::fs::copy(current, &target)
                    .await
                    .map_err(|error| CoreError::fs(&target, error))?;
                tokio::fs::remove_file(current)
                    .await
                    .map_err(|error| CoreError::fs(current, error))?;
            }
            self.database().set_archive_path(stored.id, &target).await?;
            moved += 1;
        }
        Ok((moved, bytes))
    }

    /// Check a candidate directory and answer with the path to record.
    ///
    /// Far fewer refusals than a staging root needs, and deliberately: nothing
    /// deletes the contents of a download directory, so one with other files in
    /// it is fine. What is refused is a location that would make the archives
    /// someone else's problem — inside a game, where verification would see
    /// them, or inside a staging root, where the next startup would delete them.
    async fn vet_download_root(&self, chosen: &Path) -> Result<PathBuf> {
        if !chosen.is_absolute() {
            return Err(CoreError::InvalidInput(
                "the download directory must be an absolute path".to_owned(),
            ));
        }
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

        for install in self.database().local_installs().await? {
            let mut game_directories = vec![install.install_root.clone()];
            game_directories.extend(install.user_data_roots.iter().cloned());
            game_directories.extend(install.compat_prefix.clone());
            for directory in game_directories {
                if root.starts_with(&directory) {
                    return Err(CoreError::InvalidInput(format!(
                        "{} is inside {} — an archive kept inside a game would be read as a \
                         modified game file",
                        root.display(),
                        directory.display()
                    )));
                }
            }
        }

        let mut staging_roots = vec![self.paths.staging()];
        staging_roots.extend(self.database().staging_roots().await?);
        for staging in staging_roots {
            if root.starts_with(&staging) {
                return Err(CoreError::InvalidInput(format!(
                    "{} is inside the staging directory {}, which Onera clears when it starts",
                    root.display(),
                    staging.display()
                )));
            }
        }
        Ok(root)
    }
}
