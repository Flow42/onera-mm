//! Steam library discovery.
//!
//! Onera reads Steam's own metadata instead of scanning the filesystem:
//!
//! 1. find Steam's root (native install, or the Flatpak sandbox);
//! 2. read `steamapps/libraryfolders.vdf` for every library, including ones on
//!    other drives;
//! 3. read each `steamapps/appmanifest_<appid>.acf` for the app's name and its
//!    `installdir`;
//! 4. derive the compatibility prefix from `steamapps/compatdata/<appid>/pfx`
//!    when one exists.
//!
//! Nothing here decides that a game *is* supported; it reports what Steam says
//! is installed, and the caller matches app ids against game adapters and the
//! provider's catalogue.

use crate::vdf;
use onera_core::domain::game::InstallSource;
use onera_core::Result;
use std::path::{Path, PathBuf};

/// A game Steam reports as installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamApp {
    /// Steam application id.
    pub app_id: u32,
    /// Name as Steam records it.
    pub name: String,
    /// The game's own directory.
    pub install_root: PathBuf,
    /// Proton prefix root, if the app has one.
    pub compat_prefix: Option<PathBuf>,
    /// User-data roots inside the prefix.
    pub user_data_roots: Vec<PathBuf>,
    /// Which Steam installation reported it.
    pub source: InstallSource,
}

/// A Steam installation and the libraries it knows about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamInstall {
    /// Steam's root directory.
    pub root: PathBuf,
    /// Whether this is the Flatpak build.
    pub source: InstallSource,
    /// Every library directory, including `root` itself.
    pub libraries: Vec<PathBuf>,
}

/// Candidate locations for a native Steam installation.
fn native_candidates(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".local/share/Steam"),
        home.join(".steam/steam"),
        home.join(".steam/root"),
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
    ]
}

/// The Flatpak Steam data directory.
fn flatpak_candidate(home: &Path) -> PathBuf {
    home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam")
}

/// Find every Steam installation under `home`.
///
/// Takes the home directory explicitly so the whole discovery path is testable
/// against a fixture tree rather than the developer's real machine.
#[must_use]
pub fn find_steam_installs(home: &Path) -> Vec<SteamInstall> {
    let flatpak = flatpak_candidate(home);
    let mut found: Vec<SteamInstall> = Vec::new();

    for candidate in native_candidates(home) {
        if !candidate.join("steamapps").is_dir() {
            continue;
        }
        // `.steam/steam` is usually a symlink to `.local/share/Steam`; comparing
        // canonical paths keeps one physical install from being reported twice.
        let canonical = candidate
            .canonicalize()
            .unwrap_or_else(|_| candidate.clone());
        if found
            .iter()
            .any(|i| i.root.canonicalize().unwrap_or_else(|_| i.root.clone()) == canonical)
        {
            continue;
        }
        let source = if canonical.starts_with(&flatpak) {
            InstallSource::SteamFlatpak
        } else {
            InstallSource::SteamNative
        };
        found.push(SteamInstall {
            libraries: read_libraries(&candidate),
            root: candidate,
            source,
        });
    }
    found
}

/// Read `libraryfolders.vdf`, falling back to the Steam root itself.
fn read_libraries(steam_root: &Path) -> Vec<PathBuf> {
    let mut libraries = vec![steam_root.to_path_buf()];
    let manifest = steam_root.join("steamapps/libraryfolders.vdf");
    let Ok(text) = std::fs::read_to_string(&manifest) else {
        return libraries;
    };
    let Ok(parsed) = vdf::parse(&text) else {
        return libraries;
    };
    let Some(folders) = parsed.get("libraryfolders") else {
        return libraries;
    };

    for (_, entry) in folders.entries() {
        // Older Steam wrote `"1" "/path"`; newer writes a nested object.
        let path = match entry {
            vdf::Value::String(s) => Some(PathBuf::from(s)),
            vdf::Value::Object(_) => entry.string("path").map(PathBuf::from),
        };
        if let Some(path) = path {
            if path.join("steamapps").is_dir() && !libraries.contains(&path) {
                libraries.push(path);
            }
        }
    }
    libraries
}

