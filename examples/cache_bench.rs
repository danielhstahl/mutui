//! Measure what the library index buys on a real folder:
//!
//! ```sh
//! cargo run --release --example cache_bench -- ~/Music
//! ```
//!
//! Prints the cold scan (every tag read, index written), the warm scan a restart
//! does instead, and what the sort at the end of a scan costs. Writes an index
//! under `$XDG_CACHE_HOME/music-tui/` exactly like the player does; delete it (or
//! pass `--refresh` semantics by deleting the file) to measure cold again.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use music_tui::library_cache::{self, LibraryCache};
use music_tui::model::{sort_tracks, Track};
use music_tui::scan::{self, ScanConfig, ScanMessage, ScanReport};

/// Scan `root` with `cache`, returning the rows, the report and the wall time.
fn timed(root: &Path, cache: Arc<LibraryCache>) -> (Vec<Track>, ScanReport, Duration) {
    let config = ScanConfig {
        cache: Some(cache),
        ..ScanConfig::default()
    };
    let started = Instant::now();
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let root = root.to_path_buf();
    let handle = std::thread::spawn(move || scan::scan(&root, &config, &tx, &cancel));
    let mut tracks = Vec::new();
    let mut report = ScanReport::default();
    while let Ok(message) = rx.recv() {
        match message {
            ScanMessage::Track(track) => tracks.push(track),
            ScanMessage::Done(done) => report = done,
            ScanMessage::Progress(_) => {}
        }
    }
    let _joined = handle.join();
    (tracks, report, started.elapsed())
}

/// Time `sort_tracks` on a copy of `tracks`.
fn timed_sort(tracks: &[Track]) -> (Vec<Track>, Duration) {
    let mut sorted = tracks.to_vec();
    let started = Instant::now();
    sort_tracks(&mut sorted);
    (sorted, started.elapsed())
}

fn main() {
    let Some(folder) = std::env::args().nth(1) else {
        eprintln!("usage: cache_bench <FOLDER>");
        std::process::exit(2);
    };
    let root = PathBuf::from(&folder);
    let config = ScanConfig::default();
    let file = library_cache::default_cache_file(&root, &config);
    let fingerprint = library_cache::config_fingerprint(&root, &config);
    println!("folder : {}", root.display());
    println!("index  : {}", file.display());

    // `refresh = true`: measure a cold scan even when an index already exists.
    let (tracks, cold, cold_time) = timed(
        &root,
        Arc::new(LibraryCache::open(file.clone(), fingerprint, true)),
    );

    // A restart: a brand new cache object reading the file the cold scan wrote.
    let started = Instant::now();
    let warm_cache = Arc::new(LibraryCache::open(file, fingerprint, false));
    let index_load = started.elapsed();
    let (rows, warm, warm_time) = timed(&root, Arc::clone(&warm_cache));
    let (sorted, sort_time) = timed_sort(&rows);

    println!(
        "cold   : {cold_time:>12.0?}  ({} rows read from disk, index load {index_load:?})",
        cold.succeeded
    );
    println!(
        "warm   : {warm_time:>12.0?}  ({} rows served from the index, {} loaded)",
        warm.cached,
        warm_cache.len()
    );
    println!(
        "sort   : {sort_time:>12.0?}  for {} rows (this part runs on the UI thread)",
        sorted.len()
    );
    println!(
        "rows   : {} cold, {} warm, digest {}",
        tracks.len(),
        rows.len(),
        if cold.digest == warm.digest {
            "matches"
        } else {
            "DIFFERS"
        }
    );
    if !cold_time.is_zero() && !warm_time.is_zero() {
        println!(
            "speedup: {:>9.1}x",
            cold_time.as_secs_f64() / warm_time.as_secs_f64()
        );
    }
    if warm.cache_file.is_none() {
        println!("note   : the index was not written (capped or cancelled scan?)");
    }
}
