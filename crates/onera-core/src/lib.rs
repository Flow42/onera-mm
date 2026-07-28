//! Onera domain and application core.
//!
//! This crate holds the *game-agnostic, provider-agnostic* model: identifiers,
//! hashes, normalized relative paths, the file-provider stack, installation
//! plans, and the port traits every adapter implements.
//!
//! Invariants enforced here:
//!
//! * No provider-specific identifier type ever appears in the installation
//!   domain. Providers are addressed through the opaque [`ProviderId`],
//!   [`ProviderModId`] and [`ProviderFileId`] newtypes.
//! * Every path that reaches the filesystem is a [`RelPath`], which cannot
//!   represent an absolute path, a drive prefix, a `..` component or a
//!   separator-ambiguous segment.
//! * Version strings are stored verbatim and are only ever compared within a
//!   single provider mod lineage. See [`domain::release::Release`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod domain;
pub mod error;
pub mod hash;
pub mod ids;
pub mod paths;
pub mod plan;
pub mod ports;
pub mod progress;
pub mod redact;

pub use error::{CoreError, Result};
pub use hash::{FileHash, HashAlgorithm};
pub use ids::{
    AccountId, ArchiveId, BackupId, ConflictId, DeployedFileId, DownloadJobId, GameId,
    InstallationId, LocalGameId, ModId, OperationId, ProviderFileId, ProviderId, ProviderModId,
    ReleaseId,
};
pub use paths::{DeployRootKind, RelPath, RelPathError};
