//! Streaming downloads.
//!
//! The native application performs every download; the browser is never asked
//! to. That is what lets Onera hash, deduplicate and resume, none of which the
//! browser's download manager can do for it.
//!
//! Guarantees:
//!
//! * bytes are streamed straight to a temporary file — a multi-gigabyte archive
//!   is never held in memory;
//! * the final BLAKE3 hash is computed while streaming, not by re-reading;
//! * the temporary file is promoted into content-addressed storage with a single
//!   atomic rename, so a partial download can never be mistaken for a complete
//!   one;
//! * a file already in storage is not downloaded again;
//! * redirects are followed only to HTTPS, and only a bounded number of times.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod job;
pub mod store;

pub use job::{DownloadJob, JobState};
pub use store::ContentAddressedStore;

use onera_core::hash::FileHash;
use onera_core::ports::{ArchiveStore, DownloadTarget};
use onera_core::progress::{CancelToken, ProgressEvent, ProgressSink, Stage};
use onera_core::redact::redact_url;
use onera_core::{CoreError, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt as _;

/// Tuning for the downloader.
#[derive(Debug, Clone, Copy)]
pub struct DownloadConfig {
    /// How many downloads may run at once.
    pub max_concurrent: usize,
    /// Attempts per download, including the first.
    pub max_attempts: u32,
    /// Redirects to follow.
    pub max_redirects: usize,
    /// Time allowed with no bytes arriving.
    pub stall_timeout: Duration,
    /// Refuse anything larger than this.
    pub max_bytes: u64,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            // Nexus throttles aggressively and mod archives are large; four
            // parallel downloads saturates a normal connection without
            // provoking a rate limit.
            max_concurrent: 4,
            max_attempts: 4,
            max_redirects: 5,
            stall_timeout: Duration::from_secs(60),
            max_bytes: 64 * 1024 * 1024 * 1024,
        }
    }
}

/// Downloads files into content-addressed storage.
pub struct Downloader {
    http: reqwest::Client,
    store: Arc<dyn ArchiveStore>,
    config: DownloadConfig,
    permits: Arc<tokio::sync::Semaphore>,
    temp_dir: PathBuf,
}

impl Downloader {
    /// Build a downloader.
    ///
    /// # Errors
    /// Fails if the HTTP stack cannot be initialized.
    pub fn new(
        store: Arc<dyn ArchiveStore>,
        temp_dir: PathBuf,
        config: DownloadConfig,
    ) -> Result<Self> {
        Self::build(store, temp_dir, config, true)
    }

    /// Build a downloader that will also fetch over plain HTTP.
    ///
    /// For tests against a local mock server only.
    ///
    /// # Errors
    /// Fails if the HTTP stack cannot be initialized.
    pub fn new_for_tests(
        store: Arc<dyn ArchiveStore>,
        temp_dir: PathBuf,
        config: DownloadConfig,
    ) -> Result<Self> {
        Self::build(store, temp_dir, config, false)
    }

