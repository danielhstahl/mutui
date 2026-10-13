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
                {
                    if secs > 0.0 {
                        args.push(format!("--start={secs:.3}"));
                    }
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
                {
                    if secs > 0.0 {
                        args.extend(["-ss".into(), format!("{secs:.3}")]);
                    }
                }
            }
            "mplayer" => {
                args.extend(["-nocache".into(), "-really-quiet".into()]);
                if self.volume != VolumeArg::None {
                    args.extend(["-volume".into(), (volume * 100.0).round().to_string()]);
                }
                if let (SeekArg::Seconds, Some(secs)) = (self.seek, start.map(|s| s.as_secs_f64()))
                {
                    if secs > 0.0 {
                        args.extend(["-ss".into(), format!("{secs:.3}")]);
                    }
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
                {
                    if secs > 0.0 {
                        args.push(format!("--resume={secs:.3}"));
                    }
                }
            }
            "afplay" => {
                if let Some(secs) = start.map(|s| s.as_secs_f64()) {
                    if secs > 0.0 {
                        args.extend(["-a".into(), format!("{secs:.3}")]);
                    }
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

    fn kill_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _kill = child.kill();
            let _wait = child.wait();
        }
        self.anchor = None;
        self.paused_at = None;
        self.paused_total = Duration::ZERO;
        self.path = None;
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
        match command.spawn() {
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
        let Some(paused_at) = self.paused_at.take() else {
            return Err("playback is not paused".to_string());
        };
        self.paused_total += paused_at.elapsed();
        self.signal(libc_cont())
    }

    fn stop(&mut self) {
        self.kill_child();
        self.pending.clear();
        self.track_id = 0;
    }

    fn seek(&mut self, to: Duration) -> Result<(), String> {
        if self.spec.seek == SeekArg::None {
            return Err(format!("{} cannot seek", self.spec.label));
        }
        if self.path.is_none() {
            return Err("nothing is playing".to_string());
        }
        self.pending.clear();
        self.kill_child();
        self.spawn_at(Some(to))
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
        self.kill_child();
        self.spawn_at(Some(position))
    }

    fn poll(&mut self) -> Vec<PlayerEvent> {
        if let Some(child) = self.child.as_mut() {
            if let Ok(Some(status)) = child.try_wait() {
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
}
