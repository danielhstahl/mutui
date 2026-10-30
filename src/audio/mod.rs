//! Playback: the command/event contract and the worker thread that owns output.
//!
//! Architecture (see `docs/adr/0001-stack-and-architecture.md` §4): the main
//! thread never touches audio. It sends [`PlayerCommand`]s to a worker thread
//! which owns the [`Backend`] and pushes [`PlayerEvent`]s back. The UI polls
//! [`PlayerHandle::poll_events`] once per tick, so it can never block on audio.

mod command_backend;
mod mock;
mod null;

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub use command_backend::{CommandBackend, KNOWN_PLAYERS, PlayerSpec, detect_backend};
pub use mock::{MockBackend, MockHandle};
pub use null::NullBackend;

/// Comma separated list of players the command backend can use.
#[must_use]
pub fn known_players_text() -> String {
    KNOWN_PLAYERS
        .iter()
        .map(|spec| spec.bin)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A request to start a track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayRequest {
    /// File to play.
    pub path: PathBuf,
    /// Application-assigned id, incremented for every play request so stale
    /// events can be recognised and dropped.
    pub track_id: u64,
    /// Known duration from the tag scan (used by backends without a decoder).
    pub duration: Option<Duration>,
}

/// Messages main thread -> player thread.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerCommand {
    /// Stop whatever is playing and start this track.
    Play(PlayRequest),
    /// Pause the current track (no position events while paused).
    Pause,
    /// Resume a paused track.
    Resume,
    /// Silence output and forget the current track (no `Finished` follows).
    Stop,
    /// Jump to an absolute position.
    Seek(Duration),
    /// Set the volume, `0.0..=1.0`.
    SetVolume(f32),
    /// Shut the worker down.
    Quit,
}

/// Messages player thread -> main thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerEvent {
    /// Playback of `track_id` started.
    Started {
        /// Id the play request was tagged with.
        track_id: u64,
    },
    /// Periodic position report (at most a few per second).
    Position {
        /// Id the play request was tagged with.
        track_id: u64,
        /// Elapsed playing time.
        position: Duration,
    },
    /// The track reached its end on its own.
    Finished {
        /// Id the play request was tagged with.
        track_id: u64,
    },
    /// The track could not be played, or stopped failing.
    Failed {
        /// Id the play request was tagged with.
        track_id: u64,
        /// Human readable reason (surfaced in the footer).
        reason: String,
    },
    /// The track the user was **holding paused** turned out to be over.
    ///
    /// `SIGSTOP` freezes a process but not the clock it measures itself against:
    /// a stopped decoder cannot exit, so a player whose remaining audio or
    /// device session died during the pause can only be reaped once `Resume`
    /// wakes it — by which point the UI has already been told the song is
    /// playing again. Reporting that as [`PlayerEvent::Finished`] is what makes
    /// the run walk on and the press of `Space` start the *next* song instead of
    /// the paused one (`mutu-1sg`, `mutu-nb3` and the report that survived both).
    EndedWhilePaused {
        /// Id the play request was tagged with.
        track_id: u64,
        /// Human readable reason (surfaced in the footer).
        reason: String,
    },
    /// A control request (pause/seek/volume) was refused by the backend. Does
    /// not stop playback; the UI shows the reason in the footer.
    Notice(String),
}

/// Everything a sound output needs to provide.
///
/// Implementations must be non-blocking: `poll` is called from the worker loop
/// and is the only way to report asynchronous events.
pub trait Backend: Send {
    /// Short name for logs and the footer.
    fn name(&self) -> &'static str;

    /// Start a track, stopping the current one first if needed.
    /// `Err(reason)` means "could not start" and is reported as
    /// [`PlayerEvent::Failed`].
    fn play(&mut self, req: &PlayRequest) -> Result<(), String>;

    /// Pause the current track.
    fn pause(&mut self) -> Result<(), String>;

    /// Resume the current track.
    fn resume(&mut self) -> Result<(), String>;

    /// Stop and silence. Must not produce [`PlayerEvent::Finished`].
    fn stop(&mut self);

    /// Seek to an absolute position.
    fn seek(&mut self, to: Duration) -> Result<(), String>;

    /// Set the volume (`0.0..=1.0`).
    fn set_volume(&mut self, volume: f32) -> Result<(), String>;

    /// Collect asynchronous events (end of track, failures, position).
    fn poll(&mut self) -> Vec<PlayerEvent>;
}

/// Boxed backend factory so the binary can pick one at runtime.
pub type BoxedBackend = Box<dyn Backend>;

/// Handle owned by the UI thread.
#[derive(Debug)]
pub struct PlayerHandle {
    tx: Sender<PlayerCommand>,
    rx: Arc<Mutex<Receiver<PlayerEvent>>>,
    worker: Arc<Mutex<Option<JoinHandle<()>>>>,
    backend_name: String,
    /// Warnings collected while creating the backend (shown in the footer).
    pub startup_warning: Option<String>,
}

