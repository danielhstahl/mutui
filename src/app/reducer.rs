//! The reducer: `reduce(state, message, now) -> effects`.
//!
//! This is the only place that decides what playback happens. It performs no
//! I/O — it returns [`Effect`]s (player commands, rescan, quit) that the event
//! loop delivers, which is what makes the whole click-to-play/auto-advance
//! behaviour testable with a scripted input and a fake clock.

use std::collections::HashSet;
use std::path::Path;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent};

use crate::audio::{PlayerCommand, PlayerEvent};
use crate::model::{RunEnded, sort_tracks};
use crate::scan::{ScanMessage, ScanReport};
use crate::ui::mouse::{HitContext, MouseAction, hit_test};

use super::state::{AppState, Notice, Phase, PlaybackState, ScanStatus};
use crate::scan::ScanPhase;

/// Two play requests for the same row closer than this are one click.
pub const DEBOUNCE_MS: u64 = 300;
/// Stop the run after this many consecutive playback failures.
pub const MAX_CONSECUTIVE_FAILURES: u32 = 2;

/// Everything that can change the application state.
///
/// `Mouse`/`Scan` carry structured events; boxing them would add an allocation
/// per event for no gain, since the reducer copies nothing out of them.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Msg {
    /// A keyboard event.
    Key(KeyEvent),
    /// A mouse event (already framed by crossterm).
    Mouse(MouseEvent),
    /// A message from the player thread.
    Player(PlayerEvent),
    /// A message from the scanner threads.
    Scan(ScanMessage),
    /// The terminal was resized.
    Resized {
        /// New width in columns.
        width: u16,
        /// New height in rows.
        height: u16,
    },
    /// Periodic tick (notice expiry).
    Tick,
    /// Re-scan the folder.
    Rescan,
    /// Quit requested.
    Quit,
}

/// What the event loop must do after a state change.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Deliver a command to the player thread.
    Command(PlayerCommand),
    /// Restart the library scan.
    Rescan,
    /// Leave the event loop.
    Quit,
}

/// Apply `msg` to `state`, returning the resulting side effects.
pub fn reduce(state: &mut AppState, msg: Msg, now: u64) -> Vec<Effect> {
    state.now = now;
    let is_tick = matches!(msg, Msg::Tick);
    let mut effects = match msg {
        Msg::Key(key) => reduce_key(state, key, now),
        Msg::Mouse(event) => reduce_mouse(state, event, now),
        Msg::Player(event) => reduce_player(state, event, now),
        Msg::Scan(message) => reduce_scan(state, message, now),
        Msg::Resized { width, height } => reduce_resize(state, width, height),
        Msg::Tick => expire_notices(state, now),
        Msg::Rescan => start_rescan(state),
        Msg::Quit => {
            return quit(state);
        }
    };
    if is_tick {
        effects.extend(periodic_sync(state));
    }
    if !is_tick {
        state.dirty = true;
    }
    effects
}

/// Background library sync (`--sync-interval`).
///
/// This is cheap *because* of the library index: a sync that finds nothing new
/// costs one `stat` per file, no tag reading, no re-sort and no rows moving
/// under the cursor. It is skipped while a scan is in flight (syncs must never
/// stack) and while the user is dragging the scrollbar.
fn periodic_sync(state: &mut AppState) -> Vec<Effect> {
    let interval = state.opts.sync_interval;
    if interval.is_zero() || state.phase != Phase::Ready || state.dragging {
        return Vec::new();
    }
    let since = state.now.saturating_sub(state.last_scan_ms);
    if since < u64::try_from(interval.as_millis()).unwrap_or(u64::MAX) {
        return Vec::new();
    }
    // `start_rescan` stamps `last_scan_ms`, so a slow scan cannot make every
    // following tick fire.
    tracing::debug!(now = state.now, "periodic library sync");
    start_rescan(state)
}

fn quit(state: &mut AppState) -> Vec<Effect> {
    state.quit = true;
    let mut effects = Vec::new();
    if state.playback.is_active() {
        state.playback = PlaybackState::Stopping;
        state.playback_path = None;
        effects.push(Effect::Command(PlayerCommand::Stop));
    }
    effects.push(Effect::Quit);
    effects
}

