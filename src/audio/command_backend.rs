//! Backend that plays through an external decoder (`mpv`, `ffplay`, `mpg123`,
//! `afplay`, …).
//!
//! Why not an in-process decoder? `cpal`/`rodio` need ALSA development headers
//! at *build* time, which breaks `cargo build --release` on minimal hosts and
//! containers. See `docs/adr/0001-stack-and-architecture.md` §3.
//!
//! The child process is spawned with stdin/stdout/stderr attached to `/dev/null`
//! so nothing can ever corrupt the alternate screen. End of track is detected by
//! reaping the child.
//!
//! Pausing is per-player, because "pause a process" does not mean "pause its
//! audio" ([`PauseMode`]): `SIGSTOP` freezes the *process*, so anything already
//! handed to the audio server keeps playing without it and its session may be
//! gone by the time it is woken. Players that talk to such a server (`afplay`,
//! a CoreAudio client) are paused by killing them and relaunched on resume.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::{Backend, PlayRequest, PlayerEvent};

/// Emit a position event at most this often (a few times per second, never per
/// audio frame — the UI only needs a readable clock).
pub const POSITION_INTERVAL: Duration = Duration::from_millis(250);

/// How long a woken decoder has to prove it is audible again.
///
/// A position event is *not* that proof: the clock the backend reports is the
/// application's own wall clock, so it ticks whether or not the decoder makes a
/// sound. A decoder that its pause left with nothing to play (a CoreAudio queue
/// that died behind the stop) typically lives for a few hundred milliseconds
/// after `SIGCONT` before it exits, which is long enough to emit a position and
/// far too short for anyone to have heard the song. An end inside this window is
/// an end of the pause, not an end of the track.
pub const RESUME_GRACE: Duration = Duration::from_millis(750);

/// How a player accepts a volume setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeArg {
    /// No volume switch: volume changes are reported as unsupported.
    None,
    /// Switch expects `0.0..=1.0` (mpg123, afplay).
    Scaled,
    /// Switch expects `0..=100` (mpv, ffplay, mplayer).
    Percent,
}

/// How a player accepts a start position (used for seek and for re-launching at
/// the current position after a volume change).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekArg {
    /// No start-position switch.
    None,
    /// Switch expects fractional seconds.
    Seconds,
}

/// How a player is paused.
///
/// `SIGSTOP` freezes a *process*. It does not freeze the audio that process has
/// already handed to an audio server, and it does not keep the server's side of
/// the session alive, which is why one kind of pause cannot fit every player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseMode {
    /// `SIGSTOP` / `SIGCONT`: the process keeps its own position, so this is the
    /// only mode that resumes mid-sample. Right for players that keep their
    /// output inside their own buffer (`mpv`, `ffplay`, `mplayer`).
    Signal,
    /// Kill the decoder on pause and relaunch it on resume. Required for a
    /// player whose samples already left the process: `afplay` queues samples to
    /// `coreaudiod`, so a stopped `afplay` still emits the queued audio in a
    /// short blip, and the queue it was reading from is usually finished or
    /// dropped when `SIGCONT` wakes it — whereupon it exits, which the
    /// application can only read as "that song is over". Killing means the
    /// pause is silent and the resume is a decoder that demonstrably works.
    Kill,
}

/// A known external player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerSpec {
    /// Binary name looked up in `PATH`.
    pub bin: &'static str,
    /// Human name shown in the footer.
    pub label: &'static str,
    /// Volume switch style.
    pub volume: VolumeArg,
    /// Start-position switch style.
    pub seek: SeekArg,
    /// How `pause`/`resume` are implemented for this player.
    pub pause: PauseMode,
}

/// Known players, in preference order.
pub const KNOWN_PLAYERS: &[PlayerSpec] = &[
    PlayerSpec {
        bin: "mpv",
        label: "mpv",
        volume: VolumeArg::Percent,
        seek: SeekArg::Seconds,
        pause: PauseMode::Signal,
    },
    PlayerSpec {
        bin: "ffplay",
        label: "ffplay",
        volume: VolumeArg::Percent,
        seek: SeekArg::Seconds,
        pause: PauseMode::Signal,
    },
    PlayerSpec {
        bin: "mpg123",
        label: "mpg123",
        volume: VolumeArg::Scaled,
        seek: SeekArg::None,
        pause: PauseMode::Signal,
    },
    PlayerSpec {
        bin: "mpg321",
        label: "mpg321",
        volume: VolumeArg::Percent,
        seek: SeekArg::None,
        pause: PauseMode::Signal,
    },
    PlayerSpec {
        bin: "madplay",
        label: "madplay",
        volume: VolumeArg::None,
        seek: SeekArg::Seconds,
        pause: PauseMode::Signal,
    },
    // macOS ships `afplay`, and it is a bad citizen: its only switches are
    // `-v` (volume), `-t` (play *for* N seconds), `-r` (rate), `-q` and `-d`.
    // There is **no** start-position switch, so a resume cannot restart
    // mid-song, and its samples sit in `coreaudiod`, where a `SIGSTOP` cannot
    // reach them — hence `PauseMode::Kill`. Install `mpv` or `ffplay` for a
    // pause that resumes where it stopped.
    PlayerSpec {
        bin: "afplay",
        label: "afplay",
        volume: VolumeArg::Scaled,
        seek: SeekArg::None,
        pause: PauseMode::Kill,
    },
    PlayerSpec {
        bin: "mplayer",
        label: "mplayer",
        volume: VolumeArg::Percent,
        seek: SeekArg::Seconds,
        pause: PauseMode::Signal,
    },
];

