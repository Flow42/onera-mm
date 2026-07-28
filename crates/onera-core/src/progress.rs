//! Progress reporting and cancellation.
//!
//! Long-running work in the core never blocks on a UI and never assumes one
//! exists. It emits [`ProgressEvent`]s through a [`ProgressSink`] and checks a
//! [`CancelToken`] at every safe point. The CLI renders events as lines, Tauri
//! forwards them to the frontend, and tests collect them into a vector.

use crate::ids::OperationId;
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

/// A stage of a long-running operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Reading archive metadata without writing anything.
    Inspecting,
    /// Downloading bytes from a provider.
    Downloading,
    /// Extracting into a staging directory.
    Extracting,
    /// Hashing extracted or deployed files.
    Hashing,
    /// Building a plan; no filesystem writes.
    Planning,
    /// Copying backups aside.
    BackingUp,
    /// Writing and renaming target files.
    Deploying,
    /// Re-reading deployed files to confirm them.
    Verifying,
    /// Removing files and restoring previous providers.
    Removing,
    /// Undoing a failed or interrupted operation.
    RollingBack,
}

/// An event emitted while an operation runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProgressEvent {
    /// The operation started.
    Started {
        /// Journaled operation this progress belongs to, when one exists.
        operation: Option<OperationId>,
        /// Stage being entered.
        stage: Stage,
        /// Total units of work, if known up front.
        total: Option<u64>,
    },
    /// Incremental progress within the current stage.
    Advanced {
        /// Stage being reported.
        stage: Stage,
        /// Units completed so far.
        completed: u64,
        /// Total units, if known.
        total: Option<u64>,
        /// What is being worked on right now, already redacted.
        detail: Option<String>,
    },
    /// A non-fatal problem the user should see.
    Warning {
        /// Redacted message.
        message: String,
    },
    /// The operation finished.
    Finished {
        /// Stage that completed.
        stage: Stage,
        /// Whether it completed successfully.
        success: bool,
    },
}

/// Receiver of [`ProgressEvent`]s.
///
/// Implementations must not block: they are called from inside I/O loops.
pub trait ProgressSink: Send + Sync {
    /// Handle one event.
    fn emit(&self, event: ProgressEvent);
}

/// A sink that discards everything, for callers that do not care.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullProgress;

impl ProgressSink for NullProgress {
    fn emit(&self, _event: ProgressEvent) {}
}

/// A sink that records events, for tests and for the CLI's `--json` mode.
#[derive(Debug, Default)]
pub struct RecordingProgress {
    events: Mutex<Vec<ProgressEvent>>,
}

impl RecordingProgress {
    /// Snapshot everything emitted so far.
    #[must_use]
    pub fn events(&self) -> Vec<ProgressEvent> {
        self.events.lock().expect("progress mutex poisoned").clone()
    }
}

impl ProgressSink for RecordingProgress {
    fn emit(&self, event: ProgressEvent) {
        self.events
            .lock()
            .expect("progress mutex poisoned")
            .push(event);
    }
}

/// Cooperative cancellation.
///
/// Cancellation is always cooperative and always safe: the installer only
/// checks the token between journaled steps, so a cancelled operation is left
/// in a state the recovery pass can roll back.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    /// Create an un-cancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. Idempotent and callable from any thread.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Return [`crate::CoreError::Cancelled`] if cancellation was requested.
    ///
    /// # Errors
    /// Errors exactly when [`CancelToken::is_cancelled`] is true.
    pub fn check(&self) -> crate::Result<()> {
        if self.is_cancelled() {
            return Err(crate::CoreError::Cancelled);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_token_is_shared_across_clones() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(a.check().is_ok());
        b.cancel();
        assert!(a.is_cancelled());
        assert!(matches!(a.check(), Err(crate::CoreError::Cancelled)));
    }

    #[test]
    fn recording_sink_collects_events() {
        let sink = RecordingProgress::default();
        sink.emit(ProgressEvent::Started {
            operation: None,
            stage: Stage::Planning,
            total: Some(2),
        });
        sink.emit(ProgressEvent::Finished {
            stage: Stage::Planning,
            success: true,
        });
        assert_eq!(sink.events().len(), 2);
    }
}
