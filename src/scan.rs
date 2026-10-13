//! Recursive, parallel, cancellable metadata scanner.
//!
//! The scanner is a plain library function ([`scan`]) that takes a root path, a
//! [`ScanConfig`], a sender and a cancellation flag — no terminal, no globals —
//! so it is unit-testable and runs on worker threads while the UI keeps painting.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lofty::prelude::{Accessor, AudioFile as _, TaggedFileExt as _};
use lofty::probe::Probe;
use lofty::tag::ItemKey;
use walkdir::WalkDir;

use crate::library_cache::{Fingerprint, LibraryCache, LibraryDigest, ScanToken};
use crate::model::Track;

/// Extensions treated as audio files unless `--extensions` widens the list.
pub const DEFAULT_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "opus", "m4a", "m4b", "aac", "wav", "aiff", "aif", "wma", "ape",
    "mpc", "wv",
];

/// Maximum number of progress messages per second (perf ticket).
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// ... and at most one progress message per this many tagged files, so a fast
/// scan of a small folder still streams a few ticks.
const PROGRESS_FILES: usize = 256;

/// Scanner knobs, all settable from the CLI.
#[derive(Debug, Clone)]
pub struct ScanConfig {
    /// Lowercase extensions to accept (no dot).
    pub extensions: Vec<String>,
    /// Maximum directory depth (guards against pathological trees).
    pub max_depth: usize,
    /// Hard cap on filesystem entries visited (guards against huge mounts).
    pub max_entries: usize,
    /// Hard cap on audio files returned; the footer warns when reached.
    pub max_files: usize,
    /// Number of tagging worker threads.
    pub threads: usize,
    /// Also descend into dot-directories.
    pub include_hidden: bool,
    /// Library index from a previous run, consulted before a file is opened.
    ///
    /// `None` (the default) means "always read every tag", which is what
    /// `--dump-sort` and the tests want. See [`crate::library_cache`].
    pub cache: Option<std::sync::Arc<crate::library_cache::LibraryCache>>,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            extensions: DEFAULT_EXTENSIONS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            max_depth: 32,
            max_entries: 500_000,
            max_files: 50_000,
            threads: thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
                .clamp(1, 8),
            include_hidden: false,
            cache: None,
        }
    }
}

impl ScanConfig {
    /// `true` when `path`'s extension is in the allowlist.
    #[must_use]
    pub fn accepts(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| self.accepts_extension(ext))
    }

    /// `true` when a bare extension (with or without dot) is allowed.
    #[must_use]
    pub fn accepts_extension(&self, ext: &str) -> bool {
        let want = ext.trim_start_matches('.');
        !want.is_empty() && self.extensions.iter().any(|e| e.eq_ignore_ascii_case(want))
    }
}

/// Coarse scanner phase, surfaced in the footer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScanPhase {
    /// Walking the tree.
    #[default]
    Walking,
    /// Reading tags.
    Tagging,
    /// Finished (success, cap reached, or cancelled).
    Done,
}

/// Progress tick for the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanProgress {
    /// Rows indexed so far.
    pub indexed: usize,
    /// Best guess of total rows (grows while walking).
    pub total_estimate: usize,
    /// Current phase.
    pub phase: ScanPhase,
}

/// What a completed (or aborted) scan produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    /// Filesystem entries that looked like audio.
    pub scanned: usize,
    /// Files whose tags/properties were read successfully.
    pub succeeded: usize,
    /// Files that produced an error, with the reason.
    pub failed: Vec<(PathBuf, String)>,
    /// The `--max-files` cap was hit.
    pub capped: bool,
    /// The entry cap was hit while walking.
    pub entry_capped: bool,
    /// Scanning stopped early because the user quit.
    pub cancelled: bool,
    /// No audio file was found.
    pub empty: bool,
    /// Wall time.
    pub elapsed: Duration,
    /// Extensions that were tried (for the empty-folder message).
    pub extensions: Vec<String>,
    /// Rows reused from the library index instead of opening the file.
    pub cached: usize,
    /// Order-independent hash of every row this scan produced.
    ///
    /// Two scans of the same folder that agree on every row agree on this value,
    /// which lets the reducer skip the re-sort of a sync that changed nothing
    /// ([`crate::library_cache::LibraryDigest`]).
    pub digest: u64,
    /// Set when the index is current for this scan (written now, or already
    /// byte-identical on disk). `None` for partial or cancelled scans.
    pub cache_file: Option<PathBuf>,
}

