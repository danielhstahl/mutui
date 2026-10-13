//! Typed errors.
//!
//! Library code uses the [`Error`] enum below; `anyhow` is only used at the
//! binary/worker boundaries (see `docs/adr/0001-stack-and-architecture.md`).
//! Every variant that a user can trigger also has a one-line [`Error::footer_message`]
//! which is what the status bar shows — failures are never swallowed.

use std::io;
use std::path::PathBuf;

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// All typed errors produced by the library layers.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The folder passed on the command line does not exist.
    #[error("folder does not exist: {}", .0.display())]
    FolderMissing(PathBuf),

    /// The path exists but is not a directory.
    #[error("not a directory: {}", .0.display())]
    NotADirectory(PathBuf),

    /// The directory exists but cannot be read (permissions).
    #[error("folder is not readable (check permissions): {}", .0.display())]
    FolderUnreadable(PathBuf),

    /// stdout is not a terminal (piped, /dev/null, ...).
    #[error("stdout is not a terminal; run music-tui from an interactive terminal")]
    NotATty,

    /// stdin is not a terminal, so input could never be read.
    #[error("stdin is not a terminal; music-tui needs an interactive terminal")]
    NoInput,

    /// Setting up the alternate screen / raw mode failed.
    #[error("could not initialise terminal: {0}")]
    Terminal(String),

    /// Restoring the terminal failed (reported, never fatal).
    #[error("could not restore terminal: {0}")]
    TerminalRestore(String),

    /// A file could not be read or tagged while scanning.
    #[error("could not read {}: {reason}", .path.display())]
    Scan {
        /// File that failed.
        path: PathBuf,
        /// Human readable reason.
        reason: String,
    },

    /// The scanner thread died.
    #[error("library scan failed: {0}")]
    ScanAborted(String),

    /// The playback backend refused or failed to start a track.
    #[error("playback failed: {reason}")]
    Playback {
        /// Human readable reason.
        reason: String,
    },

    /// The user asked for an action no backend on this machine can do
    /// (for example live volume control through an external player).
    #[error("{0}")]
    Unsupported(String),

    /// Anything else.
    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Short, single-line text for the footer/status bar.
    #[must_use]
    pub fn footer_message(&self) -> String {
        match self {
            Error::FolderMissing(_)
            | Error::NotADirectory(_)
            | Error::FolderUnreadable(_)
            | Error::Scan { .. } => trim_self(self),
            Error::Playback { reason } => format!("Playback error: {reason}"),
            Error::Terminal(m) | Error::TerminalRestore(m) => format!("Terminal error: {m}"),
            Error::Unsupported(m) | Error::Other(m) => m.clone(),
            Error::NotATty | Error::NoInput | Error::ScanAborted(_) => trim_self(self),
        }
    }

    /// Process exit code for this error (documented in the README).
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            // Bad usage / bad folder: shell-conventional "command misused".
            Error::FolderMissing(_) | Error::NotADirectory(_) | Error::FolderUnreadable(_) => 2,
            Error::NotATty | Error::NoInput => 3,
            Error::Terminal(_) | Error::TerminalRestore(_) => 4,
            _ => 1,
        }
    }
}

fn trim_self(e: &Error) -> String {
    e.to_string()
}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Error::Other(format!("{value}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_errors_have_distinct_messages_and_codes() {
        let p = PathBuf::from("/nope");
        let missing = Error::FolderMissing(p.clone());
        let file = Error::NotADirectory(p.clone());
        let perms = Error::FolderUnreadable(p.clone());
        assert_ne!(missing.to_string(), file.to_string());
        assert_ne!(file.to_string(), perms.to_string());
        assert_eq!(missing.exit_code(), 2);
        assert_eq!(file.exit_code(), 2);
        assert_eq!(perms.exit_code(), 2);
        assert_eq!(Error::NotATty.exit_code(), 3);
        assert!(!Error::Playback {
            reason: "boom".into()
        }
        .footer_message()
        .contains('\n'));
    }
}
