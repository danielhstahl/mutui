//! Command line: one required folder plus a few escape hatches.
//!
//! Validation happens *before* the alternate screen is entered, so a bad path
//! produces a readable message on stderr with a documented exit code instead of
//! a corrupt terminal.

use std::path::{Path, PathBuf};

use clap::Parser;

use crate::error::{Error, Result};
use crate::scan::{DEFAULT_EXTENSIONS, ScanConfig};

/// The supported `--backend` values.
pub const BACKENDS: &[&str] = &["auto", "command", "silent", "mock", "none"];

/// A terminal music player: point it at a folder and click a song.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "music-tui",
    version = crate::VERSION,
    about = "Play a folder of music in the terminal: sorted library, mouse scrolling, click to play, auto-advance until the end of the artist.",
    after_help = "PLAYBACK RULE\n  Clicking a row plays it and then every following row of the same artist,\n  stopping after the last song of that artist. See `--dump-sort` to print\n  the resolved order and 'music-tui --help' for keys.\n"
)]
pub struct Cli {
    /// Folder to scan recursively for audio files.
    pub folder: PathBuf,

    /// Extra/replacement audio extensions, comma separated (default list:
    /// mp3,flac,ogg,oga,opus,m4a,m4b,aac,wav,aiff,aif,wma,ape,mpc,wv).
    #[arg(long, value_delimiter = ',', value_name = "EXT")]
    pub extensions: Option<Vec<String>>,

    /// Tag-reading threads (0 = number of CPU cores, capped at 8).
    #[arg(long, default_value_t = 0, value_name = "N")]
    pub scan_threads: usize,

    /// Log file (default: $XDG_STATE_HOME|~/.local/state|tmp + /music-tui.log).
    #[arg(long, value_name = "PATH")]
    pub log_file: Option<PathBuf>,

    /// Verbose logging to the log file (equivalent to RUST_LOG=debug).
    #[arg(long)]
    pub debug: bool,

    /// Refuse to index more than this many files.
    #[arg(long, default_value_t = 50_000, value_name = "N")]
    pub max_files: usize,

    /// Refuse to visit more than this many directory entries.
    #[arg(long, default_value_t = 500_000, value_name = "N")]
    pub max_entries: usize,

    /// Where the library index for this folder is stored (default: a file named
    /// after the folder and the scanner flags, under $XDG_CACHE_HOME/music-tui/).
    #[arg(long, value_name = "PATH")]
    pub cache_file: Option<PathBuf>,

    /// Do not read or write the library index: open and re-read every file on
    /// every start. Use it where mtime cannot be trusted (some NFS/SMB mounts).
    #[arg(long)]
    pub no_cache: bool,

    /// Ignore the stored library index once (re-read every tag, then write the
    /// index again). Also available in the UI as shift-R.
    #[arg(long)]
    pub refresh: bool,

    /// Re-sync the library in the background every N seconds; 0 turns it off.
    /// A sync that finds nothing new costs one stat per file and no re-tagging.
    #[arg(long, default_value_t = 60, value_name = "SECONDS")]
    pub sync_interval: u64,

    /// Rows scrolled by one mouse wheel notch.
    #[arg(long, default_value_t = 3, value_name = "N")]
    pub wheel_rows: i32,

    /// Playback backend: auto, command, silent, mock, none.
    #[arg(
        long,
        default_value = "auto",
        value_parser = clap::builder::PossibleValuesParser::new(BACKENDS)
    )]
    pub backend: String,

    /// Avoid non-ASCII glyphs (for terminals without Unicode).
    #[arg(long)]
    pub ascii: bool,

    /// Do not grab the mouse (keeps native text selection; keyboard only).
    #[arg(long)]
    pub no_mouse: bool,

    /// Print the sorted library to stdout and exit (no terminal UI).
    #[arg(long)]
    pub dump_sort: bool,
}

