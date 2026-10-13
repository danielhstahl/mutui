//! End-to-end headless runs: the real event loop, a real audio worker thread,
//! scripted input and a drawer that captures frames (`mutu-2h8.21`).

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use music_tui::app::{
    AppState, CaptureDrawer, Clock, Effect, FakeClock, Input, InputSource, Msg, Notice, Options,
    Phase, PlaybackState, ScriptedInput, SystemClock,
};
use music_tui::audio::{
    MockBackend, MockHandle, NullBackend, PlayerCommand, PlayerEvent, PlayerHandle,
};
use music_tui::error::Result;
use music_tui::model::Track;
use music_tui::run::ScanJob;
use music_tui::scan::{self, ScanConfig, ScanMessage};
use music_tui::testing::FixtureSet;

/// Two artists: "Duo" has three songs, "Solo" has one.
fn library(dir: &Path) -> Vec<Track> {
    FixtureSet::new(dir)
        .song("duo/one.wav", "Duo One", "Duo", "First", 1, 1, 2000)
        .song("duo/two.wav", "Duo Two", "Duo", "First", 1, 2, 2000)
        .song("duo/three.wav", "Duo Three", "Duo", "Second", 1, 1, 2001)
        .song("solo/only.wav", "Solo Song", "Solo", "Alone", 1, 1, 2002)
        .write_all()
        .expect("write fixtures");

    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let report = scan::scan(dir, &ScanConfig::default(), &tx, &cancel);
    assert_eq!(report.scanned, 4, "{report:?}");
    let mut tracks = Vec::new();
    while let Ok(ScanMessage::Track(track)) = rx.recv() {
        tracks.push(track);
    }
    music_tui::model::sort_tracks(&mut tracks);
    tracks
}

fn click_event(row: usize) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 5,
        row: u16::try_from(row).unwrap_or(0),
        modifiers: KeyModifiers::NONE,
    }
}

fn click(row: usize) -> Input {
    Input::Mouse(click_event(row))
}

fn click_msg(row: usize) -> Msg {
    Msg::Mouse(click_event(row))
}

fn key(code: KeyCode) -> Input {
    Input::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn resize(width: u16, height: u16) -> Input {
    Input::Resize { width, height }
}

fn wait(ms: u64) -> Input {
    Input::Wait(Duration::from_millis(ms))
}

fn state_with(tracks: Vec<Track>, backend: &str) -> AppState {
    let mut app = AppState::new(Options {
        folder: "/library".into(),
        backend: backend.to_string(),
        ..Options::default()
    });
    app.set_size(60, 14);
    app.set_library(tracks);
    app.phase = Phase::Ready;
    app
}

fn empty_scan_receiver() -> Arc<Mutex<mpsc::Receiver<ScanMessage>>> {
    let (_tx, rx) = mpsc::channel();
    Arc::new(Mutex::new(rx))
}

/// Drive the real loop with a scripted input and a capturing drawer.
fn run(
    app: &mut AppState,
    player: &PlayerHandle,
    input: &mut dyn InputSource,
    drawer: &mut CaptureDrawer,
    clock: &dyn Clock,
    max_iterations: Option<usize>,
) -> Result<()> {
    let rx = empty_scan_receiver();
    let mut no_rescan = |_force| {};
    music_tui::run::run_headless(
        app,
        player,
        &rx,
        input,
        drawer,
        clock,
        &mut no_rescan,
        max_iterations,
    )
}

fn drawer60() -> CaptureDrawer {
    CaptureDrawer {
        size: (60, 14),
        ..CaptureDrawer::default()
    }
}

#[test]
fn click_plays_the_rest_of_the_artist_and_stops_end_to_end() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tracks = library(dir.path());
    assert_eq!(tracks.len(), 4);

    // Silent backend: each track "plays" for 120 ms.
    let player = PlayerHandle::spawn(Box::new(
        NullBackend::new().with_duration(Duration::from_millis(60)),
    ));
    let mut app = state_with(tracks, "null");
    // The loop drains the player queue once per iteration, so the script needs
    // one (or more) waits per expected advance: three tracks, eight waits.
    let mut input = ScriptedInput::new(
        [resize(60, 14), click(0)]
            .into_iter()
            .chain((0..8).map(|_| wait(150)))
            .chain([key(KeyCode::Char('q'))]),
    );
    let mut drawer = drawer60();
    run(
        &mut app,
        &player,
        &mut input,
        &mut drawer,
        &SystemClock,
        None,
    )
    .expect("loop");
    player.shutdown();

    assert_eq!(
        app.playlist.cursor(),
        Some(2),
        "stopped on the last Duo row, never crossing into Solo"
    );
    assert_eq!(app.end_of_artist.as_deref(), Some("Duo"));
    assert_eq!(app.playback, PlaybackState::Idle);
    let notice = app.notice.as_ref().expect("a footer message");
    assert!(notice.text.contains("End of artist: Duo"), "{notice:?}");
    assert!(drawer.frames >= 4, "frames drawn: {}", drawer.frames);
    let frame = drawer.last_frame.join("\n");
    assert!(frame.contains("Duo"), "{frame}");
    assert!(
        frame.contains("End of artist"),
        "the footer must explain the stop:\n{frame}"
    );
}

