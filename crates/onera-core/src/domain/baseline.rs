//! Game baselines: what "clean" means for one concrete installation.
//!
//! A baseline is a **local observation**, stamped with whatever build identity
//! the store exposed at capture time. It is deliberately not a claim that the
//! store attested every byte: the consumer Steam client publishes no supported
//! API for the complete expected manifest, so Onera captures what is on disk
//! after the user has run the store's own verification and records the build it
//! saw. [`crate::ports::GameManifestProvider`] exists so an authoritative
//! manifest can replace local capture later without changing anything else.
//!
//! Two rules run through this module:
//!
//! * **Build identity is compared, never ordered.** [`StoreBuildIdentity`] holds
//!   opaque strings. A different BuildID means *changed*, not *newer*; a missing
//!   BuildID means *unknown*, not *unchanged*.
//! * **`clean` requires content hashing.** A size/mtime quick scan is allowed for
//!   responsiveness, but [`BaselineVerification::is_clean`] refuses to report
//!   clean from metadata alone.
//!
//! Nothing here deletes anything. An unknown extra file is a *finding*; the
//! decision to remove it is always the user's.

use crate::hash::FileHash;
use crate::ids::{BaselineId, BaselineScanRunId, LocalGameId, StoreDlcId};
use crate::paths::{DeployRootKind, RelPath};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Which store manages a game installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameStoreKind {
    /// Valve's Steam client, native or Flatpak.
    Steam,
    /// A directory the user pointed Onera at. No store identity is available.
    Manual,
}

/// One depot as recorded in a store's local installation manifest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DepotIdentity {
    /// Store's opaque depot identifier.
    pub depot_id: String,
    /// Store's opaque manifest identifier for the installed depot content.
    pub manifest_id: String,
}

/// Best-effort build identity read from the store's own local metadata.
///
/// Every identifier is an opaque string. Onera compares them for equality and
/// nothing else — see [`StoreBuildIdentity::compare`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreBuildIdentity {
    /// Store that supplied the identity.
    pub store: GameStoreKind,
    /// Store's application identifier, when it has one.
    pub app_id: Option<String>,
    /// Store's build identifier for the installed content.
    pub build_id: Option<String>,
    /// Branch or beta key, when the installation is not on the default branch.
    pub branch: Option<String>,
    /// Installed depots with their manifest identifiers, when exposed.
    pub depots: Vec<DepotIdentity>,
    /// Path of the manifest file the identity was read from, for diagnostics.
    pub manifest_path: Option<PathBuf>,
    /// When Onera read it.
    pub observed_at: DateTime<Utc>,
}

/// The result of comparing two build identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildIdentityMatch {
    /// Both identities are known and every compared field is equal.
    Same,
    /// Both identities are known and at least one compared field differs.
    Changed,
    /// At least one side lacks the identifiers needed to decide.
    ///
    /// Distinct from [`BuildIdentityMatch::Same`] on purpose: "we could not tell"
    /// must never be presented as "nothing changed".
    Unknown,
}

impl StoreBuildIdentity {
    /// A build identity with nothing but the store kind known.
    #[must_use]
    pub fn unknown(store: GameStoreKind, observed_at: DateTime<Utc>) -> Self {
        Self {
            store,
            app_id: None,
            build_id: None,
            branch: None,
            depots: Vec::new(),
            manifest_path: None,
            observed_at,
        }
    }

    /// Whether enough is known to compare this identity with another.
    #[must_use]
    pub fn is_comparable(&self) -> bool {
        self.build_id.is_some() || !self.depots.is_empty()
    }

    /// Compare two identities by equality of opaque identifiers.
    ///
    /// Never orders and never parses: a build that differs is `Changed`, whether
    /// the store rolled forward or the user rolled back. `observed_at` and
    /// `manifest_path` are diagnostics and are not compared.
    #[must_use]
    pub fn compare(&self, other: &Self) -> BuildIdentityMatch {
        if self.store != other.store || !self.is_comparable() || !other.is_comparable() {
            return BuildIdentityMatch::Unknown;
        }
        let mut mine = self.depots.clone();
        let mut theirs = other.depots.clone();
        mine.sort();
        theirs.sort();
        if self.app_id == other.app_id
            && self.build_id == other.build_id
            && self.branch == other.branch
            && mine == theirs
        {
            BuildIdentityMatch::Same
        } else {
            BuildIdentityMatch::Changed
        }
    }
}