fn expire_notices(state: &mut AppState, now: u64) -> Vec<Effect> {
    if let Some(notice) = &state.notice
        && !notice.is_live(now)
    {
        state.notice = None;
        state.dirty = true;
    }
    Vec::new()
}

// ---------------------------------------------------------------------------
// scanning
// ---------------------------------------------------------------------------

fn reduce_scan(state: &mut AppState, message: ScanMessage, _now: u64) -> Vec<Effect> {
    match message {
        ScanMessage::Track(track) => {
            if state.scan.streaming {
                state.playlist.push(track);
            } else {
                state.scan_stage.push(track);
            }
            let indexed = state.playlist.len().max(state.scan_stage.len());
            state.scroll.set_content(state.playlist.len());
            state.scan.indexed = indexed;
            state.scan.phase = ScanPhase::Tagging;
            state.phase = Phase::Scanning;
        }
        ScanMessage::Progress(progress) => {
            let streamed = state
                .playlist
                .len()
                .max(state.scan_stage.len())
                .max(state.scan.indexed);
            state.scan.indexed = progress.indexed.max(streamed);
            state.scan.total_estimate = progress.total_estimate.max(state.scan.total_estimate);
            state.scan.phase = progress.phase;
        }
        ScanMessage::Done(report) => finish_library_scan(state, report),
    }
    Vec::new()
}

/// A scan ended: either swap the new rows in, or notice that they are identical
/// to what is already on screen and throw them away.
///
/// The "identical" case is what makes an idle periodic sync free: no re-sort
/// (which is the one part of a rescan that costs real time on the UI thread), no
/// re-anchored cursor, and no rows jumping under the mouse.
fn finish_library_scan(state: &mut AppState, report: ScanReport) {
    let first_load = state.library_digest.is_none();
    let unchanged = !state.scan.streaming && state.library_digest == Some(report.digest);
    let anchor = state.playlist.current().cloned();

    if unchanged {
        state.scan_stage.clear();
    } else if state.scan.streaming {
        // Sorting the streamed rows moves every one of them, so this is a
        // refresh like any other: `finish_first_scan` re-anchors playback by
        // path instead of leaving the index pointing at a neighbour.
        state.finish_first_scan(anchor.as_ref());
    } else {
        let mut tracks = std::mem::take(&mut state.scan_stage);
        let (added, removed) = if first_load {
            (0, 0)
        } else {
            count_changes(state.playlist.tracks(), &tracks)
        };
        sort_tracks(&mut tracks);
        // `rescan_set_library`, not `playlist.replace`: a background sync can
        // move the row the current song sits on, and if `playback` keeps the old
        // index then the paused marker — and `Space`, which replays that row —
        // ends up pointing at a different song.
        state.rescan_set_library(tracks, anchor.as_ref());
        if !first_load && (added > 0 || removed > 0) {
            state.notice = Some(Notice::info(
                format!("Library updated: {added} new, {removed} removed"),
                state.now,
            ));
        } else if !first_load {
            // The paths are all the same but a file's contents changed (a tag
            // write, a re-rip) — say so rather than staying silent.
            state.notice = Some(Notice::info("Library updated: tags refreshed", state.now));
        }
    }

    state.library_digest = Some(report.digest);
    state.last_scan_ms = state.now;
    state.scan.phase = ScanPhase::Done;
    state.scan.indexed = state.playlist.len();
    state.scan.total_estimate = state.playlist.len();
    state.scan.report = Some(report.clone());
    state.phase = Phase::Ready;
    let now = state.now;
    if report.empty {
        state.notice = Some(Notice::sticky(
            format!(
                "No audio files found in {} (looked for: {})",
                state.opts.folder.display(),
                report.extensions.join(", ")
            ),
            now,
        ));
    } else if report.capped {
        state.notice = Some(Notice::warn(
            format!(
                "Showing the first {} files (--max-files reached)",
                state.playlist.len()
            ),
            now,
        ));
    } else if !report.failed.is_empty() {
        state.notice = Some(Notice::warn(
            format!(
                "{} of {} files could not be read (marked with !)",
                report.failed.len(),
                report.scanned
            ),
            now,
        ));
    }
    // Keep the playing row on screen after the re-sort.
    if let Some(index) = state.playback_row() {
        state.scroll.ensure_visible(index);
    }
}

