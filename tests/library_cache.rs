//! The library index (`src/library_cache.rs`) and the reducer's half of the deal:
//! warm scans reuse rows, anything that changes the library invalidates exactly
//! the affected rows, and applying a scan never duplicates or reshuffles the list.
//!
//! See `docs/adr/0003-library-cache.md` for the normative rules these tests
//! check.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use music_tui::app::{reduce, AppState, Effect, Msg, Options, Phase};
use music_tui::library_cache::{self, LibraryCache};
use music_tui::model::{fixture_tracks, sort_tracks, Track};
use music_tui::scan::{self, ScanConfig, ScanMessage, ScanReport};
use music_tui::testing::FixtureSet;

/// A small library with real tags in three containers, plus a corrupt file.
fn write_library(root: &Path) -> Vec<PathBuf> {
    FixtureSet::new(root)
        .song("alpha/one.mp3", "One", "Alpha Beta", "Album 1", 1, 1, 1998)
        .song("alpha/two.flac", "Two", "Alpha Beta", "Album 1", 1, 2, 1998)
        .song("beta/one.wav", "One", "Beta Wave", "Double", 1, 1, 1994)
        .untagged("beta/hidden.mp3")
        .write_all()
        .expect("fixtures")
}

/// Same file, retagged (a longer title also changes the size, so the change is
/// visible to the fingerprint without relying on mtime granularity).
fn retag(root: &Path) {
    FixtureSet::new(root)
        .song(
            "alpha/one.mp3",
            "One (2024 remaster)",
            "Alpha Beta",
            "Album 1",
            1,
            1,
            1998,
        )
        .write_all()
        .expect("retag");
}

fn cache_for(root: &Path) -> Arc<LibraryCache> {
    cache_with(root, &ScanConfig::default(), false)
}

/// Keep the tests out of the user's real `$XDG_CACHE_HOME` (the default index
/// location is derived from it).
fn isolate_cache_dir() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("music-tui-index-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("test cache dir");
        std::env::set_var("XDG_CACHE_HOME", &dir);
    });
}

fn cache_with(root: &Path, cfg: &ScanConfig, refresh: bool) -> Arc<LibraryCache> {
    isolate_cache_dir();
    Arc::new(LibraryCache::open(
        library_cache::default_cache_file(root, cfg),
        library_cache::config_fingerprint(root, cfg),
        refresh,
    ))
}

/// Scan on a worker thread, like the app does, and return the sorted rows plus
/// the report.
fn scan(root: &Path, cfg: &ScanConfig) -> (Vec<Track>, ScanReport) {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let root = root.to_path_buf();
    let cfg = cfg.clone();
    let handle = std::thread::spawn(move || scan::scan(&root, &cfg, &tx, &cancel));
    let mut tracks = Vec::new();
    let mut report = ScanReport::default();
    while let Ok(message) = rx.recv() {
        if let ScanMessage::Track(track) = message {
            tracks.push(track);
        } else if let ScanMessage::Done(done) = message {
            report = done;
        }
    }
    let _joined = handle.join();
    sort_tracks(&mut tracks);
    (tracks, report)
}

fn scan_with_cache(root: &Path, cache: Arc<LibraryCache>) -> (Vec<Track>, ScanReport) {
    scan(
        root,
        &ScanConfig {
            cache: Some(cache),
            ..Default::default()
        },
    )
}

/// `(artist, album, title, track)` per row: enough to compare two scans exactly.
fn shape(tracks: &[Track]) -> Vec<(String, String, String, Option<u32>)> {
    tracks
        .iter()
        .map(|track| {
            (
                track.display_artist().to_string(),
                track.display_album().to_string(),
                track.title.clone(),
                track.track_no,
            )
        })
        .collect()
}

fn paths(tracks: &[Track]) -> Vec<String> {
    tracks
        .iter()
        .map(|track| track.path.display().to_string())
        .collect()
}

