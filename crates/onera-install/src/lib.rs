//! The transactional installation engine.
//!
//! This crate owns everything that touches a game directory: planning, applying,
//! rolling back, verifying, repairing and removing. It depends only on
//! [`onera_core`] ports, so the database, the filesystem and the archive backend
//! are all replaceable — which is what makes the failure paths testable.
//!
//! Two guarantees hold across every entry point here:
//!
//! * **Nothing is overwritten silently.** A file Onera did not deploy, or one
//!   that changed since it did, always stops the operation for a decision.
//! * **Every mutation is journaled before it happens.** A power cut leaves the
//!   journal ahead of the disk, never behind it, so recovery always has enough
//!   information to finish or undo.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod baseline;
pub mod engine;
pub mod fs;
pub mod lock;
pub mod planner;
pub mod reconcile;
pub mod recovery;
pub mod remove;
pub mod verify;

pub use baseline::{
    capture_baseline, quick_verify_baseline, verify_baseline, BaselineCapture,
    BaselineVerificationRequest, BaselineVerificationScan,
};
pub use engine::{InstallReport, Installer};
pub use fs::RealFileSystem;
pub use lock::GameLocks;
pub use planner::{plan_install, render_preview, PlanRequest, RootMap};
pub use reconcile::{Publication, ReconciliationAttempt, ReconciliationEngine};
pub use recovery::{recover_all, InterruptedOperation, RecoveryChoice};
pub use remove::{RemovalReport, Remover};
pub use verify::{verify_installation, VerifyReport, VerifyStatus};
