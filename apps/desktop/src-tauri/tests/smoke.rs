//! Compiled smoke suite for the desktop application.
//!
//! The Playwright specs drive the real SvelteKit views against a *stubbed*
//! bridge, which is fast and covers the frontend well. They are kept, and they
//! have one structural blind spot: the stub answers whatever the frontend asks
//! for, so it cannot notice that the compiled application would not have
//! answered at all.
//!
//! This suite covers the two things only a compiled build can check:
//!
//! 1. **Registration.** Every `#[tauri::command]` is wired into the handler
//!    list. A command that exists but was never registered compiles, passes
//!    clippy, and fails at runtime with `Command not found` the first time a
//!    user clicks the button that needs it — which is exactly the kind of
//!    failure a stubbed bridge cannot see.
//! 2. **Startup.** The desktop's own start-up path — XDG discovery, migrations,
//!    the interrupted-operation sweep — runs and produces a usable application
//!    for the four documented flows: install, profile switch, dependency
//!    prompt, and recovery.
//!
//! What this deliberately does **not** do is drive a real window through
//! WebDriver. That needs `tauri-driver`, a display server and a webkit
//! WebDriver binary in CI, and it is documented as a gap in
//! `docs/test-strategy.md` rather than half-implemented here.

use onera_app::{Onera, Paths};
use onera_core::ports::{BaselineStore, DeploymentStore, ProfileStore};
use std::collections::BTreeSet;

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Names of every `#[tauri::command]` in `commands.rs`.
fn declared_commands() -> BTreeSet<String> {
    let source = include_str!("../src/commands.rs");
    let mut names = BTreeSet::new();
    let mut lines = source.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim() != "#[tauri::command]" {
            continue;
        }
        // The attribute is followed by the function, possibly after further
        // attributes or doc comments.
        for following in lines.by_ref() {
            let trimmed = following.trim_start();
            if let Some(rest) = trimmed
                .strip_prefix("pub async fn ")
                .or_else(|| trimmed.strip_prefix("pub fn "))
            {
                let name = rest
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .next()
                    .unwrap_or_default();
                assert!(
                    !name.is_empty(),
                    "could not read a command name from {following}"
                );
                names.insert(name.to_owned());
                break;
            }
            if trimmed.starts_with("#[") || trimmed.starts_with("///") || trimmed.is_empty() {
                continue;
            }
            panic!("#[tauri::command] is not followed by a function: {following}");
        }
    }
    names
}

/// Names listed inside `generate_handler!` in `lib.rs`.
fn registered_commands() -> BTreeSet<String> {
    let source = include_str!("../src/lib.rs");
    let start = source
        .find("generate_handler![")
        .expect("the builder registers a handler list");
    let body = &source[start..];
    let end = body.find("])").expect("the handler list is closed");

    body[..end]
        .lines()
        .filter_map(|line| line.trim().strip_prefix("commands::"))
        .map(|name| name.trim_end_matches(',').to_owned())
        .collect()
}

/// The check that a stubbed bridge structurally cannot make.
#[test]
fn every_command_is_registered_with_the_application() {
    let declared = declared_commands();
    let registered = registered_commands();

    assert!(
        !declared.is_empty() && !registered.is_empty(),
        "the source scan found nothing; the parser has drifted from the code"
    );

    let unregistered: Vec<&String> = declared.difference(&registered).collect();
    assert!(
        unregistered.is_empty(),
        "these commands exist but are not registered, so the frontend would get \
         `Command not found`: {unregistered:?}"
    );

    let unknown: Vec<&String> = registered.difference(&declared).collect();
    assert!(
        unknown.is_empty(),
        "these commands are registered but no longer exist: {unknown:?}"
    );
}

