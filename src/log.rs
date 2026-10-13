//! File-only logging.
//!
//! Nothing may be written to stdout or stderr once the alternate screen is
//! active, so `tracing` always writes to a file (see the ADR, "forbidden
//! outputs"). Startup failures happen before the terminal is taken over and go
//! to stderr.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

use crate::error::{Error, Result};

/// Install the global subscriber writing to `path` and return the path used.
///
/// Level: `RUST_LOG` if set, otherwise `debug` with `--debug` and `warn`
/// otherwise (the app is interactive; we do not want per-event sync overhead).
pub fn init(path: &Path, debug: bool) -> Result<PathBuf> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|err| {
                Error::Other(format!(
                    "cannot create log directory {}: {err}",
                    parent.display()
                ))
            })?;
        }
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| Error::Other(format!("cannot open log file {}: {err}", path.display())))?;
    let mirror = file
        .try_clone()
        .map_err(|err| Error::Other(format!("cannot duplicate log file: {err}")))?;

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(if debug { "debug" } else { "warn" })
    });
    let writer = std::sync::Mutex::new(file);
    let subscriber = tracing_subscriber::registry().with(env_filter).with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(writer),
    );
    // A second `init` (possible in tests) must not panic.
    let _install = subscriber.try_init();

    note(
        mirror,
        &format_args!(
            "--- music-tui {} started (pid {})",
            crate::VERSION,
            std::process::id()
        ),
    );
    Ok(path.to_path_buf())
}

/// Append one line to a log file that is already open.
fn mut_writer(file: &mut std::fs::File, args: std::fmt::Arguments<'_>) {
    let _write = writeln!(file, "{args}");
    let _flush = file.flush();
}

fn note(mut file: std::fs::File, args: &std::fmt::Arguments<'_>) {
    mut_writer(&mut file, *args);
}

/// Log a line directly to `path` (used before/around the subscriber).
pub fn line(path: &Path, message: &str) {
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        mut_writer(&mut file, format_args!("{message}"));
    }
}
