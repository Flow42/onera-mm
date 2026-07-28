//! Entry validation shared by every archive backend.
//!
//! Backends differ in how they enumerate entries; they must not differ in what
//! they accept. Every backend funnels each entry through [`Validator::accept`],
//! which applies the path rules, the link and special-file policy, and the
//! running resource budget.
//!
//! Two severities:
//!
//! * **Skip** — the entry is dropped and recorded in
//!   [`onera_core::domain::archive::ArchiveInspection::rejected`]. Used for
//!   links and special files, which appear in legitimate archives (a tarball of
//!   a Linux source tree) and simply have no meaning as deployed mod content.
//! * **Fatal** — the whole archive is rejected. Used for path traversal and for
//!   every resource limit. A traversal entry is not a quirk; it is an attack,
//!   and an archive containing one is not trustworthy for its other entries
//!   either.

use crate::limits::ExtractionLimits;
use onera_core::domain::archive::{ArchiveEntry, EntryKind, RejectedEntry};
use onera_core::paths::{RelPath, RelPathError};
use onera_core::{CoreError, Result};

/// What to do with one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Keep the entry.
    Accept(Box<ArchiveEntry>),
    /// Drop this entry but keep reading the archive.
    Skip(RejectedEntry),
}

/// Applies limits and path rules across a whole archive.
#[derive(Debug)]
pub struct Validator {
    limits: ExtractionLimits,
    entries_seen: usize,
    bytes_declared: u64,
}

impl Validator {
    /// Start validating an archive under the given limits.
    #[must_use]
    pub fn new(limits: ExtractionLimits) -> Self {
        Self {
            limits,
            entries_seen: 0,
            bytes_declared: 0,
        }
    }

    /// The limits in force.
    #[must_use]
    pub fn limits(&self) -> &ExtractionLimits {
        &self.limits
    }

    /// Bytes declared by accepted entries so far.
    #[must_use]
    pub fn bytes_declared(&self) -> u64 {
        self.bytes_declared
    }

    /// Validate one entry.
    ///
    /// # Errors
    /// Returns [`CoreError::ArchiveRejected`] when the archive must be refused
    /// outright: a traversal attempt or an exceeded resource limit.
    pub fn accept(
        &mut self,
        raw_path: &str,
        kind: EntryKind,
        declared_size: u64,
        compressed_size: Option<u64>,
        link_target: Option<String>,
    ) -> Result<Outcome> {
        self.entries_seen += 1;
        if self.entries_seen > self.limits.max_entries {
            return Err(reject(format!(
                "archive has more than {} entries",
                self.limits.max_entries
            )));
        }
        if raw_path.len() > self.limits.max_path_len {
            return Err(reject(format!(
                "entry path exceeds {} bytes",
                self.limits.max_path_len
            )));
        }

        // Links and special files are never written to disk. Extracting a
        // symlink would let an archive point at a path outside the staging
        // directory that later writes would follow.
        if !kind.is_extractable() {
            return Ok(Outcome::Skip(RejectedEntry {
                raw_path: raw_path.to_owned(),
                reason: match kind {
                    EntryKind::Symlink => format!(
                        "symbolic links are never extracted (target: {})",
                        link_target.as_deref().unwrap_or("unknown")
                    ),
                    EntryKind::Hardlink => "hard links are never extracted".to_owned(),
                    _ => "special files are never extracted".to_owned(),
                },
            }));
        }

        let path = match RelPath::normalize(raw_path) {
            Ok(p) => p,
            Err(
                e @ (RelPathError::Traversal(_)
                | RelPathError::Absolute(_)
                | RelPathError::DrivePrefix(_)),
            ) => {
                return Err(reject(format!(
                    "entry {raw_path:?} attempts to escape the staging directory: {e}"
                )));
            }
            Err(e) => {
                // Empty, over-long or control-character paths are malformed
                // rather than hostile; drop the entry and carry on.
                return Ok(Outcome::Skip(RejectedEntry {
                    raw_path: raw_path.to_owned(),
                    reason: e.to_string(),
                }));
            }
        };

        if path.depth() > self.limits.max_depth {
            return Err(reject(format!(
                "entry {path} nests deeper than {} levels",
                self.limits.max_depth
            )));
        }

        if kind == EntryKind::File {
            if declared_size > self.limits.max_file_bytes {
                return Err(reject(format!(
                    "entry {path} declares {declared_size} bytes, over the {} byte per-file limit",
                    self.limits.max_file_bytes
                )));
            }
            self.bytes_declared = self.bytes_declared.saturating_add(declared_size);
            if self.bytes_declared > self.limits.max_total_bytes {
                return Err(reject(format!(
                    "archive declares more than {} bytes in total",
                    self.limits.max_total_bytes
                )));
            }
            if self
                .limits
                .is_suspicious_ratio(declared_size, compressed_size)
            {
                return Err(reject(format!(
                    "entry {path} has a suspicious compression ratio ({declared_size} bytes from {} compressed)",
                    compressed_size.unwrap_or(0)
                )));
            }
        }

        Ok(Outcome::Accept(Box::new(ArchiveEntry {
            path,
            kind,
            declared_size,
            compressed_size,
            link_target,
        })))
    }
}

