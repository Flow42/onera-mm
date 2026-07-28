//! Content hashing.
//!
//! BLAKE3 is the only algorithm Onera writes. The algorithm is still recorded
//! alongside every digest so that stored hashes remain interpretable if a
//! second algorithm is ever added, and so that provider-supplied MD5 digests
//! can be represented without being confused for our own.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

/// Hash algorithms Onera can represent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HashAlgorithm {
    /// BLAKE3, 256-bit output. The only algorithm Onera computes.
    Blake3,
    /// MD5, accepted only as provider-supplied metadata. Never trusted for
    /// integrity decisions.
    Md5,
}

impl HashAlgorithm {
    /// Canonical lowercase name, used in storage paths and the database.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blake3 => "blake3",
            Self::Md5 => "md5",
        }
    }
}

impl fmt::Display for HashAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A content hash: algorithm plus lowercase hex digest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FileHash {
    /// Algorithm that produced [`FileHash::hex`].
    pub algorithm: HashAlgorithm,
    /// Lowercase hex digest.
    pub hex: String,
}

/// Errors from parsing a stored hash.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HashParseError {
    /// The string was not `algorithm:hex`.
    #[error("expected `algorithm:hex`, got {0:?}")]
    Malformed(String),
    /// Unknown algorithm name.
    #[error("unknown hash algorithm {0:?}")]
    UnknownAlgorithm(String),
    /// The digest was not lowercase hex of the expected length.
    #[error("invalid digest for {0}: {1:?}")]
    InvalidDigest(HashAlgorithm, String),
}

impl FileHash {
    /// Build a BLAKE3 hash from a raw 32-byte digest.
    #[must_use]
    pub fn blake3(bytes: [u8; 32]) -> Self {
        Self {
            algorithm: HashAlgorithm::Blake3,
            hex: hex_encode(&bytes),
        }
    }

    /// Hash an in-memory buffer with BLAKE3.
    #[must_use]
    pub fn blake3_of(data: &[u8]) -> Self {
        Self::blake3(*blake3::hash(data).as_bytes())
    }

    /// Accept a provider-supplied MD5 digest as metadata.
    ///
    /// # Errors
    /// Fails if the digest is not 32 hex characters.
    pub fn md5_from_hex(hex: &str) -> Result<Self, HashParseError> {
        let lowered = hex.to_ascii_lowercase();
        if lowered.len() != 32 || !lowered.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(HashParseError::InvalidDigest(
                HashAlgorithm::Md5,
                hex.to_owned(),
            ));
        }
        Ok(Self {
            algorithm: HashAlgorithm::Md5,
            hex: lowered,
        })
    }

    /// The first `n` hex characters, used as the content-addressed shard prefix.
    #[must_use]
    pub fn prefix(&self, n: usize) -> &str {
        &self.hex[..n.min(self.hex.len())]
    }

    /// Serialize as `algorithm:hex` for storage in a single column.
    #[must_use]
    pub fn to_storage_string(&self) -> String {
        format!("{}:{}", self.algorithm, self.hex)
    }

    /// Parse the `algorithm:hex` storage form.
    ///
    /// # Errors
    /// Fails on unknown algorithms or malformed digests.
    pub fn from_storage_string(s: &str) -> Result<Self, HashParseError> {
        let (algo, hex) = s
            .split_once(':')
            .ok_or_else(|| HashParseError::Malformed(s.to_owned()))?;
        let algorithm = match algo {
            "blake3" => HashAlgorithm::Blake3,
            "md5" => HashAlgorithm::Md5,
            other => return Err(HashParseError::UnknownAlgorithm(other.to_owned())),
        };
        let expected = match algorithm {
            HashAlgorithm::Blake3 => 64,
            HashAlgorithm::Md5 => 32,
        };
        if hex.len() != expected
            || !hex
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(HashParseError::InvalidDigest(algorithm, hex.to_owned()));
        }
        Ok(Self {
            algorithm,
            hex: hex.to_owned(),
        })
    }
}

impl fmt::Display for FileHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algorithm, self.hex)
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    use fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut acc, b| {
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

/// Hash a file with BLAKE3, streaming it in fixed-size chunks.
///
/// Never loads the whole file into memory: this is used on multi-gigabyte
/// archives and on every deployed file during verification.
///
/// # Errors
/// Propagates any I/O error from opening or reading the file.
pub async fn hash_file_blake3(path: &Path) -> std::io::Result<FileHash> {
    use tokio::io::AsyncReadExt as _;

    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0_u8; 256 * 1024];
    loop {
        let read = file.read(&mut buf).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(FileHash::blake3(*hasher.finalize().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blake3_round_trips_through_storage() {
        let h = FileHash::blake3_of(b"onera");
        let stored = h.to_storage_string();
        assert_eq!(FileHash::from_storage_string(&stored).unwrap(), h);
        assert_eq!(h.hex.len(), 64);
        assert_eq!(h.prefix(2).len(), 2);
    }

    #[test]
    fn rejects_bad_storage_strings() {
        assert!(FileHash::from_storage_string("nope").is_err());
        assert!(FileHash::from_storage_string("sha256:abcd").is_err());
        assert!(FileHash::from_storage_string("blake3:zz").is_err());
        // Uppercase hex is rejected so a digest has exactly one storage form.
        assert!(FileHash::from_storage_string(&format!("blake3:{}", "A".repeat(64))).is_err());
    }

    #[test]
    fn md5_is_accepted_only_as_metadata() {
        let h = FileHash::md5_from_hex("D41D8CD98F00B204E9800998ECF8427E").unwrap();
        assert_eq!(h.algorithm, HashAlgorithm::Md5);
        assert_eq!(h.hex, "d41d8cd98f00b204e9800998ecf8427e");
        assert!(FileHash::md5_from_hex("short").is_err());
    }

    #[tokio::test]
    async fn hashes_files_from_disk() {
        let dir = std::env::temp_dir().join(format!("onera-hash-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f");
        std::fs::write(&path, b"onera").unwrap();
        assert_eq!(
            hash_file_blake3(&path).await.unwrap(),
            FileHash::blake3_of(b"onera")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
