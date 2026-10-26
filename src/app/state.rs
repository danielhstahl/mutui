//! [`AppState`]: everything the reducer can change.

use std::path::PathBuf;
use std::time::Duration;

use crate::audio::PlayerCommand;
use crate::model::Playlist;
use crate::scan::{ScanPhase, ScanReport};
use crate::ui::scroll::Scroll;
use crate::ui::{Areas, areas};

/// How long a transient footer message stays visible.
pub const NOTICE_TTL: Duration = Duration::from_secs(6);

/// A transient footer message with a severity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Text shown in the footer.
    pub text: String,
    /// Severity (drives colour).
    pub level: NoticeLevel,
    /// Timestamp (ms) the notice was created.
    pub created_ms: u64,
    /// `None` = never expires.
    pub ttl: Option<Duration>,
}

impl Notice {
    /// Info notice with the default TTL.
    #[must_use]
    pub fn info(text: impl Into<String>, now: u64) -> Self {
        Self {
            text: text.into(),
            level: NoticeLevel::Info,
            created_ms: now,
            ttl: Some(NOTICE_TTL),
        }
    }

    /// Warning notice with the default TTL.
    #[must_use]
    pub fn warn(text: impl Into<String>, now: u64) -> Self {
        Self {
            level: NoticeLevel::Warning,
            ..Self::info(text, now)
        }
    }

    /// Error notice with the default TTL.
    #[must_use]
    pub fn error(text: impl Into<String>, now: u64) -> Self {
        Self {
            level: NoticeLevel::Error,
            ..Self::info(text, now)
        }
    }

    /// Permanent notice (used for the "end of artist" state line).
    #[must_use]
    pub fn sticky(text: impl Into<String>, now: u64) -> Self {
        Self {
            text: text.into(),
            level: NoticeLevel::Info,
            created_ms: now,
            ttl: None,
        }
    }

    /// `true` when the notice should still be shown at `now`.
    #[must_use]
    pub fn is_live(&self, now: u64) -> bool {
        match self.ttl {
            None => true,
            Some(ttl) => now.saturating_sub(self.created_ms) < u64::from(ttl.as_millis() as u32),
        }
    }
}

/// Severity of a [`Notice`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    /// Informational.
    Info,
    /// Recoverable problem.
    Warning,
    /// Hard failure.
    Error,
}

/// Where the library currently is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The scanner is still producing rows.
    Scanning,
    /// Scan finished.
    Ready,
}

/// Playback state machine states (see `docs/adr/0001` §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    /// Nothing playing.
    Idle,
    /// Playing `track_id` (row `index`).
    Playing {
        /// Id of the play request that is currently audible.
        track_id: u64,
        /// Row being played.
        index: usize,
    },
    /// Paused on `track_id`.
    Paused {
        /// Id of the paused play request.
        track_id: u64,
        /// Row that is paused.
        index: usize,
    },
    /// A stop was requested; suppress the next `Finished`.
    Stopping,
}

impl PlaybackState {
    /// Id of the track that is (or was) active.
    #[must_use]
    pub fn track_id(&self) -> Option<u64> {
        match self {
            PlaybackState::Playing { track_id, .. } | PlaybackState::Paused { track_id, .. } => {
                Some(*track_id)
            }
            PlaybackState::Idle | PlaybackState::Stopping => None,
        }
    }

    /// Row index associated with the state.
    #[must_use]
    pub fn index(&self) -> Option<usize> {
        match self {
            PlaybackState::Playing { index, .. } | PlaybackState::Paused { index, .. } => {
                Some(*index)
            }
            PlaybackState::Idle | PlaybackState::Stopping => None,
        }
    }

    /// `true` when something is playing or paused.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.track_id().is_some()
    }

    /// `true` only while actually playing (not paused).
    #[must_use]
    pub fn is_playing(&self) -> bool {
        matches!(self, PlaybackState::Playing { .. })
    }
}