/// Locate `bin` on `PATH`.
#[must_use]
pub fn which(bin: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{bin}.exe")
    } else {
        bin.to_string()
    };
    std::env::split_paths(std::env::var_os("PATH").as_deref().unwrap_or_default())
        .map(|dir| dir.join(&exe))
        .find(|candidate| candidate.is_file())
}

/// First known player found on this machine.
#[must_use]
pub fn detect_backend() -> Option<PlayerSpec> {
    KNOWN_PLAYERS
        .iter()
        .find(|spec| which(spec.bin).is_some())
        .cloned()
}

impl PlayerSpec {
    /// Build the argv (without the program name) for one playback.
    #[must_use]
    pub fn args(&self, path: &Path, volume: f32, start: Option<Duration>) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        match self.bin {
            "mpv" => {
                args.extend(["--no-video".into(), "--really-quiet".into()]);
                if self.volume != VolumeArg::None {
                    args.push(format!("--volume={}", (volume * 100.0).round() as i32));
                }
                if let (SeekArg::Seconds, Some(secs)) = (self.seek, start.map(|s| s.as_secs_f64()))
                    && secs > 0.0
                {
                    args.push(format!("--start={secs:.3}"));
                }
            }
            "ffplay" => {
                args.extend([
                    "-nodisp".into(),
                    "-loglevel".into(),
                    "error".into(),
                    "-autoexit".into(),
                ]);
                if self.volume != VolumeArg::None {
                    args.extend(["-volume".into(), (volume * 100.0).round().to_string()]);
                }
                if let (SeekArg::Seconds, Some(secs)) = (self.seek, start.map(|s| s.as_secs_f64()))
                    && secs > 0.0
                {
                    args.extend(["-ss".into(), format!("{secs:.3}")]);
                }
            }
            "mplayer" => {
                args.extend(["-nocache".into(), "-really-quiet".into()]);
                if self.volume != VolumeArg::None {
                    args.extend(["-volume".into(), (volume * 100.0).round().to_string()]);
                }
                if let (SeekArg::Seconds, Some(secs)) = (self.seek, start.map(|s| s.as_secs_f64()))
                    && secs > 0.0
                {
                    args.extend(["-ss".into(), format!("{secs:.3}")]);
                }
            }
            "mpg123" => {
                args.push("-q".into());
                if self.volume != VolumeArg::None {
                    args.extend(["--volume".into(), format!("{:.3}", volume.clamp(0.0, 1.0))]);
                }
            }
            "mpg321" => {
                args.extend(["-q".into()]);
                if self.volume != VolumeArg::None {
                    args.extend([
                        "-g".into(),
                        (volume * 100.0).round().clamp(0.0, 100.0).to_string(),
                    ]);
                }
            }
            "madplay" => {
                args.push("-q".into());
                if let (SeekArg::Seconds, Some(secs)) = (self.seek, start.map(|s| s.as_secs_f64()))
                    && secs > 0.0
                {
                    args.push(format!("--resume={secs:.3}"));
                }
            }
            "afplay" => {
                // No start-position switch exists (`-t` is a *stop* time, not a
                // seek), and an option `afplay` does not know makes it print its
                // usage and exit non-zero — so nothing is ever passed here but
                // the volume.
                let _ = start;
                if self.volume != VolumeArg::None {
                    args.extend(["-v".into(), format!("{:.3}", volume.clamp(0.0, 1.0))]);
                }
            }
            _ => {}
        }
        args.push(path.display().to_string());
        args
    }
}

/// Backend implemented by an external player process.
pub struct CommandBackend {
    spec: PlayerSpec,
    child: Option<Child>,
    track_id: u64,
    path: Option<PathBuf>,
    /// Clock anchor minus the position the track started at.
    anchor: Option<Instant>,
    start_offset: Duration,
    paused_at: Option<Instant>,
    paused_total: Duration,
    last_position: Duration,
    /// Length from the tags, used to tell a real end of file from a decoder that
    /// died the moment it was woken.
    duration: Option<Duration>,
    /// `true` from a successful `pause` until the track proves it is audible
    /// again (one position event after the pause). Everything that ends a track
    /// while this is set ended *during the user's pause*, not in front of them.
    held_paused: bool,
    /// When a `SIGCONT` last woke a stopped decoder. See [`RESUME_GRACE`].
    woken_at: Option<Instant>,
    volume: f32,
    pending: Vec<PlayerEvent>,
}

impl CommandBackend {
    /// Wrap a discovered player.
    #[must_use]
    pub fn new(spec: PlayerSpec) -> Self {
        Self {
            spec,
            child: None,
            track_id: 0,
            path: None,
            anchor: None,
            start_offset: Duration::ZERO,
            paused_at: None,
            paused_total: Duration::ZERO,
            last_position: Duration::ZERO,
            duration: None,
            held_paused: false,
            woken_at: None,
            volume: 1.0,
            pending: Vec::new(),
        }
    }

    /// The spec in use.
    #[must_use]
    pub fn spec(&self) -> &PlayerSpec {
        &self.spec
    }

    fn elapsed(&self) -> Duration {
        let Some(anchor) = self.anchor else {
            return Duration::ZERO;
        };
        let raw = if let Some(paused_at) = self.paused_at {
            paused_at.duration_since(anchor)
        } else {
            anchor.elapsed()
        };
        self.start_offset + raw.saturating_sub(self.paused_total)
    }

