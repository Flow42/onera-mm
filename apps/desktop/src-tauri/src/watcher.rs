//! Background work the window does without being asked.
//!
//! Two loops, both started once at launch and both detached from any window:
//!
//! * the **heartbeat**, which is how the browser extension knows Onera is
//!   running and whether it needs to start it;
//! * the **inbox watcher**, which runs the requests the extension queued.
//!
//! The watcher polls rather than waiting on a notification because the writer
//! is a separate short-lived process — Chromium starts the Native Messaging
//! host, it writes a row, it exits — and a durable row in SQLite is the only
//! handoff that survives that process disappearing mid-request. A poll of an
//! indexed table on a local database costs less than the machinery required to
//! deliver a signal across a process that no longer exists.
//!
//! Nothing here decides whether a write is safe. That is
//! [`onera_app::inbox::run_request`], which stops at a plan needing a decision
//! and hands it back for the window to show.

use crate::state::AppState;
use onera_app::inbox::{run_request, RanRequest};
use onera_app::presence::{Presence, HEARTBEAT_INTERVAL};
use onera_core::progress::CancelToken;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter as _, Manager as _};

/// Event telling the frontend that the inbox changed.
pub const INBOX_EVENT: &str = "onera://inbox";

/// How often the inbox is checked for new browser requests.
///
/// Fast enough that pressing a button in the browser and switching to the
/// window feels immediate; slow enough to be invisible on a battery.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Record this process as the running desktop application until it exits.
///
/// The record is removed on a clean shutdown. It is deliberately *not* the only
/// signal of liveness — a killed process removes nothing — which is why the
/// reader also checks the pid and the record's age.
pub fn spawn_heartbeat(presence: Presence, version: String) {
    tauri::async_runtime::spawn(async move {
        loop {
            if let Err(error) = presence.beat(&version).await {
                // A user whose runtime directory is unwritable still gets a
                // working application; they lose only the browser's ability to
                // tell that it is open.
                tracing::debug!(%error, "could not record desktop presence");
            }
            tokio::time::sleep(HEARTBEAT_INTERVAL).await;
        }
    });
}

/// Run queued browser requests as they arrive, for as long as the window lives.
pub fn spawn_inbox_watcher(handle: AppHandle) {
    tauri::async_runtime::spawn(async move {
        // Requests leased before this process started belong to a process that
        // is gone, so this instant is the line between "abandoned" and "being
        // worked on right now".
        let lease_floor = chrono::Utc::now();
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            let Some(state) = handle.try_state::<AppState>() else {
                continue;
            };
            if let Err(error) = run_pending(&handle, &state, lease_floor).await {
                tracing::warn!(%error, "could not read the browser inbox");
            }
        }
    });
}

/// Run every request that is ready to run, one at a time.
///
/// Serialized deliberately: two downloads started at once from a queue the user
/// filled by clicking twice would compete for the same bandwidth and the same
/// per-hour API budget, and the second would usually be the one they wanted
/// first.
async fn run_pending(
    handle: &AppHandle,
    state: &tauri::State<'_, AppState>,
    lease_floor: chrono::DateTime<chrono::Utc>,
) -> onera_core::Result<()> {
    let pending: Vec<_> = state
        .onera
        .inbox_requests()
        .await?
        .into_iter()
        .filter(|request| request.is_runnable(lease_floor))
        .collect();

    for request in pending {
        let leased = state.onera.lease_inbox_request(&request).await?;
        let progress = state.progress();
        let cancel = CancelToken::new();
        let onera = Arc::clone(&state.onera);
        // Published while it runs so a cancel from the window can reach it. A
        // request that is only queued needs no token: dismissing it is enough,
        // because `is_runnable` will not start a request in that state.
        state
            .inbox_cancels
            .lock()
            .await
            .insert(leased.id, cancel.clone());

        let outcome = run_request(&onera, &leased, &progress, &cancel).await;
        state.inbox_cancels.lock().await.remove(&leased.id);

        match outcome {
            Ok(outcome) => {
                let note = describe(&outcome);
                // A plan that needs a decision is not finished: it is kept for
                // the window to show, and the request stays in the inbox until
                // the user has answered it.
                match outcome {
                    RanRequest::NeedsDecision { prepared, .. } => {
                        state
                            .cancels
                            .lock()
                            .await
                            .insert(prepared.plan.operation_id, cancel);
                        let operation = prepared.plan.operation_id;
                        state.prepared.lock().await.insert(operation, *prepared);
                        state
                            .onera
                            .set_inbox_state(leased.id, onera_db::jobs::InboxState::WaitingForUser)
                            .await?;
                        emit(handle, &leased.id.to_string(), "needs_decision", &note);
                    }
                    _ => {
                        state.onera.complete_inbox_request(leased.id).await?;
                        emit(handle, &leased.id.to_string(), "done", &note);
                    }
                }
            }
            // A request the user cancelled did not fail: it is gone because
            // they said so, and showing it back as an error would invite them
            // to retry the thing they just stopped.
            Err(onera_core::CoreError::Cancelled) => {
                state.onera.dismiss_inbox_request(leased.id).await?;
                emit(handle, &leased.id.to_string(), "cancelled", "cancelled");
            }
            Err(error) => {
                // The reason is kept on the request rather than only logged:
                // the user's next question is "what happened to the thing I
                // clicked", and the inbox is where they will look.
                let message = error.to_string();
                state.onera.fail_inbox_request(leased.id, &message).await?;
                emit(handle, &leased.id.to_string(), "failed", &message);
            }
        }
    }
    Ok(())
}

/// One line describing what running a request achieved.
fn describe(outcome: &RanRequest) -> String {
    match outcome {
        RanRequest::Noted { name } => format!("{name} was added"),
        RanRequest::Downloaded { name, bytes } => format!("{name} downloaded ({bytes} bytes)"),
        RanRequest::Installed { name, files } => format!("{name} installed ({files} files)"),
        RanRequest::NeedsDecision { name, .. } => {
            format!("{name} is ready to install but needs a decision")
        }
    }
}

/// Tell the frontend one request changed. A window that has gone away is not an
/// error: the outcome is already durable.
fn emit(handle: &AppHandle, request_id: &str, outcome: &str, message: &str) {
    let _ = handle.emit(
        INBOX_EVENT,
        serde_json::json!({
            "request_id": request_id,
            "outcome": outcome,
            "message": message,
        }),
    );
}
