//! Terminal lifecycle: raw mode, alternate screen, mouse capture, panic/SIGINT
//! recovery.
//!
//! [`TerminalGuard`] is RAII: whatever happens — normal quit, `Err` propagation,
//! panic or `SIGINT` — the shell is handed back with echo, cursor and the normal
//! screen buffer restored.

use std::io::{self, Stdout, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::{cursor, execute, terminal};

use crate::error::{Error, Result};

/// `true` between `TerminalGuard::enter` and restore.
pub static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Set by the `SIGINT` handler; the event loop polls it.
pub static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);

static LOG_PATH: OnceLock<String> = OnceLock::new();

/// Type of the hook we replace while the alternate screen is active.
type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync + 'static>;

static ORIGINAL_PANIC: OnceLock<PanicHook> = OnceLock::new();

/// Record the log file so the panic message can point at it.
pub fn set_log_path(path: &str) {
    let _set = LOG_PATH.set(path.to_string());
}

fn log_path() -> &'static str {
    LOG_PATH
        .get()
        .map(String::as_str)
        .unwrap_or("(no log file)")
}

/// Mouse capture without `?1003` (any-event tracking).
///
/// crossterm's `EnableMouseCapture` also switches on any-event tracking, which
/// permanently steals mouse selection from the terminal. Press/release (`1000`),
/// drag (`1002`) and SGR coordinates (`1006`) are all the app needs, so those —
/// and only those — are enabled here. Documented in the README.
const MOUSE_ON: &[u8] = b"\x1b[?1000h\x1b[?1002h\x1b[?1006h";
const MOUSE_OFF: &[u8] = b"\x1b[?1006l\x1b[?1002l\x1b[?1000l";

fn write_raw(bytes: &[u8]) -> io::Result<()> {
    let mut stdout = io::stdout();
    stdout.write_all(bytes)?;
    stdout.flush()
}

/// Owns the alternate screen for as long as it lives.
#[derive(Debug)]
pub struct TerminalGuard {
    mouse: bool,
    restored: bool,
}

impl TerminalGuard {
    /// Enter raw mode + alternate screen, hide the cursor and capture mouse
    /// clicks/drags/wheel (see [`MOUSE_ON`]).
    pub fn enter(no_mouse: bool) -> Result<Self> {
        terminal::enable_raw_mode()
            .map_err(|e| Error::Terminal(format!("enable raw mode: {e}")))?;
        let mut stdout = io::stdout();
        execute!(stdout, terminal::EnterAlternateScreen)
            .map_err(|e| Error::Terminal(format!("alternate screen: {e}")))?;
        if !no_mouse {
            write_raw(MOUSE_ON).map_err(|e| Error::Terminal(format!("mouse capture: {e}")))?;
        }
        execute!(stdout, cursor::Hide).map_err(|e| Error::Terminal(format!("hide cursor: {e}")))?;
        io::Write::flush(&mut stdout).map_err(|e| Error::Terminal(format!("flush: {e}")))?;
        TERMINAL_ACTIVE.store(true, Ordering::SeqCst);
        Ok(Self {
            mouse: !no_mouse,
            restored: false,
        })
    }

    /// Restore the terminal. Idempotent.
    pub fn restore(&mut self) {
        if self.restored {
            return;
        }
        self.restored = true;
        restore_terminal(self.mouse);
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Restore the terminal unconditionally (safe to call more than once).
pub fn restore_terminal(mouse: bool) {
    if mouse {
        let _ignore = write_raw(MOUSE_OFF);
    }
    let mut stdout = io::stdout();
    let _ignore = execute!(stdout, cursor::Show);
    let _ignore = execute!(stdout, terminal::LeaveAlternateScreen);
    let _ignore = terminal::disable_raw_mode();
    let _ignore = stdout.flush();
    TERMINAL_ACTIVE.store(false, Ordering::SeqCst);
}

/// Print `message` to stderr, safely (used after the terminal is restored).
pub fn note(message: &str) {
    eprintln!("{message}");
}

/// Install a panic hook that restores the terminal before forwarding to the
/// default hook, so a panic never strands the user in a broken terminal.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    let _store = ORIGINAL_PANIC.set(Box::new(move |info| default(info)));
    std::panic::set_hook(Box::new(move |info| {
        let active = TERMINAL_ACTIVE.swap(false, Ordering::SeqCst);
        if active {
            restore_terminal(true);
            note(&format!("music-tui panicked; log: {}\n", log_path()));
        }
        if let Some(previous) = ORIGINAL_PANIC.get() {
            previous(info);
        }
    }));
}

/// Install a `SIGINT` handler that asks the event loop to quit instead of dying
/// with the terminal still in raw mode.
///
/// In raw mode the terminal does not generate `SIGINT` from Ctrl-C (the app sees
/// a key event), so this only fires for `kill -INT` / `Ctrl-C` in shells that
/// still send the signal.
pub fn install_sigint_handler() {
    #[cfg(unix)]
    {
        unsafe {
            // SAFETY: the handler only stores a flag; it performs no allocation
            // and calls no non-async-signal-safe functions.
            libc::signal(libc::SIGINT, sigint as *const i32 as libc::sighandler_t);
        }
    }
}

#[cfg(unix)]
extern "C" fn sigint(_sig: i32) {
    QUIT_REQUESTED.store(true, Ordering::SeqCst);
}

/// `true` once a signal asked us to quit.
#[must_use]
pub fn quit_requested() -> bool {
    QUIT_REQUESTED.load(Ordering::SeqCst)
}

/// A `Write` handle for the ratatui backend.
#[must_use]
pub fn stdout() -> Stdout {
    io::stdout()
}

/// Current terminal size, defaulting to 80x24 when unavailable.
#[must_use]
pub fn terminal_size() -> (u16, u16) {
    terminal::size().unwrap_or((80, 24))
}

/// `true` when both stdout and stdin are interactive terminals.
#[must_use]
pub fn is_interactive() -> bool {
    use std::io::IsTerminal;
    io::stdout().is_terminal() && io::stdin().is_terminal()
}

/// Heuristic: does this terminal/Locale do Unicode box-drawing and arrows?
///
/// `--ascii` overrides this from the command line.
#[must_use]
pub fn supports_unicode() -> bool {
    let locale = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_CTYPE"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default()
        .to_ascii_lowercase();
    if locale.contains("utf-8") || locale.contains("utf8") {
        return true;
    }
    // `TERM=dumb`, `linux` (the VT) and `vt100`-style terminals do not.
    let term = std::env::var("TERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    !(term.is_empty()
        || term == "dumb"
        || term == "linux"
        || term.starts_with("vt100")
        || term.starts_with("ansi"))
}
