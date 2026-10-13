//! A silent backend: keeps the UI honest on machines with no audio output, and
//! gives the tests deterministic timing without spawning anything.
//!
//! It emits `Started`, a ticking `Position`, and `Finished` once the simulated
//! length has elapsed — but never produces sound. Timing is derived from
//! `Instant` deltas inside `poll`, so no extra threads are involved.

use std::time::{Duration, Instant};

use lofty::prelude::AudioFile as _;

use super::{Backend, PlayRequest, PlayerEvent};

/// Emit a position event at most this often.
pub const POSITION_INTERVAL: Duration = Duration::from_millis(250);

/// Simulated playback without sound.
#[derive(Debug)]
pub struct NullBackend {
    /// Forced simulated length (tests); otherwise the length comes from the
    /// play request / file probe, and unknown lengths finish immediately.
    configured: Option<Duration>,
    track_id: Option<u64>,
    length: Duration,
    started: Option<Instant>,
    paused_at: Option<Instant>,
    paused_total: Duration,
    last_position: Duration,
    volume: f32,
}

impl Default for NullBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl NullBackend {
    /// A fresh instance that takes lengths from the play request.
    pub fn new() -> Self {
        Self {
            configured: None,
            track_id: None,
            length: Duration::ZERO,
            started: None,
            paused_at: None,
            paused_total: Duration::ZERO,
            last_position: Duration::ZERO,
            volume: 1.0,
        }
    }

    /// Force a simulated length for every track.
    #[must_use]
    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.configured = Some(duration);
        self
    }

    /// Simulated position, discounting paused time.
    fn elapsed(&self) -> Duration {
        let Some(started) = self.started else {
            return Duration::ZERO;
        };
        let mut elapsed = started.elapsed();
        if let Some(paused_at) = self.paused_at {
            elapsed = elapsed.saturating_sub(paused_at.elapsed());
        }
        elapsed.saturating_sub(self.paused_total)
    }

    fn reset(&mut self) {
        self.track_id = None;
        self.started = None;
        self.paused_at = None;
        self.paused_total = Duration::ZERO;
        self.last_position = Duration::ZERO;
    }

    /// Probe a file for its length, for callers that want it.
    pub fn probe_duration(path: &std::path::Path) -> Duration {
        lofty::probe::Probe::open(path)
            .ok()
            .and_then(|probe| probe.read().ok())
            .map(|tagged| tagged.properties().duration())
            .unwrap_or(Duration::ZERO)
    }
}