    /// Reap the child if it has exited and freeze the clocks, returning its
    /// exit status.
    ///
    /// A child stopped by `SIGSTOP` **cannot** exit, so this never reports a song
    /// that died during a pause — it reports the ones that were already gone
    /// (or already zombies) before the stop landed.
    fn reap_if_exited(&mut self) -> Option<std::process::ExitStatus> {
        let status = match self.child.as_mut().and_then(|child| child.try_wait().ok()) {
            Some(Some(status)) => status,
            // Still running, or `waitpid` said something odd: leave the clocks
            // alone.
            _ => return None,
        };
        // Already reaped by `try_wait`, so dropping the handle leaves no zombie.
        self.child = None;
        self.anchor = None;
        self.paused_at = None;
        self.paused_total = Duration::ZERO;
        Some(status)
    }

    /// `true` when the tag length says the track was genuinely at its end.
    ///
    /// Deliberately unforgiving (no tolerance): a guessed head start is far
    /// cheaper than losing a song the user was about to hear again, and the
    /// position clock is only as good as the wall clock it stands on.
    fn at_track_end(&self, position: Duration) -> bool {
        self.duration.is_some_and(|length| position >= length)
    }

    /// `true` when the track's end has to be filed under the pause.
    fn ends_the_paused_song(&self) -> bool {
        self.held_paused
            || self
                .woken_at
                .is_some_and(|woken| woken.elapsed() < RESUME_GRACE)
    }

    /// Turn a child's exit status into the event the UI needs.
    ///
    /// The distinction matters: a track that ends while `held_paused` is set was
    /// not heard by anyone. Calling that `Finished` advances the run, so the one
    /// key press the user gave that song — `Space` — starts the *next* one
    /// instead (the report that `mutu-1sg` and `mutu-nb3` between them did not
    /// cover: the decoder can outlive its own pause, because `SIGSTOP` stops the
    /// process and not the clock it counts against).
    fn exit_event(&self, status: std::process::ExitStatus, position: Duration) -> PlayerEvent {
        let track_id = self.track_id;
        let code = status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string());
        if self.ends_the_paused_song() && !self.at_track_end(position) {
            let reason = if status.success() {
                format!("{} had already reached its end", self.spec.label)
            } else {
                format!("{} died when it was woken (exit {code})", self.spec.label)
            };
            return PlayerEvent::EndedWhilePaused { track_id, reason };
        }
        if status.success() {
            PlayerEvent::Finished { track_id }
        } else {
            PlayerEvent::Failed {
                track_id,
                reason: format!(
                    "{} exited with {code} (unsupported or unreadable file)",
                    self.spec.label
                ),
            }
        }
    }

    fn signal(&self, signal: i32) -> Result<(), String> {
        let Some(child) = self.child.as_ref() else {
            return Err("nothing is playing".to_string());
        };
        #[cfg(unix)]
        {
            let rc = unsafe { libc::kill(child.id() as i32, signal) };
            if rc == 0 {
                Ok(())
            } else {
                Err(format!(
                    "could not signal the player ({})",
                    std::io::Error::last_os_error()
                ))
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (child, signal);
            Err("pause is not supported on this platform".to_string())
        }
    }

    /// Stop the child without touching the position clock.
    fn kill_child_process(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _kill = child.kill();
            let _wait = child.wait();
        }
    }

    /// Reap the decoder process and freeze the position clock.
    ///
    /// `path` is deliberately *not* cleared: `seek` and `set_volume` are
    /// implemented by relaunching the same file (see [`Self::respawn`]), and
    /// forgetting the path here made those commands kill the track instead of
    /// restarting it. `stop` clears it explicitly.
    fn kill_child(&mut self) {
        self.kill_child_process();
        self.anchor = None;
        self.paused_at = None;
        self.paused_total = Duration::ZERO;
        self.held_paused = false;
        self.woken_at = None;
    }

    /// Silence a [`PauseMode::Kill`] player at the current position.
    ///
    /// The clocks are re-anchored rather than cleared, so `elapsed()` still
    /// reads the instant the user paused: that is the position a resume
    /// relaunches from, and the number the footer keeps showing while paused.
    fn kill_for_pause(&mut self) {
        let position = self.elapsed();
        self.kill_child_process();
        self.anchor = Some(Instant::now());
        self.start_offset = position;
        self.paused_total = Duration::ZERO;
        self.paused_at = Some(Instant::now());
        self.last_position = position;
        self.woken_at = None;
    }

    /// Relaunch the current track at `start`.
    ///
    /// Players whose volume and start position are spawn-time switches (mpv,
    /// ffplay, mplayer) have to be restarted for `seek`/`set_volume` to take
    /// effect. A pause is re-applied to the fresh child: the screen still says
    /// `Paused`, and a respawn that left the decoder running would resume the
    /// song behind the user's back — after which the run advances on its own a
    /// few minutes later and `Space` no longer does what it says (mutu-1sg).
    /// For a [`PauseMode::Kill`] player the relaunch is the pause: `pause`
    /// simply kills the child it just started, so nothing is audible.
    fn respawn(&mut self, start: Duration) -> Result<(), String> {
        let was_paused = self.paused_at.take().is_some();
        self.kill_child();
        self.spawn_at(Some(start))?;
        if was_paused {
            self.pause()?;
        }
        Ok(())
    }

    /// Bring a [`PauseMode::Kill`] track back: launch a fresh decoder.
    ///
    /// A player with a start-position switch comes back at the position the
    /// pause froze; one without (afplay) can only come back at the beginning,
    /// which is said out loud rather than passed off as a resume.
    fn relaunch_after_kill_pause(&mut self) -> Result<(), String> {
        let position = self.elapsed();
        let from_the_top = self.spec.seek == SeekArg::None;
        let start = if from_the_top {
            Duration::ZERO
        } else {
            position
        };
        self.paused_at = None;
        self.paused_total = Duration::ZERO;
        self.held_paused = false;
        self.woken_at = None;
        self.anchor = None;
        if let Err(reason) = self.spawn_at(Some(start)) {
            self.pending.push(PlayerEvent::Failed {
                track_id: self.track_id,
                reason,
            });
            return Ok(());
        }
        if from_the_top {
            self.pending.push(PlayerEvent::Notice(format!(
                "{} has no mid-song resume: it starts again from the beginning",
                self.spec.label
            )));
        }
        // The UI clock has to be told too: it was holding the paused position.
        self.pending.push(PlayerEvent::Position {
            track_id: self.track_id,
            position: start,
        });
        tracing::debug!(
            bin = self.spec.bin,
            ?start,
            from_the_top,
            "relaunched the decoder after a kill-pause"
        );
        Ok(())
    }

    fn spawn_at(&mut self, start: Option<Duration>) -> Result<(), String> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| "no track selected".to_string())?;
        let args = self.spec.args(&path, self.volume, start);
        let mut command = Command::new(self.spec.bin);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match spawn_child(&mut command) {
            Ok(child) => {
                self.child = Some(child);
                self.anchor = Some(Instant::now());
                self.start_offset = start.unwrap_or(Duration::ZERO);
                self.paused_total = Duration::ZERO;
                self.paused_at = None;
                self.woken_at = None;
                self.last_position = self.start_offset;
                tracing::debug!(bin = self.spec.bin, ?args, "spawned player");
                Ok(())
            }
            Err(err) => Err(format!("{} could not be started: {err}", self.spec.bin)),
        }
    }
}

