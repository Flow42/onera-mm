//! Secure archive inspection and extraction.
//!
//! Onera treats every archive as hostile. The rules, enforced identically by
//! every backend:
//!
//! * entries are enumerated and validated *before* anything is written;
//! * paths are normalized through [`onera_core::RelPath`], so traversal,
//!   absolute paths and drive prefixes cannot survive;
//! * symbolic links, hard links and special files are never created;
//! * entry count, per-file size, total size, nesting depth, path length and
//!   compression ratio are all bounded;
//! * extraction only ever targets a fresh, empty staging directory — never a
//!   game directory;
//! * the resulting [`ArchiveManifest`] is built from the bytes that actually
//!   landed on disk, not from what the archive claimed.
//!
//! See `docs/threat-model.md` for the reasoning behind each rule.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod detect;
pub mod limits;
pub mod validate;
pub mod write;

mod backends {
    pub mod sevenz;
    pub mod tar;
    pub mod zip;
}

pub use backends::sevenz::find_sevenz;
pub use detect::detect_format;
pub use limits::ExtractionLimits;

use async_trait::async_trait;
use onera_core::domain::archive::{ArchiveFormat, ArchiveInspection, ArchiveManifest};
use onera_core::hash::{hash_file_blake3, FileHash};
use onera_core::ports::ArchiveBackend;
use onera_core::progress::{CancelToken, ProgressEvent, ProgressSink};
use onera_core::{CoreError, Result};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use validate::Validator;

/// Hard ceiling on nesting depth, independent of configured limits.
pub const MAX_ARCHIVE_DEPTH: usize = 32;

/// A `Send + 'static` sink that forwards events over a channel.
///
/// Extraction runs on the blocking pool, so it cannot borrow the caller's
/// `&dyn ProgressSink`. Events are sent through this instead and replayed onto
/// the real sink as they arrive, which keeps progress streaming rather than
/// arriving in one batch at the end.
struct ChannelProgress(mpsc::UnboundedSender<ProgressEvent>);

impl ProgressSink for ChannelProgress {
    fn emit(&self, event: ProgressEvent) {
        // A closed receiver means the caller stopped listening; dropping the
        // event is correct and must never fail the extraction.
        let _ = self.0.send(event);
    }
}

/// The archive backend Onera ships.
///
/// Dispatches by detected format: pure-Rust readers for zip and the tar
/// variants, an external process for 7z and rar.
#[derive(Debug, Clone)]
pub struct SafeArchiveBackend {
    limits: ExtractionLimits,
    sevenz_binary: Option<PathBuf>,
}

impl Default for SafeArchiveBackend {
    fn default() -> Self {
        Self::new(ExtractionLimits::default())
    }
}

impl SafeArchiveBackend {
    /// Build a backend with the given limits, discovering `7zz` lazily.
    #[must_use]
    pub fn new(limits: ExtractionLimits) -> Self {
        Self {
            limits,
            sevenz_binary: None,
        }
    }

    /// Pin a specific 7-Zip binary instead of searching `PATH`.
    #[must_use]
    pub fn with_sevenz_binary(mut self, binary: PathBuf) -> Self {
        self.sevenz_binary = Some(binary);
        self
    }

    /// The limits in force.
    #[must_use]
    pub fn limits(&self) -> &ExtractionLimits {
        &self.limits
    }

    fn validator(&self) -> Validator {
        Validator::new(self.limits)
    }
}

#[async_trait]
impl ArchiveBackend for SafeArchiveBackend {
    fn supports(&self, path: &Path) -> bool {
        // Content sniffing is async; this synchronous capability check only
        // filters file pickers. Real dispatch always re-detects by content.
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .is_some_and(|e| {
                matches!(
                    e.as_str(),
                    "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst"
                )
            })
    }

    async fn inspect(&self, path: &Path, cancel: &CancelToken) -> Result<ArchiveInspection> {
        cancel.check()?;
        let format = detect_format(path).await?;
        let mut validator = self.validator();

        if format.needs_external_tool() {
            let binary = backends::sevenz::require_binary(self.sevenz_binary.as_ref())?;
            return backends::sevenz::inspect(&binary, path, format, &mut validator).await;
        }

        let owned = path.to_path_buf();
        spawn_blocking(move || match format {
            ArchiveFormat::Zip => backends::zip::inspect_blocking(&owned, &mut validator),
            _ => backends::tar::inspect_blocking(&owned, format, &mut validator),
        })
        .await
    }

    async fn extract(
        &self,
        path: &Path,
        staging: &Path,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<ArchiveManifest> {
        cancel.check()?;
        let format = detect_format(path).await?;
        // Hashing the archive ties the manifest to exactly these bytes, so a
        // manifest can never be applied to a different archive.
        let archive_hash = hash_file_blake3(path)
            .await
            .map_err(|e| CoreError::fs(path, e))?;

        tokio::fs::create_dir_all(staging)
            .await
            .map_err(|e| CoreError::fs(staging, e))?;
        let mut validator = self.validator();

        if format.needs_external_tool() {
            let binary = backends::sevenz::require_binary(self.sevenz_binary.as_ref())?;
            return backends::sevenz::extract(
                &binary,
                path,
                format,
                staging,
                archive_hash,
                &mut validator,
                progress,
                cancel,
            )
            .await;
        }

        let (tx, mut rx) = mpsc::unbounded_channel();
        let owned_path = path.to_path_buf();
        let owned_staging = staging.to_path_buf();
        let cancel = cancel.clone();
        let worker = tokio::task::spawn_blocking(move || {
            let sink = ChannelProgress(tx);
            match format {
                ArchiveFormat::Zip => backends::zip::extract_blocking(
                    &owned_path,
                    &owned_staging,
                    archive_hash,
                    &mut validator,
                    &sink,
                    &cancel,
                ),
                _ => backends::tar::extract_blocking(
                    &owned_path,
                    format,
                    &owned_staging,
                    archive_hash,
                    &mut validator,
                    &sink,
                    &cancel,
                ),
            }
        });

        // Drain while the worker runs so the UI sees progress live. The loop
        // ends when the worker drops its sender, which happens on both the
        // success and the failure path.
        while let Some(event) = rx.recv().await {
            progress.emit(event);
        }

        worker
            .await
            .map_err(|e| CoreError::Archive(format!("archive worker panicked: {e}")))?
    }
}

async fn spawn_blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| CoreError::Archive(format!("archive worker panicked: {e}")))?
}

/// Compute the BLAKE3 hash of an archive file.
///
/// # Errors
/// Propagates I/O errors.
pub async fn hash_archive(path: &Path) -> Result<FileHash> {
    hash_file_blake3(path)
        .await
        .map_err(|e| CoreError::fs(path, e))
}
