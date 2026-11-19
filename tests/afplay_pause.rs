//! The mac report, end to end: with the player a stock macOS ships (`afplay`),
//! pausing leaves a short blip of music a few seconds later, and unpausing
//! often starts the *next* song.
//!
//! Both are properties of the player, not of the reducer. `afplay` hands its
//! samples to `coreaudiod`, so `SIGSTOP` freezes the process while the audio it
//! already queued keeps playing — and the queue it was feeding finishes or is
//! dropped behind the stop, so the process exits the moment `SIGCONT` wakes it.
//! At that point the application has been told the song is playing again, and an
//! ordinary `Finished` there walks the run on.
//!
//! So an `afplay`-shaped backend is paused by killing it (nothing left to make a
//! sound) and resumed by launching it again. It has no start-position switch,
//! which means the song comes back from the beginning — and says so, rather
//! than starting somebody else's song.

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
use music_tui::audio::{
    CommandBackend, KNOWN_PLAYERS, PauseMode, PlayerHandle, PlayerSpec, SeekArg,
};
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

/// A decoder that is `afplay` for our purposes: a long-running process with no
/// start-position switch, whose audio does not live inside it.
fn afplay_like(dir: &Path) -> PlayerSpec {
    let script = dir.join("fakedecoder");
    std::fs::write(&script, "#!/bin/sh\nexec sleep 300\n").expect("script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let bin = Box::leak(script.to_string_lossy().into_owned().into_boxed_str());
    let afplay = KNOWN_PLAYERS
        .iter()
        .find(|spec| spec.bin == "afplay")
        .expect("afplay is a known player");
    assert_eq!(
        afplay.pause,
        PauseMode::Kill,
        "the real afplay is paused by killing it, or this test proves nothing"
    );
    assert_eq!(
        afplay.seek,
        SeekArg::None,
        "the real afplay has no start-position switch"
    );
    PlayerSpec {
        bin,
        label: "fakedecoder",
        ..afplay.clone()
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
fn pausing_a_player_with_no_pause_stops_the_sound_and_keeps_the_song() {
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

    let player = PlayerHandle::spawn(Box::new(CommandBackend::new(afplay_like(dir.path()))));
    let mut app = AppState::new(Options {
        folder: dir.path().to_path_buf(),
        backend: "fakedecoder".into(),
        ..Options::default()
    });
    app.set_size(100, 14);
    app.set_library(tracks);
    app.phase = music_tui::app::Phase::Ready;
    let mut input = ScriptedInput::new([
        Input::Resize {
            width: 100,
            height: 14,
        },
        click(1),
        wait(200),
        space(),
        // Long enough that a buffered-audio blip would have happened, and long
        // enough for the "afplay died behind the stop" case to bite.
        wait(1_500),
        space(),
        wait(400),
    ]);
    let mut drawer = CaptureDrawer {
        size: (100, 14),
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
        Some(80),
    )
    .expect("loop");
    player.shutdown();

    let row = app.playback.index().expect("still holding a row");
    let footer = drawer.last_frame.join("\n");
    assert_eq!(
        app.playlist.get(row).map(|track| track.path.clone()),
        Some(clicked.clone()),
        "`Space` came back to the song that was paused, never its neighbour"
    );
    assert!(
        matches!(app.playback, PlaybackState::Playing { .. }),
        "the song is playing again: {:?}",
        app.playback
    );
    assert!(
        footer.contains("from the beginning"),
        "the footer says the song restarts at the top: {footer}"
    );
}
