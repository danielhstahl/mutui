//! `music-tui` binary entry point.
//!
//! Everything that can fail before the terminal is taken over (argument
//! validation, log file, `--dump-sort`) happens first and reports on stderr
//! with a documented exit code.

use std::process::ExitCode;

use clap::Parser;

use music_tui::cli::{self, Cli};
use music_tui::error::Error;
use music_tui::{log, run};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let log_path = cli.log_file.clone().unwrap_or_else(cli::default_log_path);

    // Startup diagnostics first: nothing may be printed after the alt screen.
    if let Err(err) = log::init(&log_path, cli.debug) {
        eprintln!("music-tui: {err}");
        return exit_code(&err);
    }

    let folder = match run::prepare(&cli) {
        Ok(folder) => folder,
        Err(err) => {
            eprintln!("music-tui: {err}");
            return exit_code(&err);
        }
    };

    if cli.dump_sort {
        let config = cli.scan_config();
        let tracks = run::scan_to_sorted_vec(&folder, &config);
        print!("{}", run::dump_sort_text(&tracks));
        if tracks.is_empty() {
            // Diagnostics go to stderr so stdout stays machine-readable.
            eprintln!(
                "music-tui: no audio files found in {} (looking for: {})",
                folder.display(),
                config.extensions.join(", ")
            );
        }
        return ExitCode::SUCCESS;
    }

    if let Err(err) = run::run(&cli, folder, &log_path) {
        eprintln!("music-tui: {err} (log: {})", log_path.display());
        return exit_code(&err);
    }
    ExitCode::SUCCESS
}

fn exit_code(err: &Error) -> ExitCode {
    ExitCode::from(u8::try_from(err.exit_code()).unwrap_or(1))
}