impl Backend for NullBackend {
    fn name(&self) -> &'static str {
        "null"
    }

    fn play(&mut self, req: &PlayRequest) -> Result<(), String> {
        self.reset();
        self.track_id = Some(req.track_id);
        self.started = Some(Instant::now());
        self.length = self
            .configured
            .or(req.duration)
            .unwrap_or_else(|| Self::probe_duration(&req.path));
        Ok(())
    }

    fn pause(&mut self) -> Result<(), String> {
        if self.track_id.is_some() && self.paused_at.is_none() {
            self.paused_at = Some(Instant::now());
        }
        Ok(())
    }

    fn resume(&mut self) -> Result<(), String> {
        if let Some(at) = self.paused_at.take() {
            self.paused_total += at.elapsed();
        }
        Ok(())
    }

    fn stop(&mut self) {
        self.reset();
    }

    fn seek(&mut self, to: Duration) -> Result<(), String> {
        if self.started.is_none() {
            return Err("nothing is playing".to_string());
        }
        // Rebase the clock so `elapsed()` reports the sought position.
        let skew = self
            .paused_at
            .map(|at| at.elapsed())
            .unwrap_or(Duration::ZERO)
            + self.paused_total;
        self.started = Some(Instant::now() - to - skew);
        self.last_position = to;
        Ok(())
    }

    fn set_volume(&mut self, volume: f32) -> Result<(), String> {
        self.volume = volume.clamp(0.0, 1.0);
        Ok(())
    }

    fn poll(&mut self) -> Vec<PlayerEvent> {
        let Some(track_id) = self.track_id else {
            return Vec::new();
        };
        if self.paused_at.is_some() {
            return Vec::new();
        }
        let elapsed = self.elapsed();
        let mut out = Vec::new();
        if elapsed >= self.length {
            self.reset();
            out.push(PlayerEvent::Finished { track_id });
            return out;
        }
        if elapsed.saturating_sub(self.last_position) >= POSITION_INTERVAL {
            self.last_position = elapsed;
            out.push(PlayerEvent::Position {
                track_id,
                position: elapsed,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn req(id: u64) -> PlayRequest {
        PlayRequest {
            path: PathBuf::from("/tmp/x.wav"),
            track_id: id,
            duration: None,
        }
    }

    /// Drive `poll` for `ms`, collecting everything it emits.
    fn pump(backend: &mut NullBackend, ms: u64) -> Vec<PlayerEvent> {
        let deadline = Instant::now() + Duration::from_millis(ms);
        let mut out = Vec::new();
        while Instant::now() < deadline {
            out.extend(backend.poll());
            std::thread::sleep(Duration::from_millis(20));
        }
        out.extend(backend.poll());
        out
    }

    #[test]
    fn finishes_after_the_configured_length() {
        let mut backend = NullBackend::new().with_duration(Duration::from_millis(120));
        backend.play(&req(7)).expect("play");
        let events = pump(&mut backend, 400);
        assert!(
            events.contains(&PlayerEvent::Finished { track_id: 7 }),
            "{events:?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, PlayerEvent::Finished { .. }))
                .count(),
            1,
            "exactly one Finished"
        );
    }

    #[test]
    fn zero_length_tracks_finish_immediately() {
        let mut backend = NullBackend::new().with_duration(Duration::ZERO);
        backend.play(&req(1)).expect("play");
        assert_eq!(backend.poll(), vec![PlayerEvent::Finished { track_id: 1 }]);
        assert!(backend.poll().is_empty(), "only one Finished is emitted");
    }

    #[test]
    fn stop_cancels_the_pending_finished_event() {
        let mut backend = NullBackend::new().with_duration(Duration::from_secs(30));
        backend.play(&req(1)).expect("play");
        backend.stop();
        let events = pump(&mut backend, 150);
        assert!(
            events.is_empty(),
            "a stop must not auto-advance: {events:?}"
        );
    }

    #[test]
    fn position_advances_and_freezes_while_paused() {
        let mut backend = NullBackend::new().with_duration(Duration::from_secs(30));
        backend.play(&req(1)).expect("play");
        let events = pump(&mut backend, 600);
        let positions: Vec<Duration> = events
            .iter()
            .filter_map(|e| match e {
                PlayerEvent::Position { position, .. } => Some(*position),
                _ => None,
            })
            .collect();
        assert!(
            positions.windows(2).all(|w| w[0] <= w[1]),
            "positions must be monotonic: {positions:?}"
        );
        assert!(
            positions
                .last()
                .is_some_and(|p| *p >= Duration::from_millis(200)),
            "the position clock must tick: {positions:?}"
        );
        backend.pause().expect("pause");
        assert!(
            pump(&mut backend, 300)
                .iter()
                .all(|e| !matches!(e, PlayerEvent::Position { .. })),
            "no position events while paused"
        );
        backend.resume().expect("resume");
        assert!(pump(&mut backend, 400)
            .iter()
            .any(|e| matches!(e, PlayerEvent::Position { .. })));
    }

    #[test]
    fn seek_moves_the_simulated_clock() {
        let mut backend = NullBackend::new().with_duration(Duration::from_secs(30));
        backend.play(&req(1)).expect("play");
        backend.seek(Duration::from_secs(10)).expect("seek");
        let events = pump(&mut backend, 300);
        assert!(events.iter().any(|e| matches!(
            e,
            PlayerEvent::Position { position, .. } if *position >= Duration::from_secs(10)
        )));
    }

    #[test]
    fn playing_a_new_track_replaces_the_old_one() {
        let mut backend = NullBackend::new().with_duration(Duration::from_secs(30));
        backend.play(&req(1)).expect("play");
        backend.play(&req(2)).expect("play");
        let events = pump(&mut backend, 60);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, PlayerEvent::Finished { .. })),
            "replacing a track must not fake an end-of-track: {events:?}"
        );
        backend.seek(Duration::from_millis(29_900)).expect("seek");
        let events = pump(&mut backend, 400);
        assert!(events.contains(&PlayerEvent::Finished { track_id: 2 }));
        assert!(!events.contains(&PlayerEvent::Finished { track_id: 1 }));
    }
}