/// Scanner bookkeeping shown in the footer.
#[derive(Debug, Clone, Default)]
pub struct ScanStatus {
    /// Current phase.
    pub phase: ScanPhase,
    /// Rows indexed so far.
    pub indexed: usize,
    /// Estimated total.
    pub total_estimate: usize,
    /// Final report, once the scan is done.
    pub report: Option<ScanReport>,
    /// `true` while the scan streams rows straight into the live library.
    ///
    /// Only the *first* scan does that (the list fills in while the walker runs).
    /// Every later scan stages its rows in [`AppState::scan_stage`] and swaps
    /// them in once, which is what keeps a rescan from doubling the library or
    /// reshuffling it half-way through.
    pub streaming: bool,
}

impl ScanStatus {
    /// `Scanning… (n/m)` style label.
    #[must_use]
    pub fn label(&self) -> String {
        match self.phase {
            ScanPhase::Done => String::new(),
            ScanPhase::Walking => format!("Scanning… {} files", self.indexed),
            ScanPhase::Tagging => {
                format!("Scanning… {}/{}", self.indexed, self.total_estimate)
            }
        }
    }
}

/// Runtime options that the UI needs to know about.
#[derive(Debug, Clone)]
pub struct Options {
    /// Folder being played.
    pub folder: PathBuf,
    /// Backend label for the footer.
    pub backend: String,
    /// Rows per wheel notch.
    pub wheel_rows: i32,
    /// Avoid non-ASCII glyphs.
    pub ascii: bool,
    /// Show a debug line in the footer.
    pub debug_footer: bool,
    /// How often to re-sync the library in the background (`--sync-interval`).
    /// Zero disables it; the default here is zero so that a test state never
    /// syncs on its own, while the CLI defaults to 60 seconds.
    pub sync_interval: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            folder: PathBuf::from("."),
            backend: String::from("none"),
            wheel_rows: crate::ui::scroll::WHEEL_ROWS,
            ascii: false,
            debug_footer: false,
            sync_interval: Duration::ZERO,
        }
    }
}

/// The complete UI/application state.
#[derive(Debug)]
pub struct AppState {
    /// Sorted library + cursor + artist run.
    pub playlist: Playlist,
    /// Scroll offset and geometry.
    pub scroll: Scroll,
    /// Current layout rectangles.
    pub areas: Areas,
    /// Library lifecycle phase.
    pub phase: Phase,
    /// Scanner progress.
    pub scan: ScanStatus,
    /// Playback state machine.
    pub playback: PlaybackState,
    /// Path of the song `playback` owns, independent of the row it sits on.
    ///
    /// `PlaybackState` carries a row index because the UI needs one, but a row
    /// index is a fact about the *library*, and a refresh moves rows: the first
    /// scan sorts every row it streamed, and a sync can insert or delete one.
    /// The path is what identifies the song, so every library replacement
    /// re-derives the index from it
    /// ([`AppState::reconcile_playback_row`]). Keeping only the index is what
    /// made an unpause start the neighbour of the song that was paused
    /// (`mutu-nb3`).
    pub playback_path: Option<PathBuf>,
    /// Last reported playback position.
    pub position: Duration,
    /// Volume `0.0..=1.0`.
    pub volume: f32,
    /// `true` while muted (keeps `volume` as the un-mute target).
    pub muted: bool,
    /// Footer message.
    pub notice: Option<Notice>,
    /// Artist of the run that just finished, if the last stop was end-of-artist.
    pub end_of_artist: Option<String>,
    /// `true` while `playback` is `Paused` *and* the paused song can no longer be
    /// woken up: its player ended underneath us (EOF, a decoder that died, a
    /// backend that refused to pause), or the refresh that came in between
    /// removed the file.
    ///
    /// The run must not move on by itself while the user holds it paused, so the
    /// end is remembered here and `Space` restarts *that* song instead of
    /// whatever row the cursor sits on.
    pub paused_track_ended: bool,
    /// `true` while a scrollbar drag is active.
    pub dragging: bool,
    /// `true` while the key-binding help overlay is open.
    pub show_help: bool,
    /// Set when the app should exit.
    pub quit: bool,
    /// Id handed to the next play request.
    pub next_track_id: u64,
    /// Consecutive playback failures (see the auto-skip rule).
    pub failures: u32,
    /// `(row, ms)` of the last play request, for double-click debounce.
    pub last_play: Option<(usize, u64)>,
    /// `true` when the last handled message changed the visible state.
    pub dirty: bool,
    /// Rows produced by the scan in flight, when it is a rescan (see
    /// [`ScanStatus::streaming`]). Swapped into `playlist` when the scan ends.
    pub scan_stage: Vec<crate::model::Track>,
    /// Digest of the library currently on screen, from its [`ScanReport`].
    /// A sync whose digest matches this one changes nothing at all.
    pub library_digest: Option<u64>,
    /// `true` when the next rescan must re-read every tag (the `R` binding),
    /// consumed by the event loop when it applies [`super::reducer::Effect::Rescan`].
    pub rescan_force: bool,
    /// Timestamp (ms) of the last scan that finished, used by the periodic sync.
    pub last_scan_ms: u64,
    /// Timestamp (ms) of the most recent message the reducer saw.
    pub now: u64,
    /// Options.
    pub opts: Options,
}

