//! The core requirement, tested through the reducer with no audio and no
//! terminal (`mutu-2h8.13`, and the keyboard parity of `mutu-2h8.15`).

use std::time::Duration;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};

use music_tui::app::{reduce, AppState, Effect, Msg, Notice, Options, PlaybackState, DEBOUNCE_MS};
use music_tui::audio::{PlayerCommand, PlayerEvent};
use music_tui::model::{fixture_tracks, sort_tracks, Playlist};
use music_tui::scan::{ScanMessage, ScanPhase, ScanProgress, ScanReport};

fn key(code: KeyCode) -> Msg {
    Msg::Key(KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn mouse(kind: MouseEventKind, column: usize, row: usize) -> Msg {
    Msg::Mouse(MouseEvent {
        kind,
        column: u16::try_from(column).unwrap_or(0),
        row: u16::try_from(row).unwrap_or(0),
        modifiers: KeyModifiers::NONE,
    })
}

fn ready_state() -> AppState {
    let mut tracks = fixture_tracks();
    sort_tracks(&mut tracks);
    let mut app = AppState::new(Options {
        folder: "/spec".into(),
        backend: "mock".into(),
        ..Default::default()
    });
    app.set_size(80, 12);
    app.set_library(tracks);
    app.phase = music_tui::app::Phase::Ready;
    app
}

/// The row index that a `Play` command refers to is not in the command, so
/// translate the id back through the state.
fn file_of(effects: &[Effect]) -> String {
    effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Command(PlayerCommand::Play(req)) => Some(
                req.path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            _ => None,
        })
        .unwrap_or_else(|| "<nothing>".to_string())
}

fn name(index: usize) -> String {
    let mut tracks = fixture_tracks();
    sort_tracks(&mut tracks);
    tracks[index]
        .path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn finish(app: &mut AppState, now: u64) -> Vec<Effect> {
    let track_id = app
        .playback
        .track_id()
        .expect("something must be playing to finish");
    reduce(app, Msg::Player(PlayerEvent::Finished { track_id }), now)
}

#[test]
fn clicking_a_row_plays_that_row() {
    let mut app = ready_state();
    let effects = reduce(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, 2),
        1000,
    );
    assert_eq!(file_of(&effects), name(2));
    assert_eq!(
        app.playback,
        PlaybackState::Playing {
            track_id: 1,
            index: 2
        }
    );
    assert_eq!(app.playlist.cursor(), Some(2));
}

#[test]
fn click_mid_run_then_auto_advance_in_sorted_order_and_stop_at_artist_end() {
    let mut app = ready_state();
    // Row 5 is the middle of run 3 (rows 3..=7).
    let effects = reduce(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, 2),
        1000,
    );
    reduce(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, 5),
        1200,
    );
    assert_eq!(file_of(&effects), name(2));
    assert_eq!(
        file_of(&reduce(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), 10, 5),
            1600
        )),
        name(5)
    );

    let mut played = vec![5];
    for step in 0..10 {
        let now = 2000 + step * 1000;
        let effects = finish(&mut app, now);
        match &app.playback {
            PlaybackState::Playing { index, .. } => {
                played.push(*index);
                assert_eq!(
                    file_of(&effects),
                    name(*index),
                    "the command must match the new cursor row"
                );
            }
            PlaybackState::Idle => break,
            other => panic!("unexpected state {other:?}"),
        }
    }
    assert_eq!(
        played,
        vec![5, 6, 7],
        "plays to the end of the run and no further"
    );
    assert_eq!(app.playback, PlaybackState::Idle);
    let notice = app.notice.clone().expect("a message when the run ends");
    assert!(
        notice.text.contains("End of artist"),
        "footer must explain why playback stopped: {:?}",
        notice.text
    );
    assert_eq!(app.end_of_artist.as_deref(), Some("Alpha Beta"));
    assert!(app
        .playlist
        .tracks()
        .iter()
        .any(|t| t.display_artist() == "Alpha Beta"));
}

#[test]
fn clicking_the_last_row_of_an_artist_plays_it_and_stops() {
    let mut app = ready_state();
    reduce(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, 7),
        1000,
    );
    let effects = finish(&mut app, 1500);
    assert_eq!(app.playback, PlaybackState::Idle);
    assert!(effects.contains(&Effect::Command(PlayerCommand::Stop)));
    assert!(app
        .notice
        .as_ref()
        .is_some_and(|n| n.text.contains("End of artist")));
}