/// Where a baseline's authority comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaselineSource {
    /// Captured after the user confirmed the store's own file verification.
    ///
    /// The strongest baseline Onera can currently produce, and still a local
    /// observation rather than a store attestation.
    StoreVerifiedCapture,
    /// Captured from whatever was on disk, with no store verification step.
    ///
    /// Correct for manual and non-Steam installs. Must be labelled as such in
    /// the UI so nobody reads it as a proof of cleanliness.
    LocalSnapshot,
    /// Derived from an authoritative manifest supplied by the store.
    ///
    /// Reserved for a future [`crate::ports::GameManifestProvider`]; nothing
    /// produces this today.
    StoreManifest,
}

impl BaselineSource {
    /// Whether the user was asked to run the store's verification first.
    #[must_use]
    pub const fn is_store_verified(self) -> bool {
        matches!(self, Self::StoreVerifiedCapture | Self::StoreManifest)
    }
}

/// Lifecycle of a captured baseline.
///
/// Baselines are never overwritten. A game update supersedes the old baseline
/// and keeps it, so history survives a recapture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaselineStatus {
    /// A scan is running; the record is not usable yet.
    Capturing,
    /// The baseline Onera compares against.
    Current,
    /// Retained history, replaced by a newer capture.
    Superseded,
    /// The capture did not finish.
    Failed,
}

/// Whether the current baseline still describes the installed build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BaselineFreshness {
    /// No baseline has ever been captured for this game.
    None,
    /// Store identity is unchanged since capture.
    Fresh,
    /// The store's build identity changed; recapture after verifying files.
    Stale {
        /// Identity recorded when the baseline was captured.
        captured: Box<StoreBuildIdentity>,
        /// Identity observed now.
        observed: Box<StoreBuildIdentity>,
    },
    /// Freshness cannot be determined — no comparable identity on one side.
    ///
    /// Deliberately not [`BaselineFreshness::Fresh`]: an unverifiable baseline
    /// must be shown as unverifiable.
    Unknown {
        /// Why the comparison could not be made, safe to display.
        reason: String,
    },
}

impl BaselineFreshness {
    /// Compare a captured identity with a currently observed one.
    #[must_use]
    pub fn evaluate(
        captured: Option<&StoreBuildIdentity>,
        observed: Option<&StoreBuildIdentity>,
    ) -> Self {
        match (captured, observed) {
            (None, _) => Self::Unknown {
                reason: "the baseline was captured without a store build identity".to_owned(),
            },
            (Some(_), None) => Self::Unknown {
                reason: "no store build identity is available for this installation".to_owned(),
            },
            (Some(captured), Some(observed)) => match captured.compare(observed) {
                BuildIdentityMatch::Same => Self::Fresh,
                BuildIdentityMatch::Changed => Self::Stale {
                    captured: Box::new(captured.clone()),
                    observed: Box::new(observed.clone()),
                },
                BuildIdentityMatch::Unknown => Self::Unknown {
                    reason: "the store did not expose a comparable build identity".to_owned(),
                },
            },
        }
    }

    /// Whether the user should be prompted to verify and recapture.
    #[must_use]
    pub const fn needs_recapture(&self) -> bool {
        matches!(self, Self::None | Self::Stale { .. })
    }
}

/// An immutable capture of a game's clean file set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameBaseline {
    /// Onera's identifier.
    pub id: BaselineId,
    /// The installation this describes.
    pub local_game_id: LocalGameId,
    /// Where its authority comes from.
    pub source: BaselineSource,
    /// Build identity observed at capture time, when the store exposed one.
    pub build_identity: Option<StoreBuildIdentity>,
    /// Adapter that declared the scanned roots and exclusions.
    pub adapter_id: String,
    /// Version of the game the adapter reported, verbatim and never parsed.
    pub reported_version: Option<String>,
    /// Lifecycle state.
    pub status: BaselineStatus,
    /// When the capture completed.
    pub captured_at: DateTime<Utc>,
    /// Fingerprint of the scanned scope, so a scope change is detectable.
    pub scope_fingerprint: ScanScopeFingerprint,
    /// Number of files recorded.
    pub file_count: u64,
    /// Total bytes recorded.
    pub total_bytes: u64,
}

