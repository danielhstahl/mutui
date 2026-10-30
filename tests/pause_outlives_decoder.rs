//! The report that survived `mutu-1sg` and `mutu-nb3`, end to end: pause a song,
//! walk away for a while, press `Space` — and the *next* song starts.
//!
//! These runs use a **real decoder process** (a shell script, spawned and
//! signalled by the real `CommandBackend`) on a real audio worker thread, inside
//! the real event loop, because the fault is a property of the process: `SIGSTOP`
//! stops a decoder but not the clock it measures itself against, and a stopped
//! process cannot exit. So a player that runs out of song — or whose audio
//! session is dropped — while it is stopped is only noticed when `SIGCONT` wakes
//! it, by which time the application has already been told the song is playing.
//!
//! That end has to belong to the pause, not to the run.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use music_tui::app::{
    AppState, CaptureDrawer, Input, Options, PlaybackState, ScriptedInput, SystemClock,
};
use music_tui::audio::{CommandBackend, KNOWN_PLAYERS, PlayerHandle, PlayerSpec};
use music_tui::model::Track;
use music_tui::scan::{ScanConfig, ScanMessage};
use music_tui::testing::FixtureSet;

/// Four songs, two artists: if the run walks on, it lands on the next row.
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
    let report = music_tui::scan::scan(dir, &ScanConfig::default(), &tx, &cancel);
    assert_eq!(report.scanned, 4, "{report:?}");
    let mut tracks = Vec::new();
    while let Ok(message) = rx.recv() {
        match message {
            ScanMessage::Track(track) => tracks.push(track),
            ScanMessage::Done(_) => break,
            ScanMessage::Progress(_) => {}
        }
    }
    music_tui::model::sort_tracks(&mut tracks);
    tracks
}

/// A decoder that runs out of song the first time it plays and behaves after
/// that: the first spawn ends ~0.3 s in (in this test, while it is stopped), and
/// the replay of the same song plays on.
fn one_shot_decoder(dir: &Path) -> PlayerSpec {
    let script = dir.join("decoder");
    let marker = dir.join("first-run");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ -e '{}' ]; then exec sleep 300; fi\n: > '{}'\nexec sleep 0.3\n",
            marker.display(),
            marker.display()
        ),
    )
    .expect("script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let bin = Box::leak(script.to_string_lossy().into_owned().into_boxed_str());
    PlayerSpec {
        bin,
        label: "fakedecoder",
        ..KNOWN_PLAYERS[0].clone()
    }
}

fn click(row: usize) -> Input {
    Input::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 5,
        row: u16::try_from(row).unwrap_or(0),
        modifiers: KeyModifiers::NONE,
    })
}

fn space() -> Input {
    Input::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))
}

fn wait(ms: u64) -> Input {
    Input::Wait(Duration::from_millis(ms))
}

#[test]
fn space_after_a_long_pause_brings_back_the_same_song() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tracks = library(dir.path());
    let clicked = tracks[1].path.clone();
    assert_eq!(
        clicked
            .file_name()
            .map(|name| name.to_string_lossy().into_owned()),
        Some("two.wav".to_string()),
        "this test clicks row 1 of the sorted library"
    );

    let player = PlayerHandle::spawn(Box::new(CommandBackend::new(one_shot_decoder(dir.path()))));
    let mut app = AppState::new(Options {
        folder: dir.path().to_path_buf(),
        backend: "fakedecoder".into(),
        ..Options::default()
    });
    app.set_size(60, 14);
    app.set_library(tracks);
    app.phase = music_tui::app::Phase::Ready;
    let mut input = ScriptedInput::new([
        Input::Resize {
            width: 60,
            height: 14,
        },
        click(1),
        wait(80),
        space(),
        // The "few minutes" of the report, compressed: the decoder is stopped,
        // runs out of song behind the stop, and cannot be reaped there.
        wait(700),
        space(),
        wait(400),
    ]);
    let mut drawer = CaptureDrawer {
        size: (60, 14),
        ..CaptureDrawer::default()
    };
    let rx = Arc::new(Mutex::new(mpsc::channel().1));
    let mut no_rescan = |_force| {};
    music_tui::run::run_headless(
        &mut app,
        &player,
        &rx,
        &mut input,
        &mut drawer,
        &SystemClock,
        &mut no_rescan,
        Some(60),
    )
    .expect("loop");
    let row = app.playback.index().expect("still holding a row");
    let track_id = app.playback.track_id();
    let footer = drawer.last_frame.join("\n");
    player.shutdown();

    assert_eq!(
        app.playlist.get(row).map(|track| track.path.clone()),
        Some(clicked.clone()),
        "`Space` brought back the paused song, not its neighbour"
    );
    assert_eq!(
        app.playlist.cursor(),
        Some(row),
        "and the selection came with it"
    );
    assert!(
        matches!(app.playback, PlaybackState::Playing { .. }),
        "the song is playing, not a frozen `Paused`: {:?}",
        app.playback
    );
    assert!(
        track_id.is_some_and(|id| id > 1),
        "the paused request was replaced by a fresh play of the same song: {track_id:?}"
    );
    assert!(
        footer.contains("ended while paused"),
        "the footer says what happened: {footer}"
    );
}