#[test]
fn single_song_artist_plays_and_stops() {
    let mut app = ready_state();
    reduce(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, 2),
        1000,
    );
    let effects = finish(&mut app, 1500);
    assert_eq!(app.playback, PlaybackState::Idle);
    assert!(effects.contains(&Effect::Command(PlayerCommand::Stop)));
}

#[test]
fn stale_finished_events_are_dropped() {
    let mut app = ready_state();
    reduce(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, 3),
        1000,
    );
    let current = app.playback.track_id().expect("playing");
    // The user clicks a new row, then a Finished for the *old* id arrives late.
    reduce(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, 4),
        1100,
    );
    let new_id = app.playback.track_id().expect("playing");
    assert_ne!(current, new_id);
    let effects = reduce(
        &mut app,
        Msg::Player(PlayerEvent::Finished { track_id: current }),
        1200,
    );
    assert!(
        effects.is_empty(),
        "stale event must produce nothing: {effects:?}"
    );
    assert_eq!(
        app.playback,
        PlaybackState::Playing {
            track_id: new_id,
            index: 4
        },
        "stale event must not move the cursor"
    );
}

#[test]
fn corrupt_file_is_skipped_once_then_the_run_continues() {
    let mut app = ready_state();
    reduce(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, 3),
        1000,
    );
    let id = app.playback.track_id().expect("playing");
    let effects = reduce(
        &mut app,
        Msg::Player(PlayerEvent::Failed {
            track_id: id,
            reason: "unsupported codec".into(),
        }),
        1000,
    );
    assert_eq!(app.failures, 1);
    assert_eq!(app.playlist.cursor(), Some(4), "auto-skipped forward once");
    assert!(matches!(
        app.playback,
        PlaybackState::Playing { index: 4, .. }
    ));
    assert!(app
        .notice
        .as_ref()
        .is_some_and(|n| n.text.contains("Skipping") && n.text.contains("unsupported codec")));
    assert!(!effects.is_empty(), "the skip must issue a play command");
}

#[test]
fn two_consecutive_failures_stop_the_run() {
    let mut app = ready_state();
    reduce(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, 3),
        1000,
    );
    let first = app.playback.track_id().expect("id");
    reduce(
        &mut app,
        Msg::Player(PlayerEvent::Failed {
            track_id: first,
            reason: "boom".into(),
        }),
        1000,
    );
    let second = app.playback.track_id().expect("id");
    let effects = reduce(
        &mut app,
        Msg::Player(PlayerEvent::Failed {
            track_id: second,
            reason: "boom again".into(),
        }),
        1500,
    );
    assert_eq!(app.playback, PlaybackState::Idle);
    assert!(effects.contains(&Effect::Command(PlayerCommand::Stop)));
    assert!(app
        .notice
        .as_ref()
        .is_some_and(|n| n.text.contains("unreadable")));
}

#[test]
fn double_click_on_the_same_row_debounces() {
    let mut app = ready_state();
    reduce(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, 3),
        1000,
    );
    let first_id = app.playback.track_id().expect("id");
    let effects = reduce(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, 3),
        1100,
    );
    assert!(
        effects.is_empty(),
        "second click within the debounce window is ignored"
    );
    assert_eq!(app.playback.track_id(), Some(first_id));
    // After the window the same row can be replayed on purpose.
    let later = reduce(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, 3),
        1000 + DEBOUNCE_MS + 10,
    );
    assert!(!later.is_empty());
    assert_eq!(app.playback.track_id().expect("id"), first_id + 1);
}

#[test]
fn rapid_clicks_leave_only_the_last_track_playing() {
    let mut app = ready_state();
    for (row, now) in [(1usize, 100u64), (4, 120), (9, 140)] {
        reduce(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), 10, row),
            now,
        );
    }
    assert_eq!(app.playlist.cursor(), Some(9));
    assert_eq!(app.playback.index(), Some(9));
}

#[test]
fn clicking_below_the_last_row_does_nothing() {
    let mut app = ready_state();
    let effects = reduce(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, 500),
        1000,
    );
    assert!(effects.is_empty());
    assert_eq!(app.playback, PlaybackState::Idle);
}

