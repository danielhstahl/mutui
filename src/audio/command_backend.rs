//! Backend that plays through an external decoder (`mpv`, `ffplay`, `mpg123`,
//! `afplay`, …).
//!
//! Why not an in-process decoder? `cpal`/`rodio` need ALSA development headers
//! at *build* time, which breaks `cargo build --release` on minimal hosts and
//! containers. See `docs/adr/0001-stack-and-architecture.md` §3.
//!
//! The child process is spawned with stdin/stdout/stderr attached to `/dev/null`
//! so nothing can ever corrupt the alternate screen. End of track is detected by
//! reaping the child; pause/resume use `SIGSTOP`/`SIGCONT`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::{Backend, PlayRequest, PlayerEvent};

/// Emit a position event at most this often (a few times per second, never per
/// audio frame — the UI only needs a readable clock).
pub const POSITION_INTERVAL: Duration = Duration::from_millis(250);

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
}

/// Known players, in preference order.
pub const KNOWN_PLAYERS: &[PlayerSpec] = &[
    PlayerSpec {
        bin: "mpv",
        label: "mpv",
        volume: VolumeArg::Percent,
        seek: SeekArg::Seconds,
    },
    PlayerSpec {
        bin: "ffplay",
        label: "ffplay",
        volume: VolumeArg::Percent,
        seek: SeekArg::Seconds,
    },
    PlayerSpec {
        bin: "mpg123",
        label: "mpg123",
        volume: VolumeArg::Scaled,
        seek: SeekArg::None,
    },
    PlayerSpec {
        bin: "mpg321",
        label: "mpg321",
        volume: VolumeArg::Percent,
        seek: SeekArg::None,
    },
    PlayerSpec {
        bin: "madplay",
        label: "madplay",
        volume: VolumeArg::None,
        seek: SeekArg::Seconds,
    },
    PlayerSpec {
        bin: "afplay",
        label: "afplay",
        volume: VolumeArg::Scaled,
        seek: SeekArg::Seconds,
    },
    PlayerSpec {
        bin: "mplayer",
        label: "mplayer",
        volume: VolumeArg::Percent,
        seek: SeekArg::Seconds,
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
                if let Some(secs) = start.map(|s| s.as_secs_f64())
                    && secs > 0.0
                {
                    args.extend(["-a".into(), format!("{secs:.3}")]);
                }
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

    /// Reap the decoder process and freeze the position clock.
    ///
    /// `path` is deliberately *not* cleared: `seek` and `set_volume` are
    /// implemented by relaunching the same file (see [`Self::respawn`]), and
    /// forgetting the path here made those commands kill the track instead of
    /// restarting it. `stop` clears it explicitly.
    fn kill_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _kill = child.kill();
            let _wait = child.wait();
        }
        self.anchor = None;
        self.paused_at = None;
        self.paused_total = Duration::ZERO;
    }

    /// Relaunch the current track at `start`.
    ///
    /// Players whose volume and start position are spawn-time switches (mpv,
    /// ffplay, mplayer) have to be restarted for `seek`/`set_volume` to take
    /// effect. A pause is re-applied to the fresh child: the screen still says
    /// `Paused`, and a respawn that left the decoder running would resume the
    /// song behind the user's back — after which the run advances on its own a
    /// few minutes later and `Space` no longer does what it says (mutu-1sg).
    fn respawn(&mut self, start: Duration) -> Result<(), String> {
        let was_paused = self.paused_at.take().is_some();
        self.kill_child();
        self.spawn_at(Some(start))?;
        if was_paused {
            self.pause()?;
        }
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
        self.signal(libc_stop())?;
        self.paused_at = Some(Instant::now());
        Ok(())
    }

    fn resume(&mut self) -> Result<(), String> {
        let Some(paused_at) = self.paused_at else {
            return Err("playback is not paused".to_string());
        };
        if self.child.is_none() {
            // The decoder went away while paused; there is nothing to wake up.
            return Err("the paused song has already ended".to_string());
        }
        self.signal(libc_cont())?;
        self.paused_at = None;
        self.paused_total += paused_at.elapsed();
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
        if let Some(child) = self.child.as_mut()
            && let Ok(Some(status)) = child.try_wait()
        {
            let track_id = self.track_id;
            self.child = None;
            self.anchor = None;
            self.path = None;
            self.paused_at = None;
            if status.success() {
                self.pending.push(PlayerEvent::Finished { track_id });
            } else {
                let code = status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".to_string());
                self.pending.push(PlayerEvent::Failed {
                    track_id,
                    reason: format!(
                        "{} exited with {code} (unsupported or unreadable file)",
                        self.spec.label
                    ),
                });
            }
        }
        if self.child.is_some() && self.paused_at.is_none() {
            let position = self.elapsed();
            if position.saturating_sub(self.last_position) >= POSITION_INTERVAL {
                self.last_position = position;
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
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!("mutu-cmd-backend-{name}"));
            let _remove = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp dir");
            let script = dir.join("fakedecoder");
            // `exec` so that the child *is* `sleep`: reaping it leaves nothing
            // behind, and a stopped `sleep` really is a stopped process.
            std::fs::write(&script, "#!/bin/sh\nexec sleep 300\n").expect("script");
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
            let track = dir.join("song.mp3");
            std::fs::write(&track, b"the decoder never reads this").expect("fixture");
            // Same switches as mpv: volume and start position are spawn-time
            // arguments, so both are implemented by relaunching the child.
            let mpv = KNOWN_PLAYERS[0].clone();
            let bin = Box::leak(script.to_string_lossy().into_owned().into_boxed_str());
            Self {
                spec: PlayerSpec {
                    bin,
                    label: "fakedecoder",
                    volume: mpv.volume,
                    seek: mpv.seek,
                },
                track,
                dir,
            }
        }

        fn backend(&self) -> CommandBackend {
            CommandBackend::new(self.spec.clone())
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
}