/// The frontend contract names commands the backend must keep providing.
/// Renaming one is a breaking change that has to be made on both sides.
#[test]
fn the_documented_flows_have_the_commands_they_name() {
    let registered = registered_commands();
    for (flow, commands) in [
        (
            "install",
            &["prepare_install", "decide", "apply_plan", "verify"][..],
        ),
        (
            "profile switch",
            &[
                "profiles",
                "profile_members",
                "plan_profile_activation",
                "activate_profile",
            ][..],
        ),
        (
            "dependency prompt",
            &[
                "resolve_dependencies",
                "dependency_snapshot",
                "apply_dependency_plan",
                "set_dependency_override",
            ][..],
        ),
        (
            "recovery",
            &["startup_status", "interrupted_operations", "roll_back"][..],
        ),
    ] {
        for command in commands {
            assert!(
                registered.contains(*command),
                "the {flow} flow needs `{command}`, which is not registered"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

/// Build the application the way `AppState::start` does, against a scratch
/// root rather than the developer's real data directory.
async fn start_in(root: &std::path::Path) -> Onera {
    onera_app::logging::init(
        Some(&root.join("logs")),
        onera_app::logging::LogFormat::Json,
        false,
    )
    .expect("logging initializes");
    Onera::new(Paths::rooted_at(root.to_path_buf()))
        .await
        .expect("the desktop application starts")
}

/// The desktop's start-up path runs migrations and answers every question the
/// first screen asks, on a machine with no existing data.
#[tokio::test]
async fn a_first_run_starts_and_reports_a_clean_slate() {
    let dir = tempfile::tempdir().unwrap();
    let onera = start_in(dir.path()).await;

    // Exactly the reads `startup_status` performs.
    assert!(onera.interrupted_operations().await.unwrap().is_empty());
    assert!(onera
        .recover_profile_activations()
        .await
        .unwrap()
        .is_empty());
    assert!(onera.inbox_requests().await.unwrap().is_empty());
    assert_eq!(onera.expired_prepared_plans(), 0);
    assert!(
        !onera.is_authenticated().await.unwrap(),
        "a first run must not report a credential it does not have"
    );

    // The database really was created and migrated under the given root.
    assert!(
        Paths::rooted_at(dir.path().to_path_buf())
            .database()
            .exists(),
        "startup did not create the database"
    );
}

/// Register a real game, so rows that reference one can be written.
async fn register_game(
    onera: &Onera,
    install_root: &std::path::Path,
) -> onera_core::ids::LocalGameId {
    std::fs::create_dir_all(install_root.join("bin/x64")).unwrap();
    std::fs::write(install_root.join("bin/x64/Cyberpunk2077.exe"), b"MZ").unwrap();

    onera
        .database()
        .upsert_game(&onera_core::domain::game::Game {
            id: onera_core::ids::GameId::new(),
            provider: onera_core::ids::ProviderId::nexus(),
            provider_slug: "cyberpunk2077".to_owned(),
            name: "Cyberpunk 2077".to_owned(),
            steam_app_id: Some(1_091_500),
        })
        .await
        .unwrap();

    onera
        .confirm_game(&onera_discovery::DiscoveredGame {
            adapter_id: "cyberpunk2077".to_owned(),
            provider_slug: Some("cyberpunk2077".to_owned()),
            name: "Cyberpunk 2077".to_owned(),
            install_root: install_root.to_path_buf(),
            compat_prefix: None,
            user_data_roots: vec![],
            source: onera_core::domain::game::InstallSource::Manual,
            validation: onera_core::domain::game::InstallValidation::ok(),
        })
        .await
        .unwrap()
}

/// Restarting against the same root reopens the same database rather than
/// migrating a fresh one over it.
///
/// Confirming a game creates its built-in `Default` profile, so the profile
/// list after the restart is the observable proof that the second start read
/// the file the first one wrote.
#[tokio::test]
async fn a_restart_reopens_the_existing_database() {
    let dir = tempfile::tempdir().unwrap();
    let game = {
        let first = start_in(dir.path()).await;
        let game = register_game(&first, &dir.path().join("game")).await;
        assert!(!ProfileStore::profiles(first.database(), game)
            .await
            .unwrap()
            .is_empty());
        game
    };

    let second = start_in(dir.path()).await;
    assert!(second.interrupted_operations().await.unwrap().is_empty());
    assert!(
        !ProfileStore::profiles(second.database(), game)
            .await
            .unwrap()
            .is_empty(),
        "the restart did not reopen the database the first run wrote"
    );
    assert_eq!(
        second.database().local_installs().await.unwrap().len(),
        1,
        "the registered game did not survive the restart"
    );
}

/// Every read the four documented flows begin with must answer on a fresh
/// installation rather than erroring, because each is the first thing its
/// screen does when the user opens it.
#[tokio::test]
async fn the_documented_flows_open_without_data() {
    let dir = tempfile::tempdir().unwrap();
    let onera = start_in(dir.path()).await;
    let game = onera_core::ids::LocalGameId::new();

    // Install and recovery.
    assert!(onera.installed_mods(game).await.unwrap().is_empty());
    assert!(onera.interrupted_operations().await.unwrap().is_empty());
    assert!(onera.downloads().await.unwrap().is_empty());

    // Profile switch.
    assert!(ProfileStore::profiles(onera.database(), game)
        .await
        .unwrap()
        .is_empty());
    assert!(ProfileStore::active_profile(onera.database(), game)
        .await
        .unwrap()
        .is_none());

    // Integrity, which the recovery screen links to.
    assert!(BaselineStore::current_baseline(onera.database(), game)
        .await
        .unwrap()
        .is_none());
    assert!(DeploymentStore::all_targets(onera.database(), game)
        .await
        .unwrap()
        .is_empty());
}

/// The adapters the compiled build ships.
///
/// The window offers whatever `onera-games` registers, so a second adapter has
/// to actually reach the binary rather than only the core tests.
#[test]
fn the_compiled_build_ships_every_adapter() {
    let ids: BTreeSet<&str> = onera_games::all_adapters()
        .into_iter()
        .map(onera_core::ports::GameAdapter::id)
        .collect();

    assert!(ids.contains("cyberpunk2077"));
    assert!(
        ids.contains("skyrimspecialedition"),
        "the desktop build does not ship the second adapter: {ids:?}"
    );
}
