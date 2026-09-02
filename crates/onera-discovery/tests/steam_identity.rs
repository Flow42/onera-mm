//! Fixture-driven tests for Steam build-identity discovery.
//!
//! Every `.acf` in `tests/fixtures/steam/` is a real-shaped `appmanifest`. They
//! are files rather than inline strings because the parser's contract includes
//! the file *name* — Steam indexes `steamapps/` by it — and because the store
//! adapter locates a manifest by reading a directory.
//!
//! The invariant under test throughout: an optional identity field Onera cannot
//! read with confidence becomes `None`. It never becomes a default, a
//! placeholder or a plausible-looking value, because a fabricated identifier
//! would compare equal to the next fabricated one and report a changed build as
//! unchanged.

use onera_core::domain::baseline::{BuildIdentityMatch, DepotIdentity, GameStoreKind};
use onera_core::domain::game::{InstallSource, LocalGameInstall};
use onera_core::ids::{GameId, LocalGameId};
use onera_core::ports::{GameManifestProvider, GameStore, ManifestAvailability, StoreCapability};
use onera_core::progress::CancelToken;
use onera_discovery::identity::{self, SteamBuildIdentity};
use onera_discovery::store::{self, SteamGameStore, SteamManifestProvider};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/steam")
}

fn fixture(file_name: &str) -> PathBuf {
    fixture_dir().join(file_name)
}

/// Parse one fixture manifest, expecting it to be accepted.
fn parse(file_name: &str) -> SteamBuildIdentity {
    identity::read_app_manifest(&fixture(file_name))
        .unwrap_or_else(|| panic!("{file_name} should have parsed"))
        .identity
}

fn depot(depot_id: &str, manifest_id: &str) -> DepotIdentity {
    DepotIdentity {
        depot_id: depot_id.into(),
        manifest_id: manifest_id.into(),
    }
}

fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("valid timestamp")
}

/// Build a Steam library tree under `library`, copying in the named manifests
/// and creating the `common/<installdir>` directory each one points at.
fn steam_library(library: &Path, manifests: &[&str]) {
    let steamapps = library.join("steamapps");
    std::fs::create_dir_all(steamapps.join("common")).unwrap();
    for file_name in manifests {
        let text = std::fs::read_to_string(fixture(file_name)).unwrap();
        std::fs::write(steamapps.join(file_name), &text).unwrap();
        if let Some(manifest) = identity::parse_app_manifest(&fixture(file_name), file_name, &text)
        {
            std::fs::create_dir_all(steamapps.join("common").join(manifest.install_dir)).unwrap();
        }
    }
}

fn install_at(install_root: &Path, source: InstallSource) -> LocalGameInstall {
    LocalGameInstall {
        id: LocalGameId::new(),
        game_id: GameId::new(),
        adapter_id: "cyberpunk2077".into(),
        source,
        install_root: install_root.to_path_buf(),
        compat_prefix: None,
        user_data_roots: Vec::new(),
        confirmed: true,
    }
}

// ---------------------------------------------------------------------------
// A normal manifest
// ---------------------------------------------------------------------------

#[test]
fn a_normal_manifest_yields_appid_buildid_and_manifest_path() {
    let path = fixture("appmanifest_1091500.acf");
    let manifest = identity::read_app_manifest(&path).expect("fixture should parse");

    assert_eq!(manifest.app_id, 1_091_500);
    assert_eq!(manifest.install_dir, "Cyberpunk 2077");
    assert_eq!(manifest.name.as_deref(), Some("Cyberpunk 2077"));
    assert_eq!(manifest.identity.build_id.as_deref(), Some("18320471"));
    assert_eq!(
        manifest.identity.manifest_path, path,
        "the manifest path must be retained for diagnostics"
    );
    assert_eq!(
        manifest.identity.branch, None,
        "the default branch is absence, not the string \"public\""
    );
    assert!(manifest.identity.is_comparable());
}

#[test]
fn every_installed_depot_and_its_manifest_id_is_retained() {
    let identity = parse("appmanifest_1091500.acf");
    assert_eq!(
        identity.depots,
        vec![
            depot("1091501", "5432987651234567890"),
            depot("1091502", "1122334455667788990"),
            depot("1091503", "9988776655443322110"),
        ],
        "all three depots, sorted by depot id"
    );
}