/// Messages streamed from the scanner threads to the app.
///
/// `Track` is much larger than the other variants, but it is moved, never
/// cloned per row, so a box here would only cost an allocation per file.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum ScanMessage {
    /// A fully tagged row (arrives in filesystem order; sorting happens once,
    /// when the scan finishes).
    Track(Track),
    /// Progress tick.
    Progress(ScanProgress),
    /// Terminal message: exactly one per scan.
    Done(ScanReport),
}

fn send(tx: &Sender<ScanMessage>, msg: ScanMessage) {
    let _drop = tx.send(msg);
}

#[derive(Debug, Default)]
struct RawTags {
    title: Option<String>,
    artist: Option<String>,
    album_artist: Option<String>,
    album: Option<String>,
    track_no: Option<u32>,
    track_total: Option<u32>,
    disc_no: Option<u32>,
    disc_total: Option<u32>,
    year: Option<i32>,
    date: Option<String>,
    duration: Option<Duration>,
    bitrate: Option<u32>,
    format: Option<String>,
}

impl RawTags {
    fn apply(self, track: &mut Track) {
        if let Some(title) = self.title.filter(|s| !s.trim().is_empty()) {
            track.title = title;
        }
        track.artist = self.artist;
        track.album_artist = self.album_artist;
        track.album = self.album;
        track.track_no = self.track_no;
        track.track_total = self.track_total;
        track.disc_no = self.disc_no;
        track.disc_total = self.disc_total;
        track.year = self.year;
        track.date = self.date;
        track.duration = self.duration;
        track.bitrate = self.bitrate;
        if let Some(format) = self.format {
            track.format = format;
        }
        track.refresh_key();
    }
}

/// Read tags and properties from one file.
///
/// Never panics and never fails the scan: on error a [`Track`] marked
/// [`Track::corrupt`] is returned together with the reason.
#[must_use]
pub fn tag_file(path: &Path) -> (Track, Option<String>) {
    let mut track = Track::from_path(path.to_path_buf());
    match read_tags(path) {
        Ok(raw) => {
            raw.apply(&mut track);
            (track, None)
        }
        Err(reason) => {
            track.corrupt = true;
            (track, Some(reason))
        }
    }
}

fn read_tags(path: &Path) -> Result<RawTags, String> {
    let tagged = Probe::open(path)
        .and_then(|probe| probe.read())
        .map_err(|err| err.to_string())?;

    let mut raw = RawTags::default();
    let props = tagged.properties();
    let duration = props.duration();
    if !duration.is_zero() {
        raw.duration = Some(duration);
    }
    raw.bitrate = props.audio_bitrate().filter(|b| *b > 0);
    raw.format = Some(format!("{:?}", tagged.file_type()).to_ascii_lowercase());

    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        raw.title = tag.title().map(std::borrow::Cow::into_owned);
        raw.artist = tag.artist().map(std::borrow::Cow::into_owned);
        raw.album = tag.album().map(std::borrow::Cow::into_owned);
        raw.album_artist = tag.get_string(ItemKey::AlbumArtist).map(str::to_string);
        raw.track_no = tag.track();
        raw.track_total = tag.track_total();
        raw.disc_no = tag.disk();
        raw.disc_total = tag.disk_total();
        if let Some(ts) = tag.date() {
            raw.year = i32::from(ts.year).into();
            if raw.year == Some(0) {
                raw.year = None;
            }
            raw.date = Some(ts.to_string());
        }
        if raw.year.is_none() {
            raw.year = tag
                .get_string(ItemKey::Year)
                .and_then(|s| s.trim().chars().take(4).collect::<String>().parse().ok());
        }
    }
    Ok(raw)
}

