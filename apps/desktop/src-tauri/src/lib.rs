//! The Onera desktop application.
//!
//! A thin adapter. Every command in [`commands`] is a direct translation of one
//! [`onera_app::Onera`] method into a serialisable result: no filesystem access,
//! no installation logic and no conflict resolution happens in this crate or in
//! any frontend component. That boundary is what lets the CLI, the browser
//! extension and this window behave identically.
//!
//! Long-running operations stream [`onera_core::progress::ProgressEvent`]s to
//! the frontend over the `onera://progress` event channel and can be cancelled
//! by their operation id.
//!
//! The application is assembled by [`builder`] rather than inline in `main` so
//! that the compiled smoke suite in `tests/smoke.rs` can build and inspect the
//! same crate the window does. A command that exists but was never registered
//! is otherwise invisible until a user clicks the button that needs it.

#![forbid(unsafe_code)]

pub mod commands;
pub mod state;

use state::AppState;
use tauri::Manager as _;

/// Assemble the application, registering every command and plugin.
#[must_use]
pub fn builder() -> tauri::Builder<tauri::Wry> {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();
            // Startup does real work — XDG directories, migrations, a keyring
            // probe — so it is awaited here rather than deferred: a window that
            // paints before the database is usable would only be able to show
            // errors.
            let state = tauri::async_runtime::block_on(AppState::start(handle))?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::startup_status,
            commands::is_authenticated,
            commands::set_api_key,
            commands::forget_api_key,
            commands::account,
            commands::discover_games,
            commands::confirm_game,
            commands::add_manual_game,
            commands::local_games,
            commands::fetch_mod,
            commands::installed_mods,
            commands::check_updates,
            commands::inbox_requests,
            commands::dismiss_inbox_request,
            commands::complete_inbox_request,
            commands::downloads,
            commands::download_file,
            commands::resume_downloads,
            commands::prepare_install,
            commands::decide,
            commands::apply_plan,
            commands::cancel_operation,
            commands::verify,
            commands::profiles,
            commands::profile_members,
            commands::create_profile,
            commands::rename_profile,
            commands::delete_profile,
            commands::add_profile_member,
            commands::remove_profile_member,
            commands::set_member_state,
            commands::set_member_pin,
            commands::reorder_profile_member,
            commands::resolve_dependencies,
            commands::dependency_snapshot,
            commands::apply_dependency_plan,
            commands::set_dependency_override,
            commands::clear_dependency_override,
            commands::plan_compatible_updates,
            commands::apply_compatible_updates,
            commands::plan_profile_activation,
            commands::activate_profile,
            commands::profile_activation_history,
            commands::baseline_status,
            commands::plan_baseline_capture,
            commands::capture_baseline,
            commands::verify_baseline,
            commands::plan_return_to_clean,
            commands::apply_return_to_clean,
            commands::preview_removal,
            commands::remove_mod,
            commands::ownership,
            commands::interrupted_operations,
            commands::roll_back,
            commands::diagnostics,
        ])
}

/// Run the desktop application.
///
/// # Panics
/// Panics if the window cannot be created, which is unrecoverable.
pub fn run() {
    builder()
        .run(tauri::generate_context!())
        .expect("the Onera window could not be created");
}