#[test]
fn shared_depots_are_not_mistaken_for_installed_ones() {
    // `SharedDepots` maps a depot to the app that owns it; its values are app
    // ids, not manifest ids. Reading them as manifests would invent identity.
    let identity = parse("appmanifest_1091500.acf");
    assert!(
        !identity.depots.iter().any(|d| d.depot_id == "228990"),
        "a shared depot leaked into the installed depot list: {:?}",
        identity.depots
    );
}

#[test]
fn the_store_identity_is_provider_neutral_and_stamped() {
    let store_identity = parse("appmanifest_1091500.acf").to_store_identity(now());

    assert_eq!(store_identity.store, GameStoreKind::Steam);
    assert_eq!(store_identity.app_id.as_deref(), Some("1091500"));
    assert_eq!(store_identity.build_id.as_deref(), Some("18320471"));
    assert_eq!(store_identity.depots.len(), 3);
    assert_eq!(store_identity.observed_at, now());
    assert!(store_identity.manifest_path.is_some());
}

// ---------------------------------------------------------------------------
// Beta branches
// ---------------------------------------------------------------------------

#[test]
fn a_beta_branch_key_is_retained() {
    let identity = parse("appmanifest_570.acf");
    assert_eq!(identity.branch.as_deref(), Some("experimental"));
    assert_eq!(identity.build_id.as_deref(), Some("19004512"));
    assert_eq!(
        identity.depots,
        vec![depot("373301", "7000000000000000001")]
    );
}

#[test]
fn the_mounted_branch_wins_over_the_requested_one() {
    // After switching branches but before downloading, `UserConfig` names the
    // branch the user asked for and `MountedConfig` names the content actually
    // on disk. A baseline describes disk.
    let identity = parse("appmanifest_400.acf");
    assert_eq!(identity.branch.as_deref(), Some("steam_legacy"));
}

#[test]
fn switching_branch_changes_the_identity() {
    let default_branch = parse("appmanifest_1091500.acf").to_store_identity(now());
    let mut beta = default_branch.clone();
    beta.branch = Some("nightly".into());

    assert_eq!(default_branch.compare(&beta), BuildIdentityMatch::Changed);
}

// ---------------------------------------------------------------------------
// Missing fields
// ---------------------------------------------------------------------------

#[test]
fn missing_optional_fields_are_unknown_rather_than_invented() {
    let manifest =
        identity::read_app_manifest(&fixture("appmanifest_620.acf")).expect("should parse");

    // The required fields are there, so the app is still discoverable...
    assert_eq!(manifest.app_id, 620);
    assert_eq!(manifest.install_dir, "Portal 2");
    // ...and every optional identity field is absent rather than defaulted.
    assert_eq!(manifest.name, None);
    assert_eq!(manifest.identity.build_id, None);
    assert_eq!(manifest.identity.branch, None);
    assert!(manifest.identity.depots.is_empty());
    assert!(
        !manifest.identity.is_comparable(),
        "nothing was recovered, so nothing can be compared"
    );
}

#[test]
fn an_incomparable_identity_never_compares_as_same() {
    let blank = parse("appmanifest_620.acf").to_store_identity(now());
    let known = parse("appmanifest_1091500.acf").to_store_identity(now());

    assert_eq!(blank.compare(&blank), BuildIdentityMatch::Unknown);
    assert_eq!(blank.compare(&known), BuildIdentityMatch::Unknown);
    assert_eq!(known.compare(&blank), BuildIdentityMatch::Unknown);
}

#[test]
fn a_manifest_missing_a_field_onera_acts_on_is_skipped_entirely() {
    let text = r#""AppState" { "appid" "42" "name" "No Directory" }"#;
    assert!(
        identity::parse_app_manifest(Path::new("appmanifest_42.acf"), "appmanifest_42.acf", text)
            .is_none(),
        "without an installdir there is no directory to attach an identity to"
    );
}

#[test]
fn a_truncated_manifest_is_rejected() {
    assert!(identity::read_app_manifest(&fixture("appmanifest_900.acf")).is_none());
}

#[test]
fn a_manifest_whose_body_names_another_app_is_rejected() {
    // Steam indexes `steamapps/` by file name; a body that disagrees is corrupt,
    // and either value would attach a build identity to the wrong game.
    assert!(identity::read_app_manifest(&fixture("appmanifest_500.acf")).is_none());
}

// ---------------------------------------------------------------------------
// Malformed optional fields
// ---------------------------------------------------------------------------

