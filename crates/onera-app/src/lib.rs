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
pub mod contents;
pub mod downloads;
pub mod flow;
pub mod icon;
pub mod inbox;
pub mod logging;
pub mod paths;
pub mod presence;
pub mod profile;
pub mod secrets;
pub mod staging;

pub use baseline::{BaselineCapturePreview, BaselineStatusReport};
pub use clean::{
    CleanRestorePreview, CleanRestoreReport, RestorableFile, RestoreSource, StoreRepair,
    UnknownExtra,
};
pub use flow::{
    BrowserAction, DownloadRequest, DownloadedArchive, InstallRequest, InstalledModInfo,
    ModArtwork, ModStateInfo, Onera, PreparedInstall, PreparedState, ProfileDetails,
};
pub use contents::{ModContents, ModFileEntry, ModLocationKind};
pub use downloads::{DownloadDirChange, DownloadDirInfo, DownloadScope};
pub use icon::GameIcon;
pub use inbox::{run_request, RanRequest};
pub use paths::Paths;
pub use presence::{DesktopPresence, Presence};
pub use secrets::{InMemorySecretStore, KeyringSecretStore};
pub use staging::{StagingChange, StagingInfo};