/// Produce the row for one file.
///
/// The index answers whenever the file's [`Fingerprint`] still matches, so the
/// file is never opened; otherwise the tags are read and (on success only) stored
/// for the next scan. Failures are never stored: a corrupt or temporarily
/// unreadable file must be retried instead of staying marked corrupt forever.
///
/// Returns `(track, error, from_cache)`.
fn read_one(
    cache: Option<&LibraryCache>,
    token: &ScanToken,
    path: &Path,
    fingerprint: Option<Fingerprint>,
) -> (Track, Option<String>, bool) {
    if let (Some(cache), Some(fingerprint)) = (cache, fingerprint) {
        if let Some(track) = cache.lookup(token, path, fingerprint) {
            return (track, None, true);
        }
        let (track, err) = tag_file(path);
        if err.is_none() {
            cache.insert(token, path.to_path_buf(), fingerprint, &track);
        }
        return (track, err, false);
    }
    let (track, err) = tag_file(path);
    (track, err, false)
}

#[derive(Debug)]
struct Shared {
    tx: Mutex<Sender<ScanMessage>>,
    tagged: AtomicUsize,
    succeeded: AtomicUsize,
    cached: AtomicUsize,
    failed: Mutex<Vec<(PathBuf, String)>>,
    last_progress: AtomicU64,
    /// Lock-free accumulators for the library digest (see
    /// [`crate::library_cache::LibraryDigest`]: the mixing happens once, at the
    /// end, so the workers only ever do two atomic ops).
    digest_sum: AtomicU64,
    digest_xor: AtomicU64,
}

/// One file handed to a tagging worker.
struct Job {
    path: PathBuf,
    /// `None` when the file could not be `stat`ed: such a file is always tagged
    /// and never cached.
    fingerprint: Option<Fingerprint>,
}

/// Unknown-file stand-in for the digest, so a `stat` failure changes the digest
/// (the conservative direction: the reducer re-applies the library) instead of
/// silently matching the previous scan.
const NO_FINGERPRINT: Fingerprint = Fingerprint {
    size: 0,
    mtime_nanos: 0,
};

/// Packed `(ms, tagged_files)` stamp for progress coalescing (fits in u64).
#[derive(Debug, Clone, Copy)]
struct LastProgress {
    last_ms: u64,
    tagged: usize,
}

impl LastProgress {
    fn pack(self) -> u64 {
        self.last_ms << 20 | u64::try_from(self.tagged).unwrap_or(0) & 0x000F_FFFF
    }

    fn unpack(raw: u64) -> Self {
        Self {
            last_ms: raw >> 20,
            tagged: usize::try_from(raw & 0x000F_FFFF).unwrap_or(0),
        }
    }
}

impl Shared {
    /// Fold one produced row into the library digest.
    fn add_row(&self, track: &Track, fingerprint: Option<Fingerprint>) {
        let hash = crate::library_cache::row_digest(track, fingerprint.unwrap_or(NO_FINGERPRINT));
        self.digest_sum.fetch_add(hash, Ordering::Relaxed);
        self.digest_xor.fetch_xor(hash, Ordering::Relaxed);
    }

    fn report_progress(&self, total_estimate: usize, phase: ScanPhase) {
        let now_ms = crate::app::time::now_ms();
        let last = LastProgress::unpack(self.last_progress.load(Ordering::Relaxed));
        let tagged = self.tagged.load(Ordering::Relaxed);
        let time_ok = now_ms.saturating_sub(last.last_ms) >= PROGRESS_INTERVAL.as_millis() as u64;
        let count_ok = tagged.saturating_sub(last.tagged) >= PROGRESS_FILES;
        if !(time_ok || count_ok) {
            return;
        }
        let stamp = LastProgress {
            last_ms: now_ms,
            tagged,
        };
        self.last_progress.store(stamp.pack(), Ordering::Relaxed);
        let indexed = tagged;
        if let Ok(tx) = self.tx.lock() {
            let _drop = tx.send(ScanMessage::Progress(ScanProgress {
                indexed,
                total_estimate: total_estimate.max(indexed),
                phase,
            }));
        }
    }
}