#[test]
fn malformed_optional_fields_are_dropped_not_half_recorded() {
    let manifest =
        identity::read_app_manifest(&fixture("appmanifest_730.acf")).expect("should parse");

    assert_eq!(manifest.app_id, 730);
    assert_eq!(
        manifest.name, None,
        "a whitespace-only name is no name at all"
    );
    assert_eq!(
        manifest.identity.build_id, None,
        "buildid 0 is Steam's not-yet-known placeholder"
    );
    assert_eq!(
        manifest.identity.branch, None,
        "an empty betakey means the default branch"
    );
    assert_eq!(
        manifest.identity.depots,
        vec![depot("735", "7000000000000000003")],
        "only the one well-formed depot survives, and it is trimmed"
    );
}

#[test]
fn a_partly_malformed_manifest_still_reports_what_it_could_read() {
    // Dropping bad fields may only ever weaken an identity towards `Unknown`.
    // It must never suppress the fields that did parse.
    let identity = parse("appmanifest_730.acf");
    assert!(
        identity.is_comparable(),
        "one good depot is still enough to detect a later change"
    );
}

// ---------------------------------------------------------------------------
// Change detection
// ---------------------------------------------------------------------------

#[test]
fn a_changed_buildid_or_depot_manifest_is_detected() {
    let captured = parse("appmanifest_1091500.acf").to_store_identity(now());

    let mut updated = captured.clone();
    updated.build_id = Some("18320999".into());
    assert_eq!(captured.compare(&updated), BuildIdentityMatch::Changed);

    let mut redepoted = captured.clone();
    redepoted.depots[1].manifest_id = "1111111111111111111".into();
    assert_eq!(captured.compare(&redepoted), BuildIdentityMatch::Changed);

    assert_eq!(
        captured.compare(&captured.clone()),
        BuildIdentityMatch::Same
    );
}

// ---------------------------------------------------------------------------
// Discovery retains identity
// ---------------------------------------------------------------------------

#[test]
fn discovery_retains_the_identity_of_every_installed_app() {
    let home = tempfile::tempdir().unwrap();
    let steam = home.path().join(".local/share/Steam");
    steam_library(
        &steam,
        &[
            "appmanifest_1091500.acf",
            "appmanifest_570.acf",
            "appmanifest_620.acf",
        ],
    );

    let installs = onera_discovery::steam::find_steam_installs(home.path());
    let apps = onera_discovery::steam::installed_apps(&installs[0]).unwrap();

    let cyberpunk = apps.iter().find(|a| a.app_id == 1_091_500).unwrap();
    assert_eq!(
        cyberpunk.build_identity.build_id.as_deref(),
        Some("18320471")
    );
    assert_eq!(cyberpunk.build_identity.depots.len(), 3);
    assert!(cyberpunk
        .build_identity
        .manifest_path
        .ends_with("appmanifest_1091500.acf"));

    let dota = apps.iter().find(|a| a.app_id == 570).unwrap();
    assert_eq!(dota.build_identity.branch.as_deref(), Some("experimental"));

    let portal2 = apps.iter().find(|a| a.app_id == 620).unwrap();
    assert!(!portal2.build_identity.is_comparable());
    assert_eq!(
        portal2.name, "Portal 2",
        "with no name recorded, the installdir is the fallback"
    );
}

// ---------------------------------------------------------------------------
// The GameStore adapter
// ---------------------------------------------------------------------------

/// A native Steam layout, a Flatpak one and a second-drive library differ only
/// in the prefix in front of `steamapps`, so one parameterised test covers all
/// three and proves the adapter never looks at that prefix.
fn layout_cases(home: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("native", home.join(".local/share/Steam")),
        (
            "flatpak",
            home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
        ),
        ("second drive", home.join("mnt/games/SteamLibrary")),
    ]
}

#[tokio::test]
async fn the_store_adapter_reads_identity_from_every_steam_layout() {
    let home = tempfile::tempdir().unwrap();
    let store_adapter = SteamGameStore::new();

    for (label, library) in layout_cases(home.path()) {
        steam_library(&library, &["appmanifest_1091500.acf"]);
        let root = library.join("steamapps/common/Cyberpunk 2077");
        let install = install_at(&root, InstallSource::SteamNative);

        let identity = store_adapter.build_identity(&install).await.unwrap();
        let StoreCapability::Known { value } = identity else {
            panic!("{label}: expected a known identity, got {identity:?}");
        };
        assert_eq!(value.app_id.as_deref(), Some("1091500"), "{label}");
        assert_eq!(value.build_id.as_deref(), Some("18320471"), "{label}");
        assert_eq!(value.depots.len(), 3, "{label}");
        assert_eq!(
            value.manifest_path.as_deref(),
            Some(library.join("steamapps/appmanifest_1091500.acf").as_path()),
            "{label}"
        );
    }
}

