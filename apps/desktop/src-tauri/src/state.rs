//! Shared application state.
//!
//! Holds the wired-up [`onera_app::Onera`] plus the in-flight operations, so a
//! plan the user is looking at can be decided on and then applied across
//! several commands without the frontend ever holding a handle to anything that
//! can write to disk.

use onera_app::{Onera, Paths, PreparedInstall};
use onera_core::ids::OperationId;
use onera_core::progress::{CancelToken, ProgressEvent, ProgressSink};
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

/// The event channel long-running operations report on.
pub const PROGRESS_EVENT: &str = "onera://progress";

/// Forwards core progress events to the frontend.
pub struct WindowProgress {
    handle: AppHandle,
}

impl WindowProgress {
    /// Build a sink bound to the application handle.
    pub fn new(handle: AppHandle) -> Self {
        Self { handle }
    }
}

impl ProgressSink for WindowProgress {
    fn emit(&self, event: ProgressEvent) {
        // A frontend that has gone away is not an error: the operation carries
        // on and its result is still journaled.
        let _ = self.handle.emit(PROGRESS_EVENT, event);
    }
}

/// State managed by Tauri.
pub struct AppState {
    /// The application services.
    pub onera: Arc<Onera>,
    /// Prepared plans, keyed by operation id, awaiting decisions or an apply.
    pub prepared: Mutex<HashMap<OperationId, PreparedInstall>>,
    /// Cancellation tokens for in-flight operations.
    pub cancels: Mutex<HashMap<OperationId, CancelToken>>,
    /// Handle used to build progress sinks.
    pub handle: AppHandle,
}

impl AppState {
    /// Start Onera and prepare the shared state.
    ///
    /// # Errors
    /// Fails if the XDG directories cannot be created or the database cannot be
    /// opened and migrated.
    pub async fn start(handle: AppHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let paths = Paths::discover()?;
        onera_app::logging::init(
            Some(&paths.logs()),
            onera_app::logging::LogFormat::Json,
            cfg!(debug_assertions),
        )?;
        let onera = Arc::new(Onera::new(paths).await?);
        if !onera.interrupted_operations().await?.is_empty() {
            tracing::warn!("startup found an interrupted installation operation");
        }
        let state = Self {
            onera,
            prepared: Mutex::new(HashMap::new()),
            cancels: Mutex::new(HashMap::new()),
            handle,
        };

        // Download URLs are short-lived, so startup re-resolves each active
        // job instead of persisting credentials. The task is detached from the
        // window lifecycle but every result is durable in SQLite.
        let resume_onera = Arc::clone(&state.onera);
        let resume_progress = state.progress();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = resume_onera
                .resume_downloads(&resume_progress, &CancelToken::new())
                .await
            {
                tracing::warn!(%error, "could not inspect resumable downloads");
            }
        });
        Ok(state)
    }

    /// A progress sink bound to the main window.
    pub fn progress(&self) -> WindowProgress {
        WindowProgress::new(self.handle.clone())
    }

    /// Register and return a cancellation token for an operation.
    pub async fn cancel_token(&self, operation: OperationId) -> CancelToken {
        let token = CancelToken::new();
        self.cancels.lock().await.insert(operation, token.clone());
        token
    }
}

/// An error shape the frontend can branch on.
///
/// `code` is stable; `message` is display-only and already redacted by the core.
#[derive(Debug, serde::Serialize)]
pub struct CommandError {
    /// Machine-readable code.
    pub code: String,
    /// Human-readable, redacted message.
    pub message: String,
}

impl From<onera_core::CoreError> for CommandError {
    fn from(error: onera_core::CoreError) -> Self {
        use onera_core::CoreError as E;
        let code = match &error {
            E::Unauthenticated { .. } => "not_authenticated",
            E::SecretStore(_) => "secret_store",
            E::RateLimited { .. } => "rate_limited",
            E::NotFound { .. } => "not_found",
            E::DecisionRequired(_) => "decision_required",
            E::AmbiguousLayout(_) => "ambiguous_layout",
            E::ArchiveRejected { .. } => "archive_rejected",
            E::IntegrityMismatch { .. } => "integrity_mismatch",
            E::Cancelled => "cancelled",
            E::InvalidGameInstall(_) => "invalid_game",
            E::Provider(_) => "provider_error",
            _ => "internal",
        };
        Self {
            code: code.to_owned(),
            message: error.to_string(),
        }
    }
}

impl From<onera_core::RelPathError> for CommandError {
    fn from(error: onera_core::RelPathError) -> Self {
        Self {
            code: "invalid_path".to_owned(),
            message: error.to_string(),
        }
    }
}

/// Result alias for commands.
pub type CommandResult<T> = Result<T, CommandError>;