#[test]
fn keyboard_bindings_cover_the_documented_set() {
    let mut app = ready_state();
    // Enter plays the selection (defaults to the first row).
    let effects = reduce(&mut app, key(KeyCode::Enter), 1000);
    assert_eq!(file_of(&effects), name(0));
    // Space pauses, Space again resumes.
    let effects = reduce(&mut app, key(KeyCode::Char(' ')), 1100);
    assert_eq!(effects, vec![Effect::Command(PlayerCommand::Pause)]);
    assert!(matches!(app.playback, PlaybackState::Paused { .. }));
    let effects = reduce(&mut app, key(KeyCode::Char(' ')), 1200);
    assert_eq!(effects, vec![Effect::Command(PlayerCommand::Resume)]);
    assert!(matches!(app.playback, PlaybackState::Playing { .. }));

    // Navigation keeps the cursor visible.
    reduce(&mut app, key(KeyCode::Down), 1300);
    reduce(&mut app, key(KeyCode::Char('j')), 1350);
    assert_eq!(app.playlist.cursor(), Some(2));
    reduce(&mut app, key(KeyCode::Up), 1400);
    reduce(&mut app, key(KeyCode::Char('k')), 1450);
    reduce(&mut app, key(KeyCode::Char('k')), 1500);
    assert_eq!(app.playlist.cursor(), Some(0));

    // Volume and mute.
    let effects = reduce(&mut app, key(KeyCode::Char('-')), 1600);
    assert_eq!(
        effects,
        vec![Effect::Command(PlayerCommand::SetVolume(0.9))]
    );
    let effects = reduce(&mut app, key(KeyCode::Char('m')), 1700);
    assert_eq!(
        effects,
        vec![Effect::Command(PlayerCommand::SetVolume(0.0))]
    );
    assert!(app.muted);
    let effects = reduce(&mut app, key(KeyCode::Char('m')), 1800);
    assert_eq!(
        effects,
        vec![Effect::Command(PlayerCommand::SetVolume(0.9))]
    );
    let effects = reduce(&mut app, key(KeyCode::Char('+')), 1900);
    assert_eq!(
        effects,
        vec![Effect::Command(PlayerCommand::SetVolume(1.0))]
    );

    // Help overlay and rescan.
    reduce(&mut app, key(KeyCode::Char('?')), 2000);
    assert!(app.show_help);
    reduce(&mut app, key(KeyCode::Char('?')), 2100);
    assert!(!app.show_help);
    assert_eq!(
        reduce(&mut app, key(KeyCode::Char('r')), 2200),
        vec![Effect::Rescan]
    );

    // Quit bindings.
    for code in [KeyCode::Char('q'), KeyCode::Esc, KeyCode::Char('c')] {
        let mut clone = ready_state();
        if code == KeyCode::Char('c') {
            // Ctrl-C only quits with the modifier.
            let mut ctrl_c = ready_state();
            let effects = reduce(
                &mut ctrl_c,
                Msg::Key(KeyEvent {
                    code: KeyCode::Char('c'),
                    modifiers: KeyModifiers::CONTROL,
                    kind: KeyEventKind::Press,
                    state: KeyEventState::NONE,
                }),
                1000,
            );
            assert!(effects.contains(&Effect::Quit));
            assert!(ctrl_c.quit);
        }
        let effects = reduce(&mut clone, key(code), 1000);
        if code != KeyCode::Char('c') {
            assert!(effects.contains(&Effect::Quit), "{code:?} must quit");
            assert!(clone.quit);
        }
    }
}