impl PlayerHandle {
    /// Start a worker thread around `backend`.
    pub fn spawn(backend: BoxedBackend) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<PlayerCommand>();
        let (ev_tx, ev_rx) = mpsc::channel::<PlayerEvent>();
        let name = backend.name().to_string();
        let handle = thread::spawn(move || worker(backend, cmd_rx, ev_tx));
        Self {
            tx: cmd_tx,
            rx: Arc::new(Mutex::new(ev_rx)),
            worker: Arc::new(Mutex::new(Some(handle))),
            backend_name: name,
            startup_warning: None,
        }
    }

    /// Name of the active backend.
    #[must_use]
    pub fn backend_name(&self) -> &str {
        &self.backend_name
    }

    fn send(&self, cmd: PlayerCommand) {
        let _drop = self.tx.send(cmd);
    }

    /// Ask the worker to play a track.
    pub fn play(&self, path: PathBuf, track_id: u64, duration: Option<Duration>) {
        self.send(PlayerCommand::Play(PlayRequest {
            path,
            track_id,
            duration,
        }));
    }

    /// Pause.
    pub fn pause(&self) {
        self.send(PlayerCommand::Pause);
    }

    /// Resume.
    pub fn resume(&self) {
        self.send(PlayerCommand::Resume);
    }

    /// Stop and clear.
    pub fn stop(&self) {
        self.send(PlayerCommand::Stop);
    }

    /// Seek.
    pub fn seek(&self, to: Duration) {
        self.send(PlayerCommand::Seek(to));
    }

    /// Set volume (clamped by the backend).
    pub fn set_volume(&self, volume: f32) {
        self.send(PlayerCommand::SetVolume(volume));
    }

    /// Drain pending events (called once per tick).
    pub fn poll_events(&self) -> Vec<PlayerEvent> {
        let mut out = Vec::new();
        let Ok(rx) = self.rx.lock() else {
            return out;
        };
        while let Ok(event) = rx.try_recv() {
            out.push(event);
        }
        out
    }

    /// Tell the worker to shut down and wait briefly for it.
    pub fn shutdown(&self) {
        self.send(PlayerCommand::Quit);
        let worker = self.worker.lock().ok().and_then(|mut slot| slot.take());
        if let Some(handle) = worker {
            let _joined = handle.join();
        }
    }

    /// Deliver a command produced by the reducer.
    pub fn apply(&self, command: &PlayerCommand) {
        self.send(command.clone());
    }
}

fn worker(mut backend: BoxedBackend, cmd_rx: Receiver<PlayerCommand>, ev_tx: Sender<PlayerEvent>) {
    // Everything in here runs on the audio thread and may only send events — it
    // must never touch application state (see ADR §4).
    let forward = |events: Vec<PlayerEvent>| {
        for event in events {
            let _drop = ev_tx.send(event);
        }
    };

    loop {
        let command: Option<PlayerCommand> = match cmd_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(command) => Some(command),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let failure = match command {
            Some(PlayerCommand::Play(req)) => backend
                .play(&req)
                .err()
                .map(|reason| (req.track_id, reason)),
            Some(PlayerCommand::Pause) => {
                if let Err(reason) = backend.pause() {
                    let _drop = ev_tx.send(PlayerEvent::Notice(reason));
                }
                None
            }
            Some(PlayerCommand::Resume) => {
                if let Err(reason) = backend.resume() {
                    let _drop = ev_tx.send(PlayerEvent::Notice(reason));
                }
                None
            }
            Some(PlayerCommand::Stop) => {
                backend.stop();
                None
            }
            Some(PlayerCommand::Seek(to)) => {
                if let Err(reason) = backend.seek(to) {
                    let _drop = ev_tx.send(PlayerEvent::Notice(reason));
                }
                None
            }
            Some(PlayerCommand::SetVolume(v)) => {
                if let Err(reason) = backend.set_volume(v) {
                    let _drop = ev_tx.send(PlayerEvent::Notice(reason));
                }
                None
            }
            Some(PlayerCommand::Quit) => break,
            None => None,
        };
        if let Some((track_id, reason)) = failure {
            let _drop = ev_tx.send(PlayerEvent::Failed { track_id, reason });
        }
        forward(backend.poll());
    }
    backend.stop();
    forward(backend.poll());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Counting {
        played: Arc<AtomicUsize>,
    }
    impl Backend for Counting {
        fn name(&self) -> &'static str {
            "counting"
        }
        fn play(&mut self, _req: &PlayRequest) -> Result<(), String> {
            self.played.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        fn pause(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn resume(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn stop(&mut self) {}
        fn seek(&mut self, _to: Duration) -> Result<(), String> {
            Ok(())
        }
        fn set_volume(&mut self, _v: f32) -> Result<(), String> {
            Ok(())
        }
        fn poll(&mut self) -> Vec<PlayerEvent> {
            Vec::new()
        }
    }

    #[test]
    fn handle_sends_commands_to_backend() {
        let played = Arc::new(AtomicUsize::new(0));
        let handle = PlayerHandle::spawn(Box::new(Counting {
            played: Arc::clone(&played),
        }));
        handle.play(PathBuf::from("/x.mp3"), 1, None);
        // give the worker a moment to consume
        for _ in 0..200 {
            if played.load(Ordering::Relaxed) > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(played.load(Ordering::Relaxed), 1);
        handle.shutdown();
    }
}
