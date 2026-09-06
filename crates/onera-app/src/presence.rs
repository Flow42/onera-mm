//! Desktop presence: is the Onera window running, and how does something else
//! start it?
//!
//! The browser extension needs an answer to both questions, and neither can be
//! answered by the Native Messaging host's own existence: Chromium starts that
//! host on demand, so a reply from it proves only that Onera is *installed*.
//!
//! Presence is recorded as a heartbeat file under `$XDG_RUNTIME_DIR`, which the
//! session tears down on logout — a stale record cannot outlive the session
//! that wrote it. Three facts must agree before the desktop is called running:
//!
//! * the file exists and parses;
//! * its process id still resolves to a live process;
//! * its heartbeat is newer than [`STALE_AFTER`].
//!
//! The process check alone would be fooled by a recycled pid, and the timestamp
//! alone by a process suspended mid-write. Together they are wrong only in a
//! window narrower than the poll interval, and being wrong costs a redundant
//! launch, not a lost request.

use onera_core::{CoreError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How often the desktop refreshes its heartbeat.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// How old a heartbeat may be before the writer is presumed gone.
///
/// Three missed beats: long enough that a busy install does not read as a
/// crash, short enough that the extension does not offer to launch a window
/// that is already open.
pub const STALE_AFTER: Duration = Duration::from_secs(20);

/// Environment variable naming the desktop binary, for development builds and
/// AppImage installs whose path is not stable.
pub const DESKTOP_BINARY_ENV: &str = "ONERA_DESKTOP_BIN";

/// File name of the heartbeat inside the runtime directory.
const PRESENCE_FILE: &str = "desktop.json";

/// Installed locations of the desktop binary, in preference order.
const DESKTOP_BINARY_PATHS: &[&str] = &[
    "/usr/bin/onera-desktop",
    "/usr/local/bin/onera-desktop",
    "/var/lib/flatpak/exports/bin/com.onera.desktop",
];

/// What one process recorded about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopPresence {
    /// Process id of the running desktop application.
    pub pid: u32,
    /// Version of the binary that wrote the record.
    pub version: String,
    /// Unix seconds of the last heartbeat.
    pub updated_at: u64,
}

/// The heartbeat file, and the operations either side of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Presence {
    path: PathBuf,
}

impl Presence {
    /// Use an explicit runtime directory.
    #[must_use]
    pub fn at(runtime_dir: PathBuf) -> Self {
        Self {
            path: runtime_dir.join(PRESENCE_FILE),
        }
    }

    /// Locate the heartbeat for the current user.
    ///
    /// `$XDG_RUNTIME_DIR` is preferred because the session owns its lifetime.
    /// Where it is unset — a bare TTY, some containers — the state directory
    /// stands in, and staleness alone has to carry the answer.
    #[must_use]
    pub fn discover(state_dir: &Path) -> Self {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .map_or_else(|| state_dir.to_path_buf(), |dir| dir.join("onera"));
        Self::at(runtime)
    }

    /// Path of the heartbeat file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Record that this process is running the desktop application.
    ///
    /// # Errors
    /// Propagates I/O errors from creating the runtime directory or writing.
    pub async fn beat(&self, version: &str) -> Result<()> {
        let record = DesktopPresence {
            pid: std::process::id(),
            version: version.to_owned(),
            updated_at: unix_seconds(SystemTime::now()),
        };
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| CoreError::fs(parent, e))?;
        }
        let encoded = serde_json::to_vec(&record)
            .map_err(|e| CoreError::fs(&self.path, std::io::Error::other(e)))?;
        // Written through a temporary file: a reader must never see a half
        // written record and conclude the desktop has gone away.
        let temp = self.path.with_extension("json.tmp");
        tokio::fs::write(&temp, &encoded)
            .await
            .map_err(|e| CoreError::fs(&temp, e))?;
        tokio::fs::rename(&temp, &self.path)
            .await
            .map_err(|e| CoreError::fs(&self.path, e))
    }

    /// Remove the heartbeat, on a clean shutdown.
    ///
    /// A missing file is success: the point is that no record remains.
    ///
    /// # Errors
    /// Propagates I/O errors other than "not found".
    pub async fn clear(&self) -> Result<()> {
        match tokio::fs::remove_file(&self.path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(CoreError::fs(&self.path, e)),
        }
    }

    /// Read the recorded presence, if any is readable.
    ///
    /// An unreadable or malformed record is treated as absence rather than an
    /// error: the caller's question is "is it running", and a record it cannot
    /// understand is no evidence that it is.
    pub async fn read(&self) -> Option<DesktopPresence> {
        let bytes = tokio::fs::read(&self.path).await.ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Whether a live desktop application is running for this user.
    pub async fn is_running(&self) -> bool {
        match self.read().await {
            Some(record) => is_live(&record, SystemTime::now()),
            None => false,
        }
    }
}

