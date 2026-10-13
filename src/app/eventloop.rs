//! The event loop: 50 ms tick, non-blocking input, drain workers, draw once.
//!
//! Input and rendering are traits ([`InputSource`], [`Drawer`]) so the exact
//! loop the binary runs can be driven headlessly from tests with a
//! [`ScriptedInput`] and a [`CaptureDrawer`].

use std::collections::VecDeque;
use std::io::{Stdout, Write};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{self, Event, KeyEvent, MouseEvent};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::audio::PlayerHandle;
use crate::error::{Error, Result};
use crate::scan::ScanMessage;
use crate::ui::render;

use super::reducer::{msg_from_event, reduce, Effect, Msg};
use super::state::AppState;
use super::time::Clock;

/// How long the loop waits for input before ticking. Kept at 50 ms so a click
/// feels instant while an idle loop stays well under 1% CPU.
pub const TICK: Duration = Duration::from_millis(50);

/// One unit of input (or the absence of it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// Block this long, then behave like [`Input::Tick`]. Used by
    /// [`ScriptedInput`] so tests can let real time pass between messages.
    Wait(Duration),
    /// A key press.
    Key(KeyEvent),
    /// A mouse event.
    Mouse(MouseEvent),
    /// The terminal was resized.
    Resize {
        /// New width.
        width: u16,
        /// New height.
        height: u16,
    },
    /// Nothing happened within the timeout.
    Tick,
}

/// Where the loop reads input from.
pub trait InputSource {
    /// Wait up to `timeout` for the next [`Input`], returning
    /// [`Input::Tick`] on timeout.
    fn poll(&mut self, timeout: Duration) -> Input;
}

/// Real terminal input through crossterm.
#[derive(Debug, Default)]
pub struct CrosstermInput;

impl InputSource for CrosstermInput {
    fn poll(&mut self, timeout: Duration) -> Input {
        match event::poll(timeout) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) => Input::Key(key),
                Ok(Event::Mouse(mouse)) => Input::Mouse(mouse),
                Ok(Event::Resize(width, height)) => Input::Resize { width, height },
                Ok(_) => Input::Tick,
                Err(err) => {
                    tracing::debug!(%err, "event read failed");
                    Input::Tick
                }
            },
            Ok(false) => Input::Tick,
            Err(err) => {
                tracing::debug!(%err, "event poll failed");
                Input::Tick
            }
        }
    }
}

/// Scripted input for tests: replays a list then keeps ticking.
#[derive(Debug, Default)]
pub struct ScriptedInput {
    queue: VecDeque<Input>,
    /// Total number of `poll` calls made (useful in tests).
    pub polls: usize,
}

impl ScriptedInput {
    /// Build from any iterator of inputs.
    pub fn new<I: IntoIterator<Item = Input>>(events: I) -> Self {
        Self {
            queue: events.into_iter().collect(),
            polls: 0,
        }
    }

    /// Queue one more input while the loop runs.
    pub fn push(&mut self, input: Input) {
        self.queue.push_back(input);
    }