#[tokio::test]
async fn the_store_adapter_picks_the_manifest_that_names_this_directory() {
    let home = tempfile::tempdir().unwrap();
    let library = home.path().join(".local/share/Steam");
    steam_library(
        &library,
        &["appmanifest_1091500.acf", "appmanifest_570.acf"],
    );

    let install = install_at(
        &library.join("steamapps/common/dota 2 beta"),
        InstallSource::SteamNative,
    );
    let identity = SteamGameStore::new()
        .build_identity(&install)
        .await
        .unwrap();

    assert_eq!(
        identity.value().and_then(|i| i.app_id.as_deref()),
        Some("570"),
        "the manifest is chosen by installdir, not by directory order"
    );
}

#[tokio::test]
async fn a_directory_outside_a_steam_library_is_unknown_not_empty() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("Games/Cyberpunk 2077");
    std::fs::create_dir_all(&root).unwrap();

    let identity = SteamGameStore::new()
        .build_identity(&install_at(&root, InstallSource::SteamNative))
        .await
        .unwrap();

    assert!(
        matches!(identity, StoreCapability::Unknown { .. }),
        "{identity:?}"
    );
}

#[tokio::test]
async fn a_steam_library_with_no_matching_manifest_is_unknown() {
    let home = tempfile::tempdir().unwrap();
    let library = home.path().join(".local/share/Steam");
    steam_library(&library, &["appmanifest_570.acf"]);
    let orphan = library.join("steamapps/common/Unlisted Game");
    std::fs::create_dir_all(&orphan).unwrap();

    let identity = SteamGameStore::new()
        .build_identity(&install_at(&orphan, InstallSource::SteamNative))
        .await
        .unwrap();

    assert!(
        matches!(identity, StoreCapability::Unknown { .. }),
        "{identity:?}"
    );
}

#[tokio::test]
async fn a_manually_added_install_never_claims_a_steam_identity() {
    // Even sitting inside a real Steam library: Onera did not learn this path
    // from Steam, so it will not assert a Steam build for it.
    let home = tempfile::tempdir().unwrap();
    let library = home.path().join(".local/share/Steam");
    steam_library(&library, &["appmanifest_1091500.acf"]);
    let root = library.join("steamapps/common/Cyberpunk 2077");

    let identity = SteamGameStore::new()
        .build_identity(&install_at(&root, InstallSource::Manual))
        .await
        .unwrap();

    assert!(
        matches!(identity, StoreCapability::Unknown { .. }),
        "{identity:?}"
    );
    // The free function still works, so a caller that knowingly wants the
    // layout-derived identity for a manual path can ask for it explicitly.
    assert!(store::build_identity_at(&root).is_some());
}

#[tokio::test]
async fn dlc_ownership_is_unknown_rather_than_an_empty_list() {
    let home = tempfile::tempdir().unwrap();
    let library = home.path().join(".local/share/Steam");
    steam_library(&library, &["appmanifest_1091500.acf"]);
    let install = install_at(
        &library.join("steamapps/common/Cyberpunk 2077"),
        InstallSource::SteamNative,
    );

    let owned = SteamGameStore::new().owned_dlc(&install).await.unwrap();
    assert!(
        matches!(owned, StoreCapability::Unknown { .. }),
        "an empty list would let a solver conclude the user owns no DLC: {owned:?}"
    );
}

#[test]
fn the_store_adapter_has_the_expected_slug() {
    assert_eq!(SteamGameStore::new().id(), "steam");
}

// ---------------------------------------------------------------------------
// The manifest boundary
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_manifest_provider_reports_unsupported_rather_than_a_fabricated_manifest() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("game");
    std::fs::create_dir_all(&root).unwrap();
    let install = install_at(&root, InstallSource::SteamNative);
    let identity = parse("appmanifest_1091500.acf").to_store_identity(now());

    let availability = SteamManifestProvider::new()
        .expected_manifest(&install, &identity, &CancelToken::new())
        .await
        .unwrap();

    assert_eq!(
        availability,
        ManifestAvailability::Unsupported,
        "Steam publishes no consumer API for depot manifests; claiming otherwise \
         would present a local capture as a store attestation"
    );
}