/// Rows added and removed between the library on screen and a fresh scan.
///
/// Only ever run when a scan actually changed something, and it only hashes
/// paths, so the cost is proportional to what the user is not looking at.
fn count_changes(old: &[crate::model::Track], new: &[crate::model::Track]) -> (usize, usize) {
    let new_paths: HashSet<&Path> = new.iter().map(|track| track.path.as_path()).collect();
    let old_paths: HashSet<&Path> = old.iter().map(|track| track.path.as_path()).collect();
    let removed = old
        .iter()
        .filter(|track| !new_paths.contains(track.path.as_path()))
        .count();
    let added = new
        .iter()
        .filter(|track| !old_paths.contains(track.path.as_path()))
        .count();
    (added, removed)
}

fn start_rescan(state: &mut AppState) -> Vec<Effect> {
    state.scan = ScanStatus {
        phase: ScanPhase::Walking,
        // Only the very first scan streams into the live library; after that the
        // rows are staged and swapped in once.
        streaming: state.library_digest.is_none(),
        ..Default::default()
    };
    if state.scan.streaming && !state.playlist.is_empty() {
        // Restarting the *first* scan: drop what the cancelled scan already
        // streamed, or every row would be counted twice.
        state.playlist = crate::model::Playlist::default();
        state.scroll.set_content(0);
    }
    state.scan_stage.clear();
    state.phase = Phase::Scanning;
    state.last_scan_ms = state.now;
    vec![Effect::Rescan]
}

// ---------------------------------------------------------------------------
// playback
// ---------------------------------------------------------------------------

fn reduce_player(state: &mut AppState, event: PlayerEvent, now: u64) -> Vec<Effect> {
    match event {
        PlayerEvent::Started { track_id } => {
            if state.playback.track_id() == Some(track_id) {
                state.failures = 0;
            }
        }
        PlayerEvent::Position { track_id, position } => {
            if state.playback.track_id() == Some(track_id) {
                state.position = position;
                state.dirty = true;
            }
        }
        PlayerEvent::Finished { track_id } => {
            if state.playback.track_id() != Some(track_id) {
                // Stale event: the user clicked a new row already. Drop it.
                tracing::debug!(track_id, current = ?state.playback.track_id(), "dropping stale Finished");
                return Vec::new();
            }
            if matches!(state.playback, PlaybackState::Paused { .. }) {
                return hold_paused_song(state, now, None);
            }
            // Completing a track is the success signal that clears the failure
            // streak counter.
            state.failures = 0;
            return advance_run(state, now);
        }
        PlayerEvent::Failed { track_id, reason } => {
            if state.playback.track_id() != Some(track_id) {
                return Vec::new();
            }
            if matches!(state.playback, PlaybackState::Paused { .. }) {
                return hold_paused_song(state, now, Some(reason));
            }
            state.failures += 1;
            let title = state
                .playlist
                .current()
                .map(|t| t.title.clone())
                .unwrap_or_else(|| "file".to_string());
            if state.failures >= MAX_CONSECUTIVE_FAILURES {
                state.playback = PlaybackState::Idle;
                state.playback_path = None;
                state.position = std::time::Duration::ZERO;
                state.notice = Some(Notice::error(
                    format!(
                        "Stopped after {MAX_CONSECUTIVE_FAILURES} unreadable files (last: {title}: {reason})"
                    ),
                    now,
                ));
                return vec![Effect::Command(PlayerCommand::Stop)];
            }
            let effects = advance_run(state, now);
            // Set after advancing so the reason is what the user reads.
            state.notice = Some(Notice::warn(format!("Skipping {title}: {reason}"), now));
            return effects;
        }
        PlayerEvent::EndedWhilePaused { track_id, reason } => {
            if state.playback.track_id() != Some(track_id) {
                return Vec::new();
            }
            if state.playback.is_playing() {
                // The end was only visible because `Space` woke the decoder, so
                // the state goes back to the pause that was really still held.
                let index = state.playback.index().unwrap_or(0);
                state.playback = PlaybackState::Paused { track_id, index };
                state.paused_track_ended = true;
                return restart_song_the_pause_outlived(state, reason, now);
            }
            return hold_paused_song(state, now, Some(reason));
        }
        PlayerEvent::Notice(text) => {
            state.notice = Some(Notice::warn(text, now));
        }
    }
    Vec::new()
}

