//! `music-tui` — a terminal music player with a folder-based library.
//!
//! The crate is split so that every interesting decision is a pure function that
//! can be unit tested without a terminal or an audio device:
//!
//! * [`model`] — [`model::Track`], the sort key and the [`model::Playlist`]
//!   (cursor + artist-run boundaries).
//! * [`scan`] — recursive metadata scanner (runs on worker threads).
//! * [`audio`] — playback backend abstraction: [`audio::PlayerCommand`] /
//!   [`audio::PlayerEvent`] and the worker thread that owns the sound device.
//! * [`app`] — the application state plus the pure `reduce` function that turns
//!   input/scan/audio messages into state changes and player commands.
//! * [`ui`] — layout, list rendering, scrollbar math, footer and mouse
//!   hit-testing (all pure, given a `rect`).
//! * [`cli`], [`error`], [`log`] — startup, typed errors and file logging.
//!
//! See `docs/adr/0001-stack-and-architecture.md` and
//! `docs/adr/0002-sort-and-artist-run.md` for the normative specs.

pub mod app;
pub mod audio;
pub mod cli;
pub mod error;
pub mod log;
pub mod model;
pub mod run;
pub mod scan;
pub mod terminal;
pub mod testing;
pub mod ui;

pub use error::{Error, Result};

/// Version reported by `music-tui --version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