impl Cli {
    /// Scanner configuration derived from the flags.
    #[must_use]
    pub fn scan_config(&self) -> ScanConfig {
        let base = ScanConfig::default();
        ScanConfig {
            extensions: self
                .extensions
                .clone()
                .unwrap_or_else(|| base.extensions.clone()),
            max_files: self.max_files.max(1),
            max_entries: self.max_entries.max(1),
            threads: if self.scan_threads == 0 {
                base.threads
            } else {
                self.scan_threads.clamp(1, 32)
            },
            include_hidden: false,
            max_depth: base.max_depth,
            cache: None,
        }
    }
    /// How often the event loop should re-sync the library in the background
    /// (`--sync-interval`; zero turns the periodic sync off).
    #[must_use]
    pub fn sync_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.sync_interval)
    }
}

/// Check the folder argument and resolve it to an absolute path.
///
/// Distinct, actionable errors for: missing, not-a-directory, unreadable.
pub fn validate_folder(input: &Path) -> Result<PathBuf> {
    if !input.exists() {
        return Err(Error::FolderMissing(input.to_path_buf()));
    }
    let is_dir = input.metadata().map(|meta| meta.is_dir()).unwrap_or(false);
    if !is_dir {
        return Err(Error::NotADirectory(input.to_path_buf()));
    }
    // A directory we cannot read would produce an empty library and a confused
    // user, so refuse up front.
    if std::fs::read_dir(input).is_err() {
        return Err(Error::FolderUnreadable(input.to_path_buf()));
    }
    input
        .canonicalize()
        .map_err(|err| Error::Other(format!("could not resolve {}: {err}", input.display())))
}

/// Default log file location (documented in the README).
#[must_use]
pub fn default_log_path() -> PathBuf {
    if let Ok(state) = std::env::var("XDG_STATE_HOME")
        && !state.is_empty()
    {
        return PathBuf::from(state).join("music-tui").join("music-tui.log");
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    if home.is_empty() {
        return std::env::temp_dir().join("music-tui.log");
    }
    PathBuf::from(home)
        .join(".local")
        .join("state")
        .join("music-tui.log")
}

/// The default extension list as text (used in `--help` and docs).
#[must_use]
pub fn default_extensions_text() -> String {
    DEFAULT_EXTENSIONS.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_folder_is_distinct_from_a_file() {
        let missing = validate_folder(Path::new("/definitely/not/here"));
        assert!(matches!(missing, Err(Error::FolderMissing(_))));

        let file = std::env::temp_dir().join("mutu-cli-file.txt");
        std::fs::write(&file, b"x").expect("fixture");
        let wrong_kind = validate_folder(&file);
        assert!(matches!(wrong_kind, Err(Error::NotADirectory(_))));

        let dir = std::env::temp_dir();
        assert!(validate_folder(&dir).is_ok(), "temp dir should validate");
        let _remove = std::fs::remove_file(&file);
    }

    #[test]
    fn unreadable_directory_is_reported() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let root = std::env::temp_dir().join(format!("mutu-cli-perm-{}", std::process::id()));
            let _remove = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("fixture dir");
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o000)).expect("chmod");
            let result = validate_folder(&root);
            let _permissions =
                std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755));
            let _remove = std::fs::remove_dir_all(&root);
            // Running as root ignores the mode, so only assert when it applies.
            if result.is_err() {
                assert!(matches!(result, Err(Error::FolderUnreadable(_))));
            }
        }
    }

    #[test]
    fn relative_paths_are_resolved() {
        let resolved = validate_folder(Path::new(".")).expect("cwd resolves");
        assert!(resolved.is_absolute());
    }

    #[test]
    fn flags_map_to_scan_config() {
        let cli = Cli::try_parse_from([
            "music-tui",
            "/music",
            "--extensions",
            "mid,mod",
            "--scan-threads",
            "3",
        ])
        .expect("parse");
        let cfg = cli.scan_config();
        assert_eq!(cfg.extensions, vec!["mid".to_string(), "mod".to_string()]);
        assert_eq!(cfg.threads, 3);
        assert_eq!(cli.wheel_rows, 3);
        assert_eq!(cli.backend, "auto");

        let no_args = Cli::try_parse_from(["music-tui"]);
        assert!(no_args.is_err(), "folder is required");
    }

    #[test]
    fn default_log_path_is_a_file_in_a_directory() {
        let path = default_log_path();
        assert_eq!(
            path.file_name().map(|s| s.to_string_lossy().into_owned()),
            Some("music-tui.log".to_string())
        );
        assert!(default_extensions_text().contains("mp3"));
    }
}