#[test]
fn clicking_the_last_row_of_the_run_stops_immediately() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tracks = library(dir.path());
    let player = PlayerHandle::spawn(Box::new(
        NullBackend::new().with_duration(Duration::from_millis(60)),
    ));
    let mut app = state_with(tracks, "null");
    let mut input = ScriptedInput::new([
        resize(60, 14),
        click(2),
        wait(150),
        wait(150),
        wait(150),
        key(KeyCode::Char('q')),
    ]);
    let mut drawer = drawer60();
    run(
        &mut app,
        &player,
        &mut input,
        &mut drawer,
        &SystemClock,
        None,
    )
    .expect("loop");
    player.shutdown();
    assert_eq!(app.playlist.cursor(), Some(2));
    assert_eq!(app.end_of_artist.as_deref(), Some("Duo"));
    assert_eq!(app.playback, PlaybackState::Idle);
    // Solo never started.
    assert_eq!(
        app.playlist
            .get(app.playlist.cursor().unwrap_or(0))
            .map(|t| t.display_artist().to_string()),
        Some("Duo".to_string())
    );
}

#[test]
fn rapid_clicks_leave_exactly_one_track_playing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tracks = library(dir.path());
    let (backend, mock) = MockBackend::paired();
    let player = PlayerHandle::spawn(Box::new(backend));
    let mut app = state_with(tracks, "mock");
    let mut input = ScriptedInput::new([
        resize(60, 14),
        click(0),
        wait(600),
        click(3),
        wait(600),
        key(KeyCode::Char('q')),
    ]);
    let mut drawer = drawer60();
    run(
        &mut app,
        &player,
        &mut input,
        &mut drawer,
        &SystemClock,
        None,
    )
    .expect("loop");
    player.shutdown();

    let plays = mock.plays();
    assert_eq!(plays.len(), 2, "exactly two deliberate plays: {plays:?}");
    assert_eq!(mock.active(), None, "quitting must silence the last track");
    assert_eq!(app.playlist.cursor(), Some(3));
    assert!(mock.stops() >= 1);
}

/// Input source that performs a side effect at a chosen poll, so a test can
/// inject a late player event into a running loop.
struct HookedInput {
    queue: std::collections::VecDeque<Input>,
    polls: usize,
    hook_at: usize,
    mock: MockHandle,
}

impl InputSource for HookedInput {
    fn poll(&mut self, _timeout: Duration) -> Input {
        self.polls += 1;
        if self.polls == self.hook_at {
            // A Finished for a track the app stopped caring about long ago.
            self.mock.push(PlayerEvent::Finished { track_id: 1 });
        }
        self.queue.pop_front().unwrap_or(Input::Tick)
    }
}

#[test]
fn stale_finished_event_from_the_worker_is_ignored() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tracks = library(dir.path());
    let (backend, mock) = MockBackend::paired();
    let player = PlayerHandle::spawn(Box::new(backend));
    let mut app = state_with(tracks, "mock");
    let mut input = HookedInput {
        queue: [
            resize(60, 14),
            click(0),
            wait(120),
            wait(120),
            click(1),
            // The hook fires on this poll; the stale event lands next round.
            wait(120),
            wait(120),
            key(KeyCode::Char('q')),
        ]
        .into(),
        polls: 0,
        hook_at: 6,
        mock: mock.clone(),
    };
    let mut drawer = drawer60();
    run(
        &mut app,
        &player,
        &mut input,
        &mut drawer,
        &SystemClock,
        None,
    )
    .expect("loop");
    player.shutdown();
    assert_eq!(mock.plays(), vec![1, 2], "only the two clicks played");
    assert_eq!(
        app.playlist.cursor(),
        Some(1),
        "a stale Finished must not advance the cursor"
    );
}

#[test]
fn first_frames_are_drawn_before_the_scan_finishes() {
    let dir = tempfile::tempdir().expect("tempdir");
    for i in 0..400 {
        let path = dir.path().join(format!("d{i}/t{i}.wav"));
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(&path, music_tui::testing::wav_bytes(0.01)).expect("write");
    }
    let scan_job = ScanJob::start(dir.path().to_path_buf(), ScanConfig::default());
    let player = PlayerHandle::spawn(Box::new(NullBackend::new()));
    let mut app = AppState::new(Options::default());
    let mut input = ScriptedInput::new([resize(60, 14), wait(150), key(KeyCode::Char('q'))]);
    let mut drawer = drawer60();
    let rx = Arc::clone(&scan_job.rx);
    let mut no_rescan = |_force| {};
    music_tui::run::run_headless(
        &mut app,
        &player,
        &rx,
        &mut input,
        &mut drawer,
        &SystemClock,
        &mut no_rescan,
        None,
    )
    .expect("loop");
    assert!(
        drawer.frames >= 2,
        "the UI must repaint while the scan runs"
    );
    let frame = drawer.last_frame.join("\n");
    assert!(!frame.trim().is_empty(), "something must be on screen");
    assert!(
        !app.playlist.is_empty() || !app.scan.label().is_empty(),
        "the scan should have made progress: {:?}",
        app.scan
    );
    scan_job.cancel();
    player.shutdown();
}

