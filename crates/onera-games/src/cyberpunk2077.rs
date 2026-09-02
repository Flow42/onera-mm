//! Cyberpunk 2077 game adapter.
//!
//! The first complete adapter. Everything game-specific lives here — the
//! installer, the planner and the Nexus client know none of it.
//!
//! Cyberpunk mods are distributed as archives whose top level is one of a small
//! set of well-known directories:
//!
//! | root        | what lives there                                  |
//! |-------------|---------------------------------------------------|
//! | `archive/`  | REDengine `.archive` files (`archive/pc/mod/`)     |
//! | `bin/`      | native DLLs and ASI plugins (`bin/x64/`)           |
//! | `engine/`   | RED4ext/engine-level configuration and tweaks      |
//! | `mods/`     | REDmod packages                                    |
//! | `r6/`       | scripts, tweaks and CET/Redscript configuration    |
//! | `red4ext/`  | RED4ext plugins                                    |
//! | `tools/`    | modding tools shipped alongside a mod              |
//!
//! Archives are also routinely wrapped in one or more cosmetic directories
//! (`My Mod v1.2/archive/...`), which this adapter unwraps. When unwrapping is
//! ambiguous — two plausible readings of the same archive — it refuses rather
//! than guessing, and the caller asks the user.

use onera_core::domain::archive::ArchiveManifest;
use onera_core::domain::baseline::{BaselineExclusion, ExclusionPattern, ExclusionReason};
use onera_core::domain::game::{DeployRoot, InstallValidation, LocalGameInstall};
use onera_core::paths::DeployRootKind;
use onera_core::plan::TargetLocation;
use onera_core::ports::{GameAdapter, LayoutResolution};
use onera_core::{CoreError, RelPath, Result};
use std::path::Path;

/// Steam application id for Cyberpunk 2077.
pub const STEAM_APP_ID: u32 = 1_091_500;

/// Deployment-root key for the game directory.
pub const ROOT_GAME: &str = "game";
/// Deployment-root key for per-user data inside the compatibility prefix.
pub const ROOT_USER_DATA: &str = "user_data";

/// Top-level directories that identify a Cyberpunk mod layout.
pub const KNOWN_ROOTS: &[&str] = &["archive", "bin", "engine", "mods", "r6", "red4ext", "tools"];

/// Files that are documentation rather than mod content.
const IGNORED_EXTENSIONS: &[&str] = &[
    "txt", "md", "pdf", "jpg", "jpeg", "png", "gif", "url", "html",
];

/// How deep a cosmetic wrapper may be before the adapter gives up.
const MAX_UNWRAP_DEPTH: usize = 4;

/// The Cyberpunk 2077 adapter.
#[derive(Debug, Default, Clone, Copy)]
pub struct Cyberpunk2077;

impl GameAdapter for Cyberpunk2077 {
    fn id(&self) -> &str {
        "cyberpunk2077"
    }

    fn display_name(&self) -> &str {
        "Cyberpunk 2077"
    }

    fn provider_slugs(&self) -> &[&str] {
        &["cyberpunk2077"]
    }

    fn steam_app_ids(&self) -> &[u32] {
        &[STEAM_APP_ID]
    }

    fn validate_install(&self, install_root: &Path) -> InstallValidation {
        // The executable is the only thing that reliably identifies the game
        // across Steam, GOG and Epic installs.
        let executable = install_root.join("bin/x64/Cyberpunk2077.exe");
        if !executable.is_file() {
            return InstallValidation::invalid(format!(
                "{} does not contain bin/x64/Cyberpunk2077.exe",
                install_root.display()
            ));
        }

        let mut findings = Vec::new();
        if !install_root.join("archive/pc/content").is_dir() {
            findings.push(
                "archive/pc/content is missing; this may be an incomplete installation".to_owned(),
            );
        }
        for (marker, note) in [
            (
                "bin/x64/plugins/cyber_engine_tweaks",
                "Cyber Engine Tweaks is present",
            ),
            ("red4ext", "RED4ext is present"),
            ("mods", "REDmod content is present"),
        ] {
            if install_root.join(marker).exists() {
                findings.push(note.to_owned());
            }
        }

        InstallValidation {
            valid: true,
            reported_version: read_reported_version(install_root),
            findings,
        }
    }

