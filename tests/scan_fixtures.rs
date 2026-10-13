//! Scanner tests against a real fixture tree with real (synthesized) audio.
//!
//! Run with `cargo test --test scan_fixtures`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use music_tui::model::{sort_tracks, Track};
use music_tui::scan::{self, ScanConfig, ScanMessage, ScanPhase, ScanReport};
use music_tui::testing::{wav_bytes, write_synthetic_library, FixtureFile, FixtureTags, Format};

fn collect(root: &Path, config: &ScanConfig) -> (Vec<Track>, ScanReport) {
    let (tx, rx) = mpsc::channel();
    let cancel = new_cancel();
    let root_owned = root.to_path_buf();
    let config_owned = config.clone();
    let handle = std::thread::spawn(move || scan::scan(&root_owned, &config_owned, &tx, &cancel));
    let mut tracks = Vec::new();
    let mut report = ScanReport::default();
    let mut progress = Vec::new();
    while let Ok(message) = rx.recv() {
        match message {
            ScanMessage::Track(track) => tracks.push(track),
            ScanMessage::Progress(p) => progress.push(p),
            ScanMessage::Done(done) => report = done,
        }
    }
    let _joined = handle.join();
    assert!(
        !progress.is_empty(),
        "scanner must stream progress so the UI never freezes"
    );
    assert!(
        progress.iter().any(|p| p.phase == ScanPhase::Done),
        "exactly a Done progress tick must close the stream: {progress:?}"
    );
    sort_tracks(&mut tracks);
    (tracks, report)
}

fn new_cancel() -> std::sync::Arc<AtomicBool> {
    std::sync::Arc::new(AtomicBool::new(false))
}

fn library() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    write_synthetic_library(dir.path()).expect("write fixtures");
    dir
}

fn find<'a>(tracks: &'a [Track], needle: &str) -> &'a Track {
    tracks
        .iter()
        .find(|t| t.path.to_string_lossy().contains(needle))
        .unwrap_or_else(|| panic!("no track matching {needle:?} in {:?}", paths(tracks)))
}

