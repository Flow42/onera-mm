//! Identifier newtypes.
//!
//! Internal identifiers are UUIDs so that rows can be created offline and
//! reconciled later. Provider identifiers are opaque strings: the installation
//! domain must never interpret them.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

macro_rules! uuid_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Generate a fresh random identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Borrow the inner UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl From<Uuid> for $name {
            fn from(v: Uuid) -> Self {
                Self(v)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(s)?))
            }
        }
    };
}

macro_rules! string_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wrap an opaque provider-supplied identifier.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Borrow the identifier as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(v: &str) -> Self {
                Self(v.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(v: String) -> Self {
                Self(v)
            }
        }
    };
}

uuid_id!(
    /// A game known to Onera (usually mirrored from a provider's game catalogue).
    GameId
);
uuid_id!(
    /// A concrete game installation on this machine.
    LocalGameId
);
uuid_id!(
    /// A mod as tracked by Onera, independent of the provider it came from.
    ModId
);
uuid_id!(
    /// One published version of a mod.
    ReleaseId
);
uuid_id!(
    /// A downloaded archive in content-addressed storage.
    ArchiveId
);
uuid_id!(
    /// An installation of a release into a local game.
    InstallationId
);
uuid_id!(
    /// A deployed file under a deployment root.
    DeployedFileId
);
uuid_id!(
    /// A journaled filesystem operation.
    OperationId
);
uuid_id!(
    /// A recorded conflict awaiting or carrying a user decision.
    ConflictId
);
uuid_id!(
    /// A persisted download job.
    DownloadJobId
);
uuid_id!(
    /// A request received from the browser extension.
    InboxRequestId
);
uuid_id!(
    /// A provider account (e.g. a signed-in Nexus user).
    AccountId
);
uuid_id!(
    /// A stored copy of a file Onera was about to overwrite.
    BackupId
);
uuid_id!(
    /// A named, game-scoped selection of mods.
    ProfileId
);
uuid_id!(
    /// One mod's membership in a profile.
    ProfileMemberId
);
uuid_id!(
    /// An immutable capture of a game's clean file set.
    BaselineId
);
uuid_id!(
    /// One run of a baseline capture or verification scan.
    BaselineScanRunId
);
uuid_id!(
    /// One fetch of a provider's dependency definition for a version.
    DependencySnapshotId
);
uuid_id!(
    /// One independent requirement inside a dependency snapshot.
    DependencyGroupId
);

string_id!(
    /// Stable slug of a provider implementation, e.g. `nexus`.
    ProviderId
);
string_id!(
    /// Provider-scoped mod identifier. Opaque to the installation domain.
    ProviderModId
);
string_id!(
    /// Provider-scoped file identifier. Opaque to the installation domain.
    ProviderFileId
);
string_id!(
    /// Provider-scoped identifier for one *version* of a file.
    ///
    /// Providers that model dependencies do so against a version identity rather
    /// than an author-written version string. Onera stores that identity opaquely
    /// and only ever compares it for equality — never for ordering, and never by
    /// parsing it. Ordering within a lineage comes from
    /// [`crate::domain::dependency::DependencyCandidate::position`], which the
    /// provider supplies.
    ProviderVersionId
);
string_id!(
    /// Provider-scoped identifier for a group of files that supersede each other.
    ///
    /// Nexus calls this an update chain; other providers may call it something
    /// else. It answers "which files are alternative versions of the same thing?"
    /// so the solver can select exactly one version per group.
    ProviderFileGroupId
);
string_id!(
    /// Store-scoped identifier for a downloadable extra (DLC).
    ///
    /// Opaque: a Steam AppID and another store's SKU are both just strings here.
    StoreDlcId
);

impl ProviderId {
    /// The built-in Nexus Mods provider slug.
    #[must_use]
    pub fn nexus() -> Self {
        Self::new("nexus")
    }
}
