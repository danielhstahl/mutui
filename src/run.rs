//! Process bootstrap: pick a backend, start the scanner, take over the
//! terminal, run the loop, put everything back.
//!
//! Kept in the library (rather than `main.rs`) so integration tests can exercise
//! the same wiring headlessly.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use crate::app::eventloop::{run_loop, CrosstermInput, Drawer, InputSource, TerminalDrawer};
use crate::app::state::{AppState, Notice, Options};
use crate::app::time::{Clock, SystemClock};
use crate::audio::{
    detect_backend, known_players_text, BoxedBackend, CommandBackend, MockBackend, NullBackend,
    PlayerHandle,
};
use crate::cli::{self, Cli};
use crate::error::{Error, Result};
use crate::model::{dump_sorted_order, sort_tracks, Track};
use crate::scan::{self, ScanConfig, ScanMessage};
use crate::terminal::{self, TerminalGuard};

/// Backend selection for `--backend`.
///
/// Returns the backend plus an optional notice for the footer.
#[must_use]
pub fn make_backend(kind: &str) -> (BoxedBackend, Option<String>) {
    match kind {
        "silent" | "none" => (Box::new(NullBackend::new()), None),
        "mock" => {
            let (backend, _handle) = MockBackend::paired();
            (Box::new(backend), None)
        }
        "command" => match detect_backend() {
            Some(spec) => {
                let label = spec.label.to_string();
                (Box::new(CommandBackend::new(spec)), Some(format!("output: {label}")))
            }
            None => (
                Box::new(NullBackend::new()),
                Some(format!(
                    "No audio player found (looked for {}). Running silent.",
                    known_players_text()
                )),
            ),
        },
        // "auto": external player when one exists, otherwise the silent backend
        // so the UI stays fully usable (and testable) on headless machines.
        _ => match detect_backend() {
            Some(spec) => {
                let label = spec.label.to_string();
                (Box::new(CommandBackend::new(spec)), Some(format!("output: {label}")))
            },
            None => (
                Box::new(NullBackend::new()),
                Some(format!(
                    "No audio player found (looked for {}). Playing silently; install mpv, ffplay or mpg123.",
                    known_players_text()
                )),
            ),
        },
    }
}

/// Runs the scanner on worker threads and owns the plumbing the loop reads.
///
/// The receiving end is shared through a mutex so a rescan can swap it: dropping
/// the old receiver makes a cancelled scan's leftover messages disappear instead
/// of leaking into the fresh list.
pub struct ScanJob {
    /// Receiving end read by the event loop.
    pub rx: Arc<Mutex<mpsc::Receiver<ScanMessage>>>,
    cancel: Arc<AtomicBool>,
    folder: PathBuf,
    config: ScanConfig,
}

impl ScanJob {
    /// Start scanning `folder` in the background.
    #[must_use]
    pub fn start(folder: PathBuf, config: ScanConfig) -> Self {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        spawn_scan(&folder, &config, &tx, &cancel);
        Self {
            rx: Arc::new(Mutex::new(rx)),
            cancel,
            folder,
            config,
        }
    }

    /// Cancel the running scan (if any) and start a fresh one.
    pub fn restart(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.cancel = Arc::new(AtomicBool::new(false));
        spawn_scan(&self.folder, &self.config, &tx, &self.cancel);
        if let Ok(mut slot) = self.rx.lock() {
            *slot = rx;
        }
    }

    /// Ask the running scan to stop.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

fn spawn_scan(
    folder: &Path,
    config: &ScanConfig,
    tx: &mpsc::Sender<ScanMessage>,
    cancel: &Arc<AtomicBool>,
) {
    let folder = folder.to_path_buf();
    let config = config.clone();
    let tx = tx.clone();
    let cancel = Arc::clone(cancel);
    let _handle = std::thread::spawn(move || {
        scan::scan(&folder, &config, &tx, &cancel);
    });
}

/// Scan synchronously and return the sorted library (used by `--dump-sort`).
#[must_use]
pub fn scan_to_sorted_vec(folder: &Path, config: &ScanConfig) -> Vec<Track> {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    scan::scan(folder, config, &tx, &cancel);
    let mut tracks = Vec::new();
    while let Ok(message) = rx.try_recv() {
        if let ScanMessage::Track(track) = message {
            tracks.push(track);
        }
    }
    sort_tracks(&mut tracks);
    tracks
}

/// Text of a `--dump-sort` run.
#[must_use]
pub fn dump_sort_text(tracks: &[Track]) -> String {
    dump_sorted_order(tracks)
}
/// Validate arguments *before* touching the terminal.
pub fn prepare(cli: &Cli) -> Result<PathBuf> {
    cli::validate_folder(&cli.folder)
}

/// The whole interactive session.
pub fn run(cli: &Cli, folder: PathBuf, log_path: &Path) -> Result<()> {
    terminal::set_log_path(&log_path.display().to_string());
    terminal::install_panic_hook();
    terminal::install_sigint_handler();

    if !terminal::is_interactive() {
        return Err(Error::NotATty);
    }

    let (backend, warning) = make_backend(&cli.backend);
    let backend_name = backend.name().to_string();

    let mut state = AppState::new(Options {
        folder: folder.clone(),
        backend: backend_name,
        wheel_rows: cli.wheel_rows,
        ascii: cli.ascii || !terminal::supports_unicode(),
        debug_footer: cli.debug,
    });
    if let Some(text) = warning {
        state.notice = Some(Notice::warn(text, state.now));
    }

    let mut scan_job = ScanJob::start(folder, cli.scan_config());
    let player = PlayerHandle::spawn(backend);

    let outcome = {
        let mut guard = TerminalGuard::enter(cli.no_mouse)?;
        let outcome = session(&mut state, &player, &mut scan_job);
        guard.restore();
        outcome
    };
    player.shutdown();
    scan_job.cancel();
    outcome
}

fn session(state: &mut AppState, player: &PlayerHandle, scan_job: &mut ScanJob) -> Result<()> {
    let mut drawer = TerminalDrawer::new();
    let mut input = CrosstermInput;
    let clock = SystemClock;
    // Clone the receiving end so `on_rescan` can own `&mut scan_job`.
    let rx = Arc::clone(&scan_job.rx);
    let mut on_rescan = || scan_job.restart();
    run_loop(
        state,
        player,
        &rx,
        &mut input,
        &mut drawer,
        &clock,
        &mut on_rescan,
        None,
    )
}

/// Headless session used by tests: scripted input, buffer drawer, fake clock.
#[allow(clippy::too_many_arguments)]
pub fn run_headless(
    state: &mut AppState,
    player: &PlayerHandle,
    scan_rx: &Arc<Mutex<mpsc::Receiver<ScanMessage>>>,
    input: &mut dyn InputSource,
    drawer: &mut dyn Drawer,
    clock: &dyn Clock,
    on_rescan: &mut dyn FnMut(),
    max_iterations: Option<usize>,
) -> Result<()> {
    run_loop(
        state,
        player,
        scan_rx,
        input,
        drawer,
        clock,
        on_rescan,
        max_iterations,
    )
}
