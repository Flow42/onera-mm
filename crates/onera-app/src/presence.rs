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

/// Environment variable naming the desktop binary.
///
/// Wins over every other source, so a developer can point at a build without
/// changing anything on disk. Note that a Native Messaging host inherits the
/// *browser's* environment, so this has to be set where the browser starts —
/// which is why `onera browser setup --desktop-path` exists.
pub const DESKTOP_BINARY_ENV: &str = "ONERA_DESKTOP_BIN";

/// File name of the heartbeat inside the runtime directory.
const PRESENCE_FILE: &str = "desktop.json";

/// File name of the recorded desktop path inside the configuration directory.
const DESKTOP_PATH_FILE: &str = "desktop-path";

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
/// # Errors
/// Returns [`CoreError::NotFound`] when no desktop binary can be located, and
/// propagates spawn failures.
pub fn launch_desktop(config_dir: &Path) -> Result<PathBuf> {
    let binary = desktop_binary(config_dir).ok_or_else(|| CoreError::NotFound {
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
///
/// Four sources, in order: the environment variable, the path recorded by
/// `onera browser setup`, a binary beside the running one, and the fixed
/// install locations.
///
/// The recorded path exists because neither of the other two automatic sources
/// covers a development tree or an AppImage: the host and the window are built
/// into different directories, and an AppImage is one file with a name of its
/// own. Without it those users get "Onera is installed as a browser connector
/// only", which is true and useless.
///
/// Reading an executable's path out of a file is a real risk, so
/// [`recorded_binary`] takes it only from a file that no other user can rewrite.
/// The bar is set by what already exists: a Native Messaging manifest is a file
/// naming an executable that the *browser* runs on a page's say-so, so a file
/// naming an executable that Onera runs on the user's own say-so is not a new
/// kind of authority — provided nobody else can write it.
#[must_use]
pub fn desktop_binary(config_dir: &Path) -> Option<PathBuf> {
    if let Some(configured) = std::env::var_os(DESKTOP_BINARY_ENV) {
        let path = PathBuf::from(configured);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(recorded) = recorded_binary(config_dir) {
        return Some(recorded);
    }
    // A binary next to the running one covers a packaged layout where the host
    // and the window sit together.
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

/// Where `onera browser setup --desktop-path` records the window's location.
#[must_use]
pub fn desktop_path_file(config_dir: &Path) -> PathBuf {
    config_dir.join(DESKTOP_PATH_FILE)
}

/// Record where the desktop application lives.
///
/// Refuses a path that is not an absolute, existing, executable file, so the
/// failure is reported when the user runs setup rather than months later when
/// they press a button in a browser.
///
/// # Errors
/// Returns [`CoreError::InvalidInput`] for a path that is not a runnable
/// executable, and propagates I/O errors.
pub async fn record_desktop_binary(config_dir: &Path, binary: &Path) -> Result<PathBuf> {
    if !binary.is_absolute() {
        return Err(CoreError::InvalidInput(format!(
            "{} is not an absolute path",
            binary.display()
        )));
    }
    let metadata = tokio::fs::metadata(binary)
        .await
        .map_err(|e| CoreError::fs(binary, e))?;
    if !metadata.is_file() || !is_executable(&metadata) {
        return Err(CoreError::InvalidInput(format!(
            "{} is not an executable file",
            binary.display()
        )));
    }

    tokio::fs::create_dir_all(config_dir)
        .await
        .map_err(|e| CoreError::fs(config_dir, e))?;
    let destination = desktop_path_file(config_dir);
    tokio::fs::write(&destination, binary.as_os_str().as_encoded_bytes())
        .await
        .map_err(|e| CoreError::fs(&destination, e))?;
    // 0600: the record names something Onera will execute, and the guard on
    // reading it back refuses anything another user could have rewritten.
    set_owner_only(&destination).await?;
    Ok(destination)
}

/// Read the recorded desktop path, if it is trustworthy.
///
/// Returns `None` rather than an error for every failure — absent, unreadable,
/// writable by others, no longer a file. The caller's question is "where is the
/// window", and a record it will not act on is the same as no record.
#[must_use]
fn recorded_binary(config_dir: &Path) -> Option<PathBuf> {
    let path = desktop_path_file(config_dir);
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    // A symlink here would move the decision to wherever it points, whose
    // permissions this check has not seen.
    if !metadata.is_file() || is_writable_by_others(&metadata) {
        tracing::warn!(
            path = %path.display(),
            "ignoring a desktop-path record that other users could rewrite"
        );
        return None;
    }
    let recorded = PathBuf::from(String::from_utf8(std::fs::read(&path).ok()?).ok()?);
    let target = std::fs::metadata(&recorded).ok()?;
    (recorded.is_absolute() && target.is_file() && is_executable(&target)).then_some(recorded)
}

/// Whether a file carries any execute bit.
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
}

/// Whether a file's group or world can write it.
fn is_writable_by_others(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o022 != 0
}

/// Restrict a file to its owner.
async fn set_owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .map_err(|e| CoreError::fs(path, e))
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

    /// Build a file that passes for an executable.
    async fn fake_binary(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join(name);
        tokio::fs::write(&path, b"#!/bin/sh\n").await.unwrap();
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();
        path
    }

    #[tokio::test]
    async fn a_recorded_path_is_found_again() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let binary = fake_binary(dir.path(), "onera-desktop").await;

        assert!(
            recorded_binary(&config).is_none(),
            "nothing is recorded yet"
        );
        record_desktop_binary(&config, &binary).await.unwrap();
        assert_eq!(recorded_binary(&config), Some(binary.clone()));
        assert_eq!(desktop_binary(&config), Some(binary));
    }

    #[tokio::test]
    async fn a_path_that_is_not_a_runnable_executable_is_refused_at_setup() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");

        // Relative: the browser's working directory is not the user's.
        assert!(record_desktop_binary(&config, Path::new("onera-desktop"))
            .await
            .is_err());
        // Absent.
        assert!(record_desktop_binary(&config, &dir.path().join("nope"))
            .await
            .is_err());
        // Present but not executable, which is the commonest mistake: pointing
        // at an AppImage that was never chmod'd.
        let data = dir.path().join("Onera.AppImage");
        tokio::fs::write(&data, b"not executable").await.unwrap();
        assert!(record_desktop_binary(&config, &data).await.is_err());
        // A directory with the right name.
        tokio::fs::create_dir(dir.path().join("bin")).await.unwrap();
        assert!(record_desktop_binary(&config, &dir.path().join("bin"))
            .await
            .is_err());

        assert!(recorded_binary(&config).is_none(), "nothing was recorded");
    }

    #[tokio::test]
    async fn a_record_other_users_could_rewrite_is_ignored() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let binary = fake_binary(dir.path(), "onera-desktop").await;
        record_desktop_binary(&config, &binary).await.unwrap();

        // The record names something Onera will execute. Anyone who can rewrite
        // it chooses that program, so a loosened mode disqualifies it.
        let record = desktop_path_file(&config);
        for mode in [0o666, 0o622, 0o620] {
            tokio::fs::set_permissions(&record, std::fs::Permissions::from_mode(mode))
                .await
                .unwrap();
            assert!(
                recorded_binary(&config).is_none(),
                "mode {mode:o} should disqualify the record"
            );
        }
        tokio::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600))
            .await
            .unwrap();
        assert_eq!(recorded_binary(&config), Some(binary));
    }

    #[tokio::test]
    async fn a_symlinked_record_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let binary = fake_binary(dir.path(), "onera-desktop").await;
        tokio::fs::create_dir_all(&config).await.unwrap();

        // Following a symlink would move the decision to a file whose
        // permissions this check never saw.
        let elsewhere = dir.path().join("elsewhere");
        tokio::fs::write(&elsewhere, binary.as_os_str().as_encoded_bytes())
            .await
            .unwrap();
        std::os::unix::fs::symlink(&elsewhere, desktop_path_file(&config)).unwrap();
        assert!(recorded_binary(&config).is_none());
    }

    #[tokio::test]
    async fn a_record_pointing_at_something_since_deleted_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let binary = fake_binary(dir.path(), "onera-desktop").await;
        record_desktop_binary(&config, &binary).await.unwrap();

        tokio::fs::remove_file(&binary).await.unwrap();
        assert!(
            recorded_binary(&config).is_none(),
            "an upgrade that moved the binary must not leave a broken launch"
        );
    }

    #[test]
    fn discovery_prefers_the_runtime_directory() {
        // Asserted on shape only: the variable is environment-dependent.
        let presence = Presence::discover(Path::new("/state/onera"));
        assert!(presence.path().ends_with(PRESENCE_FILE));
    }
}
