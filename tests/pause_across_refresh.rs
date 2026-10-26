//! A library refresh must never move the song the player owns (mutu-nb3).
//!
//! The report: "if I pause and a refresh happens an unpause skips to the next
//! song". The state machine owns a *song*; the row it sits on is a fact about
//! the library, and a refresh — the first scan finishing (which sorts every
//! row), a periodic sync, or `r` — moves rows. Anywhere the paused row is
//! remembered only as an index, the refresh silently re-points it at a
//! neighbour, and `Space` then replays that neighbour: the "skip".

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseEvent};

use music_tui::app::{
    AppState, CaptureDrawer, Effect, Input, InputSource, Msg, Options, Phase, PlaybackState,
    SystemClock, reduce,
};
use music_tui::audio::{MockBackend, MockHandle, PlayerCommand, PlayerEvent, PlayerHandle};
use music_tui::model::{Track, fixture_tracks, sort_tracks};
use music_tui::scan::{ScanMessage, ScanReport};
use music_tui::testing::FixtureSet;

/// Four songs: "Duo" has three (so a run exists), "Solo" has one.
fn write_library(dir: &Path) {
    FixtureSet::new(dir)
        .song("duo/one.wav", "Duo One", "Duo", "First", 1, 1, 2000)
        .song("duo/two.wav", "Duo Two", "Duo", "First", 1, 2, 2000)
        .song("duo/three.wav", "Duo Three", "Duo", "Second", 1, 1, 2001)
        .song("solo/only.wav", "Solo Song", "Solo", "Alone", 1, 1, 2002)
        .write_all()
        .expect("write fixtures");
}