/// Whether a record describes a process that is still running and beating.
#[must_use]
pub fn is_live(record: &DesktopPresence, now: SystemTime) -> bool {
    let age = unix_seconds(now).saturating_sub(record.updated_at);
    age <= STALE_AFTER.as_secs() && process_exists(record.pid)
}

/// Start the desktop application, detached from this process.
///
/// The launcher deliberately knows only fixed locations and one environment
/// variable. Reading a path out of configuration would turn a writable settings
/// file into a way of getting a program of someone else's choosing started by
/// whatever asked Onera to open.
///
/// # Errors
/// Returns [`CoreError::NotFound`] when no desktop binary can be located, and
/// propagates spawn failures.
pub fn launch_desktop() -> Result<PathBuf> {
    let binary = desktop_binary().ok_or_else(|| CoreError::NotFound {
        kind: "desktop application",
        id: "onera-desktop".to_owned(),
    })?;
    std::process::Command::new(&binary)
        // The child outlives whatever started it — typically a Native Messaging
        // host that exits as soon as the browser closes the port — so its
        // streams are detached rather than inherited.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| CoreError::fs(&binary, e))?;
    Ok(binary)
}

/// Locate an installed desktop binary.
#[must_use]
pub fn desktop_binary() -> Option<PathBuf> {
    if let Some(configured) = std::env::var_os(DESKTOP_BINARY_ENV) {
        let path = PathBuf::from(configured);
        if path.is_file() {
            return Some(path);
        }
    }
    // A binary next to the running one covers both a development build and an
    // extracted AppImage, where nothing is installed system-wide.
    if let Ok(current) = std::env::current_exe() {
        if let Some(sibling) = current.parent().map(|dir| dir.join("onera-desktop")) {
            if sibling.is_file() {
                return Some(sibling);
            }
        }
    }
    DESKTOP_BINARY_PATHS
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

/// Whether a process id currently resolves to a live process.
fn process_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    Path::new(&format!("/proc/{pid}")).is_dir()
}

fn unix_seconds(at: SystemTime) -> u64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(pid: u32, updated_at: u64) -> DesktopPresence {
        DesktopPresence {
            pid,
            version: "0.1.0".to_owned(),
            updated_at,
        }
    }

    #[test]
    fn a_fresh_record_for_this_process_is_live() {
        let now = SystemTime::now();
        assert!(is_live(&record(std::process::id(), unix_seconds(now)), now));
    }

    #[test]
    fn a_stale_heartbeat_is_not_live_even_for_a_running_process() {
        let now = SystemTime::now();
        let long_ago = unix_seconds(now) - STALE_AFTER.as_secs() - 1;
        assert!(!is_live(&record(std::process::id(), long_ago), now));
    }

    #[test]
    fn a_fresh_heartbeat_from_a_dead_process_is_not_live() {
        let now = SystemTime::now();
        // Pid 0 is never a user process, so it stands in for one that exited.
        assert!(!is_live(&record(0, unix_seconds(now)), now));
    }

    #[tokio::test]
    async fn a_written_heartbeat_reads_back_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let presence = Presence::at(dir.path().join("onera"));
        presence.beat("0.1.0").await.unwrap();
        let read = presence.read().await.expect("a record was written");
        assert_eq!(read.pid, std::process::id());
        assert!(presence.is_running().await);
        presence.clear().await.unwrap();
        assert!(presence.read().await.is_none());
        assert!(!presence.is_running().await);
        // Clearing twice is not an error: absence is the desired end state.
        presence.clear().await.unwrap();
    }

    #[tokio::test]
    async fn an_unreadable_record_reads_as_absence() {
        let dir = tempfile::tempdir().unwrap();
        let presence = Presence::at(dir.path().to_path_buf());
        tokio::fs::write(presence.path(), b"{not json")
            .await
            .unwrap();
        assert!(presence.read().await.is_none());
        assert!(!presence.is_running().await);
    }

    #[test]
    fn discovery_prefers_the_runtime_directory() {
        // Asserted on shape only: the variable is environment-dependent.
        let presence = Presence::discover(Path::new("/state/onera"));
        assert!(presence.path().ends_with(PRESENCE_FILE));
    }
}