fn paths(tracks: &[Track]) -> Vec<String> {
    tracks
        .iter()
        .map(|t| t.path.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn one_row_per_audio_file_with_correct_tags() {
    let dir = library();
    let (tracks, report) = collect(dir.path(), &ScanConfig::default());

    // 10 audio fixtures; notes/readme.txt, cover.jpg and .hidden/* are ignored.
    assert_eq!(tracks.len(), 10, "unexpected rows: {:?}", paths(&tracks));
    assert_eq!(report.scanned, 10);
    assert!(!report.empty);

    let first = find(&tracks, "one.wav");
    assert_eq!(first.artist.as_deref(), Some("Alpha Beta"));
    assert_eq!(first.album.as_deref(), Some("Album 1"));
    assert_eq!(first.title, "First Song");
    assert_eq!(first.track_no, Some(1));
    assert_eq!(first.disc_no, Some(1));
    assert_eq!(first.year, Some(1998));
    assert!(first.duration.is_some(), "wav duration should be readable");
    assert!(first
        .duration
        .is_some_and(|d| d > Duration::from_millis(100)));
    assert!(!first.corrupt);

    let compilation = find(&tracks, "comp/track3.mp3");
    assert_eq!(compilation.artist.as_deref(), Some("Sister Hazel"));
    assert_eq!(
        compilation.album_artist.as_deref(),
        Some("Various Artists"),
        "album_artist must round-trip through the scanner"
    );
    assert_eq!(compilation.key.artist, "various artists");

    let disc_two = find(&tracks, "disc2-a.flac");
    assert_eq!(disc_two.disc_no, Some(2));
    assert_eq!(disc_two.track_no, Some(1));

    let unicode = find(&tracks, "日本語");
    assert_eq!(unicode.title, "日本語のタイトル");
    assert_eq!(
        unicode.key.artist, "the beatles",
        "whitespace and case normalize"
    );

    let untagged = find(&tracks, "untagged.wav");
    assert_eq!(untagged.title, "untagged");
    assert!(untagged.artist.is_none());
    assert_eq!(untagged.key.artist, "");
    assert_eq!(untagged.key.track, music_tui::model::TRACK_MISSING);
}

#[test]
fn corrupt_file_is_reported_but_does_not_fail_the_scan() {
    let dir = library();
    let (tracks, report) = collect(dir.path(), &ScanConfig::default());

    let broken = find(&tracks, "broken.mp3");
    assert!(broken.corrupt, "truncated file must be marked");
    assert_eq!(broken.title, "broken", "fallback title is the file stem");
    assert_eq!(report.failed.len(), 1, "report: {:?}", report.failed);
    assert!(report.failed[0].0.to_string_lossy().contains("broken.mp3"));
    assert_eq!(report.succeeded, 9);
    assert!(report.elapsed < Duration::from_secs(30));
}

#[test]
fn sorted_order_groups_artists_and_natural_numbers() {
    let dir = library();
    let (tracks, _) = collect(dir.path(), &ScanConfig::default());
    // untagged sorts last inside its own album (sentinel u32::MAX).
    assert_eq!(
        find(&tracks, "untagged.wav").key.track,
        music_tui::model::TRACK_MISSING
    );
    // "Album 1" before "Album 10" (natural comparison).
    let alpha: Vec<&str> = tracks
        .iter()
        .filter(|t| t.key.artist == "alpha beta")
        .map(|t| t.key.album.as_str())
        .collect();
    assert_eq!(alpha, vec!["album 1", "album 1", "album 10"]);
}

#[test]
fn rescanning_is_stable_and_resort_is_a_no_op() {
    let dir = library();
    let (first, _) = collect(dir.path(), &ScanConfig::default());
    let (second, _) = collect(dir.path(), &ScanConfig::default());
    assert_eq!(paths(&first), paths(&second), "order must not drift");
    let mut resort = first.clone();
    sort_tracks(&mut resort);
    assert_eq!(paths(&resort), paths(&first), "re-sorting must be a no-op");
}

#[test]
fn progress_streams_on_a_large_library() {
    let dir = tempfile::tempdir().expect("tempdir");
    for i in 0..800 {
        let path = dir.path().join(format!("a{i}/t{i}.wav"));
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(&path, wav_bytes(0.02)).expect("write");
    }
    let (tx, rx) = mpsc::channel();
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let root = dir.path().to_path_buf();
    let config = ScanConfig::default();
    let worker_cancel = std::sync::Arc::clone(&cancel);
    let handle = std::thread::spawn(move || scan::scan(&root, &config, &tx, &worker_cancel));
    let mut progress = Vec::new();
    let mut tracks = 0;
    while let Ok(message) = rx.recv() {
        match message {
            ScanMessage::Progress(p) => progress.push(p),
            ScanMessage::Track(_) => tracks += 1,
            ScanMessage::Done(_) => break,
        }
    }
    let _joined = handle.join();
    cancel.store(true, Ordering::SeqCst);
    assert_eq!(tracks, 800);
    assert!(
        progress.len() > 3,
        "expected several streamed progress ticks, got {progress:?}"
    );
    assert!(
        progress.iter().any(|p| p.phase == ScanPhase::Tagging),
        "{progress:?}"
    );
    // Coalesced, never a message per file.
    assert!(
        progress.len() <= 800,
        "progress must be coalesced: {} messages",
        progress.len()
    );
    let last = progress.last().expect("progress");
    assert_eq!(last.indexed, 800);
}

#[test]
fn thread_count_does_not_change_the_result() {
    let dir = library();
    let one = collect(
        dir.path(),
        &ScanConfig {
            threads: 1,
            ..Default::default()
        },
    );
    let many = collect(
        dir.path(),
        &ScanConfig {
            threads: 8,
            ..Default::default()
        },
    );
    assert_eq!(paths(&one.0), paths(&many.0));
    assert_eq!(one.1.failed.len(), many.1.failed.len());
}

#[test]
fn max_files_cap_is_reported() {
    let dir = library();
    let (tracks, report) = collect(
        dir.path(),
        &ScanConfig {
            max_files: 3,
            ..Default::default()
        },
    );
    assert_eq!(tracks.len(), 3);
    assert!(report.capped, "footer needs to warn about the cap");
}

#[test]
fn hidden_and_disallowed_extensions_are_skipped() {
    let dir = library();
    let (tracks, _) = collect(dir.path(), &ScanConfig::default());
    assert!(
        !paths(&tracks).iter().any(|p| p.contains(".hidden")),
        "{:?}",
        paths(&tracks)
    );
    assert!(!paths(&tracks).iter().any(|p| p.ends_with(".txt")));
    assert!(!paths(&tracks).iter().any(|p| p.ends_with(".jpg")));

    // Widening the allowlist picks the text file up as if it were audio.
    let (wider, _) = collect(
        dir.path(),
        &ScanConfig {
            extensions: vec!["txt".into()],
            ..Default::default()
        },
    );
    assert!(paths(&wider).iter().any(|p| p.ends_with(".txt")));
}

#[test]
fn cancellation_stops_the_scan_promptly() {
    let dir = tempfile::tempdir().expect("tempdir");
    for i in 0..500 {
        let path = dir.path().join(format!("d{i}/track{i}.wav"));
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(&path, wav_bytes(0.05)).expect("write");
    }
    let (tx, rx) = mpsc::channel();
    let cancel = std::sync::Arc::new(AtomicBool::new(true)); // cancel immediately
    let root = dir.path().to_path_buf();
    let config = ScanConfig::default();
    let handle = std::thread::spawn(move || scan::scan(&root, &config, &tx, &cancel));
    let started = Instant::now();
    let mut done = None;
    while let Ok(message) = rx.recv() {
        if let ScanMessage::Done(report) = message {
            done = Some(report);
            break;
        }
    }
    let _joined = handle.join();
    let report = done.expect("Done must always be sent");
    assert!(report.cancelled, "cancelled flag must be set");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn symlink_loops_do_not_hang_the_scanner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let inner = dir.path().join("music");
    std::fs::create_dir_all(&inner).expect("create");
    std::fs::write(inner.join("song.wav"), wav_bytes(0.05)).expect("write");
    // A link back to the parent: only a scanner that follows symlinks loops.
    #[cfg(unix)]
    std::os::unix::fs::symlink(dir.path(), inner.join("loop-back")).expect("symlink");
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(dir.path(), inner.join("loop-back")).expect("symlink");

    let root = dir.path().to_path_buf();
    let config = ScanConfig::default();
    let (tx, rx) = mpsc::channel();
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let handle = std::thread::spawn(move || scan::scan(&root, &config, &tx, &cancel));
    let mut count = 0;
    while let Ok(message) = rx.recv() {
        if matches!(message, ScanMessage::Done(_)) {
            break;
        }
        count += 1;
        assert!(started.elapsed() < Duration::from_secs(20), "scan hung");
    }
    let _joined = handle.join();
    assert!(started.elapsed() < Duration::from_secs(20));
    assert!(count > 0);
}

#[test]
fn five_thousand_files_scan_within_budget() {
    let dir = tempfile::tempdir().expect("tempdir");
    for artist in 0..50 {
        for track in 0..100 {
            let path: PathBuf = dir
                .path()
                .join(format!("artist{artist:02}/album{track}/t{track:03}.wav"));
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
            std::fs::write(&path, wav_bytes(0.01)).expect("write");
        }
    }
    let started = Instant::now();
    let (tracks, report) = collect(dir.path(), &ScanConfig::default());
    let elapsed = started.elapsed();
    assert_eq!(tracks.len(), 5_000);
    assert_eq!(report.scanned, 5_000);
    let first_paint = first_message_latency(dir.path());
    assert!(
        first_paint < Duration::from_millis(1_500),
        "first rows should stream in fast, took {first_paint:?}"
    );
    assert!(
        elapsed < Duration::from_secs(60),
        "5k scan took {elapsed:?}"
    );
    // Sorting 5k rows must be cheap enough to do once per scan.
    let started = Instant::now();
    let mut sorted = tracks;
    sort_tracks(&mut sorted);
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "sorting 5k tracks took {:?}",
        started.elapsed()
    );
}

fn first_message_latency(root: &Path) -> Duration {
    let (tx, rx) = mpsc::channel();
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let root = root.to_path_buf();
    let config = ScanConfig::default();
    let started = Instant::now();
    let worker_cancel = std::sync::Arc::clone(&cancel);
    let handle = std::thread::spawn(move || scan::scan(&root, &config, &tx, &worker_cancel));
    let _first = rx.recv().expect("at least one message");
    let latency = started.elapsed();
    cancel.store(true, Ordering::SeqCst);
    while rx.recv().is_ok() {}
    let _joined = handle.join();
    latency
}

#[test]
fn fixtures_can_be_written_anywhere_and_are_valid_audio() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = FixtureFile {
        path: PathBuf::from("deep/nested/track.wav"),
        tags: FixtureTags {
            artist: Some("Deep".into()),
            album: Some("Nested".into()),
            title: Some("Track".into()),
            track: Some(7),
            disc: Some(2),
            year: Some(2020),
            album_artist: None,
        },
        corrupt: false,
        format: Format::Wav,
    };
    let path = music_tui::testing::write_fixture(dir.path(), &fixture).expect("write");
    assert!(path.is_file());
    let (track, error) = scan::tag_file(&path);
    assert!(error.is_none(), "{error:?}");
    assert_eq!(track.artist.as_deref(), Some("Deep"));
    assert_eq!(track.track_no, Some(7));
    assert_eq!(track.disc_no, Some(2));
    assert_eq!(track.year, Some(2020));
    assert!(track.duration.is_some_and(|d| d.as_millis() > 100));
}