/// The song the user paused has ended in the backend.
///
/// Nothing may advance the run while the user holds it paused. Advancing made
/// the *next* song start on its own, so the next `Space` landed on that song
/// instead of the one they paused — the report reads "space doesn't unpause, it
/// moves to the next song" (mutu-1sg). Keep the paused row, remember that its
/// song is over, and let `Space` restart it.
fn hold_paused_song(state: &mut AppState, now: u64, reason: Option<String>) -> Vec<Effect> {
    let title = state.playback_title().unwrap_or_else(|| "Song".to_string());
    tracing::debug!(title, "paused song ended; holding the paused row");
    state.paused_track_ended = true;
    state.notice = Some(Notice::warn(
        match reason {
            Some(reason) => format!("{title} ended while paused ({reason}) — Space replays it"),
            None => format!("{title} ended while paused — Space replays it"),
        },
        now,
    ));
    Vec::new()
}

/// The user's `Space` woke a decoder that had already died behind the pause.
///
/// The press asked for *that* song, so it gets that song: the row is replayed by
/// path, exactly like a paused song that ended quietly. Advancing instead — which
/// is what a `Finished` here does — is the reported "pause, walk away, press
/// space, it skipped to the next song".
fn restart_song_the_pause_outlived(state: &mut AppState, reason: String, now: u64) -> Vec<Effect> {
    let title = state.playback_title().unwrap_or_else(|| "Song".to_string());
    tracing::debug!(
        title,
        reason,
        "paused song was already over when it was woken; replaying it"
    );
    let effects = replay_paused_song(state, now);
    if state.playback.is_playing() {
        state.notice = Some(Notice::warn(
            format!("{title} ended while paused ({reason}) — starting it again"),
            now,
        ));
    }
    effects
}

/// Auto-advance inside the artist run, or stop at its end.
fn advance_run(state: &mut AppState, now: u64) -> Vec<Effect> {
    let artist = state
        .playlist
        .current()
        .map(|t| t.display_artist().to_string())
        .unwrap_or_default();
    match state.playlist.advance() {
        Some(next) => match state.request_play(next) {
            Some(cmd) => vec![Effect::Command(cmd)],
            None => vec![Effect::Command(PlayerCommand::Stop)],
        },
        None => {
            let reason = if state.playlist.is_empty() {
                RunEnded::EmptyList
            } else {
                RunEnded::EndOfArtist {
                    artist: artist.clone(),
                }
            };
            state.playback = PlaybackState::Idle;
            state.playback_path = None;
            state.position = std::time::Duration::ZERO;
            match reason {
                RunEnded::EmptyList => {
                    state.notice = Some(Notice::warn("Nothing left to play", now));
                }
                RunEnded::EndOfArtist { artist } => {
                    state.end_of_artist = Some(artist.clone());
                    state.notice = Some(Notice::sticky(
                        format!("End of artist: {artist} — stopped at the end of the run"),
                        now,
                    ));
                }
            }
            vec![Effect::Command(PlayerCommand::Stop)]
        }
    }
}

// ---------------------------------------------------------------------------
// mouse
// ---------------------------------------------------------------------------

