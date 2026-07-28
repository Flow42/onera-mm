//! Shared fixtures for the installation-engine tests.
//!
//! Builds a complete, real stack — SQLite in memory, a real temporary game
//! directory, real files — with only the filesystem swappable so faults can be
//! injected.

#![allow(dead_code)]

use onera_core::domain::archive::{ArchiveFormat, ArchiveManifest, ManifestFile};
use onera_core::domain::game::{DeployRoot, InstallSource, LocalGameInstall};
use onera_core::hash::FileHash;
use onera_core::ids::*;
use onera_core::paths::DeployRootKind;
use onera_core::plan::TargetLocation;
use onera_core::ports::{FileSystem, GameAdapter, LayoutResolution};
use onera_core::{CoreError, RelPath, Result};
use onera_db::backup::FileBackupStore;
use onera_db::Database;
use onera_install::planner::RootMap;
use onera_install::{Installer, RealFileSystem, Remover};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A game adapter with a single `game` root and no special rules, so the tests
/// exercise the installer rather than a particular game's layout logic.
pub struct FlatAdapter;

impl GameAdapter for FlatAdapter {
    fn id(&self) -> &str {
        "test-flat"
    }
    fn display_name(&self) -> &str {
        "Test Game"
    }
    fn provider_slugs(&self) -> &[&str] {
        &["testgame"]
    }
    fn steam_app_ids(&self) -> &[u32] {
        &[1]
    }
    fn validate_install(&self, _root: &Path) -> onera_core::domain::game::InstallValidation {
        onera_core::domain::game::InstallValidation::ok()
    }
    fn deploy_roots(&self, install: &LocalGameInstall) -> Result<Vec<DeployRoot>> {
        Ok(vec![DeployRoot {
            key: "game".into(),
            kind: DeployRootKind::GameInstall,
            path: install.install_root.clone(),
        }])
    }
    fn resolve_layout(&self, manifest: &ArchiveManifest) -> Result<LayoutResolution> {
        Ok(LayoutResolution {
            mappings: manifest
                .files
                .iter()
                .map(|f| {
                    (
                        f.path.clone(),
                        TargetLocation {
                            root_key: "game".into(),
                            path: f.path.clone(),
                        },
                    )
                })
                .collect(),
            rationale: "flat one-to-one mapping".into(),
            ignored: vec![],
        })
    }
    fn validate_target(&self, target: &TargetLocation) -> Result<()> {
        if target.path.as_str().starts_with("forbidden/") {
            return Err(CoreError::InvalidInput(
                "this game forbids `forbidden/`".into(),
            ));
        }
        Ok(())
    }
}

/// A fully wired test world.
pub struct World {
    pub db: Database,
    pub dir: tempfile::TempDir,
    pub game_dir: PathBuf,
    pub local_game: LocalGameId,
    pub mod_id: ModId,
    pub release: ReleaseId,
    pub archive: ArchiveId,
    pub roots: RootMap,
    pub backups: Arc<FileBackupStore>,
}

impl World {
    pub async fn new() -> Self {
        let db = Database::open_in_memory().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let game_dir = dir.path().join("game");
        std::fs::create_dir_all(&game_dir).unwrap();

        let provider = ProviderId::nexus();
        db.upsert_provider(&provider, "Nexus Mods", "https://api.nexusmods.com/v3")
            .await
            .unwrap();
        let game_id = db
            .upsert_game(&onera_core::domain::game::Game {
                id: GameId::new(),
                provider: provider.clone(),
                provider_slug: "testgame".into(),
                name: "Test Game".into(),
                steam_app_id: Some(1),
            })
            .await
            .unwrap();
        let local_game = db
            .upsert_local_install(&LocalGameInstall {
                id: LocalGameId::new(),
                game_id,
                adapter_id: "test-flat".into(),
                source: InstallSource::Manual,
                install_root: game_dir.clone(),
                compat_prefix: None,
                user_data_roots: vec![],
                confirmed: true,
            })
            .await
            .unwrap();

        let mod_id = db
            .upsert_mod(&onera_core::domain::release::Mod {
                id: ModId::new(),
                provider: provider.clone(),
                provider_mod_id: ProviderModId::new("1"),
                game_slug: "testgame".into(),
                name: "Test Mod".into(),
                author: None,
            })
            .await
            .unwrap();
        let release = db
            .upsert_release(&onera_core::domain::release::Release {
                id: ReleaseId::new(),
                mod_id,
                version: "1.0".into(),
                published_at: chrono::DateTime::from_timestamp(1_000, 0),
                metadata: serde_json::Value::Null,
            })
            .await
            .unwrap();
        let archive = db
            .upsert_archive(
                &FileHash::blake3_of(b"archive"),
                7,
                "mod.zip",
                ArchiveFormat::Zip,
                Path::new("/archives/x"),
            )
            .await
            .unwrap();

        let roots = RootMap::from([("game".to_owned(), game_dir.clone())]);
        let backups = Arc::new(FileBackupStore::new(db.clone(), dir.path().join("backups")));

        Self {
            db,
            dir,
            game_dir,
            local_game,
            mod_id,
            release,
            archive,
            roots,
            backups,
        }
    }