#[test]
fn cold_scan_writes_the_index_and_warm_scan_never_opens_the_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    write_library(root);

    let cache = cache_for(root);
    assert!(cache.is_empty(), "nothing stored before the first scan");

    let (first, first_report) = scan_with_cache(root, Arc::clone(&cache));
    assert_eq!(first.len(), 4);
    assert_eq!(first_report.cached, 0, "a cold scan reads everything");
    assert_eq!(first_report.succeeded, 4);
    let file = first_report.cache_file.clone().expect("index was written");
    assert!(file.exists(), "index file exists at {}", file.display());

    let (second, second_report) = scan_with_cache(root, Arc::clone(&cache));
    assert_eq!(
        second_report.cached,
        second.len(),
        "a warm scan answers every row from the index"
    );
    assert_eq!(second_report.digest, first_report.digest);
    assert_eq!(
        shape(&second),
        shape(&first),
        "the cached rows must be exactly the rows a cold scan produced"
    );
}

#[test]
fn the_index_survives_a_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    write_library(root);

    let (cold, _) = scan_with_cache(root, cache_for(root));
    // A brand new cache object for the same file is what the next process does.
    let reloaded = cache_for(root);
    assert_eq!(reloaded.len(), cold.len(), "the file holds every row");
    let (warm, report) = scan_with_cache(root, Arc::clone(&reloaded));
    assert_eq!(report.cached, cold.len());
    assert_eq!(shape(&warm), shape(&cold));
}

#[test]
fn a_retagged_file_is_read_again_and_the_digest_moves() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    write_library(root);

    let cache = cache_for(root);
    let (before, before_report) = scan_with_cache(root, Arc::clone(&cache));
    retag(root);
    let (after, after_report) = scan_with_cache(root, Arc::clone(&cache));

    assert_eq!(after_report.cached, 3, "only the changed file was re-read");
    assert_ne!(after_report.digest, before_report.digest);
    let titles: Vec<&str> = after.iter().map(|track| track.title.as_str()).collect();
    assert!(
        titles.contains(&"One (2024 remaster)"),
        "the new tag must reach the UI: {titles:?}"
    );
    assert_eq!(after.len(), before.len());
}

#[test]
fn a_deleted_file_leaves_the_library_and_the_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    write_library(root);

    let cache = cache_for(root);
    let (before, _) = scan_with_cache(root, Arc::clone(&cache));
    assert_eq!(before.len(), 4);

    std::fs::remove_file(root.join("beta/one.wav")).expect("delete");
    let (after, report) = scan_with_cache(root, Arc::clone(&cache));
    assert_eq!(after.len(), 3, "the walk notices the deletion");
    assert_eq!(report.cached, 3);
    assert_eq!(
        cache.len(),
        3,
        "the saved index was pruned to what still exists"
    );
    assert!(!paths(&after).iter().any(|path| path.ends_with("one.wav")));
}

#[test]
fn failures_are_never_cached() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    FixtureSet::new(root)
        .song("alpha/one.mp3", "One", "Alpha Beta", "Album 1", 1, 1, 1998)
        .corrupt("alpha/broken.mp3")
        .write_all()
        .expect("fixtures");

    let cache = cache_for(root);
    let (_, first) = scan_with_cache(root, Arc::clone(&cache));
    let (rows, second) = scan_with_cache(root, Arc::clone(&cache));
    assert_eq!(first.failed.len(), 1, "the broken file is reported");
    assert_eq!(
        second.failed.len(),
        1,
        "and retried by every scan: a failure must never be stored as an answer"
    );
    assert_eq!(second.cached, 1, "the good file is still cached");
    let corrupt = rows
        .iter()
        .find(|track| track.corrupt)
        .expect("the corrupt row is still shown");
    assert!(corrupt.path.to_string_lossy().ends_with("broken.mp3"));
}

#[test]
fn a_capped_scan_does_not_rewrite_the_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    write_library(root);

    let cache = cache_for(root);
    let (_, warm) = scan_with_cache(root, Arc::clone(&cache));
    assert_eq!(warm.cache_file.as_ref().map(|p| p.exists()), Some(true));

    let capped = scan(
        root,
        &ScanConfig {
            max_files: 2,
            cache: Some(Arc::clone(&cache)),
            ..Default::default()
        },
    );
    assert!(capped.1.capped, "the cap was hit");
    assert_eq!(
        capped.1.cache_file, None,
        "a partial scan must not prune the index"
    );
    assert_eq!(cache.len(), 4, "every row is still indexed");
}