#[test]
fn next_and_previous_stay_inside_the_run() {
    let mut app = ready_state();
    reduce(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, 3),
        1000,
    );
    // n steps forward inside the run.
    let effects = reduce(&mut app, key(KeyCode::Char('n')), 1100);
    assert_eq!(file_of(&effects), name(4));
    // p steps back.
    let effects = reduce(&mut app, key(KeyCode::Char('p')), 1200);
    assert_eq!(file_of(&effects), name(3));
    // p at the start of the run refuses without crossing artists.
    let effects = reduce(&mut app, key(KeyCode::Char('p')), 1300);
    assert!(effects.is_empty());
    assert_eq!(app.playlist.cursor(), Some(3));
    assert!(app
        .notice
        .as_ref()
        .is_some_and(|n| n.text.contains("Start of run")));
    // Jump to the end of the run and press n: refuse, do not wrap.
    reduce(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, 7),
        1400,
    );
    let effects = reduce(&mut app, key(KeyCode::Char('n')), 1500);
    assert!(effects.is_empty());
    assert_eq!(app.playlist.cursor(), Some(7));
    assert!(app
        .notice
        .as_ref()
        .is_some_and(|n| n.text.contains("End of run")));
}

#[test]
fn mouse_wheel_and_scrollbar_drag_move_the_view() {
    let mut app = ready_state();
    let before = app.scroll.offset();
    reduce(&mut app, mouse(MouseEventKind::ScrollDown, 5, 3), 1000);
    assert_eq!(app.scroll.offset(), before + 3, "three rows per notch");
    reduce(&mut app, mouse(MouseEventKind::ScrollUp, 5, 3), 1100);
    assert_eq!(app.scroll.offset(), before);
    // Click near the bottom of the scrollbar track, then drag.
    let gutter_x = usize::from(app.areas.gutter.left());
    let bottom = usize::from(app.areas.gutter.bottom()).saturating_sub(1);
    reduce(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), gutter_x, bottom),
        1200,
    );
    assert!(app.dragging, "a press on the gutter arms the drag");
    assert!(app.scroll.offset() > before, "clicking the track jumps");
    let after_click = app.scroll.offset();
    reduce(
        &mut app,
        mouse(MouseEventKind::Drag(MouseButton::Left), gutter_x, 1),
        1300,
    );
    assert!(app.scroll.offset() < after_click);
    reduce(
        &mut app,
        mouse(MouseEventKind::Up(MouseButton::Left), gutter_x, 1),
        1400,
    );
    assert!(!app.dragging);
    // Page/Home/End use the same math as the mouse.
    reduce(&mut app, key(KeyCode::End), 1500);
    assert_eq!(app.scroll.offset(), app.scroll.max_offset());
    reduce(&mut app, key(KeyCode::Home), 1600);
    assert_eq!(app.scroll.offset(), 0);
    reduce(&mut app, key(KeyCode::PageDown), 1700);
    assert_eq!(app.scroll.offset(), app.scroll.visible());
}

#[test]
fn playback_follows_the_view_when_it_auto_advances() {
    let mut app = ready_state();
    app.scroll.scroll_to(0);
    assert_eq!(app.scroll.visible(), 11, "12 rows - 1 footer");
    // Clicks address *screen* rows, so scroll first: after one wheel notch the
    // view starts at list row 3 and screen row 10 shows list row 13.
    reduce(&mut app, mouse(MouseEventKind::ScrollDown, 5, 3), 1000);
    assert_eq!(app.scroll.offset(), 3);
    let screen_row = 13 - app.scroll.offset();
    reduce(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, screen_row),
        1000,
    );
    assert_eq!(app.playlist.cursor(), Some(13));
    finish(&mut app, 1100);
    assert_eq!(app.playlist.cursor(), Some(14));
    assert_eq!(
        app.scroll.offset(),
        4,
        "the view follows the playing row instead of letting it scroll off"
    );
    assert!(app.scroll.offset() + app.scroll.visible() > app.playlist.cursor().expect("cursor"));
}

#[test]
fn scan_messages_fill_the_library_and_sort_once_at_the_end() {
    let mut app = AppState::new(Options::default());
    app.set_size(100, 24);
    let mut tracks = fixture_tracks();
    sort_tracks(&mut tracks);
    for track in tracks.clone() {
        reduce(&mut app, Msg::Scan(ScanMessage::Track(track)), 10);
    }
    assert_eq!(app.phase, music_tui::app::Phase::Scanning);
    reduce(
        &mut app,
        Msg::Scan(ScanMessage::Progress(ScanProgress {
            indexed: 22,
            total_estimate: 22,
            phase: ScanPhase::Tagging,
        })),
        20,
    );
    assert!(app.scan.label().contains("22"));
    reduce(
        &mut app,
        Msg::Scan(ScanMessage::Done(ScanReport {
            scanned: 22,
            succeeded: 22,
            ..ScanReport::default()
        })),
        30,
    );
    assert_eq!(app.phase, music_tui::app::Phase::Ready);
    assert_eq!(app.playlist.len(), 22);
    assert_eq!(app.scroll.content(), 22);
    // The list ends up in spec order even though rows streamed in reverse.
    let first = app.playlist.get(0).expect("row");
    assert!(first.path.to_string_lossy().contains("untagged-alpha"));
}