/// A running budget enforced against bytes actually written.
///
/// Declared sizes are advisory; this counts what really lands on disk so a
/// lying header cannot get past the limits.
#[derive(Debug)]
pub struct WriteBudget {
    remaining_total: u64,
    max_file: u64,
}

impl WriteBudget {
    /// Start a budget from the configured limits.
    #[must_use]
    pub fn new(limits: &ExtractionLimits) -> Self {
        Self {
            remaining_total: limits.max_total_bytes,
            max_file: limits.max_file_bytes,
        }
    }

    /// Per-file ceiling.
    #[must_use]
    pub fn max_file(&self) -> u64 {
        self.max_file.min(self.remaining_total)
    }

    /// Record bytes written.
    ///
    /// # Errors
    /// Returns [`CoreError::ArchiveRejected`] once the total budget is spent.
    pub fn consume(&mut self, written: u64) -> Result<()> {
        self.remaining_total = self.remaining_total.checked_sub(written).ok_or_else(|| {
            reject("archive expanded past its total size limit while extracting".to_owned())
        })?;
        Ok(())
    }
}

fn reject(reason: String) -> CoreError {
    CoreError::ArchiveRejected { reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validator() -> Validator {
        Validator::new(ExtractionLimits::strict())
    }

    fn accept_file(v: &mut Validator, path: &str, size: u64) -> Result<Outcome> {
        v.accept(path, EntryKind::File, size, None, None)
    }

    #[test]
    fn accepts_ordinary_entries() {
        let mut v = validator();
        let out = accept_file(&mut v, "archive/pc/mod/x.archive", 100).unwrap();
        let Outcome::Accept(entry) = out else {
            panic!("expected accept")
        };
        assert_eq!(entry.path.as_str(), "archive/pc/mod/x.archive");
        assert_eq!(v.bytes_declared(), 100);
    }

    #[test]
    fn normalizes_windows_separators() {
        let mut v = validator();
        let Outcome::Accept(e) = accept_file(&mut v, r"bin\x64\plugin.dll", 1).unwrap() else {
            panic!("expected accept")
        };
        assert_eq!(e.path.as_str(), "bin/x64/plugin.dll");
    }

    #[test]
    fn traversal_rejects_the_whole_archive() {
        for hostile in [
            "../../../../etc/cron.d/backdoor",
            r"..\..\Windows\System32\evil.dll",
            "/etc/passwd",
            r"C:\Windows\evil.dll",
            "good/../../../escape",
        ] {
            let mut v = validator();
            let err = accept_file(&mut v, hostile, 1).unwrap_err();
            assert!(
                matches!(err, CoreError::ArchiveRejected { .. }),
                "{hostile:?} must be fatal, got {err:?}"
            );
        }
    }

    #[test]
    fn links_and_specials_are_skipped_not_fatal() {
        let mut v = validator();
        for kind in [EntryKind::Symlink, EntryKind::Hardlink, EntryKind::Special] {
            let out = v
                .accept("link", kind, 0, None, Some("/etc/passwd".into()))
                .expect("links must not abort the archive");
            let Outcome::Skip(rejected) = out else {
                panic!("{kind:?} must never be accepted")
            };
            assert_eq!(rejected.raw_path, "link");
            assert!(!rejected.reason.is_empty());
        }
    }

    #[test]
    fn a_symlink_pointing_outside_is_skipped_with_its_target_recorded() {
        let mut v = validator();
        let Outcome::Skip(r) = v
            .accept(
                "evil",
                EntryKind::Symlink,
                0,
                None,
                Some("../../root/.ssh".into()),
            )
            .unwrap()
        else {
            panic!("expected skip")
        };
        assert!(r.reason.contains("../../root/.ssh"));
    }

    #[test]
    fn malformed_paths_are_skipped_not_fatal() {
        let mut v = validator();
        // A control character is malformed, not an escape attempt.
        let out = v
            .accept("bad\u{7}name", EntryKind::File, 1, None, None)
            .unwrap();
        assert!(matches!(out, Outcome::Skip(_)));
    }

    #[test]
    fn entry_count_limit_is_enforced() {
        let limits = ExtractionLimits {
            max_entries: 2,
            ..ExtractionLimits::strict()
        };
        let mut v = Validator::new(limits);
        accept_file(&mut v, "a", 1).unwrap();
        accept_file(&mut v, "b", 1).unwrap();
        assert!(accept_file(&mut v, "c", 1).is_err());
    }

    #[test]
    fn per_file_and_total_size_limits_are_enforced() {
        let limits = ExtractionLimits {
            max_file_bytes: 100,
            max_total_bytes: 150,
            ..ExtractionLimits::strict()
        };
        let mut v = Validator::new(limits);
        assert!(accept_file(&mut v, "big", 101).is_err(), "per-file limit");

        let mut v = Validator::new(limits);
        accept_file(&mut v, "a", 100).unwrap();
        assert!(accept_file(&mut v, "b", 100).is_err(), "total limit");
    }

    #[test]
    fn depth_limit_is_enforced() {
        let limits = ExtractionLimits {
            max_depth: 3,
            ..ExtractionLimits::strict()
        };
        let mut v = Validator::new(limits);
        accept_file(&mut v, "a/b/c", 1).unwrap();
        assert!(accept_file(&mut v, "a/b/c/d", 1).is_err());
    }

    #[test]
    fn zip_bomb_ratios_are_rejected() {
        let mut v = validator();
        let err = v
            .accept(
                "bomb.bin",
                EntryKind::File,
                50 * 1024 * 1024,
                Some(1_024),
                None,
            )
            .unwrap_err();
        assert!(format!("{err}").contains("compression ratio"));
    }

    #[test]
    fn directories_do_not_consume_the_byte_budget() {
        let mut v = validator();
        v.accept("dir", EntryKind::Directory, 4096, None, None)
            .unwrap();
        assert_eq!(v.bytes_declared(), 0);
    }

    #[test]
    fn write_budget_counts_real_bytes_not_declared_ones() {
        let limits = ExtractionLimits {
            max_total_bytes: 10,
            ..ExtractionLimits::strict()
        };
        let mut budget = WriteBudget::new(&limits);
        assert_eq!(budget.max_file(), 10);
        budget.consume(6).unwrap();
        assert_eq!(budget.max_file(), 4);
        // An archive that declared 0 bytes but wrote 6 more is still stopped.
        assert!(budget.consume(6).is_err());
    }
}
