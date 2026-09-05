//! Skyrim Special Edition game adapter.
//!
//! The second adapter, and deliberately not shaped like the first. Cyberpunk
//! archives always name their destination directory at the top level, so its
//! adapter only ever *strips* wrapper directories. Skyrim's do not:
//!
//! | archive top level                      | what it means                     |
//! |----------------------------------------|-----------------------------------|
//! | `Data/meshes/…`                        | already game-relative             |
//! | `meshes/…`, `MyMod.esp`, `MyMod.bsa`   | `Data`-relative; needs prefixing  |
//! | `My Mod v3/Data/meshes/…`              | a wrapper around a game-relative  |
//! | `My Mod v3/meshes/…`                   | a wrapper around a `Data`-relative|
//!
//! So this adapter both strips wrappers *and* adds a root, which is what makes
//! it a real test of whether the installer, planner, profile and baseline code
//! is game-agnostic: a mapping here is not the identity function on paths.
//!
//! Everything else follows the same rules as any adapter — refuse when an
//! archive can be read two ways rather than guessing, never write over the
//! game's own executables or master files, and keep runtime-written files out
//! of the baseline.

use onera_core::domain::archive::ArchiveManifest;
use onera_core::domain::baseline::{BaselineExclusion, ExclusionPattern, ExclusionReason};
use onera_core::domain::game::{DeployRoot, InstallValidation, LocalGameInstall};
use onera_core::paths::DeployRootKind;
use onera_core::plan::TargetLocation;
use onera_core::ports::{GameAdapter, LayoutResolution};
use onera_core::{CoreError, RelPath, Result};
use std::path::Path;

/// Steam application id for Skyrim Special Edition (and Anniversary Edition,
/// which is the same application with additional content).
pub const STEAM_APP_ID: u32 = 489_830;

/// Deployment-root key for the game directory.
pub const ROOT_GAME: &str = "game";
/// Deployment-root key for per-user data inside the compatibility prefix.
pub const ROOT_USER_DATA: &str = "user_data";

/// The one directory mod content is ever deployed into.
const DATA_DIR: &str = "Data";

/// Top-level directories that identify content already relative to the game
/// root rather than to `Data`.
const GAME_RELATIVE_ROOTS: &[&str] = &[DATA_DIR];

/// Top-level directories of an archive whose contents are relative to `Data`.
///
/// These are the directories the engine itself reads out of `Data`, plus the
/// script-extender and configuration trees the community standardized on.
pub const DATA_RELATIVE_ROOTS: &[&str] = &[
    "meshes",
    "textures",
    "scripts",
    "interface",
    "sound",
    "music",
    "video",
    "seq",
    "shadersfx",
    "grass",
    "lodsettings",
    "materials",
    "strings",
    "dialogueviews",
    "skse",
    "skyproc patchers",
    "source",
    "netscriptframework",
    "calientetools",
    "tools",
];

/// Loose file extensions that are `Data`-relative when they sit at an archive's
/// top level: plugins, archives and the string tables beside them.
pub const DATA_RELATIVE_EXTENSIONS: &[&str] = &["esp", "esm", "esl", "bsa", "bsl"];

/// The base game's master files. A mod replacing one is never legitimate and
/// leaves an installation that cannot be repaired without a full redownload.
pub const BASE_GAME_MASTERS: &[&str] = &[
    "skyrim.esm",
    "update.esm",
    "dawnguard.esm",
    "hearthfires.esm",
    "dragonborn.esm",
];

/// Executables that are the game itself.
const GAME_EXECUTABLES: &[&str] = &["SkyrimSE.exe", "SkyrimSELauncher.exe"];

/// Files that are documentation rather than mod content.
const IGNORED_EXTENSIONS: &[&str] = &[
    "txt", "md", "pdf", "jpg", "jpeg", "png", "gif", "url", "html", "docx",
];

/// How deep a cosmetic wrapper may be before the adapter gives up.
const MAX_UNWRAP_DEPTH: usize = 4;

