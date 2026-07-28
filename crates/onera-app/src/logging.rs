//! Structured logging.
//!
//! `tracing` is configured once, at startup, by whichever driver is running. Two
//! rules apply everywhere:
//!
//! * secrets never reach a log, because [`onera_core::redact::Secret`] cannot
//!   render itself and URLs go through
//!   [`onera_core::redact::redact_url`] before they are recorded;
//! * the default filter is quiet enough that a user can paste a log into an
//!   issue without leaking their library paths at `TRACE` verbosity.

use onera_core::Result;
use std::path::Path;
use tracing_subscriber::EnvFilter;

/// Output format for logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Human-readable, for a terminal.
    Text,
    /// One JSON object per line, for the desktop app's diagnostics pane.
    Json,
}

/// Initialize logging.
///
/// Safe to call more than once: a second call is a no-op rather than a panic, so
/// a CLI embedded in a test harness does not blow up.
///
/// # Errors
/// Fails only if the log directory cannot be created.
pub fn init(directory: Option<&Path>, format: LogFormat, verbose: bool) -> Result<()> {
    if let Some(directory) = directory {
        std::fs::create_dir_all(directory).map_err(|e| onera_core::CoreError::fs(directory, e))?;
    }

    let default = if verbose {
        "onera=debug,onera_core=debug,onera_install=debug,onera_nexus=debug"
    } else {
        "onera=info,warn"
    };
    let filter = EnvFilter::try_from_env("ONERA_LOG").unwrap_or_else(|_| EnvFilter::new(default));

    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_ansi(format == LogFormat::Text);

    // `try_init` rather than `init`: repeated initialization is a normal thing
    // for a library used by three different binaries and by tests.
    let _ = match format {
        LogFormat::Json => builder.json().try_init(),
        LogFormat::Text => builder.try_init(),
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initializing_twice_is_harmless() {
        let dir = tempfile::tempdir().unwrap();
        init(Some(dir.path()), LogFormat::Text, false).unwrap();
        init(Some(dir.path()), LogFormat::Json, true).unwrap();
    }

    #[test]
    fn the_log_directory_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("nested/logs");
        init(Some(&logs), LogFormat::Text, false).unwrap();
        assert!(logs.is_dir());
    }
}