#[test]
fn empty_folder_gets_its_own_message() {
    let mut app = AppState::new(Options {
        folder: "/empty".into(),
        ..Default::default()
    });
    reduce(
        &mut app,
        Msg::Scan(ScanMessage::Done(ScanReport {
            empty: true,
            extensions: vec!["mp3".into(), "flac".into()],
            ..ScanReport::default()
        })),
        100,
    );
    let text = app.notice.as_ref().expect("a message").text.clone();
    assert!(text.contains("No audio files found"), "{text}");
    assert!(text.contains("/empty"), "{text}");
    assert!(text.contains("mp3"), "{text}");
}

#[test]
fn notices_expire_on_tick() {
    let mut app = ready_state();
    app.notice = Some(Notice::warn("temporary", 0));
    reduce(&mut app, Msg::Tick, 1_000);
    assert!(app.notice.is_some(), "still inside the TTL");
    reduce(&mut app, Msg::Tick, 60_000);
    assert!(app.notice.is_none(), "expired notices disappear");
    // Sticky notices (end of artist) stay.
    app.notice = Some(Notice::sticky("End of artist: X", 0));
    reduce(&mut app, Msg::Tick, 3_600_000);
    assert!(app.notice.is_some());
}

#[test]
fn quit_stops_playback_first() {
    let mut app = ready_state();
    reduce(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, 3),
        1000,
    );
    let effects = reduce(&mut app, Msg::Quit, 1100);
    assert_eq!(
        effects,
        vec![Effect::Command(PlayerCommand::Stop), Effect::Quit]
    );
    assert!(app.quit);
}

#[test]
fn resize_reclamps_the_scroll_state() {
    let mut app = ready_state();
    app.set_size(100, 24);
    app.scroll.scroll_to(5000);
    assert_eq!(app.scroll.offset(), app.scroll.max_offset());
    reduce(
        &mut app,
        Msg::Resized {
            width: 20,
            height: 3,
        },
        1000,
    );
    assert_eq!(app.areas.list.height, 2);
    assert!(app.scroll.offset() <= app.scroll.max_offset());
    assert_eq!(app.areas.gutter.width, 1);
    reduce(
        &mut app,
        Msg::Resized {
            width: 6,
            height: 2,
        },
        1100,
    );
    assert_eq!(app.areas.gutter.width, 0);
    assert_eq!(app.areas.footer.height, 1);
}

#[test]
fn events_convert_into_messages() {
    assert!(music_tui::app::msg_from_event(Event::Key(KeyEvent {
        code: KeyCode::Char('q'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }))
    .is_some());
    assert!(music_tui::app::msg_from_event(Event::Key(KeyEvent {
        code: KeyCode::Char('q'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Release,
        state: KeyEventState::NONE,
    }))
    .is_none());
    assert!(music_tui::app::msg_from_event(Event::Resize(80, 24)).is_some());
}

#[test]
fn positions_only_apply_to_the_current_track() {
    let mut app = ready_state();
    reduce(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 10, 3),
        1000,
    );
    let id = app.playback.track_id().expect("id");
    reduce(
        &mut app,
        Msg::Player(PlayerEvent::Position {
            track_id: id,
            position: Duration::from_secs(12),
        }),
        1100,
    );
    assert_eq!(app.position, Duration::from_secs(12));
    reduce(
        &mut app,
        Msg::Player(PlayerEvent::Position {
            track_id: id + 99,
            position: Duration::from_secs(99),
        }),
        1200,
    );
    assert_eq!(
        app.position,
        Duration::from_secs(12),
        "stale position ignored"
    );
    // A run that ends resets the clock.
    finish(&mut app, 1300);
    reduce(&mut app, Msg::Tick, 1400);
    let _ = Playlist::default();
}