    /// A second mod lineage, for cross-mod conflict tests.
    pub async fn another_mod(&self, provider_mod_id: &str) -> (ModId, ReleaseId) {
        let m = self
            .db
            .upsert_mod(&onera_core::domain::release::Mod {
                id: ModId::new(),
                provider: ProviderId::nexus(),
                provider_mod_id: ProviderModId::new(provider_mod_id),
                game_slug: "testgame".into(),
                name: format!("Other Mod {provider_mod_id}"),
                author: None,
            })
            .await
            .unwrap();
        let r = self
            .db
            .upsert_release(&onera_core::domain::release::Release {
                id: ReleaseId::new(),
                mod_id: m,
                version: "1.0".into(),
                published_at: chrono::DateTime::from_timestamp(2_000, 0),
                metadata: serde_json::Value::Null,
            })
            .await
            .unwrap();
        (m, r)
    }

    pub fn installer(&self) -> Installer {
        self.installer_with(Arc::new(RealFileSystem))
    }

    pub fn installer_with(&self, fs: Arc<dyn FileSystem>) -> Installer {
        Installer::new(
            fs,
            Arc::new(self.db.clone()),
            Arc::new(self.db.clone()),
            self.backups.clone(),
        )
    }

    pub fn remover(&self) -> Remover {
        Remover::new(
            Arc::new(RealFileSystem),
            Arc::new(self.db.clone()),
            self.backups.clone(),
        )
    }

    /// Write `(path, contents)` into a fresh staging directory and build the
    /// manifest that extraction would have produced.
    pub fn stage(&self, name: &str, files: &[(&str, &[u8])]) -> (PathBuf, ArchiveManifest) {
        let staging = self.dir.path().join("staging").join(name);
        std::fs::create_dir_all(&staging).unwrap();
        let mut manifest_files = Vec::new();
        for (path, contents) in files {
            let rel = RelPath::normalize(path).unwrap();
            let abs = rel.resolve_under(&staging);
            std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
            std::fs::write(&abs, contents).unwrap();
            manifest_files.push(ManifestFile {
                path: rel,
                size: contents.len() as u64,
                hash: FileHash::blake3_of(contents),
                executable: false,
            });
        }
        let manifest = ArchiveManifest::new(
            self.archive,
            FileHash::blake3_of(b"archive"),
            ArchiveFormat::Zip,
            manifest_files,
            vec![],
        );
        (staging, manifest)
    }

    /// Put a file into the game directory that Onera does not know about.
    pub fn write_unmanaged(&self, path: &str, contents: &[u8]) {
        let abs = self.game_dir.join(path);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(abs, contents).unwrap();
    }

    pub fn read_game_file(&self, path: &str) -> Option<Vec<u8>> {
        std::fs::read(self.game_dir.join(path)).ok()
    }

    pub fn game_file_exists(&self, path: &str) -> bool {
        self.game_dir.join(path).exists()
    }
}

pub fn target(path: &str) -> TargetLocation {
    TargetLocation {
        root_key: "game".into(),
        path: RelPath::normalize(path).unwrap(),
    }
}