fn key(code: KeyCode) -> Msg {
    Msg::Key(KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn click(row: usize) -> Msg {
    Msg::Mouse(MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 5,
        row: u16::try_from(row).unwrap_or(0),
        modifiers: KeyModifiers::NONE,
    })
}

/// The spec library, in sorted order.
fn sorted() -> Vec<Track> {
    let mut tracks = fixture_tracks();
    sort_tracks(&mut tracks);
    tracks
}

/// The same library in the order a walker would hand it over (the fixtures are
/// deliberately reversed), which is what a scan streams.
fn unsorted() -> Vec<Track> {
    fixture_tracks()
}

fn file_of(effects: &[Effect]) -> Option<String> {
    effects.iter().find_map(|effect| match effect {
        Effect::Command(PlayerCommand::Play(req)) => Some(
            req.path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
        ),
        _ => None,
    })
}

fn effects_text(effects: &[Effect]) -> String {
    effects
        .iter()
        .map(|effect| format!("{effect:?}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// State after a *finished* first scan (so `library_digest` is set and a later
/// scan is treated as a sync).
fn app_after_first_scan(digest: u64) -> AppState {
    let mut app = AppState::new(Options {
        folder: "/spec".into(),
        backend: "mock".into(),
        ..Default::default()
    });
    app.set_size(80, 12);
    let tracks = sorted();
    let count = tracks.len();
    for track in tracks {
        reduce(&mut app, Msg::Scan(ScanMessage::Track(track)), 10);
    }
    reduce(
        &mut app,
        Msg::Scan(ScanMessage::Done(ScanReport {
            scanned: count,
            succeeded: count,
            digest,
            ..Default::default()
        })),
        20,
    );
    assert_eq!(app.phase, Phase::Ready);
    app
}

/// Run a whole scan through the reducer the way the scanner threads would.
fn feed_scan(app: &mut AppState, tracks: Vec<Track>, digest: u64, now: u64) {
    reduce(app, Msg::Rescan, now);
    let count = tracks.len();
    for track in tracks {
        reduce(app, Msg::Scan(ScanMessage::Track(track)), now + 1);
    }
    reduce(
        app,
        Msg::Scan(ScanMessage::Done(ScanReport {
            scanned: count,
            succeeded: count,
            digest,
            ..Default::default()
        })),
        now + 2,
    );
}

/// The row `playback` points at, as a path.
fn playback_path(app: &AppState) -> Option<PathBuf> {
    app.playback
        .index()
        .and_then(|i| app.playlist.get(i))
        .map(|t| t.path.clone())
}

/// State with the first scan still running: the rows are streamed in walker
/// order and nothing is sorted yet.
fn app_mid_first_scan(streamed: &[Track]) -> AppState {
    let mut app = AppState::new(Options {
        folder: "/spec".into(),
        backend: "mock".into(),
        ..Default::default()
    });
    app.set_size(80, 24);
    for track in streamed {
        reduce(&mut app, Msg::Scan(ScanMessage::Track(track.clone())), 10);
    }
    assert_eq!(app.phase, Phase::Scanning);
    app
}

/// Finish the scan that [`app_mid_first_scan`] left running.
fn end_first_scan(app: &mut AppState, count: usize, digest: u64, now: u64) {
    reduce(
        app,
        Msg::Scan(ScanMessage::Done(ScanReport {
            scanned: count,
            succeeded: count,
            digest,
            ..Default::default()
        })),
        now,
    );
    assert_eq!(app.phase, Phase::Ready);
}

// --- the first scan finishing under a paused song ---------------------------

/// The first scan streams rows in walker order and sorts them when it ends —
/// that is a refresh, and it moves every row in the list.
#[test]
fn the_first_scan_sorting_does_not_move_the_paused_song() {
    let streamed = unsorted();
    let mut app = app_mid_first_scan(&streamed);
    // The user picks a row out of the half-sorted list and pauses it.
    let row = 2;
    let wanted = streamed[row].path.clone();
    reduce(&mut app, click(row), 100);
    assert_eq!(
        playback_path(&app).as_ref(),
        Some(&wanted),
        "the click plays the row it clicked"
    );
    let effects = reduce(&mut app, key(KeyCode::Char(' ')), 110);
    assert_eq!(effects, vec![Effect::Command(PlayerCommand::Pause)]);

    // The scan ends: every row moves into sorted order.
    end_first_scan(&mut app, streamed.len(), 0xaa, 200);

    let paused_row = app
        .playback
        .index()
        .expect("still paused on a row after the sort");
    assert_eq!(
        playback_path(&app).as_ref(),
        Some(&wanted),
        "the paused song must still be the song that was paused ({} is now at row {paused_row})",
        wanted.display()
    );
    assert!(matches!(app.playback, PlaybackState::Paused { .. }));

    let effects = reduce(&mut app, key(KeyCode::Char(' ')), 300);
    assert_eq!(
        effects_text(&effects),
        "Command(Resume)",
        "space resumes the song that was paused, it does not start a new one"
    );
    assert_eq!(
        playback_path(&app).as_ref(),
        Some(&wanted),
        "and it stays on that row"
    );
}

/// Same refresh, but the paused decoder ends underneath us, so `Space` has to
/// *replay* a row instead of resuming a process: it must replay the paused
/// song, not whatever row its old number now names.
#[test]
fn a_refresh_then_space_replays_the_paused_song_not_its_neighbour() {
    let streamed = unsorted();
    let mut app = app_mid_first_scan(&streamed);
    let row = 2;
    let wanted = streamed[row].path.clone();
    reduce(&mut app, click(row), 100);
    let track_id = app.playback.track_id().expect("playing");
    reduce(&mut app, key(KeyCode::Char(' ')), 110);

    end_first_scan(&mut app, streamed.len(), 0xaa, 200);
    // The paused player reaches the end of the file.
    reduce(
        &mut app,
        Msg::Player(PlayerEvent::Finished { track_id }),
        250,
    );
    assert!(app.paused_track_ended, "the paused song is over");

    let effects = reduce(&mut app, key(KeyCode::Char(' ')), 400);
    assert_eq!(
        file_of(&effects).as_deref(),
        wanted.file_name().and_then(|s| s.to_str()),
        "space replays {} (row {} of the sorted list), not a neighbour",
        wanted.display(),
        sorted()
            .iter()
            .position(|t| t.path == wanted)
            .unwrap_or(usize::MAX),
    );
}

// --- a sync while the cursor has wandered off -------------------------------

/// Pausing does not park the cursor: the user can browse while a song holds
/// still. A sync must re-anchor the *paused song*, not whatever row is selected.
#[test]
fn a_sync_does_not_move_a_paused_song_the_cursor_left() {
    let mut app = app_after_first_scan(0xaa);
    let library = sorted();
    let row = 2;
    let wanted = library[row].path.clone();
    reduce(&mut app, click(row), 1000);
    reduce(&mut app, key(KeyCode::Char(' ')), 1010);
    // Browse two rows away while paused.
    reduce(&mut app, key(KeyCode::Down), 1020);
    reduce(&mut app, key(KeyCode::Down), 1030);
    assert_eq!(app.playlist.cursor(), Some(4), "the cursor is elsewhere");

    let mut refreshed = sorted();
    let mut extra = Track::from_path(PathBuf::from("/spec/zz-new.mp3"));
    extra.artist = Some("Zed".into());
    extra.album = Some("Late".into());
    extra.refresh_key();
    refreshed.push(extra);
    feed_scan(&mut app, refreshed, 0xbb, 2000);

    assert_eq!(
        playback_path(&app).as_ref(),
        Some(&wanted),
        "the paused song stayed put (it was at row {row})"
    );
    let effects = reduce(&mut app, key(KeyCode::Char(' ')), 3000);
    assert_eq!(
        effects_text(&effects),
        "Command(Resume)",
        "space unpauses the song the user paused"
    );
    assert_eq!(playback_path(&app).as_ref(), Some(&wanted));
    assert_eq!(
        app.playlist.cursor(),
        Some(4),
        "and the cursor stays where the user left it"
    );
}

// --- a sync that removes the paused song ------------------------------------

#[test]
fn a_sync_that_removes_the_paused_song_never_plays_its_neighbour() {
    let mut app = app_after_first_scan(0xaa);
    let library = sorted();
    let row = 4;
    let wanted = library[row].path.clone();
    reduce(&mut app, click(row), 1000);
    let track_id = app.playback.track_id().expect("playing");
    reduce(&mut app, key(KeyCode::Char(' ')), 1010);

    // The file disappears from disk; the sync reports the rest.
    let mut refreshed = sorted();
    refreshed.retain(|track| track.path != wanted);
    // The paused decoder ends (its file is gone).
    reduce(
        &mut app,
        Msg::Player(PlayerEvent::Finished { track_id }),
        1500,
    );
    feed_scan(&mut app, refreshed, 0xbb, 2000);

    let effects = reduce(&mut app, key(KeyCode::Char(' ')), 3000);
    assert_eq!(
        file_of(&effects),
        None,
        "space must not start a replacement song: {}",
        effects_text(&effects)
    );
    assert!(
        !matches!(app.playback, PlaybackState::Playing { .. }),
        "the app does not claim to be playing something it cannot play"
    );
    assert!(
        app.notice
            .as_ref()
            .is_some_and(|notice| !notice.text.is_empty()),
        "and it says why"
    );
}

// --- an unchanged sync (the boring case) still resumes ----------------------

#[test]
fn an_unchanged_sync_still_resumes_the_paused_song() {
    let mut app = app_after_first_scan(0xaa);
    let library = sorted();
    let row = 3;
    let wanted = library[row].path.clone();
    reduce(&mut app, click(row), 1000);
    reduce(&mut app, key(KeyCode::Char(' ')), 1010);
    feed_scan(&mut app, sorted(), 0xaa, 2000);
    assert_eq!(playback_path(&app).as_ref(), Some(&wanted));
    let effects = reduce(&mut app, key(KeyCode::Char(' ')), 3000);
    assert_eq!(effects_text(&effects), "Command(Resume)");
    assert_eq!(playback_path(&app).as_ref(), Some(&wanted));
}

/// A sync must not break the *playing* row either: the marker and the run both
/// read it, and a stale index marks the wrong row while audio plays elsewhere.
#[test]
fn a_sync_keeps_the_playing_marker_on_the_playing_song() {
    let mut app = app_after_first_scan(0xaa);
    let library = sorted();
    let row = 6;
    let wanted = library[row].path.clone();
    reduce(&mut app, click(row), 1000);
    reduce(&mut app, key(KeyCode::Down), 1010);

    // A file is added at the top of the list, so every row after it shifts.
    let mut refreshed = sorted();
    let mut extra = Track::from_path(PathBuf::from("/spec/aa-early.mp3"));
    extra.artist = Some("Alpha Beta".into());
    extra.album = Some("Album 1".into());
    extra.disc_no = Some(1);
    extra.track_no = Some(1);
    extra.year = Some(1990);
    extra.refresh_key();
    refreshed.insert(0, extra);
    feed_scan(&mut app, refreshed, 0xbb, 2000);

    assert_eq!(
        playback_path(&app).as_ref(),
        Some(&wanted),
        "the playing row followed the song, not the number"
    );
    assert_eq!(
        app.playback.index(),
        Some(7),
        "one row was inserted ahead of it, so it moved by one"
    );
    assert_eq!(
        app.playlist.cursor(),
        Some(8),
        "the cursor followed the row the user left it on"
    );
}

// --- the report, end to end ------------------------------------------------

/// Scan `dir` for real and return the sorted rows plus the digest the scanner
/// computed for them.
fn scan_library(dir: &Path) -> (Vec<Track>, u64) {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let report = music_tui::scan::scan(dir, &music_tui::scan::ScanConfig::default(), &tx, &cancel);
    let mut tracks = Vec::new();
    while let Ok(message) = rx.recv() {
        match message {
            ScanMessage::Track(track) => tracks.push(track),
            ScanMessage::Done(_) => break,
            ScanMessage::Progress(_) => {}
        }
    }
    music_tui::model::sort_tracks(&mut tracks);
    (tracks, report.digest)
}

/// Where [`RefreshInput`] is in the script the end-to-end test replays.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Resize,
    /// The scan is streaming: only some of the rows are on screen.
    ScanHalf,
    Settle,
    Click,
    Pause,
    /// The rest of the rows plus `Done`: the list sorts itself under the user.
    Refresh,
    Drain,
    /// Wait for the audio worker to really have forwarded the injected event.
    WaitForWorker,
    /// Rounds so the loop drains that event before the next key.
    Drain2,
    Space,
    Settle2,
    /// Just ticks; the loop is stopped by its iteration limit so the resumed
    /// state is still there to assert on.
    Idle,
}

/// Scripted input for [`refresh_between_pause_and_space`]. It owns the scanner
/// channel as well, so a whole scan can be streamed into the real event loop
/// between the pause and the unpause.
struct RefreshInput {
    tx: mpsc::Sender<ScanMessage>,
    /// Rows in walker order, i.e. the order they arrive in.
    rows: Vec<Track>,
    digest: u64,
    mock: MockHandle,
    end_paused_song: bool,
    step: Step,
    rounds: usize,
}

impl RefreshInput {
    fn send_rows(&self, range: std::ops::Range<usize>) {
        for track in &self.rows[range] {
            let _drop = self.tx.send(ScanMessage::Track(track.clone()));
        }
    }
}

impl InputSource for RefreshInput {
    fn poll(&mut self, _timeout: Duration) -> Input {
        match self.step {
            Step::Resize => {
                self.step = Step::ScanHalf;
                Input::Resize {
                    width: 60,
                    height: 14,
                }
            }
            Step::ScanHalf => {
                // Half the library on screen, and nothing sorted yet: this is
                // where the user gets to the list on a slow folder.
                self.send_rows(0..self.rows.len() / 2);
                self.step = Step::Settle;
                Input::Tick
            }
            Step::Settle => {
                std::thread::sleep(Duration::from_millis(40));
                self.step = Step::Click;
                Input::Tick
            }
            Step::Click => {
                self.step = Step::Pause;
                Input::Mouse(MouseEvent {
                    kind: crossterm::event::MouseEventKind::Down(
                        crossterm::event::MouseButton::Left,
                    ),
                    column: 5,
                    row: 1,
                    modifiers: KeyModifiers::NONE,
                })
            }
            Step::Pause => {
                self.step = Step::Refresh;
                Input::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))
            }
            Step::Refresh => {
                self.send_rows(self.rows.len() / 2..self.rows.len());
                let _drop = self.tx.send(ScanMessage::Done(ScanReport {
                    scanned: self.rows.len(),
                    succeeded: self.rows.len(),
                    digest: self.digest,
                    ..Default::default()
                }));
                self.step = Step::Drain;
                Input::Tick
            }
            Step::Drain => {
                self.rounds += 1;
                if self.rounds < 3 {
                    return Input::Tick;
                }
                if self.end_paused_song {
                    // The paused decoder reaches the end of the file while the
                    // screen says `Paused`, so `Space` has to replay a row
                    // rather than wake a process.
                    self.mock.push(PlayerEvent::Finished { track_id: 1 });
                    self.step = Step::WaitForWorker;
                    return Input::Tick;
                }
                self.step = Step::Space;
                Input::Tick
            }
            Step::WaitForWorker => {
                if self.mock.has_scripted_events() {
                    std::thread::sleep(Duration::from_millis(10));
                    return Input::Tick;
                }
                self.rounds = 0;
                self.step = Step::Drain2;
                Input::Tick
            }
            Step::Drain2 => {
                self.rounds += 1;
                if self.rounds < 2 {
                    return Input::Tick;
                }
                self.step = Step::Space;
                Input::Tick
            }
            Step::Space => {
                self.step = Step::Settle2;
                Input::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))
            }
            Step::Settle2 => {
                std::thread::sleep(Duration::from_millis(40));
                self.step = Step::Idle;
                Input::Tick
            }
            Step::Idle => Input::Tick,
        }
    }
}

