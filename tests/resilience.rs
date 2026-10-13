//! Resilience: resizes, empty folders, vanished files, rescans and the exact
//! rendered output (`mutu-2h8.22`, plus snapshot coverage for `mutu-2h8.9/14`).

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use music_tui::app::{reduce, AppState, Msg, Notice, Options, Phase, PlaybackState};
use music_tui::audio::PlayerEvent;
use music_tui::model::{fixture_tracks, sort_tracks, Track};
use music_tui::scan::{ScanMessage, ScanPhase, ScanProgress, ScanReport};
use music_tui::testing::FixtureSet;

fn key(code: KeyCode) -> Msg {
    Msg::Key(KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    })
}

fn click(column: u16, row: u16) -> Msg {
    Msg::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn ready_state(width: u16, height: u16) -> AppState {
    let mut tracks = fixture_tracks();
    sort_tracks(&mut tracks);
    let mut app = AppState::new(Options {
        folder: "/spec".into(),
        backend: "silent".into(),
        ..Options::default()
    });
    app.set_size(width, height);
    app.set_library(tracks);
    app.phase = Phase::Ready;
    app
}

fn frame(app: &AppState, width: u16, height: u16) -> String {
    music_tui::ui::render_lines(app, width, height).join("\n")
}

#[test]
fn resize_storm_keeps_everything_in_range() {
    let mut app = ready_state(80, 24);
    let sizes = [
        (80u16, 24u16),
        (1, 1),
        (2, 1),
        (3, 2),
        (20, 3),
        (43, 6),
        (44, 6),
        (200, 500),
        (12, 4000),
        (80, 24),
    ];
    for (width, height) in sizes {
        reduce(&mut app, Msg::Resized { width, height }, 1_000);
        reduce(&mut app, key(KeyCode::End), 1_001);
        reduce(&mut app, key(KeyCode::Down), 1_002);
        reduce(&mut app, Msg::Tick, 1_003);
        assert!(
            app.scroll.offset() <= app.scroll.max_offset(),
            "{width}x{height}: offset {} > max {}",
            app.scroll.offset(),
            app.scroll.max_offset()
        );
        assert!(
            app.playlist
                .cursor()
                .is_some_and(|c| c < app.playlist.len()),
            "{width}x{height}: cursor out of range"
        );
        // Rendering at any size must not panic.
        let _ = frame(&app, width, height);
    }
}

#[test]
fn renders_at_pathological_sizes_without_panicking() {
    for (width, height) in [(1u16, 1u16), (2, 1), (5, 2), (10, 3), (240, 60)] {
        let mut app = ready_state(width, height);
        reduce(&mut app, click(1, 1), 10);
        let text = frame(&app, width, height);
        assert_eq!(
            text.lines().count(),
            usize::from(height),
            "{width}x{height} must render exactly `height` lines"
        );
        app.set_library(Vec::new());
        let _ = frame(&app, width, height);
    }
}

#[test]
fn snapshot_list_at_a_normal_size() {
    let app = ready_state(72, 14);
    insta::assert_snapshot!(frame(&app, 72, 14));
}

#[test]
fn snapshot_with_a_playing_track_and_notice() {
    let mut app = ready_state(72, 14);
    reduce(&mut app, click(1, 4), 10_000);
    let id = app.playback.track_id().expect("id");
    reduce(
        &mut app,
        Msg::Player(PlayerEvent::Position {
            track_id: id,
            position: std::time::Duration::from_secs(95),
        }),
        10_100,
    );
    insta::assert_snapshot!(frame(&app, 72, 14));
}

#[test]
fn snapshot_of_an_empty_folder() {
    let mut app = AppState::new(Options {
        folder: "/home/user/Music".into(),
        ..Options::default()
    });
    app.set_size(72, 14);
    reduce(
        &mut app,
        Msg::Scan(ScanMessage::Done(ScanReport {
            empty: true,
            extensions: vec!["mp3".into(), "flac".into(), "ogg".into()],
            ..ScanReport::default()
        })),
        500,
    );
    insta::assert_snapshot!(frame(&app, 72, 14));
}

#[test]
fn snapshot_while_scanning() {
    let mut app = AppState::new(Options::default());
    app.set_size(72, 14);
    reduce(
        &mut app,
        Msg::Scan(ScanMessage::Progress(ScanProgress {
            indexed: 1_240,
            total_estimate: 5_000,
            phase: ScanPhase::Tagging,
        })),
        500,
    );
    let mut tracks = fixture_tracks();
    sort_tracks(&mut tracks);
    for track in tracks.iter().take(6) {
        reduce(&mut app, Msg::Scan(ScanMessage::Track(track.clone())), 520);
    }
    insta::assert_snapshot!(frame(&app, 72, 14));
}

#[test]
fn snapshot_help_overlay() {
    let mut app = ready_state(72, 20);
    reduce(&mut app, key(KeyCode::Char('?')), 1_000);
    assert!(app.show_help);
    let text = frame(&app, 72, 20);
    insta::assert_snapshot!(text);
    for needle in [
        "click row",
        "wheel",
        "drag scrollbar",
        "Enter",
        "Space",
        "n / p",
        "q, Esc, Ctrl-C",
    ] {
        assert!(text.contains(needle), "help must document {needle:?}");
    }
    // Any key closes it again.
    reduce(&mut app, key(KeyCode::Esc), 1_100);
    assert!(!app.show_help);
}

#[test]
fn empty_folder_has_its_own_message_and_stays_usable() {
    let mut app = AppState::new(Options {
        folder: "/tmp/nowhere".into(),
        ..Options::default()
    });
    app.set_size(60, 10);
    reduce(
        &mut app,
        Msg::Scan(ScanMessage::Done(ScanReport {
            empty: true,
            extensions: vec!["mp3".into()],
            ..ScanReport::default()
        })),
        100,
    );
    assert_eq!(app.phase, Phase::Ready);
    let notice = app.notice.as_ref().expect("message");
    assert!(notice.text.contains("No audio files found in /tmp/nowhere"));
    // Scrolling and clicking on an empty list are no-ops, not panics.
    reduce(&mut app, key(KeyCode::PageDown), 200);
    reduce(&mut app, click(5, 3), 300);
    assert_eq!(app.playback, PlaybackState::Idle);
    assert_eq!(app.scroll.offset(), 0);
    // The message is not erased by ordinary interaction.
    assert!(app.notice.is_some());
}

#[test]
fn folder_that_vanished_after_starting_reports_clearly() {
    let mut app = ready_state(60, 12);
    // A rescan finds the folder gone.
    reduce(
        &mut app,
        Msg::Scan(ScanMessage::Done(ScanReport {
            failed: vec![(
                PathBuf::from("/spec/gone.mp3"),
                "folder is not readable (check permissions)".to_string(),
            )],
            ..ScanReport::default()
        })),
        500,
    );
    assert!(app
        .notice
        .as_ref()
        .is_some_and(|n| n.text.contains("could not be read")));
}

#[test]
fn file_deleted_mid_play_is_skipped_and_the_run_continues() {
    let mut app = ready_state(60, 12);
    reduce(&mut app, click(1, 3), 1_000);
    let id = app.playback.track_id().expect("id");
    // The decoder fails because the file disappeared mid-buffer.
    reduce(
        &mut app,
        Msg::Player(PlayerEvent::Failed {
            track_id: id,
            reason: "file not found".into(),
        }),
        1_500,
    );
    assert_eq!(app.playlist.cursor(), Some(4), "the run continues");
    assert!(app
        .notice
        .as_ref()
        .is_some_and(|n| n.text.contains("Skipping")));
    // Then a rescan discovers the file is really gone.
    let mut tracks: Vec<Track> = app
        .playlist
        .tracks()
        .iter()
        .filter(|t| t.path.as_path() != std::path::Path::new("/spec/ab-album1-d1t2.mp3"))
        .cloned()
        .collect();
    sort_tracks(&mut tracks);
    let anchor = app
        .playlist
        .current()
        .cloned()
        .or_else(|| Some(Track::from_path("/spec/ab-album1-d1t2.mp3".into())));
    reduce(
        &mut app,
        Msg::Scan(ScanMessage::Done(ScanReport::default())),
        2_000,
    );
    app.rescan_set_library(tracks, anchor.as_ref());
    assert_eq!(app.playlist.len(), 21);
    assert!(
        app.playlist.run().is_some(),
        "run recomputed after the rescan"
    );
    assert!(app
        .playlist
        .cursor()
        .is_some_and(|c| c < app.playlist.len()));
}

#[test]
fn rescan_keeps_the_current_song_playing() {
    let mut app = ready_state(60, 12);
    reduce(&mut app, click(1, 5), 1_000);
    let playing = app.playlist.current().cloned().expect("a track");
    let mut tracks: Vec<Track> = app.playlist.tracks().to_vec();
    // A new song by the same artist appears in the middle of the run.
    let mut extra = Track::from_path(PathBuf::from("/spec/ab-album1-d1t1b.mp3"));
    extra.artist = Some("Alpha Beta".into());
    extra.album = Some("Album 1".into());
    extra.track_no = Some(1);
    extra.refresh_key();
    tracks.push(extra);
    sort_tracks(&mut tracks);
    app.rescan_set_library(tracks, Some(&playing));
    assert_eq!(
        app.playlist.current().map(|t| t.path.clone()),
        Some(playing.path.clone()),
        "the same song must still be the current row"
    );
    assert_eq!(
        app.playback.index(),
        app.playlist.cursor(),
        "playback follows the re-anchored cursor"
    );
    // The run grew, and the stop point moved with it.
    let (_start, end) = app.playlist.run().expect("a run");
    let names: Vec<String> = app
        .playlist
        .tracks()
        .iter()
        .take(end)
        .filter(|t| t.display_artist() == "Alpha Beta")
        .map(|t| t.title.clone())
        .collect();
    assert!(
        names.len() >= 3,
        "the run picked up the new file: {names:?}"
    );
}

#[test]
fn rescan_that_removes_the_current_row_reanchorsto_the_same_artist() {
    let mut app = ready_state(60, 12);
    reduce(&mut app, click(1, 4), 1_000);
    let playing = app.playlist.current().cloned().expect("a track");
    let mut tracks: Vec<Track> = app
        .playlist
        .tracks()
        .iter()
        .filter(|t| t.path != playing.path)
        .cloned()
        .collect();
    sort_tracks(&mut tracks);
    app.rescan_set_library(tracks, Some(&playing));
    assert_eq!(app.playlist.len(), 21);
    assert_eq!(
        app.playlist
            .current()
            .map(|t| t.display_artist().to_string()),
        Some("Alpha Beta".to_string()),
        "the cursor must stay inside the same artist run"
    );
}

#[test]
fn library_can_grow_and_shrink_without_breaking_the_view() {
    let mut app = AppState::new(Options::default());
    app.set_size(60, 10);
    for round in 0..5 {
        let mut tracks = fixture_tracks();
        sort_tracks(&mut tracks);
        if round % 2 == 0 {
            tracks.truncate(3);
        }
        app.set_library(tracks);
        app.phase = Phase::Ready;
        reduce(&mut app, key(KeyCode::End), 1_000 + round);
        reduce(&mut app, key(KeyCode::Home), 1_010 + round);
        reduce(&mut app, key(KeyCode::PageDown), 1_020 + round);
        assert!(app.scroll.offset() <= app.scroll.max_offset());
        assert!(app.playlist.cursor().is_some());
    }
}

#[test]
fn clicking_outside_the_list_never_changes_playback() {
    let mut app = ready_state(60, 12);
    let before = app.playback;
    // Below the list (row >= list height), on the footer row, and off-screen.
    let list_bottom = app.areas.list.bottom();
    for row in [list_bottom, list_bottom + 1, 500] {
        reduce(&mut app, click(30, row), 1_000 + u64::from(row));
    }
    reduce(&mut app, click(80, 4), 1_500);
    assert_eq!(app.playback, before);
    assert_eq!(app.playlist.cursor(), None, "nothing was selected yet");
    // A drag that did not start on the scrollbar must not scroll.
    let offset = app.scroll.offset();
    reduce(
        &mut app,
        mouse(MouseEventKind::Drag(MouseButton::Left), 10, 1),
        2_000,
    );
    assert_eq!(app.scroll.offset(), offset);
    assert!(!app.dragging);
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Msg {
    Msg::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

#[test]
fn wheel_hits_the_bounds_and_stops_there() {
    let mut app = ready_state(60, 10);
    for _ in 0..200 {
        reduce(&mut app, mouse(MouseEventKind::ScrollDown, 5, 4), 1_000);
    }
    assert_eq!(app.scroll.offset(), app.scroll.max_offset());
    for _ in 0..200 {
        reduce(&mut app, mouse(MouseEventKind::ScrollUp, 5, 4), 1_100);
    }
    assert_eq!(app.scroll.offset(), 0);
    // Scrolling with the wheel over the scrollbar gutter works too.
    let gutter = app.areas.gutter.left();
    reduce(
        &mut app,
        mouse(MouseEventKind::ScrollDown, gutter, 4),
        1_200,
    );
    assert!(app.scroll.offset() > 0);
}

#[test]
fn notices_do_not_stack_and_the_latest_wins() {
    let mut app = ready_state(60, 10);
    app.notice = Some(Notice::warn("first", 1_000));
    reduce(
        &mut app,
        Msg::Player(PlayerEvent::Notice("second".into())),
        2_000,
    );
    assert_eq!(app.notice.as_ref().expect("notice").text, "second");
}

#[test]
fn stale_position_from_a_deleted_file_is_dropped() {
    let mut app = ready_state(60, 10);
    reduce(&mut app, click(1, 2), 1_000);
    let id = app.playback.track_id().expect("id");
    reduce(
        &mut app,
        Msg::Player(PlayerEvent::Position {
            track_id: id,
            position: std::time::Duration::from_secs(4),
        }),
        1_100,
    );
    // Quit resets everything; a late event must not revive the state.
    reduce(&mut app, Msg::Quit, 1_200);
    reduce(
        &mut app,
        Msg::Player(PlayerEvent::Position {
            track_id: id,
            position: std::time::Duration::from_secs(9),
        }),
        1_300,
    );
    assert_eq!(app.position, std::time::Duration::from_secs(4));
}

#[test]
fn unsupported_backend_control_surfaces_instead_of_stopping_playback() {
    let mut app = ready_state(60, 10);
    reduce(&mut app, click(1, 2), 1_000);
    reduce(
        &mut app,
        Msg::Player(PlayerEvent::Notice(
            "backend does not support seeking".into(),
        )),
        1_100,
    );
    assert!(matches!(app.playback, PlaybackState::Playing { .. }));
    assert!(app
        .notice
        .as_ref()
        .is_some_and(|n| n.text.contains("seeking")));
}

#[test]
fn fixtures_written_to_disk_survive_a_full_rescan_cycle() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_two_artist_library(dir.path());
    let first = scan_tracks(dir.path());
    let second = scan_tracks(dir.path());
    assert_eq!(
        first.iter().map(|t| t.key.clone()).collect::<Vec<_>>(),
        second.iter().map(|t| t.key.clone()).collect::<Vec<_>>(),
        "two scans of the same folder must produce the same order"
    );
    let mut app = AppState::new(Options::default());
    app.set_size(60, 12);
    app.set_library(first);
    app.phase = Phase::Ready;
    reduce(&mut app, click(1, 0), 1_000);
    app.rescan_set_library(second, app.playlist.current().cloned().as_ref());
    assert!(app.playlist.cursor().is_some());
}

fn write_two_artist_library(dir: &Path) {
    FixtureSet::new(dir)
        .song("a/1.wav", "A One", "Artist A", "Album", 1, 1, 2000)
        .song("a/2.wav", "A Two", "Artist A", "Album", 1, 2, 2000)
        .song("b/1.wav", "B One", "Artist B", "Album", 1, 1, 2001)
        .write_all()
        .expect("write");
}

fn scan_tracks(dir: &Path) -> Vec<Track> {
    let (tx, rx) = std::sync::mpsc::channel();
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    music_tui::scan::scan(dir, &music_tui::scan::ScanConfig::default(), &tx, &cancel);
    let mut tracks = Vec::new();
    while let Ok(message) = rx.recv() {
        match message {
            ScanMessage::Track(track) => tracks.push(track),
            // `Progress` may arrive anywhere in the stream; `Done` ends it.
            ScanMessage::Done(_) => break,
            ScanMessage::Progress(_) => {}
        }
    }
    sort_tracks(&mut tracks);
    tracks
}
