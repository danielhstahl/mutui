//! Benchmarks for the two operations whose cost must not grow with the library:
//! sorting (`mutu-2h8.6`) and drawing a frame (`mutu-2h8.9/18`).
//!
//! Run with `cargo bench --bench sort_and_window`.

use std::path::PathBuf;
use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use music_tui::app::{AppState, Options, Phase};
use music_tui::model::{sort_tracks, Track};
use music_tui::ui::render_lines;

/// Build `n` tracks with realistic-looking tags.
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

fn state_with(mut library: Vec<Track>, width: u16, height: u16) -> AppState {
    sort_tracks(&mut library);
    let mut app = AppState::new(Options::default());
    app.set_size(width, height);
    app.set_library(library);
    app.phase = Phase::Ready;
    app
}

fn bench_sort(c: &mut Criterion) {
    let mut group = c.benchmark_group("sort");
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(10);
    for size in [1_000usize, 10_000, 50_000] {
        let library = tracks(size);
        group.bench_function(BenchmarkId::new("unsorted", size), |b| {
            b.iter(|| {
                let mut library = black_box(library.clone());
                sort_tracks(&mut library);
                black_box(library.len())
            })
        });
    }
    // Re-sorting an already sorted list is what a rescan does.
    for size in [10_000usize, 50_000] {
        let mut library = tracks(size);
        sort_tracks(&mut library);
        group.bench_function(BenchmarkId::new("already_sorted", size), |b| {
            b.iter(|| {
                let mut library = black_box(library.clone());
                sort_tracks(&mut library);
                black_box(library.len())
            })
        });
    }
    group.finish();
}

fn bench_render(c: &mut Criterion) {
    let mut group = c.benchmark_group("render");
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(20);
    for size in [100usize, 10_000, 50_000] {
        let app = state_with(tracks(size), 120, 40);
        group.bench_function(BenchmarkId::new("frame", size), |b| {
            b.iter(|| {
                let lines = render_lines(black_box(&app), 120, 40);
                black_box(lines.len())
            })
        });
    }
    group.finish();
}

fn bench_scroll(c: &mut Criterion) {
    let mut scroll = music_tui::ui::Scroll::new();
    scroll.set_content(50_000);
    scroll.set_visible(40);
    c.bench_function("scroll_scroll_by_rows", |b| {
        b.iter(|| {
            for _ in 0..100 {
                scroll.scroll_by(black_box(3));
            }
        })
    });
    c.bench_function("scroll_thumb", |b| b.iter(|| black_box(scroll.thumb())));
}

criterion_group!(benches, bench_sort, bench_render, bench_scroll);
criterion_main!(benches);