/// List the apps installed in one Steam installation.
///
/// # Errors
/// Never fails on a malformed or unreadable manifest: a single bad `.acf` skips
/// that app rather than hiding every other game.
pub fn installed_apps(install: &SteamInstall) -> Result<Vec<SteamApp>> {
    let mut apps = Vec::new();
    for library in &install.libraries {
        let steamapps = library.join("steamapps");
        let Ok(entries) = std::fs::read_dir(&steamapps) else {
            continue;
        };

        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("appmanifest_") || !name.ends_with(".acf") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let Ok(parsed) = vdf::parse(&text) else {
                continue;
            };
            let Some(state) = parsed.get("AppState") else {
                continue;
            };

            let Some(app_id) = state
                .string("appid")
                .and_then(|s| s.trim().parse::<u32>().ok())
            else {
                continue;
            };
            let Some(install_dir) = state.string("installdir") else {
                continue;
            };
            let install_root = steamapps.join("common").join(install_dir);
            if !install_root.is_dir() {
                // Steam keeps manifests for apps that are queued or removed.
                continue;
            }

            let prefix = steamapps
                .join("compatdata")
                .join(app_id.to_string())
                .join("pfx");
            let compat_prefix = prefix.is_dir().then_some(prefix.clone());
            let user_data_roots = compat_prefix
                .as_ref()
                .map(|p| user_data_candidates(p))
                .unwrap_or_default();

            apps.push(SteamApp {
                app_id,
                name: state.string("name").unwrap_or(install_dir).to_owned(),
                install_root,
                compat_prefix,
                user_data_roots,
                source: install.source,
            });
        }
    }
    apps.sort_by_key(|a| a.app_id);
    Ok(apps)
}

/// User-data directories inside a Proton prefix.
///
/// A prefix's `drive_c/users/steamuser` holds `Documents`, `Saved Games` and
/// `AppData`, which is where a Windows game writes per-user files. These are
/// modelled separately from the install root because mods, saves and config
/// genuinely live in different places.
fn user_data_candidates(prefix: &Path) -> Vec<PathBuf> {
    let user = prefix.join("drive_c/users/steamuser");
    [
        "Documents",
        "Saved Games",
        "AppData/Local",
        "AppData/Roaming",
    ]
    .iter()
    .map(|sub| user.join(sub))
    .filter(|p| p.is_dir())
    .collect()
}

