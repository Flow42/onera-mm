//! Startup recovery for interrupted operations.
//!
//! On every launch Onera reads back operations that are not in a terminal state.
//! Each one is presented with the choices its state allows — the state machine in
//! [`onera_core::domain::operation`] decides which, so recovery cannot offer
//! something unsafe.

use crate::engine::Installer;
use onera_core::domain::operation::{Operation, Recovery};
use onera_core::ids::OperationId;
use onera_core::progress::ProgressSink;
use onera_core::{CoreError, Result};

/// An interrupted operation and what can be done about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptedOperation {
    /// The operation as journaled.
    pub operation: Operation,
    /// What recovery can offer.
    pub recovery: Recovery,
    /// How many of its files had already been committed.
    pub committed_files: usize,
    /// How many were staged but not committed.
    pub staged_files: usize,
}

/// What the user chose for an interrupted operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryChoice {
    /// Undo it.
    RollBack,
    /// Leave it alone for now; it will be offered again next launch.
    Defer,
}

/// Find every interrupted operation and describe it.
///
/// # Errors
/// Propagates journal errors.
pub async fn recover_all(installer: &Installer) -> Result<Vec<InterruptedOperation>> {
    let mut out = Vec::new();
    for operation in installer.journal().interrupted().await? {
        let entries = installer.journal().entries(operation.id).await?;
        let committed = entries
            .iter()
            .filter(|e| e.status == onera_core::ports::JournalStatus::Committed)
            .count();
        let staged = entries
            .iter()
            .filter(|e| e.status == onera_core::ports::JournalStatus::Staged)
            .count();
        out.push(InterruptedOperation {
            recovery: operation.state.recovery(),
            operation,
            committed_files: committed,
            staged_files: staged,
        });
    }
    Ok(out)
}

/// Act on a recovery choice.
///
/// # Errors
/// Fails if the operation is unknown or the chosen action is not available for
/// its state.
pub async fn apply_choice(
    installer: &Installer,
    operation: OperationId,
    choice: RecoveryChoice,
    progress: &dyn ProgressSink,
) -> Result<()> {
    let Some(op) = installer.journal().get(operation).await? else {
        return Err(CoreError::NotFound {
            kind: "operation",
            id: operation.to_string(),
        });
    };
    match choice {
        RecoveryChoice::Defer => Ok(()),
        RecoveryChoice::RollBack => {
            if op.state.recovery() == Recovery::None {
                return Err(CoreError::Conflict(format!(
                    "operation {operation} is {} and cannot be rolled back",
                    op.state
                )));
            }
            installer.rollback(operation, progress).await
        }
    }
}