/// Fingerprint of the roots and exclusions a scan covered.
///
/// Two baselines are only comparable when they covered the same scope. Changing
/// an adapter's exclusion list changes this value, which is how a "clean" result
/// from a narrower scan is prevented from masquerading as the old one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScanScopeFingerprint(String);

impl ScanScopeFingerprint {
    /// Compute the fingerprint of a scan scope.
    ///
    /// Order-independent: roots and exclusions are canonicalized before hashing
    /// so that merely reordering an adapter's declarations does not invalidate a
    /// baseline.
    #[must_use]
    pub fn of(roots: &[BaselineRoot], exclusions: &[BaselineExclusion]) -> Self {
        let mut lines: Vec<String> = roots
            .iter()
            .map(|r| format!("root\u{1f}{}\u{1f}{}", r.key, r.path.display()))
            .chain(exclusions.iter().map(|e| {
                format!(
                    "exclude\u{1f}{}\u{1f}{}",
                    e.root_key.as_deref().unwrap_or("*"),
                    e.pattern.canonical()
                )
            }))
            .collect();
        lines.sort();
        lines.dedup();
        Self(FileHash::blake3_of(lines.join("\u{1e}").as_bytes()).hex)
    }

    /// The fingerprint as a hex string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ScanScopeFingerprint {
    /// Rebuild a fingerprint from a stored value.
    ///
    /// Only persistence should use this: a fingerprint that was not produced by
    /// [`ScanScopeFingerprint::of`] describes no scope and would compare equal
    /// to nothing.
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// A directory a baseline scan covers.
///
/// Only store-managed locations belong here. User-data roots are excluded by
/// default: saves and per-user configuration change constantly and are not part
/// of what "clean" means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineRoot {
    /// Stable adapter-defined key, matching the deploy-root key where they
    /// coincide, so findings can be reported against the same location space.
    pub key: String,
    /// Which class of location this is.
    pub kind: DeployRootKind,
    /// Absolute path on this machine.
    pub path: PathBuf,
}

/// What an exclusion matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExclusionPattern {
    /// The path itself and everything under it.
    Prefix {
        /// Directory or file, relative to the root.
        path: RelPath,
    },
    /// Exactly this path.
    Exact {
        /// File, relative to the root.
        path: RelPath,
    },
    /// Any file with this extension, compared case-insensitively.
    Extension {
        /// Extension without the leading dot.
        extension: String,
    },
    /// Any directory component with this name, compared case-insensitively.
    DirectoryName {
        /// Directory name.
        name: String,
    },
}

impl ExclusionPattern {
    /// Whether this pattern covers a path under some root.
    #[must_use]
    pub fn matches(&self, path: &RelPath) -> bool {
        match self {
            Self::Exact { path: exact } => path == exact,
            Self::Prefix { path: prefix } => {
                let (p, needle) = (path.as_str(), prefix.as_str());
                p == needle
                    || (p.len() > needle.len()
                        && p.starts_with(needle)
                        && p.as_bytes()[needle.len()] == b'/')
            }
            Self::Extension { extension } => path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case(extension)),
            Self::DirectoryName { name } => path
                .as_str()
                .split('/')
                .rev()
                .skip(1)
                .any(|c| c.eq_ignore_ascii_case(name)),
        }
    }

    /// Stable textual form, used by [`ScanScopeFingerprint`].
    #[must_use]
    pub fn canonical(&self) -> String {
        match self {
            Self::Exact { path } => format!("exact:{path}"),
            Self::Prefix { path } => format!("prefix:{path}"),
            Self::Extension { extension } => format!("ext:{}", extension.to_ascii_lowercase()),
            Self::DirectoryName { name } => format!("dir:{}", name.to_ascii_lowercase()),
        }
    }
}