impl AppState {
    /// State for a folder that is about to be scanned.
    #[must_use]
    pub fn new(opts: Options) -> Self {
        Self {
            playlist: Playlist::new(Vec::new()),
            scroll: Scroll::new(),
            areas: areas(ratatui::layout::Rect::new(0, 0, 80, 24)),
            phase: Phase::Scanning,
            scan: ScanStatus {
                // The first scan streams into the (empty) library.
                streaming: true,
                ..ScanStatus::default()
            },
            playback: PlaybackState::Idle,
            playback_path: None,
            position: Duration::ZERO,
            volume: 1.0,
            muted: false,
            notice: None,
            end_of_artist: None,
            paused_track_ended: false,
            dragging: false,
            show_help: false,
            quit: false,
            next_track_id: 1,
            failures: 0,
            last_play: None,
            dirty: true,
            now: 0,
            opts,
            scan_stage: Vec::new(),
            library_digest: None,
            rescan_force: false,
            last_scan_ms: 0,
        }
    }

    /// Row currently under the cursor.
    #[must_use]
    pub fn cursor(&self) -> Option<usize> {
        self.playlist.cursor()
    }

    /// The row that would be played by "activate".
    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        self.playlist.cursor().or(if self.playlist.is_empty() {
            None
        } else {
            Some(0)
        })
    }

    /// Ask for a track: returns the command the caller must deliver to the
    /// player thread.
    pub fn request_play(&mut self, index: usize) -> Option<PlayerCommand> {
        if !self.playlist.select_start(index) {
            return None;
        }
        let track_id = self.next_track_id;
        self.next_track_id += 1;
        self.playback = PlaybackState::Playing { track_id, index };
        self.paused_track_ended = false;
        self.position = Duration::ZERO;
        self.end_of_artist = None;
        self.notice = None;
        self.scroll.ensure_visible(index);
        let track = self.playlist.get(index)?;
        self.playback_path = Some(track.path.clone());
        Some(PlayerCommand::Play(crate::audio::PlayRequest {
            path: track.path.clone(),
            track_id,
            duration: track.duration,
        }))
    }

    /// Row that the song `playback` owns occupies *right now*, or `None` when
    /// that song is not in the library (any more).
    ///
    /// Anything that points at the current song — the `▶`/`▮` marker, the
    /// scroll-follow, `Space` after a pause — goes through this rather than
    /// through `playback.index()`, so a refresh that moved or removed the song
    /// cannot aim it at a neighbour.
    #[must_use]
    pub fn playback_row(&self) -> Option<usize> {
        if !self.playback.is_active() {
            return None;
        }
        match self.playback_path.as_ref() {
            Some(path) => self
                .playlist
                .index_of_path(path)
                .filter(|index| *index < self.playlist.len()),
            // A state built without going through `request_play` (a test, a
            // restored session) only has the row number to go on.
            None => self
                .playback
                .index()
                .filter(|index| *index < self.playlist.len()),
        }
    }

    /// Display title of the song `playback` owns, for a footer message.
    #[must_use]
    pub fn playback_title(&self) -> Option<String> {
        let path = self.playback_path.as_ref()?;
        if let Some(track) = self
            .playlist
            .tracks()
            .iter()
            .find(|track| &track.path == path)
        {
            return Some(track.title.clone());
        }
        // The song left the library: fall back to the file name it had.
        Some(
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Song".to_string()),
        )
    }

    /// Re-derive the row `playback` points at from [`AppState::playback_path`].
    ///
    /// Must run after every library replacement. When the song is gone the row
    /// it used to have is *not* clamped to a neighbour: the state machine keeps
    /// owning the song it owned, says so, and `Space` refuses to play a stranger
    /// (`mutu-nb3`).
    pub fn reconcile_playback_row(&mut self) {
        if !self.playback.is_active() {
            self.playback_path = None;
            return;
        }
        let resolved = self
            .playback_path
            .as_ref()
            .and_then(|path| self.playlist.index_of_path(path));
        if let Some(index) = resolved {
            self.playback = match self.playback {
                PlaybackState::Playing { track_id, .. } => {
                    PlaybackState::Playing { track_id, index }
                }
                PlaybackState::Paused { track_id, .. } => PlaybackState::Paused { track_id, index },
                other => other,
            };
            self.scroll.ensure_visible(index);
            return;
        }
        if matches!(self.playback, PlaybackState::Paused { .. }) {
            // A paused song whose file left the library can never be resumed,
            // which is the same problem for `Space` as one whose player ended:
            // there is nothing to wake, only a row number left behind.
            self.paused_track_ended = true;
        }
    }

    /// The first scan ended: sort the rows it streamed in and re-anchor both the
    /// cursor and the song `playback` owns.
    ///
    /// Sorting is a refresh in the harshest form — every row in the list moves —
    /// so a paused song clicked out of the half-sorted list must be found by
    /// path afterwards.
    pub fn finish_first_scan(&mut self, anchor: Option<&crate::model::Track>) {
        self.playlist.finish_scan(anchor);
        self.scroll.set_content(self.playlist.len());
        self.playlist.clamp_cursor();
        self.reconcile_playback_row();
        self.dirty = true;
    }

    /// Replace the library and keep the scroll geometry in sync.
    ///
    /// Tests and the scanner use this so `scroll.content` can never drift away
    /// from `playlist.len()`.
    pub fn set_library(&mut self, tracks: Vec<crate::model::Track>) {
        self.playlist = crate::model::Playlist::new(tracks);
        self.scroll.set_content(self.playlist.len());
        if self.playlist.is_empty() {
            self.playlist.clamp_cursor();
        }
        self.dirty = true;
    }

    /// Replace the library after a rescan, keeping the cursor (and therefore
    /// playback) on the same song when it still exists.
    pub fn rescan_set_library(
        &mut self,
        tracks: Vec<crate::model::Track>,
        anchor: Option<&crate::model::Track>,
    ) {
        self.playlist.replace(tracks, anchor);
        self.scroll.set_content(self.playlist.len());
        self.playlist.clamp_cursor();
        // The paused/playing song may have moved anywhere in the new order (and
        // the cursor is not it when the user has been browsing): re-anchor it by
        // path, not from the cursor.
        self.reconcile_playback_row();
        self.dirty = true;
    }

    /// Recompute layout after a resize (or at startup).
    pub fn set_size(&mut self, width: u16, height: u16) {
        self.areas = areas(ratatui::layout::Rect::new(0, 0, width, height));
        self.scroll.set_visible(usize::from(self.areas.list.height));
        if let Some(index) = self.playback_row().or(self.cursor()) {
            self.scroll.ensure_visible(index);
        }
        self.scroll.clamp();
        self.dirty = true;
    }
}
