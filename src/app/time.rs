//! Monotonic time in milliseconds, with a fake for tests.
//!
//! The reducer never reads the clock itself: the event loop passes `now` in, so
//! every timing rule (notice expiry, click debounce) is deterministic in tests.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

static START: OnceLock<Instant> = OnceLock::new();

/// Milliseconds since the process started (cheap, monotonic).
#[must_use]
pub fn now_ms() -> u64 {
    u64::try_from(START.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Something that can tell us the current time in ms.
pub trait Clock {
    /// Current time in milliseconds.
    fn now(&self) -> u64;

    /// Called once per loop iteration. A real clock just lets wall time pass;
    /// a virtual clock advances by exactly `delta`.
    fn tick(&self, delta: Duration) {
        let _drop = delta;
    }
}

/// Real wall clock.
#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> u64 {
        now_ms()
    }
}

/// Manually advanced clock for tests.
#[derive(Debug, Default)]
pub struct FakeClock(std::cell::Cell<u64>);

impl FakeClock {
    /// Start at `start` ms.
    #[must_use]
    pub fn new(start: u64) -> Self {
        Self(std::cell::Cell::new(start))
    }

    /// Advance the clock.
    pub fn advance(&self, delta: Duration) {
        self.0
            .set(self.0.get() + u64::from(delta.as_millis() as u32));
    }
}

impl Clock for FakeClock {
    fn now(&self) -> u64 {
        self.0.get()
    }

    /// Virtual time advances one tick per loop iteration, so timing rules can
    /// be tested without sleeping.
    fn tick(&self, delta: Duration) {
        self.advance(delta);
    }
}