impl Backend for CommandBackend {
    fn name(&self) -> &'static str {
        self.spec.label
    }

    fn play(&mut self, req: &PlayRequest) -> Result<(), String> {
        // Rapid clicks must leave exactly one audible track: always silence the
        // previous one first.
        self.kill_child();
        self.pending.clear();
        // A play that cannot start must not leave the previous track's path
        // behind for `seek`/`set_volume` to resurrect.
        self.path = None;
        self.last_position = Duration::ZERO;
        if !req.path.is_file() {
            return Err(format!("file is gone: {}", req.path.display()));
        }
        self.track_id = req.track_id;
        self.duration = req.duration;
        self.path = Some(req.path.clone());
        self.spawn_at(None)?;
        self.pending.push(PlayerEvent::Started {
            track_id: req.track_id,
        });
        Ok(())
    }

    fn pause(&mut self) -> Result<(), String> {
        if self.paused_at.is_some() {
            return Ok(());
        }
        if self.child.is_none() {
            return Err("nothing is playing".to_string());
        }
        match self.spec.pause {
            PauseMode::Signal => {
                self.signal(libc_stop())?;
                self.paused_at = Some(Instant::now());
                self.held_paused = true;
            }
            PauseMode::Kill => {
                // Nothing may be left running: a stopped player keeps emitting
                // whatever the audio server still holds, seconds after the
                // screen said `Paused`.
                self.kill_for_pause();
                self.held_paused = true;
            }
        }
        Ok(())
    }

    fn resume(&mut self) -> Result<(), String> {
        let Some(paused_at) = self.paused_at else {
            return Err("playback is not paused".to_string());
        };
        if self.spec.pause == PauseMode::Kill {
            return self.relaunch_after_kill_pause();
        }
        if self.child.is_none() {
            // The decoder went away while paused; there is nothing to wake up.
            // The press of `Space` still asked for *this* song, so what it gets
            // is the end of that song, not a silent "playing".
            let event = PlayerEvent::EndedWhilePaused {
                track_id: self.track_id,
                reason: "the player had already ended".to_string(),
            };
            self.paused_at = None;
            self.held_paused = false;
            self.pending.push(event);
            return Ok(());
        }
        // Look before signalling: `kill()` on a process that has already exited
        // succeeds, so a decoder that was gone before the stop landed (or that
        // the stop never reached) would otherwise be reported as resumed while
        // the song is over. Its end belongs to the pause, not to the user.
        let position = self.elapsed();
        if let Some(status) = self.reap_if_exited() {
            let event = self.exit_event(status, position);
            self.path = None;
            self.held_paused = false;
            self.pending.push(event);
            return Ok(());
        }
        self.signal(libc_cont())?;
        self.paused_at = None;
        self.paused_total += paused_at.elapsed();
        self.woken_at = Some(Instant::now());
        Ok(())
    }

    fn stop(&mut self) {
        self.kill_child();
        self.pending.clear();
        self.track_id = 0;
        self.path = None;
        self.last_position = Duration::ZERO;
    }

    fn seek(&mut self, to: Duration) -> Result<(), String> {
        if self.spec.seek == SeekArg::None {
            return Err(format!("{} cannot seek", self.spec.label));
        }
        if self.path.is_none() {
            return Err("nothing is playing".to_string());
        }
        self.pending.clear();
        self.respawn(to)
    }

    fn set_volume(&mut self, volume: f32) -> Result<(), String> {
        self.volume = volume.clamp(0.0, 1.0);
        if self.spec.volume == VolumeArg::None {
            return Err(format!("{} has no volume control", self.spec.label));
        }
        if self.path.is_none() {
            // Applies to the next track.
            return Ok(());
        }
        if self.spec.seek == SeekArg::None {
            // Volume is a spawn-time switch: without a seek switch we can only
            // apply it to the next track.
            return Ok(());
        }
        let position = self.elapsed();
        self.pending.clear();
        self.respawn(position)
    }

    fn poll(&mut self) -> Vec<PlayerEvent> {
        let position = self.elapsed();
        if self
            .woken_at
            .is_some_and(|woken| woken.elapsed() >= RESUME_GRACE)
        {
            // Past the window in which an end still belongs to the pause.
            self.woken_at = None;
        }
        if let Some(status) = self.reap_if_exited() {
            let event = self.exit_event(status, position);
            self.path = None;
            self.held_paused = false;
            self.pending.push(event);
        }
        if self.child.is_some() && self.paused_at.is_none() {
            let position = self.elapsed();
            if position.saturating_sub(self.last_position) >= POSITION_INTERVAL {
                self.last_position = position;
                // Proof that the song is audible again: from here on an end is
                // an end the user actually heard.
                self.held_paused = false;
                self.pending.push(PlayerEvent::Position {
                    track_id: self.track_id,
                    position,
                });
            }
        }
        std::mem::take(&mut self.pending)
    }
}