#[test]
fn a_garbage_cache_file_is_ignored_not_fatal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    write_library(root);

    let cfg = ScanConfig::default();
    isolate_cache_dir();
    let file = library_cache::default_cache_file(root, &cfg);
    std::fs::create_dir_all(file.parent().expect("parent")).expect("dirs");
    std::fs::write(&file, b"this is not a library cache at all").expect("write garbage");

    let cache = Arc::new(LibraryCache::open(
        file.clone(),
        library_cache::config_fingerprint(root, &cfg),
        false,
    ));
    let (tracks, report) = scan_with_cache(root, Arc::clone(&cache));
    assert_eq!(tracks.len(), 4, "the scan simply starts cold");
    assert_eq!(report.cached, 0);
    assert!(report.cache_file.is_some(), "and the file is repaired");
    let second = cache_for(root);
    let (_, warm) = scan_with_cache(root, Arc::clone(&second));
    assert_eq!(warm.cached, 4, "the repaired index works");
}

#[test]
fn a_forced_rescan_rereads_everything_and_then_repairs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    write_library(root);

    let cache = cache_for(root);
    scan_with_cache(root, Arc::clone(&cache));
    cache.force_full_scan();
    let (_, forced) = scan_with_cache(root, Arc::clone(&cache));
    assert_eq!(forced.cached, 0, "shift-R ignores the index");
    assert!(forced.cache_file.is_some(), "but it still refreshes it");
    let (_, warm) = scan_with_cache(root, Arc::clone(&cache));
    assert_eq!(warm.cached, 4);
}

#[test]
fn other_scanner_flags_use_their_own_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    write_library(root);

    let mp3_only = ScanConfig {
        extensions: vec!["mp3".into()],
        ..Default::default()
    };
    let (_, mp3_scan) = scan(
        root,
        &ScanConfig {
            cache: Some(cache_with(root, &mp3_only, false)),
            extensions: vec!["mp3".into()],
            ..Default::default()
        },
    );
    assert_eq!(mp3_scan.scanned, 2, "only mp3 rows exist in that index");

    let full = cache_for(root);
    let (_, everything) = scan_with_cache(root, Arc::clone(&full));
    assert_eq!(everything.scanned, 4);
    assert_eq!(
        full.len(),
        4,
        "the mp3-only scan wrote its own file and left this one alone"
    );
}

// ---------------------------------------------------------------------------
// the reducer side
// ---------------------------------------------------------------------------

fn app_after_first_scan(stream: usize) -> AppState {
    let mut app = AppState::new(Options {
        folder: "/library".into(),
        ..Default::default()
    });
    app.set_size(80, 24);
    let mut tracks = fixture_tracks();
    sort_tracks(&mut tracks);
    for track in tracks.into_iter().take(stream) {
        reduce(&mut app, Msg::Scan(ScanMessage::Track(track)), 10);
    }
    reduce(
        &mut app,
        Msg::Scan(ScanMessage::Done(ScanReport {
            scanned: stream,
            succeeded: stream,
            digest: 0xaa,
            ..Default::default()
        })),
        20,
    );
    assert_eq!(app.phase, Phase::Ready);
    assert_eq!(app.playlist.len(), stream);
    app
}

fn rescan(app: &mut AppState, tracks: Vec<Track>, digest: u64, now: u64) {
    reduce(app, Msg::Rescan, now);
    for track in tracks {
        reduce(app, Msg::Scan(ScanMessage::Track(track)), now + 1);
    }
    reduce(
        app,
        Msg::Scan(ScanMessage::Done(ScanReport {
            digest,
            ..Default::default()
        })),
        now + 2,
    );
}

#[test]
fn a_rescan_never_duplicates_the_library() {
    let mut app = app_after_first_scan(22);
    let before = paths(app.playlist.tracks());

    // The same rows again, same digest: nothing changed on disk.
    let mut again = fixture_tracks();
    sort_tracks(&mut again);
    rescan(&mut app, again, 0xaa, 100);

    assert_eq!(app.playlist.len(), 22, "no rows were doubled");
    assert_eq!(paths(app.playlist.tracks()), before, "nor reshuffled");
    assert!(
        app.scan_stage.is_empty(),
        "the staged rows were thrown away"
    );
}

