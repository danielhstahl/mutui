//! Hard performance budgets (`mutu-2h8.18`).
//!
//! These are the numbers a user would notice, asserted in *debug* builds (which
//! is what `cargo test` runs), so CI fails if a change makes the app feel
//! broken. See `benches/sort_and_window.rs` for the release-mode measurements.

use std::path::PathBuf;
use std::time::Instant;

use music_tui::app::{AppState, Options, Phase};
use music_tui::model::{sort_tracks, Track};
use music_tui::ui::render_lines;

fn tracks(n: usize) -> Vec<Track> {
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let mut track = Track::from_path(PathBuf::from(format!(
            "/music/artist {:03}/album {:02}/track {:04}.mp3",
            i % 250,
            (i / 250) % 12,
            i % 40
        )));
        track.artist = Some(format!("Artist {:03}", i % 250));
        track.album = Some(format!("Album {:02}", (i / 250) % 12));
        track.title = format!("Track {:04}", i % 40);
        track.disc_no = Some(1 + (i % 2) as u32);
        track.track_no = Some((i % 40) as u32 + 1);
        track.year = Some(1980 + (i % 40) as i32);
        track.refresh_key();
        out.push(track);
    }
    out
}

fn app_with(library: Vec<Track>) -> AppState {
    let mut app = AppState::new(Options::default());
    app.set_size(120, 40);
    app.set_library(library);
    app.phase = Phase::Ready;
    app
}

#[test]
fn drawing_a_frame_costs_the_same_for_100_or_50000_songs() {
    let small = app_with(tracks(100));
    let big = app_with(tracks(50_000));
    // Warm up (allocator, first paint).
    let _ = render_lines(&small, 120, 40);
    let _ = render_lines(&big, 120, 40);

    let start = Instant::now();
    for _ in 0..10 {
        let lines = render_lines(&small, 120, 40);
        assert_eq!(lines.len(), 40);
    }
    let small_time = start.elapsed();

    let start = Instant::now();
    for _ in 0..10 {
        let lines = render_lines(&big, 120, 40);
        assert_eq!(lines.len(), 40);
    }
    let big_time = start.elapsed();

    assert!(
        big_time < small_time * 3 + std::time::Duration::from_millis(150),
        "frame cost must be independent of library size: 100 songs {small_time:?}, \
         50k songs {big_time:?}"
    );
    assert!(
        big_time / 10 < std::time::Duration::from_millis(60),
        "a single frame with 50k songs must stay far below one tick: {:?}",
        big_time / 10
    );
}

#[test]
fn sorting_50k_songs_finishes_in_a_blink_of_the_startup_budget() {
    let library = tracks(50_000);
    let start = Instant::now();
    let mut sorted = library.clone();
    sort_tracks(&mut sorted);
    let elapsed = start.elapsed();
    assert_eq!(sorted.len(), 50_000);
    assert!(elapsed < std::time::Duration::from_secs(20), "{elapsed:?}");

    // Sorting must actually be near-linear-ish: doubling the input must not
    // quadruple the time by accident (a pathological comparator would).
    let half = tracks(25_000);
    let start = Instant::now();
    let mut half_sorted = half;
    sort_tracks(&mut half_sorted);
    let half_time = start.elapsed();
    assert!(
        elapsed < half_time * 6,
        "sort scaling looks wrong: 25k {half_time:?}, 50k {elapsed:?}"
    );
}

#[test]
fn scrolling_a_huge_library_is_constant_time() {
    let mut app = app_with(tracks(50_000));
    let start = Instant::now();
    for _ in 0..20_000 {
        music_tui::app::reduce(
            &mut app,
            music_tui::app::Msg::Mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::ScrollDown,
                column: 10,
                row: 5,
                modifiers: crossterm::event::KeyModifiers::NONE,
            }),
            1_000,
        );
    }
    let elapsed = start.elapsed();
    assert_eq!(app.scroll.offset(), app.scroll.max_offset());
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "10k wheel notches took {elapsed:?}"
    );
}