fn reduce_mouse(state: &mut AppState, event: MouseEvent, now: u64) -> Vec<Effect> {
    let ctx = HitContext {
        list: state.areas.list,
        gutter: state.areas.gutter,
        scroll: &state.scroll,
        len: state.playlist.len(),
        dragging: state.dragging,
    };
    let action = hit_test(&event, ctx);
    match action {
        MouseAction::Ignore => Vec::new(),
        MouseAction::EndDrag => {
            state.dragging = false;
            Vec::new()
        }
        MouseAction::ScrollTo(row) => {
            if matches!(
                event.kind,
                crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left)
            ) {
                state.dragging = true;
            }
            state.scroll.scroll_to_row_in_track(row);
            Vec::new()
        }
        MouseAction::Wheel(delta) => {
            let step = if delta < 0 {
                -state.opts.wheel_rows
            } else {
                state.opts.wheel_rows
            };
            state.scroll.scroll_by(step);
            Vec::new()
        }
        MouseAction::PlayRow(index) => play_row(state, index, now),
    }
}

/// Click-to-play (and the `Enter` key): the start of a new artist run.
fn play_row(state: &mut AppState, index: usize, now: u64) -> Vec<Effect> {
    if let Some((last_index, last_now)) = state.last_play
        && last_index == index
        && now.saturating_sub(last_now) < DEBOUNCE_MS
    {
        tracing::debug!(index, "ignoring duplicate click");
        return Vec::new();
    }
    state.last_play = Some((index, now));
    state.dragging = false;
    state.failures = 0;
    match state.request_play(index) {
        Some(cmd) => {
            tracing::debug!(index, "playing row");
            vec![Effect::Command(cmd)]
        }
        None => {
            state.notice = Some(Notice::warn("That row no longer exists", now));
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// keyboard
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
fn reduce_key(state: &mut AppState, key: KeyEvent, now: u64) -> Vec<Effect> {
    if key.kind == KeyEventKind::Release {
        return Vec::new();
    }
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
    {
        return quit(state);
    }
    if state.show_help {
        // The overlay is modal: any key closes it and nothing else happens, so
        // a stray key can never both dismiss help and change playback.
        state.show_help = false;
        state.dirty = true;
        return if key.code == KeyCode::Char('q') {
            quit(state)
        } else {
            Vec::new()
        };
    }
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => quit(state),
        KeyCode::Char('?') | KeyCode::F(1) => {
            state.show_help = !state.show_help;
            Vec::new()
        }
        KeyCode::Char('r') => start_rescan(state),
        // Shift-R: a rescan that ignores the library index and re-reads every
        // tag (the escape hatch for a rewrite that preserved mtime).
        KeyCode::Char('R') => {
            state.rescan_force = true;
            start_rescan(state)
        }
        KeyCode::Enter => state
            .selected()
            .map_or(Vec::new(), |index| play_row(state, index, now)),
        KeyCode::Char(' ') => toggle_pause_or_play(state, now),
        KeyCode::Up | KeyCode::Char('k') => move_cursor(state, -1),
        KeyCode::Down | KeyCode::Char('j') => move_cursor(state, 1),
        KeyCode::PageUp => {
            state.scroll.page_up();
            Vec::new()
        }
        KeyCode::PageDown => {
            state.scroll.page_down();
            Vec::new()
        }
        KeyCode::Home => {
            jump_to(state, 0);
            Vec::new()
        }
        KeyCode::End => {
            let last = state.playlist.len().saturating_sub(1);
            jump_to(state, last);
            Vec::new()
        }
        KeyCode::Char('n') => step_within_run(state, 1, now),
        KeyCode::Char('p') => step_within_run(state, -1, now),
        KeyCode::Char('m') => toggle_mute(state),
        KeyCode::Char('+') | KeyCode::Char('=') => change_volume(state, 0.1),
        KeyCode::Char('-') => change_volume(state, -0.1),
        _ => Vec::new(),
    }
}

/// Move the selection to an absolute row (Home/End) without playing it.
fn jump_to(state: &mut AppState, index: usize) {
    if state.playlist.select_start(index) {
        state.scroll.ensure_visible(index);
    } else if state.scroll.content() > 0 {
        state.scroll.scroll_to(state.scroll.max_offset());
    }
    state.dirty = true;
}

fn move_cursor(state: &mut AppState, delta: i64) -> Vec<Effect> {
    if let Some(index) = state.playlist.move_cursor(delta) {
        state.scroll.ensure_visible(index);
    }
    Vec::new()
}

fn toggle_pause_or_play(state: &mut AppState, now: u64) -> Vec<Effect> {
    match state.playback {
        PlaybackState::Playing { .. } => {
            if let PlaybackState::Playing { track_id, index } = state.playback {
                state.playback = PlaybackState::Paused { track_id, index };
            }
            vec![Effect::Command(PlayerCommand::Pause)]
        }
        PlaybackState::Paused { track_id, index } => {
            if state.paused_track_ended {
                return replay_paused_song(state, now);
            }
            state.playback = PlaybackState::Playing { track_id, index };
            vec![Effect::Command(PlayerCommand::Resume)]
        }
        _ => state
            .selected()
            .map_or(Vec::new(), |index| play_row(state, index, now)),
    }
}

/// `Space` on a paused song that can no longer be woken up.
///
/// The row it belongs to may have moved since the pause — a library refresh in
/// between sorts rows, adds and deletes them — so the song is looked up by path
/// again and only then replayed (`mutu-nb3`: replaying the stale row number is
/// exactly "pause, a refresh happens, unpause skips to the next song"). When
/// the song itself is gone there is nothing to replay, and starting the row its
/// old number now names would be the same skip wearing a different hat.
fn replay_paused_song(state: &mut AppState, now: u64) -> Vec<Effect> {
    state.paused_track_ended = false;
    let title = state.playback_title().unwrap_or_else(|| "Song".to_string());
    let Some(index) = state.playback_row() else {
        state.playback = PlaybackState::Idle;
        state.playback_path = None;
        state.position = std::time::Duration::ZERO;
        state.notice = Some(Notice::warn(
            format!("{title} is no longer in the library — nothing to resume"),
            now,
        ));
        return vec![Effect::Command(PlayerCommand::Stop)];
    };
    // The restart is deliberate, so it is not a duplicate click.
    state.last_play = None;
    play_row(state, index, now)
}

fn step_within_run(state: &mut AppState, direction: i8, now: u64) -> Vec<Effect> {
    if state.playlist.is_empty() {
        state.notice = Some(Notice::warn("Nothing to play", now));
        return Vec::new();
    }
    // Make sure a run is established before honouring next/previous.
    let cursor = state.playback.index().or(state.cursor()).unwrap_or(0);
    state.playlist.select_start(cursor);
    let artist = state
        .playlist
        .current()
        .map(|t| t.display_artist().to_string())
        .unwrap_or_default();
    let next = if direction > 0 {
        if state.playlist.at_run_end() {
            state.notice = Some(Notice::info(
                format!("End of run: {artist} (that was the last song)"),
                now,
            ));
            None
        } else {
            state.playlist.advance()
        }
    } else if state
        .playlist
        .run()
        .is_some_and(|(start, _)| cursor > start)
    {
        state.playlist.prev()
    } else {
        state.notice = Some(Notice::info(format!("Start of run: {artist}"), now));
        None
    };
    match next {
        Some(index) => play_row(state, index, now),
        None => Vec::new(),
    }
}

fn toggle_mute(state: &mut AppState) -> Vec<Effect> {
    state.muted = !state.muted;
    let target = if state.muted { 0.0 } else { state.volume };
    vec![Effect::Command(PlayerCommand::SetVolume(target))]
}

fn change_volume(state: &mut AppState, delta: f32) -> Vec<Effect> {
    state.volume = (state.volume + delta).clamp(0.0, 1.0);
    if state.muted && state.volume > 0.0 {
        state.muted = false;
    }
    vec![Effect::Command(PlayerCommand::SetVolume(state.volume))]
}

fn reduce_resize(state: &mut AppState, width: u16, height: u16) -> Vec<Effect> {
    state.set_size(width, height);
    Vec::new()
}

/// Convert a crossterm [`Event`] into a [`Msg`] (returns `None` for events the
/// app ignores: focus, paste, key release).
#[must_use]
pub fn msg_from_event(event: Event) -> Option<Msg> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => Some(Msg::Key(key)),
        Event::Mouse(mouse) => Some(Msg::Mouse(mouse)),
        Event::Resize(width, height) => Some(Msg::Resized { width, height }),
        _ => None,
    }
}