    fn deploy_roots(&self, install: &LocalGameInstall) -> Result<Vec<DeployRoot>> {
        let mut roots = vec![DeployRoot {
            key: ROOT_GAME.to_owned(),
            kind: DeployRootKind::GameInstall,
            path: install.install_root.clone(),
        }];

        // Saves and per-user configuration live in the prefix, not the install.
        if let Some(user_data) = install.user_data_roots.first() {
            roots.push(DeployRoot {
                key: ROOT_USER_DATA.to_owned(),
                kind: DeployRootKind::UserData,
                path: user_data.clone(),
            });
        } else if let Some(prefix) = &install.compat_prefix {
            roots.push(DeployRoot {
                key: ROOT_USER_DATA.to_owned(),
                kind: DeployRootKind::CompatPrefix,
                path: prefix.join("drive_c/users/steamuser/Documents"),
            });
        }
        Ok(roots)
    }

    fn resolve_layout(&self, manifest: &ArchiveManifest) -> Result<LayoutResolution> {
        let paths: Vec<&RelPath> = manifest.files.iter().map(|f| &f.path).collect();
        if paths.is_empty() {
            return Err(CoreError::AmbiguousLayout(
                "the archive contains no files".into(),
            ));
        }

        let candidates = unwrap_candidates(&paths);
        match candidates.len() {
            0 => Err(CoreError::AmbiguousLayout(format!(
                "no recognized Cyberpunk directory found; expected one of {}",
                KNOWN_ROOTS.join(", ")
            ))),
            1 => Ok(build_resolution(&paths, candidates[0])),
            _ => Err(CoreError::AmbiguousLayout(format!(
                "the archive can be read {} different ways (wrapper depths {:?}); \
                 pick one rather than guessing",
                candidates.len(),
                candidates
            ))),
        }
    }

