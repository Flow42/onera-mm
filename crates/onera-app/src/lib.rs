//! Application services.
//!
//! This crate is the seam between Onera's core and its drivers. It owns the
//! wiring — which database, which secret store, which provider — and exposes one
//! API that the Tauri commands, the CLI and the Native Messaging host all call.
//!
//! Nothing here contains filesystem, installation or conflict logic. Those live
//! in [`onera_install`]; this crate sequences them.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod baseline;
pub mod clean;
pub mod flow;
pub mod logging;
pub mod paths;
pub mod secrets;

pub use baseline::{BaselineCapturePreview, BaselineStatusReport};
pub use clean::{
    CleanRestorePreview, CleanRestoreReport, RestorableFile, RestoreSource, StoreRepair,
    UnknownExtra,
};
pub use flow::{
    DownloadRequest, DownloadedArchive, InstallRequest, InstalledModInfo, Onera, PreparedInstall,
    PreparedState, ProfileDetails,
};
pub use paths::Paths;
pub use secrets::{InMemorySecretStore, KeyringSecretStore};