    fn build(
        store: Arc<dyn ArchiveStore>,
        temp_dir: PathBuf,
        config: DownloadConfig,
        https_only: bool,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            // A redirect chain is where a signed CDN URL could be turned into a
            // request to somewhere else; bounding it and requiring HTTPS is the
            // whole defence.
            .redirect(reqwest::redirect::Policy::limited(config.max_redirects))
            .https_only(https_only)
            .connect_timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| CoreError::Provider(format!("cannot build HTTP client: {e}")))?;
        Ok(Self {
            http,
            store,
            config,
            permits: Arc::new(tokio::sync::Semaphore::new(config.max_concurrent)),
            temp_dir,
        })
    }

    /// Fetch a target into content-addressed storage.
    ///
    /// Returns the stored path and the hash. If `expected_hash` is supplied and
    /// already present in storage, nothing is downloaded.
    ///
    /// # Errors
    /// Fails on transport errors, a size or content-length mismatch,
    /// cancellation, or an integrity mismatch against `expected_hash`.
    pub async fn fetch(
        &self,
        target: &DownloadTarget,
        expected_hash: Option<&FileHash>,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<DownloadOutcome> {
        // Deduplication: a file whose bytes are already stored is free.
        if let Some(hash) = expected_hash {
            if self.store.contains(hash).await? {
                return Ok(DownloadOutcome {
                    path: self.store.path_for(hash),
                    hash: hash.clone(),
                    bytes: 0,
                    deduplicated: true,
                });
            }
        }

        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| CoreError::Cancelled)?;

        let mut attempt = 0_u32;
        loop {
            cancel.check()?;
            match self.attempt(target, progress, cancel).await {
                Ok(outcome) => {
                    if let Some(expected) = expected_hash {
                        if &outcome.hash != expected {
                            let _ = tokio::fs::remove_file(&outcome.path).await;
                            return Err(CoreError::IntegrityMismatch {
                                path: target.filename.clone(),
                                expected: expected.to_string(),
                                actual: outcome.hash.to_string(),
                            });
                        }
                    }
                    return Ok(outcome);
                }
                Err(error) => {
                    attempt += 1;
                    if attempt >= self.config.max_attempts || !error.is_retryable() {
                        return Err(error);
                    }
                    // Same full-jitter backoff as the API client.
                    let ceiling = Duration::from_millis(500) * (1 << attempt.min(6));
                    let jitter: f64 = rand::random();
                    tracing::debug!(
                        attempt,
                        url = %redact_url(target.url.as_str()),
                        "retrying download"
                    );
                    tokio::time::sleep(ceiling.mul_f64(jitter)).await;
                }
            }
        }
    }

    async fn attempt(
        &self,
        target: &DownloadTarget,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<DownloadOutcome> {
        let mut request = self.http.get(target.url.clone());
        for (name, value) in &target.headers {
            request = request.header(name, value.expose());
        }

        let response = request.send().await.map_err(|e| {
            CoreError::Provider(format!(
                "download from {} failed: {}",
                redact_url(target.url.as_str()),
                redact_url(&e.to_string())
            ))
        })?;

        let status = response.status();
        if !status.is_success() {
            let error =
                CoreError::Provider(format!("download refused with status {}", status.as_u16()));
            // 4xx will not become 3xx by trying again.
            return Err(if status.is_server_error() || status.as_u16() == 429 {
                error
            } else {
                CoreError::InvalidInput(format!("download refused with status {}", status.as_u16()))
            });
        }

        let declared = response.content_length().or(target.expected_size);
        if let Some(size) = declared {
            if size > self.config.max_bytes {
                return Err(CoreError::InvalidInput(format!(
                    "refusing a {size} byte download; the limit is {}",
                    self.config.max_bytes
                )));
            }
        }

        tokio::fs::create_dir_all(&self.temp_dir)
            .await
            .map_err(|e| CoreError::fs(&self.temp_dir, e))?;
        let temp = self.temp_dir.join(format!("download-{}.part", uuid_v4()));
        let mut file = tokio::fs::File::create(&temp)
            .await
            .map_err(|e| CoreError::fs(&temp, e))?;

        progress.emit(ProgressEvent::Started {
            operation: None,
            stage: Stage::Downloading,
            total: declared,
        });

        let mut hasher = blake3::Hasher::new();
        let mut written = 0_u64;
        let mut stream = response.bytes_stream();
        use futures::StreamExt as _;

        loop {
            // A stalled connection must not hang the whole application.
            let next = tokio::time::timeout(self.config.stall_timeout, stream.next()).await;
            let chunk = match next {
                Err(_) => {
                    drop(file);
                    let _ = tokio::fs::remove_file(&temp).await;
                    return Err(CoreError::Provider("the download stalled".to_owned()));
                }
                Ok(None) => break,
                Ok(Some(Err(e))) => {
                    drop(file);
                    let _ = tokio::fs::remove_file(&temp).await;
                    return Err(CoreError::Provider(format!(
                        "download interrupted: {}",
                        redact_url(&e.to_string())
                    )));
                }
                Ok(Some(Ok(chunk))) => chunk,
            };

            if cancel.is_cancelled() {
                drop(file);
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(CoreError::Cancelled);
            }

            written += chunk.len() as u64;
            if written > self.config.max_bytes {
                drop(file);
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(CoreError::InvalidInput(
                    "the download exceeded the maximum allowed size".to_owned(),
                ));
            }
            // Hashing as the bytes go past means no second pass over the file.
            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|e| CoreError::fs(&temp, e))?;

            progress.emit(ProgressEvent::Advanced {
                stage: Stage::Downloading,
                completed: written,
                total: declared,
                detail: Some(target.filename.clone()),
            });
        }

        file.flush().await.map_err(|e| CoreError::fs(&temp, e))?;
        file.sync_all().await.map_err(|e| CoreError::fs(&temp, e))?;
        drop(file);

        // A truncated transfer that still ended cleanly is caught here.
        if let Some(expected) = declared {
            if written != expected {
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(CoreError::Provider(format!(
                    "download was {written} bytes but the server declared {expected}"
                )));
            }
        }

        let hash = FileHash::blake3(*hasher.finalize().as_bytes());
        let path = self.store.promote(&temp, &hash).await?;
        progress.emit(ProgressEvent::Finished {
            stage: Stage::Downloading,
            success: true,
        });

        Ok(DownloadOutcome {
            path,
            hash,
            bytes: written,
            deduplicated: false,
        })
    }
}

/// Where a completed download ended up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadOutcome {
    /// Path in content-addressed storage.
    pub path: PathBuf,
    /// BLAKE3 hash of the bytes.
    pub hash: FileHash,
    /// Bytes transferred. Zero when the file was already stored.
    pub bytes: u64,
    /// Whether the file was already present and nothing was transferred.
    pub deduplicated: bool,
}

fn uuid_v4() -> String {
    // A small local generator avoids pulling `uuid` in purely for temp names.
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let random: u64 = rand::random();
    format!("{nanos:x}-{random:x}")
}

/// Whether a path looks like an incomplete download left by a previous run.
#[must_use]
pub fn is_partial(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "part")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_downloads_are_recognizable() {
        assert!(is_partial(Path::new("/tmp/download-abc.part")));
        assert!(!is_partial(Path::new("/tmp/mod.zip")));
    }

    #[test]
    fn temp_names_do_not_collide() {
        let a = uuid_v4();
        let b = uuid_v4();
        assert_ne!(a, b);
    }

    #[test]
    fn the_default_config_is_conservative() {
        let config = DownloadConfig::default();
        assert!(
            config.max_concurrent <= 8,
            "too many parallel downloads invites a rate limit"
        );
        assert!(config.max_redirects <= 10);
        assert!(config.max_attempts >= 2);
    }
}