    fn validate_target(&self, target: &TargetLocation) -> Result<()> {
        if target.root_key == ROOT_USER_DATA {
            return Ok(());
        }
        if target.root_key != ROOT_GAME {
            return Err(CoreError::InvalidInput(format!(
                "unknown deployment root {:?}",
                target.root_key
            )));
        }

        // Replacing the game's own executable or its shipped content would turn
        // a mod install into an unrecoverable game corruption.
        let path = target.path.as_str();
        for forbidden in ["bin/x64/Cyberpunk2077.exe", "REDprelauncher.exe"] {
            if path.eq_ignore_ascii_case(forbidden) {
                return Err(CoreError::InvalidInput(format!(
                    "{path} is a game executable and is never replaced by a mod"
                )));
            }
        }
        if path.starts_with("archive/pc/content/") {
            return Err(CoreError::InvalidInput(
                "archive/pc/content holds the base game's own archives; mods belong in archive/pc/mod"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    /// Everything the game rewrites on its own.
    ///
    /// Cyberpunk regenerates `r6/cache/final.redscripts` on almost every launch,
    /// writes crash reports and logs beside its executables, and keeps a
    /// per-machine shader cache. None of that says anything about whether the
    /// installation is clean, so including it would make every baseline
    /// verification report a modified game within one play session.
    ///
    /// The default deployment roots already keep the compatibility prefix and
    /// the user-data root out of the baseline; these are the exclusions *inside*
    /// the game directory.
    fn baseline_exclusions(&self) -> Vec<BaselineExclusion> {
        let prefix = |path: &str, reason: ExclusionReason, note: &str| BaselineExclusion {
            root_key: Some(ROOT_GAME.to_owned()),
            pattern: ExclusionPattern::Prefix {
                path: RelPath::normalize(path).expect("static exclusion path is valid"),
            },
            reason,
            note: Some(note.to_owned()),
        };
        vec![
            prefix(
                "r6/cache",
                ExclusionReason::Cache,
                "Redscript recompiles this on launch",
            ),
            prefix("r6/logs", ExclusionReason::Logs, "script and mod logs"),
            prefix(
                "r6/storage",
                ExclusionReason::GeneratedConfig,
                "per-mod configuration written at runtime",
            ),
            prefix(
                "bin/x64/plugins/cyber_engine_tweaks",
                ExclusionReason::GeneratedConfig,
                "CET keeps its bindings and mod settings here",
            ),
            prefix("red4ext/logs", ExclusionReason::Logs, "RED4ext logs"),
            BaselineExclusion {
                root_key: Some(ROOT_GAME.to_owned()),
                pattern: ExclusionPattern::Extension {
                    extension: "log".to_owned(),
                },
                reason: ExclusionReason::Logs,
                note: Some("logs are written throughout the game directory".to_owned()),
            },
            BaselineExclusion {
                root_key: Some(ROOT_GAME.to_owned()),
                pattern: ExclusionPattern::DirectoryName {
                    name: "ShaderCache".to_owned(),
                },
                reason: ExclusionReason::ShaderCache,
                note: Some("shader caches differ per driver and per machine".to_owned()),
            },
        ]
    }
}

/// Read the version the game reports, verbatim.
fn read_reported_version(install_root: &Path) -> Option<String> {
    let path = install_root.join("version.txt");
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Wrapper depths at which the archive looks like a valid Cyberpunk layout.
///
/// Returns every depth that works. More than one means the archive is genuinely
/// ambiguous — for example an archive containing both `archive/` at the top and
/// `Something/archive/` underneath.
fn unwrap_candidates(paths: &[&RelPath]) -> Vec<usize> {
    let mut depths = Vec::new();
    for depth in 0..=MAX_UNWRAP_DEPTH {
        let stripped: Vec<RelPath> = paths
            .iter()
            .filter_map(|p| {
                if depth == 0 {
                    Some((*p).clone())
                } else {
                    p.strip_prefix_components(depth)
                }
            })
            .collect();
        if stripped.is_empty() {
            break;
        }
        // A layout is valid when at least one content file sits under a known
        // root and nothing that matters sits outside one.
        let content: Vec<&RelPath> = stripped.iter().filter(|p| !is_ignorable(p)).collect();
        if content.is_empty() {
            continue;
        }
        if content
            .iter()
            .all(|p| KNOWN_ROOTS.contains(&p.first_component()))
        {
            depths.push(depth);
        }
    }
    depths
}

/// Whether a file is documentation or an image rather than mod content.
fn is_ignorable(path: &RelPath) -> bool {
    path.extension()
        .is_some_and(|e| IGNORED_EXTENSIONS.contains(&e.as_str()))
}

fn build_resolution(paths: &[&RelPath], depth: usize) -> LayoutResolution {
    let mut mappings = Vec::new();
    let mut ignored = Vec::new();

    for source in paths {
        let stripped = if depth == 0 {
            Some((*source).clone())
        } else {
            source.strip_prefix_components(depth)
        };
        let Some(stripped) = stripped else {
            ignored.push((*source).clone());
            continue;
        };
        if is_ignorable(&stripped) || !KNOWN_ROOTS.contains(&stripped.first_component()) {
            ignored.push((*source).clone());
            continue;
        }
        mappings.push((
            (*source).clone(),
            TargetLocation {
                root_key: ROOT_GAME.to_owned(),
                path: stripped,
            },
        ));
    }

    LayoutResolution {
        rationale: if depth == 0 {
            "archive roots map directly onto the game directory".to_owned()
        } else {
            format!(
                "stripped {depth} wrapper director{}",
                if depth == 1 { "y" } else { "ies" }
            )
        },
        mappings,
        ignored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onera_core::domain::archive::{ArchiveFormat, ManifestFile};
    use onera_core::hash::FileHash;
    use onera_core::ids::ArchiveId;

    fn manifest(paths: &[&str]) -> ArchiveManifest {
        ArchiveManifest::new(
            ArchiveId::new(),
            FileHash::blake3_of(b"a"),
            ArchiveFormat::Zip,
            paths
                .iter()
                .map(|p| ManifestFile {
                    path: RelPath::normalize(p).unwrap(),
                    size: 1,
                    hash: FileHash::blake3_of(p.as_bytes()),
                    executable: false,
                })
                .collect(),
            vec![],
        )
    }

    fn targets(resolution: &LayoutResolution) -> Vec<String> {
        resolution
            .mappings
            .iter()
            .map(|(_, t)| t.path.to_string())
            .collect()
    }

    #[test]
    fn maps_a_plain_archive_directly() {
        let m = manifest(&["archive/pc/mod/thing.archive", "r6/scripts/thing.reds"]);
        let r = Cyberpunk2077.resolve_layout(&m).unwrap();
        assert_eq!(
            targets(&r),
            vec!["archive/pc/mod/thing.archive", "r6/scripts/thing.reds"]
        );
        assert!(r.mappings.iter().all(|(_, t)| t.root_key == ROOT_GAME));
    }

    #[test]
    fn unwraps_a_cosmetic_top_level_directory() {
        let m = manifest(&["My Cool Mod v1.2/archive/pc/mod/thing.archive"]);
        let r = Cyberpunk2077.resolve_layout(&m).unwrap();
        assert_eq!(targets(&r), vec!["archive/pc/mod/thing.archive"]);
        assert!(
            r.rationale.contains("stripped 1 wrapper directory"),
            "{}",
            r.rationale
        );
    }

    #[test]
    fn unwraps_several_nested_wrappers() {
        let m = manifest(&["Download/Mod v3/Install this/bin/x64/plugins/thing.asi"]);
        let r = Cyberpunk2077.resolve_layout(&m).unwrap();
        assert_eq!(targets(&r), vec!["bin/x64/plugins/thing.asi"]);
    }

    #[test]
    fn recognizes_every_documented_root() {
        for root in KNOWN_ROOTS {
            let path = format!("{root}/thing.dat");
            let r = Cyberpunk2077.resolve_layout(&manifest(&[&path])).unwrap();
            assert_eq!(targets(&r), vec![path], "root {root} was not recognized");
        }
    }

    #[test]
    fn documentation_is_ignored_rather_than_deployed() {
        let m = manifest(&[
            "My Mod/readme.txt",
            "My Mod/preview.png",
            "My Mod/archive/pc/mod/thing.archive",
        ]);
        let r = Cyberpunk2077.resolve_layout(&m).unwrap();
        assert_eq!(targets(&r), vec!["archive/pc/mod/thing.archive"]);
        assert_eq!(r.ignored.len(), 2);
    }

    #[test]
    fn an_unrecognizable_archive_is_refused() {
        let m = manifest(&["random/thing.dat", "another/thing.dat"]);
        let err = Cyberpunk2077.resolve_layout(&m).unwrap_err();
        assert!(matches!(err, CoreError::AmbiguousLayout(_)), "{err:?}");
        assert!(format!("{err}").contains("no recognized Cyberpunk directory"));
    }

    #[test]
    fn an_ambiguous_archive_asks_instead_of_guessing() {
        // Valid at depth 0 (`archive/...`) and also at depth 1 (`archive/` ->
        // `pc/...` is not a root, so use a case that really is ambiguous):
        // a wrapper that is itself named after a known root.
        let m = manifest(&["mods/mods/mymod/info.json", "mods/archive/pc/mod/a.archive"]);
        let result = Cyberpunk2077.resolve_layout(&m);
        match result {
            Err(CoreError::AmbiguousLayout(message)) => {
                assert!(message.contains("different ways"), "{message}");
            }
            other => panic!("expected an ambiguity, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_archive_is_refused() {
        let err = Cyberpunk2077.resolve_layout(&manifest(&[])).unwrap_err();
        assert!(format!("{err}").contains("no files"));
    }

    #[test]
    fn game_executables_are_never_valid_targets() {
        for path in ["bin/x64/Cyberpunk2077.exe", "REDprelauncher.exe"] {
            let target = TargetLocation {
                root_key: ROOT_GAME.into(),
                path: RelPath::normalize(path).unwrap(),
            };
            assert!(
                Cyberpunk2077.validate_target(&target).is_err(),
                "{path} was allowed"
            );
        }
    }

    #[test]
    fn base_game_content_is_protected() {
        let target = TargetLocation {
            root_key: ROOT_GAME.into(),
            path: RelPath::normalize("archive/pc/content/basegame_1_engine.archive").unwrap(),
        };
        let err = Cyberpunk2077.validate_target(&target).unwrap_err();
        assert!(format!("{err}").contains("archive/pc/mod"), "{err}");

        // The mod directory next to it is fine.
        let ok = TargetLocation {
            root_key: ROOT_GAME.into(),
            path: RelPath::normalize("archive/pc/mod/mymod.archive").unwrap(),
        };
        assert!(Cyberpunk2077.validate_target(&ok).is_ok());
    }

    #[test]
    fn unknown_roots_are_rejected() {
        let target = TargetLocation {
            root_key: "somewhere_else".into(),
            path: RelPath::normalize("a").unwrap(),
        };
        assert!(Cyberpunk2077.validate_target(&target).is_err());
    }

    #[test]
    fn validation_requires_the_game_executable() {
        let dir = tempfile::tempdir().unwrap();
        let bad = Cyberpunk2077.validate_install(dir.path());
        assert!(!bad.valid);
        assert!(bad.findings[0].contains("Cyberpunk2077.exe"));

        std::fs::create_dir_all(dir.path().join("bin/x64")).unwrap();
        std::fs::write(dir.path().join("bin/x64/Cyberpunk2077.exe"), b"MZ").unwrap();
        assert!(Cyberpunk2077.validate_install(dir.path()).valid);
    }

    #[test]
    fn validation_reports_the_version_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("bin/x64")).unwrap();
        std::fs::write(dir.path().join("bin/x64/Cyberpunk2077.exe"), b"MZ").unwrap();
        std::fs::write(dir.path().join("version.txt"), b"  2.21  \n").unwrap();

        let v = Cyberpunk2077.validate_install(dir.path());
        assert_eq!(v.reported_version.as_deref(), Some("2.21"));
    }

    #[test]
    fn deploy_roots_separate_the_install_from_user_data() {
        use onera_core::domain::game::InstallSource;
        use onera_core::ids::{GameId, LocalGameId};

        let install = LocalGameInstall {
            id: LocalGameId::new(),
            game_id: GameId::new(),
            adapter_id: "cyberpunk2077".into(),
            source: InstallSource::SteamNative,
            install_root: "/games/Cyberpunk 2077".into(),
            compat_prefix: Some("/steam/compatdata/1091500/pfx".into()),
            user_data_roots: vec![],
            confirmed: true,
        };
        let roots = Cyberpunk2077.deploy_roots(&install).unwrap();
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].kind, DeployRootKind::GameInstall);
        assert_eq!(roots[1].kind, DeployRootKind::CompatPrefix);
        assert!(roots[1].path.starts_with("/steam/compatdata/1091500/pfx"));

        // With no prefix at all there is only the install root.
        let bare = LocalGameInstall {
            compat_prefix: None,
            ..install
        };
        assert_eq!(Cyberpunk2077.deploy_roots(&bare).unwrap().len(), 1);
    }

    #[test]
    fn the_baseline_scope_covers_the_install_but_not_user_data() {
        use onera_core::domain::game::InstallSource;
        use onera_core::ids::{GameId, LocalGameId};

        let install = LocalGameInstall {
            id: LocalGameId::new(),
            game_id: GameId::new(),
            adapter_id: "cyberpunk2077".into(),
            source: InstallSource::SteamNative,
            install_root: "/games/Cyberpunk 2077".into(),
            compat_prefix: Some("/steam/compatdata/1091500/pfx".into()),
            user_data_roots: vec![],
            confirmed: true,
        };

        // deploy_roots offers two roots; only the store-managed one is scanned.
        assert_eq!(Cyberpunk2077.deploy_roots(&install).unwrap().len(), 2);
        let roots = Cyberpunk2077.baseline_roots(&install).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].key, ROOT_GAME);
        assert_eq!(roots[0].kind, DeployRootKind::GameInstall);
    }

    #[test]
    fn regenerated_files_are_excluded_from_the_baseline_but_real_content_is_not() {
        use onera_core::domain::baseline::excluded_by;

        let exclusions = Cyberpunk2077.baseline_exclusions();
        for excluded in [
            "r6/cache/final.redscripts",
            "r6/logs/scc.log",
            "r6/storage/mymod/settings.json",
            "bin/x64/plugins/cyber_engine_tweaks/mods/thing/init.lua",
            "red4ext/logs/red4ext.log",
            "bin/x64/ShaderCache/a.bin",
        ] {
            let path = RelPath::normalize(excluded).unwrap();
            assert!(
                excluded_by(&exclusions, ROOT_GAME, &path).is_some(),
                "{excluded} should not be part of a baseline"
            );
        }

        for included in [
            "bin/x64/Cyberpunk2077.exe",
            "archive/pc/content/basegame_1_engine.archive",
            "r6/config/inputUserMappings.xml",
        ] {
            let path = RelPath::normalize(included).unwrap();
            assert!(
                excluded_by(&exclusions, ROOT_GAME, &path).is_none(),
                "{included} belongs in the baseline"
            );
        }
    }
}