#[test]
fn the_loop_ticks_and_expires_notices() {
    let clock = FakeClock::new(1_000);
    let player = PlayerHandle::spawn(Box::new(NullBackend::new()));
    let mut app = AppState::new(Options::default());
    app.notice = Some(Notice::warn("gone soon", 1_000));
    let mut input = ScriptedInput::new([resize(60, 14)]);
    let mut drawer = drawer60();
    run(
        &mut app,
        &player,
        &mut input,
        &mut drawer,
        &clock,
        Some(300),
    )
    .expect("loop");
    // 300 iterations of a 50 ms tick.
    assert_eq!(
        clock.now(),
        1_000 + 300 * 50,
        "the tick must drive the clock"
    );
    assert!(
        app.notice.is_none(),
        "the notice must have expired via Tick"
    );
    player.shutdown();
}

#[test]
fn quit_stops_playback_before_the_loop_exits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tracks = library(dir.path());
    let (backend, mock) = MockBackend::paired();
    let player = PlayerHandle::spawn(Box::new(backend));
    let mut app = state_with(tracks, "mock");
    let mut input =
        ScriptedInput::new([resize(60, 14), click(0), wait(300), key(KeyCode::Char('q'))]);
    let mut drawer = drawer60();
    run(
        &mut app,
        &player,
        &mut input,
        &mut drawer,
        &SystemClock,
        None,
    )
    .expect("loop");
    player.shutdown();
    assert!(app.quit);
    assert_eq!(mock.active(), None, "audio must be silent after quit");
}

#[test]
fn reducer_effects_reach_the_player() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tracks = library(dir.path());
    let (backend, mock) = MockBackend::paired();
    let player = PlayerHandle::spawn(Box::new(backend));
    let mut app = state_with(tracks, "mock");
    let mut input = ScriptedInput::new([
        resize(60, 14),
        click(0),
        wait(200),
        key(KeyCode::Char('m')),
        key(KeyCode::Char('q')),
    ]);
    let mut drawer = drawer60();
    run(
        &mut app,
        &player,
        &mut input,
        &mut drawer,
        &SystemClock,
        None,
    )
    .expect("loop");
    player.shutdown();
    assert!(
        mock.volumes().contains(&0.0),
        "mute must reach the backend: {:?}",
        mock.volumes()
    );
}

#[test]
fn quit_effects_are_ordered_stop_then_quit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = state_with(library(dir.path()), "null");
    music_tui::app::reduce(&mut app, click_msg(0), 10);
    assert!(matches!(app.playback, PlaybackState::Playing { .. }));
    let effects = music_tui::app::reduce(&mut app, Msg::Quit, 20);
    assert_eq!(
        effects,
        vec![Effect::Command(PlayerCommand::Stop), Effect::Quit]
    );
}

#[test]
fn periodic_syncs_and_shift_r_reach_the_scan_job_through_the_loop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = state_with(library(dir.path()), "null");
    app.opts.sync_interval = Duration::from_secs(60);
    let player = PlayerHandle::spawn(Box::new(NullBackend::new()));
    let rx = empty_scan_receiver();

    // One `Wait(0)` is one loop iteration, and the fake clock advances one tick
    // per iteration, so 1_300 of them are 65 virtual seconds without any real
    // sleeping: enough for exactly one periodic sync.
    let mut input = ScriptedInput::new(
        [resize(60, 14)]
            .into_iter()
            .chain((0..1_300).map(|_| wait(0)))
            .chain([key(KeyCode::Char('R')), key(KeyCode::Char('q'))]),
    );
    let mut drawer = drawer60();
    let clock = FakeClock::new(0);
    let rescans: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&rescans);
    let mut on_rescan = move |force: bool| {
        if let Ok(mut guard) = log.lock() {
            guard.push(force);
        }
    };
    music_tui::run::run_headless(
        &mut app,
        &player,
        &rx,
        &mut input,
        &mut drawer,
        &clock,
        &mut on_rescan,
        None,
    )
    .expect("loop");
    player.shutdown();
    assert_eq!(
        *rescans.lock().expect("log"),
        vec![false, true],
        "one periodic sync, then the forced rescan from shift-R"
    );
    assert!(
        app.last_scan_ms >= 60_000,
        "the sync stamped the clock: {}",
        app.last_scan_ms
    );
}