impl Drop for CommandBackend {
    fn drop(&mut self) {
        // Never leave a player running after our process is done with it
        // (`std::process::Command` has no `kill_on_drop`).
        self.kill_child();
    }
}

/// Spawn, retrying the momentary `ETXTBSY` ("Text file busy") that `execve`
/// can return while the kernel is still releasing the binary image of the
/// process that was just killed.
///
/// `seek` and `set_volume` are implemented by killing the decoder and launching
/// the same program again, so without this a volume key can fail with a
/// "player could not be started" message and leave the track silent.
#[cfg(unix)]
fn spawn_child(command: &mut Command) -> std::io::Result<Child> {
    /// Attempts before giving up; ~100 ms in total, on the audio worker thread.
    const ATTEMPTS: u32 = 20;
    /// How long to wait between attempts.
    const WAIT: Duration = Duration::from_millis(5);
    let mut last: Option<std::io::Error> = None;
    for _ in 0..ATTEMPTS {
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(err) if err.raw_os_error() == Some(libc::ETXTBSY) => {
                std::thread::sleep(WAIT);
                last = Some(err);
            }
            Err(err) => return Err(err),
        }
    }
    Err(last.unwrap_or_else(std::io::Error::last_os_error))
}

/// Spawn, retrying the momentary `ETXTBSY` see [`spawn_child`] (no-op elsewhere).
#[cfg(not(unix))]
fn spawn_child(command: &mut Command) -> std::io::Result<Child> {
    command.spawn()
}