/// Why something is outside the trusted baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    /// Saves and per-user data.
    UserData,
    /// Logs and crash dumps.
    Logs,
    /// Caches the game rebuilds on demand.
    Cache,
    /// Shader caches, which differ per driver and per machine.
    ShaderCache,
    /// Configuration the game writes at runtime.
    GeneratedConfig,
    /// A directory Onera itself deploys into and therefore manages.
    ModManaged,
    /// Anything else the adapter chose to exclude, explained in `note`.
    Other,
}

/// One declared exclusion from a baseline scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineExclusion {
    /// Root the exclusion applies to; `None` applies it to every root.
    pub root_key: Option<String>,
    /// What it matches.
    pub pattern: ExclusionPattern,
    /// Why it is excluded.
    pub reason: ExclusionReason,
    /// Displayable note, shown in the capture summary.
    pub note: Option<String>,
}

impl BaselineExclusion {
    /// Whether this exclusion applies to a path under a given root.
    #[must_use]
    pub fn matches(&self, root_key: &str, path: &RelPath) -> bool {
        self.root_key.as_deref().is_none_or(|k| k == root_key) && self.pattern.matches(path)
    }
}

/// The first exclusion covering a path, if any.
#[must_use]
pub fn excluded_by<'a>(
    exclusions: &'a [BaselineExclusion],
    root_key: &str,
    path: &RelPath,
) -> Option<&'a BaselineExclusion> {
    exclusions.iter().find(|e| e.matches(root_key, path))
}

/// One file recorded in a baseline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineFile {
    /// Root the file lives under.
    pub root_key: String,
    /// Path relative to that root.
    pub path: RelPath,
    /// BLAKE3 of the contents at capture time.
    pub hash: FileHash,
    /// Size in bytes.
    pub size: u64,
    /// Unix mode, when the platform reports one. Recorded so a lost executable
    /// bit is visible; never used as an integrity decision on its own.
    pub mode: Option<u32>,
}

/// How one path compares with the baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileClassification {
    /// Present with the recorded contents.
    Matching,
    /// Present with different contents.
    Modified,
    /// Recorded in the baseline but absent from disk.
    Missing,
    /// Not in the baseline, but Onera deployed it and knows who provides it.
    ExtraManaged,
    /// Not in the baseline and not deployed by Onera.
    ///
    /// Never deleted automatically. Each one needs an individual user decision.
    ExtraUnknown,
    /// Could not be read: permissions, I/O error, or a vanished file.
    Unreadable,
    /// A symlink, device node, socket or other non-regular file.
    ///
    /// Rejected from the trusted baseline and reported rather than hashed: a
    /// symlink's target is outside the scope Onera can reason about.
    SpecialFile,
}

impl FileClassification {
    /// Whether this classification is compatible with a clean installation.
    #[must_use]
    pub const fn is_clean(self) -> bool {
        matches!(self, Self::Matching)
    }

    /// Whether Onera can repair this itself, given a trusted backup.
    ///
    /// `Modified` and `Missing` baseline files may be restorable from an
    /// unmanaged backup; everything else is either fine, Onera's own, or the
    /// store's problem to repair.
    #[must_use]
    pub const fn may_be_restorable(self) -> bool {
        matches!(self, Self::Modified | Self::Missing)
    }
}

/// One difference between the baseline and the disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineFinding {
    /// Root the path lives under.
    pub root_key: String,
    /// Path relative to that root.
    pub path: RelPath,
    /// What kind of difference this is.
    pub classification: FileClassification,
    /// Hash recorded in the baseline, when the path is part of it.
    pub expected: Option<FileHash>,
    /// Hash observed on disk, when the file could be hashed.
    pub observed: Option<FileHash>,
    /// Displayable detail: the I/O error, the link target kind, the owning mod.
    pub detail: Option<String>,
}

/// How thoroughly a scan looked at file contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanEvidence {
    /// Every included file was hashed. The only evidence that can report clean.
    ContentHashed,
    /// Size and mtime only. Fast, and never sufficient for a clean verdict.
    MetadataOnly,
}

