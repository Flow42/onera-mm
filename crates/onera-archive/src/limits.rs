//! Resource limits applied to every archive.
//!
//! Limits are enforced twice: once during inspection against the sizes the
//! archive *declares*, and again during extraction against the bytes actually
//! written. A malicious archive can understate its sizes, so the declared
//! numbers are only ever used to reject early — never to allocate, and never to
//! decide that extraction is safe.

use serde::{Deserialize, Serialize};

/// Bounds on what an archive may contain.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ExtractionLimits {
    /// Maximum number of entries, including directories.
    pub max_entries: usize,
    /// Maximum total uncompressed bytes across all files.
    pub max_total_bytes: u64,
    /// Maximum uncompressed bytes in any single file.
    pub max_file_bytes: u64,
    /// Maximum path depth inside the archive.
    pub max_depth: usize,
    /// Maximum length of any path inside the archive.
    pub max_path_len: usize,
    /// Maximum ratio of uncompressed to compressed size for a single entry.
    pub max_compression_ratio: f64,
    /// Entries smaller than this are exempt from the ratio check.
    ///
    /// Tiny files routinely compress at absurd ratios (a 4-byte file in a
    /// 1-byte deflate block) without being an attack, so the ratio heuristic
    /// only applies once an entry is large enough to matter.
    pub ratio_check_min_bytes: u64,
}

impl Default for ExtractionLimits {
    /// Defaults sized for real mods: large enough for a multi-gigabyte texture
    /// pack, small enough that a bomb is stopped long before it fills a disk.
    fn default() -> Self {
        Self {
            max_entries: 200_000,
            max_total_bytes: 64 * 1024 * 1024 * 1024,
            max_file_bytes: 16 * 1024 * 1024 * 1024,
            max_depth: crate::MAX_ARCHIVE_DEPTH,
            max_path_len: 4096,
            max_compression_ratio: 200.0,
            ratio_check_min_bytes: 1024 * 1024,
        }
    }
}

impl ExtractionLimits {
    /// Very small limits, for tests and for inspecting untrusted samples.
    #[must_use]
    pub fn strict() -> Self {
        Self {
            max_entries: 1_000,
            max_total_bytes: 256 * 1024 * 1024,
            max_file_bytes: 64 * 1024 * 1024,
            max_depth: 16,
            max_path_len: 1024,
            max_compression_ratio: 100.0,
            ratio_check_min_bytes: 64 * 1024,
        }
    }

    /// Whether an entry's declared sizes look like a decompression bomb.
    #[must_use]
    pub fn is_suspicious_ratio(&self, uncompressed: u64, compressed: Option<u64>) -> bool {
        if uncompressed < self.ratio_check_min_bytes {
            return false;
        }
        let Some(compressed) = compressed else {
            return false;
        };
        if compressed == 0 {
            return true;
        }
        (uncompressed as f64 / compressed as f64) > self.max_compression_ratio
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_files_are_exempt_from_the_ratio_check() {
        let l = ExtractionLimits::strict();
        // 100 bytes from 1 byte is a 100x ratio but far too small to matter.
        assert!(!l.is_suspicious_ratio(100, Some(1)));
    }

    #[test]
    fn large_high_ratio_entries_are_suspicious() {
        let l = ExtractionLimits::strict();
        assert!(l.is_suspicious_ratio(1_000_000_000, Some(1_000)));
        assert!(
            l.is_suspicious_ratio(1_000_000, Some(0)),
            "zero compressed size is always suspicious"
        );
    }

    #[test]
    fn ordinary_compression_is_not_suspicious() {
        let l = ExtractionLimits::default();
        // A 4:1 ratio is what a normal text-heavy mod archive looks like.
        assert!(!l.is_suspicious_ratio(400 * 1024 * 1024, Some(100 * 1024 * 1024)));
    }

    #[test]
    fn unknown_compressed_size_does_not_trip_the_heuristic() {
        // Tar has no per-entry compressed size; the running byte budget during
        // extraction is what protects us there.
        assert!(!ExtractionLimits::default().is_suspicious_ratio(u64::MAX, None));
    }
}