fn libc_stop() -> i32 {
    #[cfg(unix)]
    {
        libc::SIGSTOP
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn libc_cont() -> i32 {
    #[cfg(unix)]
    {
        libc::SIGCONT
    }
    #[cfg(not(unix))]
    {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A decoder stand-in: a real external process that runs for a long time,
    /// ignores the argv the backend builds, and can be inspected from the test.
    /// It exists so the pause/volume/seek behaviour of the *process* can be
    /// asserted, which is what the pause bug in `mutu-1sg` is about.
    #[cfg(unix)]
    struct FakePlayer {
        spec: PlayerSpec,
        track: PathBuf,
        dir: PathBuf,
    }

    #[cfg(unix)]
    impl FakePlayer {
        fn new(name: &str) -> Self {
            Self::with_script(name, "#!/bin/sh\nexec sleep 300\n")
        }

        /// A decoder that runs `script` instead of the long `sleep`, so a test
        /// can stage what the *player* does (end, die, survive a stop).
        fn with_script(name: &str, script: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!("mutu-cmd-backend-{name}"));
            let _remove = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp dir");
            let script_path = dir.join("fakedecoder");
            // `exec` so that the child *is* `sleep`: reaping it leaves nothing
            // behind, and a stopped `sleep` really is a stopped process.
            std::fs::write(&script_path, script).expect("script");
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
            let track = dir.join("song.mp3");
            std::fs::write(&track, b"the decoder never reads this").expect("fixture");
            // Same switches as mpv: volume and start position are spawn-time
            // arguments, so both are implemented by relaunching the child.
            let mpv = KNOWN_PLAYERS[0].clone();
            let bin = Box::leak(script_path.to_string_lossy().into_owned().into_boxed_str());
            Self {
                spec: PlayerSpec {
                    bin,
                    label: "fakedecoder",
                    volume: mpv.volume,
                    seek: mpv.seek,
                    pause: PauseMode::Signal,
                },
                track,
                dir,
            }
        }

        fn backend(&self) -> CommandBackend {
            CommandBackend::new(self.spec.clone())
        }

        /// Same fake decoder, but paused/seeked like another known player, so
        /// the CoreAudio situation (`afplay`) can be staged on any unix box.
        fn backend_with(&self, pause: PauseMode, seek: SeekArg) -> CommandBackend {
            CommandBackend::new(PlayerSpec {
                pause,
                seek,
                ..self.spec.clone()
            })
        }

        fn req(&self, track_id: u64) -> PlayRequest {
            PlayRequest {
                path: self.track.clone(),
                track_id,
                duration: Some(Duration::from_secs(300)),
            }
        }

        fn cleanup(&self) {
            let _remove = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// The child's process-state letter (`T` = stopped by a signal), when this
    /// system exposes it.
    #[cfg(unix)]
    fn child_state(backend: &CommandBackend) -> Option<char> {
        let pid = backend.child.as_ref()?.id();
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // The comm field is parenthesised and may itself contain spaces, so
        // everything after the last `)` is what we want.
        let after_comm = stat.rfind(')')? + 1;
        stat[after_comm..].split_whitespace().next()?.chars().next()
    }

    /// Wait for the child's process state to become `want`. Delivering a signal
    /// and the kernel updating `/proc` are not synchronous, so a test must poll
    /// for the state it caused rather than reading it once.
    #[cfg(unix)]
    fn child_state_is(backend: &CommandBackend, want: char) -> bool {
        for _ in 0..200 {
            if child_state(backend) == Some(want) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    /// `true` once the child is no longer stopped by a signal.
    #[cfg(unix)]
    fn child_is_running(backend: &CommandBackend) -> bool {
        for _ in 0..200 {
            if child_state(backend) != Some('T') {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    /// Collect everything the backend reports while it runs for `ms`.
    #[cfg(unix)]
    fn pump(backend: &mut CommandBackend, ms: u64) -> Vec<PlayerEvent> {
        let deadline = Instant::now() + Duration::from_millis(ms);
        let mut out = Vec::new();
        while Instant::now() < deadline {
            out.extend(backend.poll());
            std::thread::sleep(Duration::from_millis(20));
        }
        out.extend(backend.poll());
        out
    }

    #[cfg(unix)]
    fn positions(events: &[PlayerEvent]) -> Vec<Duration> {
        events
            .iter()
            .filter_map(|event| match event {
                PlayerEvent::Position { position, .. } => Some(*position),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn argv_carries_volume_and_start() {
        let mpv = KNOWN_PLAYERS[0].clone();
        let args = mpv.args(
            Path::new("/m/a b.mp3"),
            0.5,
            Some(Duration::from_millis(1500)),
        );
        assert!(args.iter().any(|a| a == "--volume=50"), "{args:?}");
        assert!(args.iter().any(|a| a == "--start=1.500"), "{args:?}");
        assert_eq!(args.last().map(String::as_str), Some("/m/a b.mp3"));

        let ff = &KNOWN_PLAYERS[1];
        let args = ff.args(Path::new("/m/a.mp3"), 1.0, None);
        assert!(args.contains(&"-autoexit".to_string()));
        assert!(!args.iter().any(|a| a == "-ss"), "no seek when start=None");
    }

    #[test]
    fn missing_file_fails_instead_of_hanging() {
        let spec = PlayerSpec {
            bin: "definitely-not-a-player",
            label: "ghost",
            volume: VolumeArg::None,
            seek: SeekArg::None,
            pause: PauseMode::Signal,
        };
        let mut be = CommandBackend::new(spec);
        let err = be
            .play(&PlayRequest {
                path: PathBuf::from("/nope/nothing.mp3"),
                track_id: 3,
                duration: None,
            })
            .expect_err("missing file must fail");
        assert!(err.contains("gone"), "{err}");
    }

    #[test]
    fn unstartable_player_reports_failure() {
        let spec = PlayerSpec {
            bin: "definitely-not-a-player",
            label: "ghost",
            volume: VolumeArg::None,
            seek: SeekArg::None,
            pause: PauseMode::Signal,
        };
        let mut be = CommandBackend::new(spec);
        let path = std::env::temp_dir().join("mutu-cmd-test.txt");
        std::fs::write(&path, b"x").expect("fixture");
        let result = be.play(&PlayRequest {
            path: path.clone(),
            track_id: 9,
            duration: None,
        });
        let _remove = std::fs::remove_file(&path);
        assert!(result.is_err(), "spawn of a missing binary must fail");
    }

    #[cfg(unix)]
    #[test]
    fn pause_stops_the_child_and_resume_wakes_it() {
        let player = FakePlayer::new("pause-resume");
        let mut be = player.backend();
        be.play(&player.req(1)).expect("play");
        assert!(child_is_running(&be), "the child starts out running");
        be.pause().expect("pause");
        assert!(
            child_state_is(&be, 'T'),
            "SIGSTOP must stop the child (state {:?})",
            child_state(&be)
        );
        assert!(
            pump(&mut be, 150).iter().all(|e| !matches!(
                e,
                PlayerEvent::Position { .. } | PlayerEvent::Finished { .. }
            )),
            "a stopped track emits nothing"
        );
        be.resume().expect("resume");
        assert!(
            child_is_running(&be),
            "SIGCONT must wake the child (state {:?})",
            child_state(&be)
        );
        be.stop();
        player.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn volume_change_while_playing_relaunches_instead_of_killing() {
        let player = FakePlayer::new("volume-playing");
        let mut be = player.backend();
        be.play(&player.req(1)).expect("play");
        std::thread::sleep(Duration::from_millis(150));
        be.set_volume(0.5)
            .expect("a respawn-style player must accept volume while playing");
        let events = pump(&mut be, 500);
        assert!(
            be.child.is_some(),
            "the decoder must still be running after a volume change"
        );
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, PlayerEvent::Failed { .. } | PlayerEvent::Finished { .. })),
            "changing the volume must not end the track: {events:?}"
        );
        assert!(
            positions(&events)
                .last()
                .is_some_and(|p| *p >= Duration::from_millis(300)),
            "the position clock must keep ticking: {:?}",
            positions(&events)
        );
        be.stop();
        player.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn volume_change_while_paused_keeps_the_track_paused() {
        let player = FakePlayer::new("volume-paused");
        let mut be = player.backend();
        be.play(&player.req(1)).expect("play");
        std::thread::sleep(Duration::from_millis(120));
        be.pause().expect("pause");
        be.set_volume(0.4).expect("volume while paused");
        assert!(
            child_state_is(&be, 'T'),
            "restarting the decoder for a volume change may not quietly unpause it (state {:?})",
            child_state(&be)
        );
        assert!(
            pump(&mut be, 200)
                .iter()
                .all(|e| !matches!(e, PlayerEvent::Position { .. })),
            "a paused track must not report positions after a volume change"
        );
        be.resume()
            .expect("the pause survives the volume change, so resume works");
        assert!(
            pump(&mut be, 300)
                .iter()
                .any(|e| matches!(e, PlayerEvent::Position { .. })),
            "and the track really does resume"
        );
        be.stop();
        player.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn seeking_while_paused_stays_paused_at_the_new_position() {
        let player = FakePlayer::new("seek-paused");
        let mut be = player.backend();
        be.play(&player.req(1)).expect("play");
        be.pause().expect("pause");
        be.seek(Duration::from_secs(40)).expect("seek while paused");
        assert!(
            child_state_is(&be, 'T'),
            "still paused (state {:?})",
            child_state(&be)
        );
        assert!(
            be.elapsed() >= Duration::from_secs(40),
            "the position moved to the seek target"
        );
        be.resume().expect("resume after the seek");
        assert!(
            positions(&pump(&mut be, 300))
                .last()
                .is_some_and(|p| *p >= Duration::from_secs(40)),
            "playback continues from the sought position"
        );
        be.stop();
        player.cleanup();
    }

    /// The report, at the process level: `SIGSTOP` stops a process, not the
    /// clock that process measures itself against. A decoder that runs out of
    /// song while it is stopped cannot exit until it is woken, so nothing can be
    /// reaped while the screen says `Paused` — and the moment `Space` wakes it,
    /// it is gone. That end belongs to the pause: reporting it as an ordinary
    /// `Finished` is what made the run walk on and `Space` start the next song
    /// (`mutu-1sg`/`mutu-nb3`, and the report that outlived both fixes).
    #[cfg(unix)]
    #[test]
    fn a_decoder_that_runs_out_of_song_while_stopped_ends_the_paused_song() {
        let player = FakePlayer::with_script("died-on-resume", "#!/bin/sh\nexec sleep 0.3\n");
        let mut be = player.backend();
        be.play(&player.req(1)).expect("play");
        std::thread::sleep(Duration::from_millis(60));
        be.pause().expect("pause");
        // Far past the decoder's own 0.3 s, and it is still there, stopped: its
        // end is invisible until something wakes it.
        std::thread::sleep(Duration::from_millis(600));
        assert!(child_state_is(&be, 'T'), "stopped, not reaped");
        let quiet = pump(&mut be, 100);
        assert!(
            quiet
                .iter()
                .all(|e| !matches!(e, PlayerEvent::Finished { .. } | PlayerEvent::Failed { .. })),
            "a stopped decoder has no observable end yet: {quiet:?}"
        );
        be.resume()
            .expect("the child exists, so the resume is a real SIGCONT");
        let events = pump(&mut be, 400);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, PlayerEvent::EndedWhilePaused { track_id: 1, .. })),
            "waking it reveals an end the user never heard: {events:?}"
        );
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, PlayerEvent::Finished { .. } | PlayerEvent::Failed { .. })),
            "and it must not be filed as a track that played out: {events:?}"
        );
        player.cleanup();
    }

    /// A child that was already gone before the `SIGSTOP` landed (the stop of a
    /// zombie simply succeeds) must not be reported as resumed: `Space` asked
    /// for that song, and the only honest answer is that it is over.
    #[cfg(unix)]
    #[test]
    fn resuming_a_decoder_that_was_already_gone_reports_the_end_not_a_resume() {
        let player = FakePlayer::with_script("gone-behind-the-stop", "#!/bin/sh\nexit 0\n");
        let mut be = player.backend();
        be.play(&player.req(7)).expect("play");
        std::thread::sleep(Duration::from_millis(150));
        // No `poll` in between: the pause is taken by a process that is already
        // a zombie, exactly the state a `SIGSTOP` leaves behind.
        be.pause().expect(" signalling an unreaped child succeeds");
        be.resume()
            .expect("nothing was woken, so no resume is claimed");
        let events = pump(&mut be, 100);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, PlayerEvent::EndedWhilePaused { track_id: 7, .. })),
            "the end is reported instead: {events:?}"
        );
        be.stop();
        player.cleanup();
    }

    /// A track that really did play to its tagged length is a normal end, even
    /// if it was paused a moment before: the run has to advance.
    #[test]
    fn a_track_at_its_tagged_length_is_a_real_end_even_straight_after_a_pause() {
        let spec = KNOWN_PLAYERS[0].clone();
        let mut be = CommandBackend::new(spec);
        be.duration = Some(Duration::from_secs(200));
        assert!(
            !be.at_track_end(Duration::from_secs(12)),
            "a song that died at 0:12 of 3:20 did not finish"
        );
        assert!(
            be.at_track_end(Duration::from_secs(200)),
            "a song that reached its tagged length did"
        );
        assert!(
            !be.at_track_end(Duration::from_secs(199)),
            "one second short of its tagged length is still a song the user \
             never heard the end of"
        );
        be.duration = None;
        assert!(
            !be.at_track_end(Duration::from_secs(199)),
            "an unknown length is never assumed to have been reached"
        );
    }

    #[cfg(unix)]
    #[test]
    fn stopping_forgets_the_track_so_volume_cannot_resurrect_it() {
        let player = FakePlayer::new("stop-then-volume");
        let mut be = player.backend();
        be.play(&player.req(1)).expect("play");
        be.stop();
        be.set_volume(0.2)
            .expect("volume applies to the next track");
        assert!(be.child.is_none(), "nothing may be running after a stop");
        player.cleanup();
    }

    /// The mac report: `afplay` is the only decoder a stock macOS has, and it
    /// is a poor one. Its samples are already inside `coreaudiod`, so a
    /// `SIGSTOP` freezes the player but not the audio it queued (the "short
    /// blip a few seconds after I paused"), and the queue it was feeding is
    /// finished or dropped by the time `SIGCONT` wakes it — so it exits then,
    /// which used to read as an ordinary end of track and start the next song.
    /// It also has no start-position switch, so anything that passed one made
    /// `afplay` print its usage and quit.
    #[test]
    fn afplay_is_paused_by_killing_and_is_given_no_start_offset() {
        let afplay = KNOWN_PLAYERS
            .iter()
            .find(|spec| spec.bin == "afplay")
            .expect("afplay is a known player");
        assert_eq!(
            afplay.pause,
            PauseMode::Kill,
            "stopping the process cannot stop audio that left it"
        );
        assert_eq!(
            afplay.seek,
            SeekArg::None,
            "afplay has no start-position switch: -t is a stop time"
        );

        let args = afplay.args(Path::new("/m/a.mp3"), 0.5, Some(Duration::from_secs(12)));
        assert!(
            !args
                .iter()
                .any(|arg| arg.starts_with("-a") || arg.starts_with("-t")),
            "an option afplay does not know makes it exit with a usage error: {args:?}"
        );
        let switches: Vec<&str> = args
            .iter()
            .map(String::as_str)
            .filter(|arg| arg.starts_with('-'))
            .collect();
        assert_eq!(
            switches,
            vec!["-v"],
            "volume is the only switch afplay has: {args:?}"
        );
        assert!(args.iter().any(|arg| arg == "0.500"), "{args:?}");
        assert_eq!(args.last().map(String::as_str), Some("/m/a.mp3"));
    }

    #[cfg(unix)]
    #[test]
    fn a_pause_that_kills_leaves_nothing_behind_to_make_a_sound() {
        let player = FakePlayer::new("kill-pause");
        let mut be = player.backend_with(PauseMode::Kill, SeekArg::Seconds);
        be.play(&player.req(1)).expect("play");
        let _started = be.poll();
        std::thread::sleep(Duration::from_millis(150));
        be.pause().expect("pause");
        assert!(
            be.child.is_none(),
            "a stopped process can still be draining buffered audio"
        );
        let frozen = be.elapsed();
        assert!(
            frozen >= Duration::from_millis(100),
            "the pause still knows where the song was: {frozen:?}"
        );
        assert!(
            pump(&mut be, 200).is_empty(),
            "a paused track reports nothing at all"
        );

        be.resume().expect("resume launches a fresh decoder");
        assert!(be.child.is_some(), "the song came back");
        let events = pump(&mut be, 400);
        assert!(
            positions(&events)
                .last()
                .is_some_and(|p| *p >= Duration::from_millis(300)),
            "the clock runs again: {:?}",
            positions(&events)
        );
        assert!(
            events.iter().all(|e| !matches!(
                e,
                PlayerEvent::Finished { .. }
                    | PlayerEvent::Failed { .. }
                    | PlayerEvent::EndedWhilePaused { .. }
            )),
            "waking a kill-pause cannot reveal a hidden end: {events:?}"
        );
        assert!(
            events.iter().all(|e| !matches!(e, PlayerEvent::Notice(_))),
            "a player that can relaunch at a position has nothing to apologise about: {events:?}"
        );
        be.stop();
        player.cleanup();
    }

    #[cfg(unix)]
    #[test]
    fn a_kill_player_without_a_start_position_says_it_starts_again_from_the_top() {
        let player = FakePlayer::new("kill-pause-no-seek");
        let mut be = player.backend_with(PauseMode::Kill, SeekArg::None);
        be.play(&player.req(1)).expect("play");
        let _started = be.poll();
        std::thread::sleep(Duration::from_millis(200));
        be.pause().expect("pause");
        be.resume().expect("resume");
        let events = be.poll();
        assert!(
            events.iter().any(|event| match event {
                PlayerEvent::Position { track_id, position } =>
                    *track_id == 1 && position.is_zero(),
                _ => false,
            }),
            "the footer clock has to come back to the top too: {events:?}"
        );
        assert!(
            events.iter().any(|event| match event {
                PlayerEvent::Notice(text) => text.contains("from the beginning"),
                _ => false,
            }),
            "restarting from the top is said out loud, not passed off as a resume: {events:?}"
        );
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, PlayerEvent::Finished { .. } | PlayerEvent::Failed { .. })),
            "{events:?}"
        );
        be.stop();
        player.cleanup();
    }

    /// The other half of the mac report, for a signal-paused player: `Space`
    /// wakes a decoder that is only going to live a few hundred milliseconds.
    /// A position event is no proof that anything was heard — the clock is our
    /// own — so an end that soon still belongs to the pause.
    #[cfg(unix)]
    #[test]
    fn a_decoder_that_dies_shortly_after_the_wake_ends_the_paused_song() {
        let player = FakePlayer::with_script(
            "dies-a-moment-after-the-wake",
            "#!/bin/sh\nwhile [ ! -e \"$(dirname \"$0\")/go\" ]; do sleep 0.02; done\nexit 0\n",
        );
        let mut be = player.backend();
        be.play(&player.req(1)).expect("play");
        let _started = be.poll();
        std::thread::sleep(Duration::from_millis(100));
        be.pause().expect("pause");
        std::thread::sleep(Duration::from_millis(200));
        be.resume().expect("the decoder is really there to wake");
        // Past the position interval, so the app has already been told the song
        // is playing again, and inside the grace window.
        let woken = pump(&mut be, 300);
        assert!(
            !positions(&woken).is_empty(),
            "the resume looked like a resume: {woken:?}"
        );
        std::fs::write(player.dir.join("go"), b"").expect("marker");
        let events = pump(&mut be, 300);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, PlayerEvent::EndedWhilePaused { track_id: 1, .. })),
            "a wake nobody heard does not advance the run: {events:?}"
        );
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, PlayerEvent::Finished { .. } | PlayerEvent::Failed { .. })),
            "and it is not an end of the track: {events:?}"
        );
        be.stop();
        player.cleanup();
    }
}