/// How far a scan got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanState {
    /// Still walking or hashing.
    Running,
    /// Finished and produced a complete result.
    Completed,
    /// Stopped at the user's request; the result is partial.
    Cancelled,
    /// Stopped by an error; the result is partial.
    Failed,
}

impl ScanState {
    /// Whether the scan covered its whole declared scope.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Completed)
    }
}

/// Counts of each classification in a scan result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingCounts {
    /// Files matching the baseline.
    pub matching: u64,
    /// Files present with different contents.
    pub modified: u64,
    /// Baseline files absent from disk.
    pub missing: u64,
    /// Extra files Onera deployed.
    pub extra_managed: u64,
    /// Extra files nobody claims.
    pub extra_unknown: u64,
    /// Files that could not be read.
    pub unreadable: u64,
    /// Symlinks and other non-regular files.
    pub special: u64,
}

impl FindingCounts {
    /// Tally a set of findings.
    #[must_use]
    pub fn of(findings: &[BaselineFinding]) -> Self {
        let mut counts = Self::default();
        for finding in findings {
            let slot = match finding.classification {
                FileClassification::Matching => &mut counts.matching,
                FileClassification::Modified => &mut counts.modified,
                FileClassification::Missing => &mut counts.missing,
                FileClassification::ExtraManaged => &mut counts.extra_managed,
                FileClassification::ExtraUnknown => &mut counts.extra_unknown,
                FileClassification::Unreadable => &mut counts.unreadable,
                FileClassification::SpecialFile => &mut counts.special,
            };
            *slot += 1;
        }
        counts
    }

    /// Whether anything other than matching files was found.
    #[must_use]
    pub const fn has_differences(&self) -> bool {
        self.modified > 0
            || self.missing > 0
            || self.extra_managed > 0
            || self.extra_unknown > 0
            || self.unreadable > 0
            || self.special > 0
    }
}

/// One run of a capture or verification scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineScanRun {
    /// Onera's identifier.
    pub id: BaselineScanRunId,
    /// Installation being scanned.
    pub local_game_id: LocalGameId,
    /// Baseline being verified, or produced. `None` while a capture is deciding.
    pub baseline_id: Option<BaselineId>,
    /// What the scan is for.
    pub purpose: ScanPurpose,
    /// How far it got.
    pub state: ScanState,
    /// How thoroughly it looked.
    pub evidence: ScanEvidence,
    /// When it started.
    pub started_at: DateTime<Utc>,
    /// When it reached a terminal state.
    pub finished_at: Option<DateTime<Utc>>,
    /// Files visited so far.
    pub files_scanned: u64,
    /// Bytes hashed so far.
    pub bytes_hashed: u64,
    /// Tally of the findings so far.
    pub counts: FindingCounts,
    /// Displayable failure reason, when the run failed.
    pub error: Option<String>,
}

/// Why a scan is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanPurpose {
    /// Recording a new baseline.
    Capture,
    /// Comparing the disk against an existing baseline.
    Verify,
    /// Confirming the result of a return-to-clean operation.
    CleanRestore,
}

/// The result of comparing an installation against its baseline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineVerification {
    /// Baseline compared against.
    pub baseline_id: BaselineId,
    /// The scan that produced this result.
    pub scan_run_id: BaselineScanRunId,
    /// How far it got.
    pub state: ScanState,
    /// How thoroughly it looked.
    pub evidence: ScanEvidence,
    /// Whether the scanned scope still matches the captured one.
    pub scope_fingerprint: ScanScopeFingerprint,
    /// Every difference found.
    pub findings: Vec<BaselineFinding>,
    /// Tally of those differences.
    pub counts: FindingCounts,
    /// When the comparison finished.
    pub verified_at: DateTime<Utc>,
}

impl BaselineVerification {
    /// Whether the installation is byte-for-byte clean.
    ///
    /// Requires all four of: a completed scan, content hashing, a scope matching
    /// the baseline's, and no non-matching findings. A quick metadata scan can
    /// prove something *changed*, but never that nothing did.
    #[must_use]
    pub fn is_clean(&self, baseline: &GameBaseline) -> bool {
        self.state.is_complete()
            && self.evidence == ScanEvidence::ContentHashed
            && self.scope_fingerprint == baseline.scope_fingerprint
            && !self.counts.has_differences()
    }