/// Walk `root`, tag everything that looks like audio and stream [`ScanMessage`]s.
///
/// Spreads tagging over `cfg.threads` workers and always sends exactly one
/// [`ScanMessage::Done`] last. Returns the report (also sent).
///
/// Files whose [`Fingerprint`] matches the [`ScanConfig::cache`] entry are never
/// opened; the walk itself always happens, because the walk is what notices
/// added and deleted files.
pub fn scan(
    root: &Path,
    cfg: &ScanConfig,
    tx: &Sender<ScanMessage>,
    cancel: &Arc<AtomicBool>,
) -> ScanReport {
    let started = Instant::now();
    let token = cfg
        .cache
        .as_ref()
        .map(|cache| cache.begin_scan())
        .unwrap_or(ScanToken::uncached());
    let shared = Arc::new(Shared {
        tx: Mutex::new(tx.clone()),
        tagged: AtomicUsize::new(0),
        succeeded: AtomicUsize::new(0),
        cached: AtomicUsize::new(0),
        failed: Mutex::new(Vec::new()),
        // Seeded with *now*, not zero: `now_ms` counts from process start, so a
        // zero here would make the very first `report_progress` call pass its
        // throttle and put a `Progress` message in front of the first `Track`.
        // Consumers that read "until the first non-Track message" would then see
        // an empty library, and a scan of a small folder is exactly that case.
        last_progress: AtomicU64::new(
            LastProgress {
                last_ms: crate::app::time::now_ms(),
                tagged: 0,
            }
            .pack(),
        ),
        digest_sum: AtomicU64::new(0),
        digest_xor: AtomicU64::new(0),
    });

    // Bounded queue: a huge tree cannot make us hold every path in memory.
    let (path_tx, path_rx) =
        std::sync::mpsc::sync_channel::<Job>(cfg.threads.saturating_mul(64).max(64));
    let path_rx = Arc::new(Mutex::new(path_rx));

    let mut workers = Vec::with_capacity(cfg.threads.max(1));
    for _ in 0..cfg.threads.max(1) {
        let rx = Arc::clone(&path_rx);
        let shared = Arc::clone(&shared);
        let cancel = Arc::clone(cancel);
        let cache = cfg.cache.clone();
        workers.push(thread::spawn(move || loop {
            let next = {
                let guard = rx.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                guard.recv()
            };
            let Ok(job) = next else { break };
            if cancel.load(Ordering::Relaxed) {
                continue;
            }
            let Job { path, fingerprint } = job;
            let (track, err, from_cache) = read_one(cache.as_deref(), &token, &path, fingerprint);
            shared.tagged.fetch_add(1, Ordering::Relaxed);
            match err {
                None => {
                    shared.succeeded.fetch_add(1, Ordering::Relaxed);
                    if from_cache {
                        shared.cached.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Some(reason) => {
                    if let Ok(mut failed) = shared.failed.lock() {
                        failed.push((path, reason));
                    }
                }
            }
            shared.add_row(&track, fingerprint);
            if let Ok(guard) = shared.tx.lock() {
                let _drop = guard.send(ScanMessage::Track(track));
            }
            shared.report_progress(0, ScanPhase::Tagging);
        }));
    }

    let mut indexed: usize = 0;
    let mut seen: usize = 0;
    let mut report = ScanReport {
        extensions: cfg.extensions.clone(),
        ..Default::default()
    };
    let mut walker_dirs = 0usize;

    // `filter_entry` prunes whole subtrees; the root itself is always kept
    // (temporary directories in tests start with a dot).
    let include_hidden = cfg.include_hidden;
    let walker = WalkDir::new(root)
        .follow_links(false)
        .max_depth(cfg.max_depth)
        .into_iter()
        .filter_entry(move |entry| {
            include_hidden
                || entry.depth() == 0
                || !(entry.file_type().is_dir()
                    && entry.file_name().to_string_lossy().starts_with('.'))
        });
    for entry in walker {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        seen += 1;
        if seen > cfg.max_entries {
            report.entry_capped = true;
            break;
        }
        let Ok(entry) = entry else {
            // Unreadable subdirectory or vanished file: keep going.
            continue;
        };
        if entry.file_type().is_dir() {
            if !cfg.include_hidden && entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            walker_dirs += 1;
            continue;
        }
        if !entry.file_type().is_file() || !cfg.accepts(entry.path()) {
            continue;
        }
        if indexed >= cfg.max_files {
            report.capped = true;
            break;
        }
        indexed += 1;
        // One `stat` per audio file is what the cache is bought with; it replaces
        // an `open()` + tag parse for every file that has not changed.
        let fingerprint = entry.metadata().ok().as_ref().map(Fingerprint::of);
        if path_tx
            .send(Job {
                path: entry.path().to_path_buf(),
                fingerprint,
            })
            .is_err()
        {
            report.cancelled = true;
            break;
        }
        shared.report_progress(indexed + walker_dirs, ScanPhase::Walking);
    }

    drop(path_tx);
    for worker in workers {
        let _joined = worker.join();
    }

    let failed = shared.failed.lock().map(|v| v.clone()).unwrap_or_default();
    report.scanned = indexed;
    report.succeeded = shared.succeeded.load(Ordering::Relaxed);
    report.failed = failed;
    report.empty = indexed == 0;
    report.elapsed = started.elapsed();
    report.cached = shared.cached.load(Ordering::Relaxed);
    report.digest = LibraryDigest::from_accumulators(
        shared.digest_sum.load(Ordering::Relaxed),
        shared.digest_xor.load(Ordering::Relaxed),
        u64::try_from(shared.tagged.load(Ordering::Relaxed)).unwrap_or(u64::MAX),
    )
    .finish();

    // Refresh the index, but never from a partial scan: a scan that stopped at a
    // cap (or was cancelled) did not see most of the library, and pruning to
    // what it did see would quietly lose rows.
    if let Some(cache) = cfg.cache.as_ref() {
        if !report.capped && !report.entry_capped && !report.cancelled {
            match cache.save(&token, report.digest) {
                Ok(wrote) => {
                    tracing::debug!(
                        wrote,
                        path = %cache.path().display(),
                        "library index up to date"
                    );
                    report.cache_file = Some(cache.path().to_path_buf());
                }
                Err(reason) => tracing::warn!(
                    reason = %reason,
                    path = %cache.path().display(),
                    "could not write the library index"
                ),
            }
        }
    }

    send(
        tx,
        ScanMessage::Progress(ScanProgress {
            indexed,
            total_estimate: indexed,
            phase: ScanPhase::Done,
        }),
    );
    send(tx, ScanMessage::Done(report.clone()));
    report
}

/// Spawn [`scan`] on a background thread.
#[must_use]
pub fn spawn(
    root: PathBuf,
    cfg: ScanConfig,
) -> (
    std::sync::mpsc::Receiver<ScanMessage>,
    Arc<AtomicBool>,
    thread::JoinHandle<ScanReport>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_worker = Arc::clone(&cancel);
    let handle = thread::spawn(move || scan(&root, &cfg, &tx, &cancel_worker));
    (rx, cancel, handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_matches_ticket_expectations() {
        let cfg = ScanConfig::default();
        for good in ["a.mp3", "b.FLAC", "c.ogg", "d.m4a", "e.opus", "f.wav"] {
            assert!(cfg.accepts(Path::new(good)), "{good} should be audio");
        }
        for bad in ["notes.txt", "cover.jpg", "archive.zip", "noext", ".mp3"] {
            assert!(!cfg.accepts(Path::new(bad)), "{bad} should be ignored");
        }
        let wide = ScanConfig {
            extensions: vec!["mid".into()],
            ..Default::default()
        };
        assert!(wide.accepts(Path::new("song.mid")));
        assert!(!wide.accepts(Path::new("song.mp3")));
    }

    #[test]
    fn tag_file_survives_garbage() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("mutu-garbage-{}.mp3", std::process::id()));
        std::fs::write(&path, b"not audio at all").expect("test fixture write");
        let (track, err) = tag_file(&path);
        let _remove = std::fs::remove_file(&path);
        assert!(track.corrupt, "garbage file must be marked corrupt");
        assert!(err.is_some());
        assert_eq!(track.title, format!("mutu-garbage-{}", std::process::id()));
    }
}