#[test]
fn an_unchanged_sync_keeps_the_cursor_and_the_run_exactly_as_they_were() {
    let mut app = app_after_first_scan(22);
    reduce(
        &mut app,
        Msg::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        )),
        300,
    );
    let cursor = app.playlist.cursor().expect("a cursor");
    let run = app.playlist.run().expect("a run");
    let scroll = app.scroll.offset();

    let mut again = fixture_tracks();
    sort_tracks(&mut again);
    rescan(&mut app, again, 0xaa, 400);

    assert_eq!(app.playlist.cursor(), Some(cursor), "still on the song");
    assert_eq!(app.playlist.run(), Some(run), "same artist run");
    assert_eq!(app.scroll.offset(), scroll, "the list did not move");
    assert_eq!(app.phase, Phase::Ready);
}

#[test]
fn a_changed_sync_swaps_in_the_new_rows_and_says_what_it_did() {
    let mut app = app_after_first_scan(22);

    // One new file appears on disk (and one disappears).
    let mut tracks = fixture_tracks();
    tracks.retain(|track| !track.path.to_string_lossy().contains("aaron-singles"));
    let mut extra = Track::from_path(PathBuf::from("/spec/new-song.mp3"));
    extra.artist = Some("Newcomer".into());
    extra.album = Some("Debut".into());
    extra.refresh_key();
    tracks.push(extra);
    sort_tracks(&mut tracks);
    rescan(&mut app, tracks, 0xbb, 500);

    assert_eq!(app.playlist.len(), 22, "one added, one removed");
    let notice = app.notice.expect("the notice");
    assert!(
        notice.text.contains("Library updated")
            && notice.text.contains("1 new")
            && notice.text.contains("1 removed"),
        "{}",
        notice.text
    );
    assert_eq!(app.library_digest, Some(0xbb));
}

#[test]
fn periodic_sync_fires_on_the_interval_and_not_before() {
    let mut app = app_after_first_scan(22);
    app.opts.sync_interval = Duration::from_secs(60);

    // Nothing before the interval has passed.
    assert!(reduce(&mut app, Msg::Tick, 20 + 59_000).is_empty());
    assert_eq!(app.phase, Phase::Ready);

    let effects = reduce(&mut app, Msg::Tick, 20 + 61_000);
    assert_eq!(effects, vec![Effect::Rescan], "one sync is requested");
    assert_eq!(app.phase, Phase::Scanning, "and it is a proper rescan");
    assert!(!app.scan.streaming, "so its rows are staged, not appended");

    // While that scan is in flight a second one must never stack.
    assert!(reduce(&mut app, Msg::Tick, 20 + 900_000).is_empty());
}

#[test]
fn periodic_sync_is_off_by_default_and_skips_a_drag() {
    let mut app = app_after_first_scan(22);
    assert!(
        reduce(&mut app, Msg::Tick, 10_000_000).is_empty(),
        "--sync-interval 0 is the default"
    );

    app.opts.sync_interval = Duration::from_secs(60);
    app.last_scan_ms = 0;
    app.dragging = true;
    assert!(
        reduce(&mut app, Msg::Tick, 10_000_000).is_empty(),
        "never swap the library out from under a drag"
    );
}

#[test]
fn restarting_the_first_scan_does_not_double_the_rows() {
    let mut app = AppState::new(Options::default());
    app.set_size(80, 24);
    let tracks = fixture_tracks();
    for track in tracks.iter().take(6) {
        reduce(&mut app, Msg::Scan(ScanMessage::Track(track.clone())), 10);
    }
    assert_eq!(
        app.playlist.len(),
        6,
        "rows stream in during the first scan"
    );
    // The user hits `r` before the first scan finished: what it already
    // streamed must not be counted twice by the new scan.
    assert_eq!(reduce(&mut app, Msg::Rescan, 20), vec![Effect::Rescan]);
    assert!(app.playlist.is_empty(), "the partial rows were dropped");
}

#[test]
fn an_unchanged_sync_leaves_the_index_file_untouched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    write_library(root);

    scan_with_cache(root, cache_for(root));
    let file = library_cache::default_cache_file(root, &ScanConfig::default());
    let before = std::fs::metadata(&file).expect("index metadata");

    // A periodic sync in a restarted process: nothing changed, so the file must
    // still be byte-identical (and must not have been rewritten every minute).
    let (rows, report) = scan_with_cache(root, cache_for(root));
    assert_eq!(report.cached, rows.len());
    let after = std::fs::metadata(&file).expect("index metadata");
    assert_eq!(before.len(), after.len(), "same size");
    assert_eq!(
        before.modified().ok(),
        after.modified().ok(),
        "the index was not rewritten"
    );
}