    /// Findings the user or the store must resolve because Onera will not.
    ///
    /// Unknown extras are never deleted and damaged store files are never
    /// synthesized, so both are handed back rather than acted on.
    #[must_use]
    pub fn requires_user_decision(&self) -> Vec<&BaselineFinding> {
        self.findings
            .iter()
            .filter(|f| {
                matches!(
                    f.classification,
                    FileClassification::ExtraUnknown
                        | FileClassification::Unreadable
                        | FileClassification::SpecialFile
                )
            })
            .collect()
    }
}

/// A store-owned extra Onera knows the identity of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreDlc {
    /// Store's opaque identifier.
    pub id: StoreDlcId,
    /// Display name, when the store exposes one.
    pub name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).expect("valid timestamp")
    }

    fn identity(build: Option<&str>, depots: &[(&str, &str)]) -> StoreBuildIdentity {
        StoreBuildIdentity {
            store: GameStoreKind::Steam,
            app_id: Some("1091500".into()),
            build_id: build.map(str::to_owned),
            branch: None,
            depots: depots
                .iter()
                .map(|(d, m)| DepotIdentity {
                    depot_id: (*d).to_owned(),
                    manifest_id: (*m).to_owned(),
                })
                .collect(),
            manifest_path: None,
            observed_at: now(),
        }
    }

    fn rel(p: &str) -> RelPath {
        RelPath::normalize(p).expect("valid relative path")
    }

    fn baseline(fingerprint: ScanScopeFingerprint) -> GameBaseline {
        GameBaseline {
            id: BaselineId::new(),
            local_game_id: LocalGameId::new(),
            source: BaselineSource::StoreVerifiedCapture,
            build_identity: Some(identity(Some("1"), &[])),
            adapter_id: "cyberpunk2077".into(),
            reported_version: Some("2.21".into()),
            status: BaselineStatus::Current,
            captured_at: now(),
            scope_fingerprint: fingerprint,
            file_count: 1,
            total_bytes: 1,
        }
    }

    fn verification(
        state: ScanState,
        evidence: ScanEvidence,
        fingerprint: ScanScopeFingerprint,
        findings: Vec<BaselineFinding>,
    ) -> BaselineVerification {
        BaselineVerification {
            baseline_id: BaselineId::new(),
            scan_run_id: BaselineScanRunId::new(),
            state,
            evidence,
            scope_fingerprint: fingerprint,
            counts: FindingCounts::of(&findings),
            findings,
            verified_at: now(),
        }
    }

    fn finding(path: &str, classification: FileClassification) -> BaselineFinding {
        BaselineFinding {
            root_key: "game".into(),
            path: rel(path),
            classification,
            expected: None,
            observed: None,
            detail: None,
        }
    }

    #[test]
    fn build_identity_is_compared_not_ordered() {
        let a = identity(Some("18234000"), &[("1091501", "77")]);
        let b = identity(Some("18234000"), &[("1091501", "77")]);
        assert_eq!(a.compare(&b), BuildIdentityMatch::Same);

        // A rollback is "changed", exactly like a roll-forward.
        let older = identity(Some("18000000"), &[("1091501", "77")]);
        assert_eq!(a.compare(&older), BuildIdentityMatch::Changed);
        assert_eq!(older.compare(&a), BuildIdentityMatch::Changed);

        // A changed depot manifest counts even with the same build id.
        let redepoted = identity(Some("18234000"), &[("1091501", "78")]);
        assert_eq!(a.compare(&redepoted), BuildIdentityMatch::Changed);
    }

    #[test]
    fn depot_order_does_not_make_an_identity_look_changed() {
        let a = identity(Some("1"), &[("a", "1"), ("b", "2")]);
        let b = identity(Some("1"), &[("b", "2"), ("a", "1")]);
        assert_eq!(a.compare(&b), BuildIdentityMatch::Same);
    }

    #[test]
    fn an_incomparable_identity_is_unknown_rather_than_same() {
        let known = identity(Some("1"), &[]);
        let blank = StoreBuildIdentity::unknown(GameStoreKind::Steam, now());
        assert!(!blank.is_comparable());
        assert_eq!(known.compare(&blank), BuildIdentityMatch::Unknown);
        assert_eq!(blank.compare(&blank), BuildIdentityMatch::Unknown);

        // Different stores are never comparable either.
        let manual = StoreBuildIdentity {
            store: GameStoreKind::Manual,
            ..identity(Some("1"), &[])
        };
        assert_eq!(known.compare(&manual), BuildIdentityMatch::Unknown);
    }

    #[test]
    fn freshness_reports_stale_only_on_a_known_change() {
        let captured = identity(Some("1"), &[]);
        let same = identity(Some("1"), &[]);
        let changed = identity(Some("2"), &[]);

        assert_eq!(
            BaselineFreshness::evaluate(Some(&captured), Some(&same)),
            BaselineFreshness::Fresh
        );
        assert!(matches!(
            BaselineFreshness::evaluate(Some(&captured), Some(&changed)),
            BaselineFreshness::Stale { .. }
        ));
        assert!(matches!(
            BaselineFreshness::evaluate(Some(&captured), None),
            BaselineFreshness::Unknown { .. }
        ));
        assert!(matches!(
            BaselineFreshness::evaluate(None, Some(&same)),
            BaselineFreshness::Unknown { .. }
        ));
        assert!(BaselineFreshness::None.needs_recapture());
        assert!(!BaselineFreshness::Fresh.needs_recapture());
        // "Cannot tell" must not silently behave like "nothing changed", but it
        // is also not a reason to force a recapture.
        assert!(!BaselineFreshness::Unknown {
            reason: String::new()
        }
        .needs_recapture());
    }

    #[test]
    fn a_metadata_only_scan_can_never_report_clean() {
        let fingerprint = ScanScopeFingerprint::of(&[], &[]);
        let base = baseline(fingerprint.clone());
        let quick = verification(
            ScanState::Completed,
            ScanEvidence::MetadataOnly,
            fingerprint.clone(),
            vec![],
        );
        assert!(!quick.is_clean(&base));

        let hashed = verification(
            ScanState::Completed,
            ScanEvidence::ContentHashed,
            fingerprint.clone(),
            vec![],
        );
        assert!(hashed.is_clean(&base));

        // Neither can a cancelled scan, nor one covering a different scope.
        let cancelled = verification(
            ScanState::Cancelled,
            ScanEvidence::ContentHashed,
            fingerprint,
            vec![],
        );
        assert!(!cancelled.is_clean(&base));

        let narrower = verification(
            ScanState::Completed,
            ScanEvidence::ContentHashed,
            ScanScopeFingerprint::of(
                &[],
                &[BaselineExclusion {
                    root_key: None,
                    pattern: ExclusionPattern::DirectoryName {
                        name: "cache".into(),
                    },
                    reason: ExclusionReason::Cache,
                    note: None,
                }],
            ),
            vec![],
        );
        assert!(!narrower.is_clean(&base));
    }

    #[test]
    fn unknown_extras_and_special_files_always_need_a_decision() {
        let fingerprint = ScanScopeFingerprint::of(&[], &[]);
        let v = verification(
            ScanState::Completed,
            ScanEvidence::ContentHashed,
            fingerprint.clone(),
            vec![
                finding("a", FileClassification::Matching),
                finding("b", FileClassification::ExtraUnknown),
                finding("c", FileClassification::ExtraManaged),
                finding("d", FileClassification::SpecialFile),
                finding("e", FileClassification::Unreadable),
                finding("f", FileClassification::Modified),
            ],
        );
        let paths: Vec<&str> = v
            .requires_user_decision()
            .iter()
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(paths, vec!["b", "d", "e"]);
        assert!(!v.is_clean(&baseline(fingerprint)));
        assert_eq!(v.counts.extra_unknown, 1);
        assert_eq!(v.counts.matching, 1);
        assert!(v.counts.has_differences());
        assert!(FileClassification::Modified.may_be_restorable());
        assert!(!FileClassification::ExtraUnknown.may_be_restorable());
    }

    #[test]
    fn exclusion_patterns_match_what_they_claim() {
        let prefix = ExclusionPattern::Prefix {
            path: rel("r6/cache"),
        };
        assert!(prefix.matches(&rel("r6/cache")));
        assert!(prefix.matches(&rel("r6/cache/final.redscripts")));
        // A sibling with a shared textual prefix must not be swept in.
        assert!(!prefix.matches(&rel("r6/cachexyz/file")));
        assert!(!prefix.matches(&rel("r6/scripts/a.reds")));

        let exact = ExclusionPattern::Exact {
            path: rel("version.txt"),
        };
        assert!(exact.matches(&rel("version.txt")));
        assert!(!exact.matches(&rel("a/version.txt")));

        let ext = ExclusionPattern::Extension {
            extension: "log".into(),
        };
        assert!(ext.matches(&rel("bin/x64/Game.LOG")));
        assert!(!ext.matches(&rel("bin/x64/Game.exe")));

        let dir = ExclusionPattern::DirectoryName {
            name: "ShaderCache".into(),
        };
        assert!(dir.matches(&rel("bin/shadercache/a.bin")));
        // Matching only directory components: a file named shadercache stays in.
        assert!(!dir.matches(&rel("bin/shadercache")));
    }

    #[test]
    fn exclusions_can_be_scoped_to_one_root() {
        let scoped = BaselineExclusion {
            root_key: Some("game".into()),
            pattern: ExclusionPattern::Extension {
                extension: "log".into(),
            },
            reason: ExclusionReason::Logs,
            note: None,
        };
        assert!(scoped.matches("game", &rel("a.log")));
        assert!(!scoped.matches("user_data", &rel("a.log")));

        let global = BaselineExclusion {
            root_key: None,
            ..scoped.clone()
        };
        assert!(global.matches("user_data", &rel("a.log")));

        let set = [scoped, global];
        assert!(excluded_by(&set, "user_data", &rel("a.log")).is_some());
        assert!(excluded_by(&set, "user_data", &rel("a.exe")).is_none());
    }

    #[test]
    fn the_scope_fingerprint_ignores_declaration_order() {
        let roots = vec![
            BaselineRoot {
                key: "game".into(),
                kind: DeployRootKind::GameInstall,
                path: "/games/cp2077".into(),
            },
            BaselineRoot {
                key: "aux".into(),
                kind: DeployRootKind::Auxiliary,
                path: "/games/cp2077/tools".into(),
            },
        ];
        let exclusions = vec![
            BaselineExclusion {
                root_key: None,
                pattern: ExclusionPattern::Extension {
                    extension: "log".into(),
                },
                reason: ExclusionReason::Logs,
                note: None,
            },
            BaselineExclusion {
                root_key: Some("game".into()),
                pattern: ExclusionPattern::Prefix {
                    path: rel("r6/cache"),
                },
                reason: ExclusionReason::Cache,
                note: None,
            },
        ];
        let forward = ScanScopeFingerprint::of(&roots, &exclusions);

        let mut reversed_roots = roots.clone();
        reversed_roots.reverse();
        let mut reversed_exclusions = exclusions.clone();
        reversed_exclusions.reverse();
        assert_eq!(
            ScanScopeFingerprint::of(&reversed_roots, &reversed_exclusions),
            forward
        );

        // Dropping an exclusion narrows the scope and must be visible.
        assert_ne!(ScanScopeFingerprint::of(&roots, &exclusions[..1]), forward);
        assert_eq!(forward.as_str().len(), 64);
    }

    #[test]
    fn a_baseline_round_trips_through_json() {
        let original = baseline(ScanScopeFingerprint::of(&[], &[]));
        let json = serde_json::to_string(&original).unwrap();
        assert_eq!(
            serde_json::from_str::<GameBaseline>(&json).unwrap(),
            original
        );
        // Status and source are stable snake_case strings the frontend reads.
        assert!(json.contains("\"store_verified_capture\""), "{json}");
        assert!(json.contains("\"current\""), "{json}");
    }
}