/// The user's home directory.
///
/// # Errors
/// Fails if the platform cannot report one.
pub fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| {
        onera_core::CoreError::InvalidInput("cannot determine the home directory".into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fake Steam tree: a main library plus a second one on "another
    /// drive", with one installed game and one manifest whose directory is
    /// missing.
    fn fixture() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let steam = home.path().join(".local/share/Steam");
        let other = home.path().join("mnt/games/SteamLibrary");

        for library in [&steam, &other] {
            std::fs::create_dir_all(library.join("steamapps/common")).unwrap();
        }
        std::fs::create_dir_all(steam.join("steamapps/common/Cyberpunk 2077/bin/x64")).unwrap();
        std::fs::create_dir_all(
            steam.join("steamapps/compatdata/1091500/pfx/drive_c/users/steamuser/Documents"),
        )
        .unwrap();
        std::fs::create_dir_all(other.join("steamapps/common/Other Game")).unwrap();

        std::fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            format!(
                r#"
"libraryfolders"
{{
    "0" {{ "path" "{}" }}
    "1" {{ "path" "{}" }}
}}
"#,
                steam.display(),
                other.display()
            ),
        )
        .unwrap();

        std::fs::write(
            steam.join("steamapps/appmanifest_1091500.acf"),
            r#"
"AppState"
{
    "appid"      "1091500"
    "name"       "Cyberpunk 2077"
    "installdir" "Cyberpunk 2077"
}
"#,
        )
        .unwrap();
        std::fs::write(
            other.join("steamapps/appmanifest_700.acf"),
            r#""AppState" { "appid" "700" "name" "Other Game" "installdir" "Other Game" }"#,
        )
        .unwrap();
        // A manifest whose directory Steam has already deleted.
        std::fs::write(
            steam.join("steamapps/appmanifest_999.acf"),
            r#""AppState" { "appid" "999" "name" "Ghost" "installdir" "Ghost" }"#,
        )
        .unwrap();
        // A file that is not a manifest at all.
        std::fs::write(steam.join("steamapps/readme.txt"), b"ignore me").unwrap();

        home
    }

    #[test]
    fn finds_a_native_install_and_all_its_libraries() {
        let home = fixture();
        let installs = find_steam_installs(home.path());
        assert_eq!(installs.len(), 1);
        assert_eq!(installs[0].source, InstallSource::SteamNative);
        assert_eq!(
            installs[0].libraries.len(),
            2,
            "the second drive was not picked up"
        );
    }

    #[test]
    fn lists_installed_apps_across_libraries() {
        let home = fixture();
        let installs = find_steam_installs(home.path());
        let apps = installed_apps(&installs[0]).unwrap();

        assert_eq!(apps.len(), 2, "expected one app per library: {apps:?}");
        let cp = apps.iter().find(|a| a.app_id == 1_091_500).unwrap();
        assert_eq!(cp.name, "Cyberpunk 2077");
        assert!(cp.install_root.ends_with("common/Cyberpunk 2077"));
        assert!(apps.iter().any(|a| a.app_id == 700));
    }

    #[test]
    fn ignores_manifests_whose_directory_is_gone() {
        let home = fixture();
        let apps = installed_apps(&find_steam_installs(home.path())[0]).unwrap();
        assert!(
            !apps.iter().any(|a| a.app_id == 999),
            "a stale manifest was reported as installed"
        );
    }

    #[test]
    fn finds_the_compatibility_prefix_and_user_data_roots() {
        let home = fixture();
        let apps = installed_apps(&find_steam_installs(home.path())[0]).unwrap();
        let cp = apps.iter().find(|a| a.app_id == 1_091_500).unwrap();

        let prefix = cp.compat_prefix.as_ref().expect("prefix not detected");
        assert!(prefix.ends_with("compatdata/1091500/pfx"));
        assert_eq!(cp.user_data_roots.len(), 1);
        assert!(cp.user_data_roots[0].ends_with("steamuser/Documents"));

        // A game with no prefix reports none rather than an invented path.
        let other = apps.iter().find(|a| a.app_id == 700).unwrap();
        assert_eq!(other.compat_prefix, None);
        assert!(other.user_data_roots.is_empty());
    }

    #[test]
    fn detects_a_flatpak_install() {
        let home = tempfile::tempdir().unwrap();
        let flatpak = home
            .path()
            .join(".var/app/com.valvesoftware.Steam/.local/share/Steam");
        std::fs::create_dir_all(flatpak.join("steamapps/common")).unwrap();

        let installs = find_steam_installs(home.path());
        assert_eq!(installs.len(), 1);
        assert_eq!(installs[0].source, InstallSource::SteamFlatpak);
    }

    #[test]
    fn a_missing_steam_is_not_an_error() {
        let home = tempfile::tempdir().unwrap();
        assert!(find_steam_installs(home.path()).is_empty());
    }

    #[test]
    fn a_corrupt_library_file_falls_back_to_the_steam_root() {
        let home = tempfile::tempdir().unwrap();
        let steam = home.path().join(".local/share/Steam");
        std::fs::create_dir_all(steam.join("steamapps/common")).unwrap();
        std::fs::write(steam.join("steamapps/libraryfolders.vdf"), b"{{{ not valid").unwrap();

        let installs = find_steam_installs(home.path());
        assert_eq!(
            installs[0].libraries,
            vec![steam],
            "a corrupt file must not hide the main library"
        );
    }

    #[test]
    fn a_corrupt_app_manifest_skips_only_that_app() {
        let home = fixture();
        let steam = home.path().join(".local/share/Steam");
        std::fs::write(
            steam.join("steamapps/appmanifest_1.acf"),
            b"\"AppState\" { unterminated",
        )
        .unwrap();

        let apps = installed_apps(&find_steam_installs(home.path())[0]).unwrap();
        assert_eq!(apps.len(), 2, "one bad manifest must not hide the others");
    }
}