    /// `true` when nothing is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

impl InputSource for ScriptedInput {
    fn poll(&mut self, _timeout: Duration) -> Input {
        self.polls += 1;
        if self
            .queue
            .front()
            .is_some_and(|input| matches!(input, Input::Wait(_)))
        {
            if let Some(Input::Wait(duration)) = self.queue.pop_front() {
                std::thread::sleep(duration);
            }
            return Input::Tick;
        }
        self.queue.pop_front().unwrap_or(Input::Tick)
    }
}

/// What the loop draws into.
pub trait Drawer {
    /// Paint the current state.
    fn draw(&mut self, state: &AppState) -> Result<()>;
}

/// Draws to the real terminal.
pub struct TerminalDrawer {
    term: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalDrawer {
    /// Take over the screen (call after [`crate::terminal::TerminalGuard::enter`]).
    #[must_use]
    #[allow(clippy::expect_used, reason = "terminal init path")]
    pub fn new() -> Self {
        Self {
            term: Terminal::new(CrosstermBackend::new(crate::terminal::stdout()))
                .expect("could not initialise the terminal"),
        }
    }
}

impl Default for TerminalDrawer {
    fn default() -> Self {
        Self::new()
    }
}

impl Drawer for TerminalDrawer {
    fn draw(&mut self, state: &AppState) -> Result<()> {
        self.term
            .draw(|frame| render::draw(frame, state))
            .map(drop)
            .map_err(|err| Error::Terminal(format!("draw: {err}")))
    }
}

/// Drawer that keeps the last frame as text (used by headless tests).
#[derive(Debug, Default)]
pub struct CaptureDrawer {
    /// Number of frames drawn.
    pub frames: usize,
    /// Contents of the most recent frame.
    pub last_frame: Vec<String>,
    /// Size to render at.
    pub size: (u16, u16),
}

impl Drawer for CaptureDrawer {
    fn draw(&mut self, state: &AppState) -> Result<()> {
        self.frames += 1;
        self.last_frame = render::render_lines(state, self.size.0, self.size.1);
        Ok(())
    }
}

/// Run the main loop until a quit effect (or `max_iterations`).
///
/// Each iteration: drain scanner messages, drain player events, read one input
/// (50 ms timeout), apply the resulting effects, then draw once if anything
/// changed.
#[allow(clippy::too_many_arguments)]
pub fn run_loop(
    state: &mut AppState,
    player: &PlayerHandle,
    scan_rx: &Arc<Mutex<Receiver<ScanMessage>>>,
    input: &mut dyn InputSource,
    drawer: &mut dyn Drawer,
    clock: &dyn Clock,
    on_rescan: &mut dyn FnMut(bool),
    max_iterations: Option<usize>,
) -> Result<()> {
    let mut iterations = 0usize;
    let (width, height) = crate::terminal::terminal_size();
    // First paint must not wait for the scan (perf ticket).
    state.set_size(width, height);
    drawer.draw(state)?;
    state.dirty = false;

    loop {
        iterations += 1;
        if let Some(limit) = max_iterations {
            if iterations > limit {
                tracing::warn!("event loop hit its iteration limit; forcing quit");
                break;
            }
        }
        let now = clock.now();
        let mut effects: Vec<Effect> = Vec::new();

        // 1. scanner (bounded so a huge burst cannot delay a repaint)
        if let Ok(receiver) = scan_rx.lock() {
            for _ in 0..1024 {
                match receiver.try_recv() {
                    Ok(message) => effects.extend(reduce(state, Msg::Scan(message), now)),
                    Err(_) => break,
                }
            }
        }

        // 2. player thread
        for event in player.poll_events() {
            effects.extend(reduce(state, Msg::Player(event), now));
        }

        // 3. one input event (or a tick)
        match input.poll(TICK) {
            Input::Tick | Input::Wait(_) => effects.extend(reduce(state, Msg::Tick, now)),
            Input::Key(key) => effects.extend(reduce(state, Msg::Key(key), now)),
            Input::Mouse(mouse) => effects.extend(reduce(state, Msg::Mouse(mouse), now)),
            Input::Resize { width, height } => {
                effects.extend(reduce(state, Msg::Resized { width, height }, now))
            }
        }

        // 4. external quit request (SIGINT)
        if crate::terminal::quit_requested() {
            effects.extend(reduce(state, Msg::Quit, now));
        }

        for effect in effects {
            match effect {
                Effect::Command(command) => {
                    player.apply(&command);
                    state.dirty = true;
                }
                Effect::Rescan => {
                    // `rescan_force` is the `R` binding: this one scan ignores
                    // the library index and re-reads every tag.
                    let force = std::mem::take(&mut state.rescan_force);
                    on_rescan(force);
                }
                Effect::Quit => {
                    state.quit = true;
                }
            }
        }

        if state.quit {
            break;
        }
        if state.dirty {
            drawer.draw(state)?;
            state.dirty = false;
        }
        // Pacing: a real clock simply lets the input timeout pass; a virtual
        // clock moves by exactly one tick.
        clock.tick(TICK);
    }
    Ok(())
}

/// Convert a crossterm event into a message (re-exported for tests).
#[must_use]
pub fn event_to_msg(event: Event) -> Option<Msg> {
    msg_from_event(event)
}

/// Flush helper used by the terminal guard.
pub fn flush_stdout() {
    let _flush = std::io::stdout().flush();
}