/// The Skyrim Special Edition adapter.
#[derive(Debug, Default, Clone, Copy)]
pub struct SkyrimSpecialEdition;

/// How an archive's paths map onto the game directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Reading {
    /// Wrapper directories to strip.
    depth: usize,
    /// Whether the stripped paths still need a `Data/` prefix.
    needs_data_prefix: bool,
}

impl GameAdapter for SkyrimSpecialEdition {
    fn id(&self) -> &str {
        "skyrimspecialedition"
    }

    fn display_name(&self) -> &str {
        "Skyrim Special Edition"
    }

    fn provider_slugs(&self) -> &[&str] {
        &["skyrimspecialedition"]
    }

    fn steam_app_ids(&self) -> &[u32] {
        &[STEAM_APP_ID]
    }

    fn validate_install(&self, install_root: &Path) -> InstallValidation {
        // The executable identifies the game across Steam, GOG and Epic. The
        // launcher is not enough: it ships with the original Skyrim too.
        if !install_root.join("SkyrimSE.exe").is_file() {
            return InstallValidation::invalid(format!(
                "{} does not contain SkyrimSE.exe",
                install_root.display()
            ));
        }

        let mut findings = Vec::new();
        if !install_root.join("Data/Skyrim.esm").is_file() {
            findings.push(
                "Data/Skyrim.esm is missing; this may be an incomplete installation".to_owned(),
            );
        }
        // Anniversary Edition is the same application plus creations, so the
        // presence of its content is worth reporting but never required.
        if install_root.join("Data/_ResourcePack.esl").is_file() {
            findings.push("Anniversary Edition content is present".to_owned());
        }
        for (marker, note) in [
            ("skse64_loader.exe", "SKSE64 is installed"),
            ("Data/SKSE/Plugins", "SKSE plugins are present"),
            ("Data/DynDOLOD.esp", "DynDOLOD output is present"),
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

        // `Skyrim.ini`, `SkyrimPrefs.ini` and every save live here, not in the
        // install. Mods that ship INI presets target this root.
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
                path: prefix
                    .join("drive_c/users/steamuser/Documents/My Games/Skyrim Special Edition"),
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

        // Two readings that place every file identically are one reading. An
        // archive rooted at `Data/` is always also readable as a wrapper around
        // a `Data`-relative one, and that is not an ambiguity a user can
        // usefully be asked about.
        let mut distinct: Vec<(Reading, LayoutResolution)> = Vec::new();
        for reading in candidate_readings(&paths) {
            let resolution = build_resolution(&paths, reading);
            if !distinct
                .iter()
                .any(|(_, seen)| same_targets(seen, &resolution))
            {
                distinct.push((reading, resolution));
            }
        }

        match distinct.len() {
            0 => Err(CoreError::AmbiguousLayout(format!(
                "no recognized Skyrim layout found; expected a Data directory, \
                 one of {}, or plugin files at the top level",
                DATA_RELATIVE_ROOTS[..6].join(", ")
            ))),
            1 => Ok(distinct.remove(0).1),
            _ => Err(CoreError::AmbiguousLayout(format!(
                "the archive can be read {} different ways ({}); \
                 pick one rather than guessing",
                distinct.len(),
                describe(&distinct.iter().map(|(r, _)| *r).collect::<Vec<_>>())
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

        let path = target.path.as_str();
        for forbidden in GAME_EXECUTABLES {
            if path.eq_ignore_ascii_case(forbidden) {
                return Err(CoreError::InvalidInput(format!(
                    "{path} is a game executable and is never replaced by a mod"
                )));
            }
        }

        // Replacing a master leaves an installation Steam cannot repair without
        // a full redownload, and no legitimate mod does it — they add plugins
        // that depend on the masters instead.
        if let Some(name) = path.strip_prefix("Data/").or_else(|| {
            // Case-insensitively, since archives disagree about `Data` vs `data`.
            path.get(..5)
                .filter(|p| p.eq_ignore_ascii_case("data/"))
                .map(|_| &path[5..])
        }) {
            if BASE_GAME_MASTERS
                .iter()
                .any(|m| name.eq_ignore_ascii_case(m))
            {
                return Err(CoreError::InvalidInput(format!(
                    "{name} is a base-game master file; a mod adds a plugin that \
                     depends on it rather than replacing it"
                )));
            }
        }
        Ok(())
    }

    /// Everything the game and its script extender rewrite on their own.
    ///
    /// Skyrim keeps its INIs and saves in `Documents`, which the default
    /// baseline scope already excludes, so the install directory is unusually
    /// static. What does get written there are logs from crash loggers and
    /// SKSE plugins, and Steam's own cloud bookkeeping.
    fn baseline_exclusions(&self) -> Vec<BaselineExclusion> {
        vec![
            BaselineExclusion {
                root_key: Some(ROOT_GAME.to_owned()),
                pattern: ExclusionPattern::Extension {
                    extension: "log".to_owned(),
                },
                reason: ExclusionReason::Logs,
                note: Some("SKSE plugins and crash loggers write logs in place".to_owned()),
            },
            BaselineExclusion {
                root_key: Some(ROOT_GAME.to_owned()),
                pattern: ExclusionPattern::Exact {
                    path: RelPath::normalize("steam_autocloud.vdf")
                        .expect("static exclusion path is valid"),
                },
                reason: ExclusionReason::GeneratedConfig,
                note: Some("Steam rewrites its cloud manifest on every sync".to_owned()),
            },
            BaselineExclusion {
                root_key: Some(ROOT_GAME.to_owned()),
                pattern: ExclusionPattern::Prefix {
                    path: RelPath::normalize("Data/NetScriptFramework/Crash")
                        .expect("static exclusion path is valid"),
                },
                reason: ExclusionReason::Logs,
                note: Some("NetScriptFramework writes crash dumps here".to_owned()),
            },
        ]
    }
}

/// Read the version the game reports, verbatim.
///
/// Skyrim ships no version file, so this is the Steam depot marker when one is
/// present and nothing otherwise. The adapter never invents a version.
fn read_reported_version(install_root: &Path) -> Option<String> {
    std::fs::read_to_string(install_root.join("Skyrim_Default.ini"))
        .ok()
        .and_then(|contents| {
            contents
                .lines()
                .find_map(|line| line.trim().strip_prefix("sVersion=").map(str::to_owned))
        })
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// Whether a file is documentation or an image rather than mod content.
fn is_ignorable(path: &RelPath) -> bool {
    path.extension()
        .is_some_and(|e| IGNORED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Whether a stripped path is already relative to the game root.
fn is_game_relative(path: &RelPath) -> bool {
    GAME_RELATIVE_ROOTS
        .iter()
        .any(|r| path.first_component().eq_ignore_ascii_case(r))
}

/// Whether a stripped path is relative to `Data`.
fn is_data_relative(path: &RelPath) -> bool {
    let first = path.first_component();
    if DATA_RELATIVE_ROOTS
        .iter()
        .any(|r| first.eq_ignore_ascii_case(r))
    {
        return true;
    }
    // A plugin or archive at the top level — the shape of a plugin-only mod.
    path.as_str() == first
        && path
            .extension()
            .is_some_and(|e| DATA_RELATIVE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Every way the archive can be read as a valid Skyrim layout.
///
/// More than one means the archive is genuinely ambiguous — for example one
/// containing both `Data/` at the top and a `Data/` nested inside a wrapper.
fn candidate_readings(paths: &[&RelPath]) -> Vec<Reading> {
    let mut readings = Vec::new();
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
        // A layout is valid when at least one content file is recognized and
        // nothing that matters sits outside a recognized location.
        let content: Vec<&RelPath> = stripped.iter().filter(|p| !is_ignorable(p)).collect();
        if content.is_empty() {
            continue;
        }
        if content.iter().all(|p| is_game_relative(p)) {
            readings.push(Reading {
                depth,
                needs_data_prefix: false,
            });
        } else if content.iter().all(|p| is_data_relative(p)) {
            readings.push(Reading {
                depth,
                needs_data_prefix: true,
            });
        }
    }
    readings
}

/// Whether two readings deploy every file to exactly the same place.
fn same_targets(a: &LayoutResolution, b: &LayoutResolution) -> bool {
    a.mappings.len() == b.mappings.len()
        && a.mappings
            .iter()
            .zip(&b.mappings)
            .all(|((a_src, a_dst), (b_src, b_dst))| {
                a_src == b_src && a_dst.root_key == b_dst.root_key && a_dst.path == b_dst.path
            })
}

fn describe(readings: &[Reading]) -> String {
    readings
        .iter()
        .map(|r| {
            format!(
                "depth {} {}",
                r.depth,
                if r.needs_data_prefix {
                    "under Data"
                } else {
                    "at the game root"
                }
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn build_resolution(paths: &[&RelPath], reading: Reading) -> LayoutResolution {
    let mut mappings = Vec::new();
    let mut ignored = Vec::new();

    for source in paths {
        let stripped = if reading.depth == 0 {
            Some((*source).clone())
        } else {
            source.strip_prefix_components(reading.depth)
        };
        let Some(stripped) = stripped else {
            ignored.push((*source).clone());
            continue;
        };
        let recognized = if reading.needs_data_prefix {
            is_data_relative(&stripped)
        } else {
            is_game_relative(&stripped)
        };
        if is_ignorable(&stripped) || !recognized {
            ignored.push((*source).clone());
            continue;
        }

        // Archives spell it `Data`, `data` and `DATA`. The game directory has
        // exactly one of those, and on a case-sensitive filesystem deploying
        // the archive's spelling verbatim would build a second one beside it
        // that the engine never reads.
        let canonical = if reading.needs_data_prefix {
            format!("{DATA_DIR}/{stripped}")
        } else {
            let rest = &stripped.as_str()[stripped.first_component().len()..];
            format!("{DATA_DIR}{rest}")
        };
        let target = match RelPath::normalize(&canonical) {
            Ok(path) => path,
            // Unreachable for an already-normalized path, but a layout
            // resolver must never panic on archive-controlled input.
            Err(_) => {
                ignored.push((*source).clone());
                continue;
            }
        };

        mappings.push((
            (*source).clone(),
            TargetLocation {
                root_key: ROOT_GAME.to_owned(),
                path: target,
            },
        ));
    }

    LayoutResolution {
        rationale: rationale(reading),
        mappings,
        ignored,
    }
}

fn rationale(reading: Reading) -> String {
    let wrapper = match reading.depth {
        0 => String::new(),
        1 => "stripped 1 wrapper directory; ".to_owned(),
        n => format!("stripped {n} wrapper directories; "),
    };
    let placement = if reading.needs_data_prefix {
        "contents are relative to Data, so they deploy under Data/"
    } else {
        "archive roots map directly onto the game directory"
    };
    format!("{wrapper}{placement}")
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
        let mut out: Vec<String> = resolution
            .mappings
            .iter()
            .map(|(_, t)| t.path.to_string())
            .collect();
        out.sort();
        out
    }

    fn resolve(paths: &[&str]) -> LayoutResolution {
        SkyrimSpecialEdition
            .resolve_layout(&manifest(paths))
            .unwrap()
    }

    // -----------------------------------------------------------------------
    // Layout resolution
    // -----------------------------------------------------------------------

    #[test]
    fn a_game_relative_archive_maps_directly() {
        let r = resolve(&["Data/meshes/thing.nif", "Data/MyMod.esp"]);
        assert_eq!(targets(&r), vec!["Data/MyMod.esp", "Data/meshes/thing.nif"]);
        assert!(r.rationale.contains("directly onto the game directory"));
    }

    /// The shape Cyberpunk never produces: the archive is relative to `Data`,
    /// so every path gains a component it did not have.
    #[test]
    fn a_data_relative_archive_gains_the_data_prefix() {
        let r = resolve(&["meshes/armor/thing.nif", "textures/armor/thing.dds"]);
        assert_eq!(
            targets(&r),
            vec![
                "Data/meshes/armor/thing.nif",
                "Data/textures/armor/thing.dds"
            ]
        );
        assert!(r.rationale.contains("relative to Data"), "{}", r.rationale);
    }

    #[test]
    fn a_plugin_only_archive_is_data_relative() {
        let r = resolve(&["MyMod.esp", "MyMod.bsa"]);
        assert_eq!(targets(&r), vec!["Data/MyMod.bsa", "Data/MyMod.esp"]);
    }

    #[test]
    fn a_wrapped_game_relative_archive_is_unwrapped() {
        let r = resolve(&["My Mod v3/Data/meshes/thing.nif"]);
        assert_eq!(targets(&r), vec!["Data/meshes/thing.nif"]);
        assert!(
            r.rationale.contains("stripped 1 wrapper directory"),
            "{}",
            r.rationale
        );
    }

    #[test]
    fn a_wrapped_data_relative_archive_is_unwrapped_and_prefixed() {
        let r = resolve(&["Download/My Mod v3/meshes/thing.nif"]);
        assert_eq!(targets(&r), vec!["Data/meshes/thing.nif"]);
        assert!(r.rationale.contains("stripped 2 wrapper directories"));
        assert!(r.rationale.contains("relative to Data"));
    }

    #[test]
    fn skse_plugins_keep_their_place_under_data() {
        let r = resolve(&["SKSE/Plugins/thing.dll", "SKSE/Plugins/thing.ini"]);
        assert_eq!(
            targets(&r),
            vec!["Data/SKSE/Plugins/thing.dll", "Data/SKSE/Plugins/thing.ini"]
        );
    }

    #[test]
    fn every_documented_data_relative_root_is_recognized() {
        for root in DATA_RELATIVE_ROOTS {
            let path = format!("{root}/thing.dat");
            let r = resolve(&[&path]);
            assert_eq!(
                targets(&r),
                vec![format!("Data/{path}")],
                "root {root} was not recognized"
            );
        }
    }

    #[test]
    fn every_data_relative_extension_is_recognized_at_the_top_level() {
        for extension in DATA_RELATIVE_EXTENSIONS {
            let path = format!("MyMod.{extension}");
            let r = resolve(&[&path]);
            assert_eq!(
                targets(&r),
                vec![format!("Data/{path}")],
                "extension {extension} was not recognized"
            );
        }
    }

    /// Archives disagree about the case of `Data`, and a case-sensitive
    /// filesystem would otherwise produce a second, broken directory.
    /// Archives disagree about the case of `Data`. The game directory has one
    /// spelling, so every reading is rewritten to it: deploying `data/` beside
    /// the real `Data/` on a case-sensitive filesystem installs a mod the
    /// engine will never load.
    #[test]
    fn the_data_directory_is_canonicalized_whatever_the_archive_spelled() {
        for spelling in ["data", "DATA", "Data"] {
            let r = resolve(&[&format!("{spelling}/meshes/thing.nif")]);
            assert_eq!(
                targets(&r),
                vec!["Data/meshes/thing.nif"],
                "{spelling}/ was not canonicalized"
            );
        }
    }

    #[test]
    fn documentation_is_ignored_rather_than_deployed() {
        let r = resolve(&[
            "My Mod/readme.txt",
            "My Mod/screenshot.png",
            "My Mod/meshes/thing.nif",
        ]);
        assert_eq!(targets(&r), vec!["Data/meshes/thing.nif"]);
        assert_eq!(r.ignored.len(), 2);
    }

    #[test]
    fn an_unrecognizable_archive_is_refused() {
        let err = SkyrimSpecialEdition
            .resolve_layout(&manifest(&["random/thing.dat", "another/thing.dat"]))
            .unwrap_err();
        assert!(matches!(err, CoreError::AmbiguousLayout(_)), "{err:?}");
        assert!(format!("{err}").contains("no recognized Skyrim layout"));
    }

    /// A doubled `Data` directory reads two ways that land in different
    /// places: `Data/Data/meshes/a.nif` or `Data/meshes/a.nif`. Guessing wrong
    /// puts the whole mod one directory too deep, where the engine ignores it.
    #[test]
    fn an_ambiguous_archive_asks_instead_of_guessing() {
        let m = manifest(&["Data/Data/meshes/a.nif"]);
        match SkyrimSpecialEdition.resolve_layout(&m) {
            Err(CoreError::AmbiguousLayout(message)) => {
                assert!(message.contains("different ways"), "{message}");
            }
            other => panic!("expected an ambiguity, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_archive_is_refused() {
        let err = SkyrimSpecialEdition
            .resolve_layout(&manifest(&[]))
            .unwrap_err();
        assert!(format!("{err}").contains("no files"));
    }

    /// A mod that mixes a game-relative and a `Data`-relative top level is not
    /// two readings of one layout — it is not a layout at all.
    #[test]
    fn a_mixed_archive_is_refused_rather_than_half_deployed() {
        let err = SkyrimSpecialEdition
            .resolve_layout(&manifest(&["Data/MyMod.esp", "meshes/thing.nif"]))
            .unwrap_err();
        assert!(matches!(err, CoreError::AmbiguousLayout(_)), "{err:?}");
    }

    // -----------------------------------------------------------------------
    // Target validation
    // -----------------------------------------------------------------------

    fn target(path: &str) -> TargetLocation {
        TargetLocation {
            root_key: ROOT_GAME.into(),
            path: RelPath::normalize(path).unwrap(),
        }
    }

    #[test]
    fn game_executables_are_never_valid_targets() {
        for path in GAME_EXECUTABLES {
            assert!(
                SkyrimSpecialEdition.validate_target(&target(path)).is_err(),
                "{path} was allowed"
            );
        }
    }

    #[test]
    fn base_game_masters_are_protected() {
        for master in BASE_GAME_MASTERS {
            let err = SkyrimSpecialEdition
                .validate_target(&target(&format!("Data/{master}")))
                .unwrap_err();
            assert!(format!("{err}").contains("master file"), "{err}");
        }

        // A mod's own plugin next to them is fine.
        assert!(SkyrimSpecialEdition
            .validate_target(&target("Data/MyMod.esp"))
            .is_ok());
    }

    /// Archives write `data/skyrim.esm` as often as `Data/Skyrim.esm`, and a
    /// case-sensitive comparison would wave the destructive one through.
    #[test]
    fn master_protection_is_case_insensitive() {
        for path in [
            "data/skyrim.esm",
            "DATA/SKYRIM.ESM",
            "Data/Skyrim.esm",
            "data/Update.ESM",
        ] {
            assert!(
                SkyrimSpecialEdition.validate_target(&target(path)).is_err(),
                "{path} was allowed"
            );
        }
    }

    #[test]
    fn a_master_name_outside_data_is_not_the_master() {
        // `Data` is the only place the engine loads masters from.
        assert!(SkyrimSpecialEdition
            .validate_target(&target("Tools/skyrim.esm"))
            .is_ok());
    }

    #[test]
    fn unknown_roots_are_rejected() {
        let t = TargetLocation {
            root_key: "somewhere_else".into(),
            path: RelPath::normalize("a").unwrap(),
        };
        assert!(SkyrimSpecialEdition.validate_target(&t).is_err());
    }

    #[test]
    fn user_data_targets_are_allowed() {
        let t = TargetLocation {
            root_key: ROOT_USER_DATA.into(),
            path: RelPath::normalize("Skyrim.ini").unwrap(),
        };
        assert!(SkyrimSpecialEdition.validate_target(&t).is_ok());
    }

    // -----------------------------------------------------------------------
    // Installation validation and roots
    // -----------------------------------------------------------------------

    #[test]
    fn validation_requires_the_game_executable() {
        let dir = tempfile::tempdir().unwrap();
        let bad = SkyrimSpecialEdition.validate_install(dir.path());
        assert!(!bad.valid);
        assert!(bad.findings[0].contains("SkyrimSE.exe"));

        std::fs::write(dir.path().join("SkyrimSE.exe"), b"MZ").unwrap();
        assert!(SkyrimSpecialEdition.validate_install(dir.path()).valid);
    }

    #[test]
    fn an_incomplete_install_is_valid_but_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("SkyrimSE.exe"), b"MZ").unwrap();

        let v = SkyrimSpecialEdition.validate_install(dir.path());
        assert!(v.valid, "a missing master is a finding, not a rejection");
        assert!(v.findings.iter().any(|f| f.contains("Skyrim.esm")));
    }

    #[test]
    fn validation_reports_the_version_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("SkyrimSE.exe"), b"MZ").unwrap();
        std::fs::write(
            dir.path().join("Skyrim_Default.ini"),
            b"[General]\nsVersion= 1.6.1170 \n",
        )
        .unwrap();

        let v = SkyrimSpecialEdition.validate_install(dir.path());
        assert_eq!(v.reported_version.as_deref(), Some("1.6.1170"));
    }

    #[test]
    fn a_missing_version_is_absent_rather_than_invented() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("SkyrimSE.exe"), b"MZ").unwrap();
        assert!(SkyrimSpecialEdition
            .validate_install(dir.path())
            .reported_version
            .is_none());
    }

    fn install() -> LocalGameInstall {
        use onera_core::domain::game::InstallSource;
        use onera_core::ids::{GameId, LocalGameId};

        LocalGameInstall {
            id: LocalGameId::new(),
            game_id: GameId::new(),
            adapter_id: "skyrimspecialedition".into(),
            source: InstallSource::SteamNative,
            install_root: "/games/Skyrim Special Edition".into(),
            compat_prefix: Some("/steam/compatdata/489830/pfx".into()),
            user_data_roots: vec![],
            confirmed: true,
        }
    }

    #[test]
    fn deploy_roots_separate_the_install_from_user_data() {
        let roots = SkyrimSpecialEdition.deploy_roots(&install()).unwrap();
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].kind, DeployRootKind::GameInstall);
        assert_eq!(roots[1].kind, DeployRootKind::CompatPrefix);
        assert!(
            roots[1].path.ends_with("My Games/Skyrim Special Edition"),
            "{:?}",
            roots[1].path
        );

        let bare = LocalGameInstall {
            compat_prefix: None,
            ..install()
        };
        assert_eq!(SkyrimSpecialEdition.deploy_roots(&bare).unwrap().len(), 1);
    }

    #[test]
    fn the_baseline_scope_covers_the_install_but_not_user_data() {
        assert_eq!(
            SkyrimSpecialEdition.deploy_roots(&install()).unwrap().len(),
            2
        );
        let roots = SkyrimSpecialEdition.baseline_roots(&install()).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].key, ROOT_GAME);
        assert_eq!(roots[0].kind, DeployRootKind::GameInstall);
    }

    #[test]
    fn regenerated_files_are_excluded_from_the_baseline_but_real_content_is_not() {
        use onera_core::domain::baseline::excluded_by;

        let exclusions = SkyrimSpecialEdition.baseline_exclusions();
        for excluded in [
            "Data/SKSE/Plugins/EngineFixes.log",
            "crash.log",
            "steam_autocloud.vdf",
            "Data/NetScriptFramework/Crash/crash-2024.txt",
        ] {
            let path = RelPath::normalize(excluded).unwrap();
            assert!(
                excluded_by(&exclusions, ROOT_GAME, &path).is_some(),
                "{excluded} should not be part of a baseline"
            );
        }

        for included in [
            "SkyrimSE.exe",
            "Data/Skyrim.esm",
            "Data/Skyrim - Textures0.bsa",
            "Data/SKSE/Plugins/EngineFixes.dll",
        ] {
            let path = RelPath::normalize(included).unwrap();
            assert!(
                excluded_by(&exclusions, ROOT_GAME, &path).is_none(),
                "{included} belongs in the baseline"
            );
        }
    }
}