/// The report, through the real loop, the real audio worker and a real scan:
/// click a song out of a library that is still being indexed, pause it, let the
/// scan finish (which sorts every row), press space.
///
/// `end_paused_song` adds the other half of the report — the paused decoder
/// ending in between — which is the case where `Space` has to pick a row to
/// replay instead of waking the song that is holding still.
fn refresh_between_pause_and_space(end_paused_song: bool) {
    let dir = tempfile::tempdir().expect("tempdir");
    write_library(dir.path());
    let (mut sorted_library, digest) = scan_library(dir.path());
    let sorted = sorted_library.clone();
    // Walker order is nobody's order, so make it the worst one for an index:
    // reversed, so the row clicked while scanning lands somewhere else entirely
    // once the scan sorts the library.
    sorted_library.reverse();
    let rows = sorted_library.clone();
    // Row 1 of the half-streamed list, which is the row the test clicks.
    let clicked = rows[1].path.clone();
    let sorted_row = sorted
        .iter()
        .position(|track| track.path == clicked)
        .expect("the clicked song is in the library");
    assert_ne!(
        sorted_row,
        1,
        "the refresh must actually move the clicked song ({} stays at row 1)",
        clicked.display()
    );

    let (backend, mock) = MockBackend::paired();
    let player = PlayerHandle::spawn(Box::new(backend));
    let (tx, rx) = mpsc::channel();
    let scan_rx = Arc::new(Mutex::new(rx));

    let mut app = AppState::new(Options {
        folder: dir.path().to_path_buf(),
        backend: "mock".into(),
        ..Default::default()
    });
    let mut input = RefreshInput {
        tx,
        rows,
        digest,
        mock: mock.clone(),
        end_paused_song,
        step: Step::Resize,
        rounds: 0,
    };
    let mut drawer = CaptureDrawer {
        size: (60, 14),
        ..Default::default()
    };
    let mut no_rescan = |_force| {};
    music_tui::run::run_headless(
        &mut app,
        &player,
        &scan_rx,
        &mut input,
        &mut drawer,
        &SystemClock,
        &mut no_rescan,
        Some(120),
    )
    .expect("loop");
    let audible = mock.active();
    player.shutdown();

    if end_paused_song {
        assert_eq!(
            mock.plays(),
            vec![1, 2],
            "space replayed exactly one song (a new id, the same song)"
        );
    } else {
        assert_eq!(
            mock.plays(),
            vec![1],
            "space resumed the paused song instead of starting another one"
        );
        assert_eq!(audible, Some(1), "and that song was still audible");
    }
    let row = app.playback.index().expect("still on a row");
    assert_eq!(
        app.playlist
            .get(row)
            .map(|track| track.path.clone())
            .as_ref(),
        Some(&clicked),
        "the paused song is still the song that was paused (row {row})"
    );
    assert_eq!(
        app.playlist.cursor(),
        Some(row),
        "the selection sits on the paused song too"
    );
}

#[test]
fn a_refresh_between_pause_and_space_keeps_the_song() {
    refresh_between_pause_and_space(false);
}

#[test]
fn a_refresh_between_pause_and_space_replays_the_song_that_ended() {
    refresh_between_pause_and_space(true);
}
