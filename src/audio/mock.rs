//! Backend used by the test-suite and available to users as `--backend mock`:
//! records everything the application asks for and replays scripted events.
//!
//! It is what makes the "rapid click must leave exactly one audible track" and
//! "auto-advance" acceptance tests possible without a sound device.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::{Backend, PlayRequest, PlayerEvent};

/// Everything a [`MockBackend`] recorded.
#[derive(Debug, Default)]
pub struct MockLog {
    /// One entry per `play` request, in order.
    pub plays: Vec<u64>,
    /// Number of times output was silenced.
    pub stops: usize,
    /// The track that would currently be audible (`None` = silence).
    pub active: Option<u64>,
    /// Volume changes.
    pub volumes: Vec<f32>,
    /// `true` after the last pause with no matching resume.
    pub paused: bool,
}

/// Shared handle to a running [`MockBackend`].
#[derive(Debug, Default, Clone)]
pub struct MockHandle {
    log: Arc<Mutex<MockLog>>,
    scripted: Arc<Mutex<VecDeque<PlayerEvent>>>,
}

impl MockHandle {
    /// All play requests seen so far.
    #[must_use]
    pub fn plays(&self) -> Vec<u64> {
        self.log
            .lock()
            .map(|log| log.plays.clone())
            .unwrap_or_default()
    }

    /// Track that would currently be audible.
    #[must_use]
    pub fn active(&self) -> Option<u64> {
        self.log.lock().ok().and_then(|log| log.active)
    }

    /// `true` when the backend is paused.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.log.lock().map(|log| log.paused).unwrap_or_default()
    }

    /// Number of times output was silenced.
    #[must_use]
    pub fn stops(&self) -> usize {
        self.log.lock().map(|log| log.stops).unwrap_or_default()
    }

    /// Volume changes.
    #[must_use]
    pub fn volumes(&self) -> Vec<f32> {
        self.log
            .lock()
            .map(|log| log.volumes.clone())
            .unwrap_or_default()
    }

    /// Queue an event to be emitted on the next poll.
    pub fn push(&self, event: PlayerEvent) {
        if let Ok(mut queue) = self.scripted.lock() {
            queue.push_back(event);
        }
    }
}

/// Scriptable backend (see module docs).
pub struct MockBackend {
    handle: MockHandle,
    pending: Vec<PlayerEvent>,
    volume: f32,
}

impl MockBackend {
    /// Create a backend plus the handle used to observe/script it.
    #[must_use]
    pub fn paired() -> (Self, MockHandle) {
        let handle = MockHandle::default();
        (
            Self {
                handle: handle.clone(),
                pending: Vec::new(),
                volume: 1.0,
            },
            handle,
        )
    }

    fn record<F: FnOnce(&mut MockLog)>(handle: &MockHandle, f: F) {
        if let Ok(mut log) = handle.log.lock() {
            f(&mut log);
        }
    }
}

impl Backend for MockBackend {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn play(&mut self, req: &PlayRequest) -> Result<(), String> {
        let track_id = req.track_id;
        Self::record(&self.handle, |log| {
            if log.active.is_some() {
                log.stops += 1;
            }
            log.plays.push(track_id);
            log.active = Some(track_id);
            log.paused = false;
        });
        Ok(())
    }

    fn pause(&mut self) -> Result<(), String> {
        let Some(mut log) = self.handle.log.lock().ok() else {
            return Err("mock backend lock poisoned".to_string());
        };
        if log.active.is_none() {
            return Err("nothing is playing".to_string());
        }
        log.paused = true;
        Ok(())
    }

    fn resume(&mut self) -> Result<(), String> {
        let Some(mut log) = self.handle.log.lock().ok() else {
            return Err("mock backend lock poisoned".to_string());
        };
        if !log.paused {
            return Err("not paused".to_string());
        }
        log.paused = false;
        Ok(())
    }

    fn stop(&mut self) {
        Self::record(&self.handle, |log| {
            if log.active.is_some() {
                log.stops += 1;
            }
            log.active = None;
            log.paused = false;
        });
    }

    fn seek(&mut self, _to: std::time::Duration) -> Result<(), String> {
        Ok(())
    }

    fn set_volume(&mut self, volume: f32) -> Result<(), String> {
        self.volume = volume;
        Self::record(&self.handle, move |log| log.volumes.push(volume));
        Ok(())
    }

    fn poll(&mut self) -> Vec<PlayerEvent> {
        if let Ok(mut queue) = self.handle.scripted.lock() {
            while let Some(event) = queue.pop_front() {
                self.pending.push(event);
            }
        }
        std::mem::take(&mut self.pending)
    }
}

impl std::fmt::Debug for MockBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockBackend")
            .field("volume", &self.volume)
            .finish()
    }
}
