//! A versioned, fingerprint-keyed cache of the library index.
//!
//! Reading tags is the expensive part of a scan: measured on 14 400 files,
//! `readdir` over the whole tree costs ~15 ms while reading the tags costs
//! ~200 ms, and it barely parallelises (it is syscall-latency bound, not CPU
//! bound — see `docs/benchmarks.md`). Everything in the file format below exists
//! to turn that ~200 ms into a `stat()` per file (~2 µs) instead of an
//! `open()`/parse per file (~14 µs).
//!
//! Two ideas carry the design:
//!
//! 1. **A fingerprint per file** (`size` + mtime, nanosecond resolution). The
//!    scanner still walks the whole tree — that is what detects new, renamed and
//!    deleted files — but for every audio file it compares the fingerprint
//!    before opening the file. Equal fingerprint ⇒ the cached row is reused and
//!    the file is never opened.
//! 2. **A digest of the whole library** (order-independent hash of every row).
//!    A rescan whose digest matches the previous one produced *exactly* the same
//!    library, so the reducer can throw the rows away instead of re-sorting and
//!    re-anchoring: an idle periodic sync costs no re-sort and no rows jumping
//!    under the cursor.
//!
//! What is deliberately *not* cached:
//!
//! * **Failures.** A file that could not be tagged is never stored, so a
//!   half-downloaded or temporarily unreadable file is retried on every scan
//!   instead of staying marked corrupt forever.
//! * **Derived keys.** [`crate::model::SortKey`] is rebuilt on load, because it
//!   is derived; in particular `album_year` is a whole-library property that only
//!   [`crate::model::sort_tracks`] can compute.
//!
//! Cache invalidation is versioned three ways, because "the user's library is
//! cached" must never mean "a fix to the sorting code never reaches the user":
//! `SCHEMA_VERSION` (file layout), [`crate::model::SORT_ALGORITHM`]
//! (the *meaning* of a row) and [`config_fingerprint`] (the scanner flags that
//! decide which rows exist). See `docs/adr/0003-library-cache.md`.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use crate::model::{SORT_ALGORITHM, SortKey, Track};
use crate::scan::ScanConfig;

/// File layout version. Bump when [`write_row`] changes shape.
const SCHEMA_VERSION: u32 = 1;
/// Magic bytes at the start of every cache file.
const MAGIC: &[u8; 4] = b"MTUC";

// FNV-1a 64: no dependency, well spread, ~1 byte/ns which is irrelevant next to
// the syscalls it is replacing.
const HASH_SEED: u64 = 0xcbf2_9ce4_8422_2325;
const HASH_PRIME: u64 = 0x0000_0100_0000_01b3;

/// How a file changed between two scans, cheaply.
///
/// `size` + mtime catches the things that matter in practice (a new file, a
/// re-rip, a tag write — taggers update mtime). It cannot catch a rewrite that
/// deliberately preserves mtime; `R` (or `--refresh`) is the escape hatch for
/// that, and `--no-cache` is the escape hatch for filesystems whose mtime cannot
/// be trusted (some NFS/SMB mounts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint {
    /// File size in bytes.
    pub size: u64,
    /// Last modification time, nanoseconds since the Unix epoch.
    pub mtime_nanos: u64,
}

impl Fingerprint {
    /// Fingerprint of an already-`stat`ed file (the scanner reuses the
    /// `DirEntry` metadata it needed anyway).
    #[must_use]
    pub fn of(metadata: &fs::Metadata) -> Self {
        let mtime_nanos = metadata
            .modified()
            .ok()
            .and_then(|when| when.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| {
                u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
            });
        Self {
            size: metadata.len(),
            mtime_nanos,
        }
    }
}

/// One cached row plus the file state it describes.
#[derive(Debug, Clone)]
struct Entry {
    fingerprint: Fingerprint,
    track: Track,
    /// Scan that last confirmed this row; entries from older scans are pruned.
    generation: u64,
}

/// Per-scan handle on a [`LibraryCache`].
///
/// A restart (or a periodic sync racing a manual `r`) must not make another
/// scan's rows look stale, so every scan carries the generation it started with.
#[derive(Debug, Clone, Copy)]
pub struct ScanToken {
    generation: u64,
    bypass: bool,
}

impl ScanToken {
    /// A token for a scan that has no cache: lookups are skipped and nothing is
    /// stamped or pruned.
    #[must_use]
    pub fn uncached() -> Self {
        Self {
            generation: 0,
            bypass: true,
        }
    }

    /// `true` when this scan re-reads every tag (no cache, or a forced rescan).
    #[must_use]
    pub fn is_bypassed(&self) -> bool {
        self.bypass
    }
}

impl LibraryCache {
    /// Rows currently held (loaded from disk plus read this session).
    ///
    /// Useful for the footer/debug and for tests: a save prunes everything the
    /// scan did not confirm, so this drops back to the size of the library.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.lock().map_or(0, |entries| entries.len())
    }
    /// `true` when no rows are held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A summary, never the rows: a `{:?}` of a scanner config that carries a
/// 50 000-row index must not print 50 000 rows.
impl std::fmt::Debug for LibraryCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LibraryCache")
            .field("path", &self.path)
            .field("enabled", &self.enabled)
            .field("rows", &self.len())
            .finish_non_exhaustive()
    }
}

/// The on-disk library index for one folder.
///
/// Cheap to share (`Arc`-friendly): every mutating method takes `&self`.
pub struct LibraryCache {
    path: PathBuf,
    enabled: bool,
    /// The folder + scanner flags this file belongs to, written into the header
    /// so a save can never silently re-label a file from another configuration.
    config_fingerprint: AtomicU64,
    /// Set by `R` / `--refresh`: the next scan re-reads every tag but still
    /// writes the cache afterwards.
    bypass_next: AtomicBool,
    generation: AtomicU64,
    entries: Mutex<HashMap<PathBuf, Entry>>,
    /// Digest of the rows that were read from the file, if any. When a scan ends
    /// with the same digest and read nothing new, the file is already exactly
    /// right and is not rewritten (`--sync-interval` must not rewrite a multi
    /// megabyte index every minute to say "nothing happened").
    loaded_digest: Mutex<Option<u64>>,
    /// `true` once a row was read from disk this session.
    dirty: AtomicBool,
}

impl LibraryCache {
    /// A cache that never reads and never writes (used by `--no-cache` and by
    /// one-shot code paths like `--dump-sort`).
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            path: PathBuf::new(),
            enabled: false,
            config_fingerprint: AtomicU64::new(0),
            bypass_next: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            entries: Mutex::new(HashMap::new()),
            loaded_digest: Mutex::new(None),
            dirty: AtomicBool::new(false),
        }
    }

    /// Load `path` if it is a usable cache for `config_fingerprint`.
    ///
    /// Never fails: a missing, truncated, foreign or unreadable file silently
    /// becomes an empty cache (a cold scan instead of a startup error).
    #[must_use]
    pub fn open(path: PathBuf, config_fingerprint: u64, refresh: bool) -> Self {
        let cache = Self {
            path,
            enabled: true,
            config_fingerprint: AtomicU64::new(config_fingerprint),
            bypass_next: AtomicBool::new(refresh),
            generation: AtomicU64::new(0),
            entries: Mutex::new(HashMap::new()),
            loaded_digest: Mutex::new(None),
            dirty: AtomicBool::new(false),
        };
        if refresh {
            return cache;
        }
        match read_cache(&cache.path, config_fingerprint) {
            Ok((entries, digest)) => {
                tracing::debug!(
                    path = %cache.path.display(),
                    rows = entries.len(),
                    "library cache loaded"
                );
                if let Ok(mut slot) = cache.entries.lock() {
                    *slot = entries;
                }
                if let Ok(mut slot) = cache.loaded_digest.lock() {
                    *slot = Some(digest);
                }
            }
            Err(reason) => {
                tracing::debug!(
                    path = %cache.path.display(),
                    reason = %reason,
                    "library cache unusable; doing a full scan"
                );
            }
        }
        cache
    }

    /// `false` for the disabled cache (the caller then skips every cache call).
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Where this cache is stored (empty for the disabled cache).
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Ask the next [`crate::scan::scan`] to re-read every tag (ignoring this
    /// cache) while still refreshing it afterwards.
    pub fn force_full_scan(&self) {
        self.bypass_next.store(true, Ordering::SeqCst);
    }

    /// Open a scan: hands out a generation and consumes a pending forced rescan.
    pub fn begin_scan(&self) -> ScanToken {
        ScanToken {
            generation: self.generation.fetch_add(1, Ordering::SeqCst) + 1,
            bypass: self.bypass_next.swap(false, Ordering::SeqCst),
        }
    }

    /// The cached row for `path`, when the file has not changed since.
    ///
    /// A hit also stamps the entry so a later [`LibraryCache::save`] keeps it.
    #[must_use]
    pub fn lookup(
        &self,
        token: &ScanToken,
        path: &Path,
        fingerprint: Fingerprint,
    ) -> Option<Track> {
        if !self.enabled || token.bypass {
            return None;
        }
        let mut guard = self.entries.lock().ok()?;
        let entry = guard.get_mut(path)?;
        if entry.fingerprint != fingerprint {
            return None;
        }
        entry.generation = token.generation;
        Some(entry.track.clone())
    }

    /// Remember a freshly read row (never called for failures).
    ///
    /// A bypassed scan (shift-R, `--refresh`) stores what it read as usual: it
    /// ignores the *answers* in the index, not the index itself.
    pub fn insert(
        &self,
        token: &ScanToken,
        path: PathBuf,
        fingerprint: Fingerprint,
        track: &Track,
    ) {
        if !self.enabled {
            return;
        }
        if let Ok(mut guard) = self.entries.lock() {
            guard.insert(
                path,
                Entry {
                    fingerprint,
                    track: track.clone(),
                    generation: token.generation,
                },
            );
        }
        self.dirty.store(true, Ordering::Release);
    }

    /// Write the cache, dropping rows that this scan did not confirm.
    ///
    /// Returns `true` when the file was rewritten and `false` when it was already
    /// up to date. Writing is atomic (temporary file + rename) with mode `0600`,
    /// so two instances on one folder can never read a half-written file.
    pub fn save(&self, token: &ScanToken, digest: u64) -> Result<bool, String> {
        if !self.enabled {
            return Ok(false);
        }
        let fresh = self.dirty.load(Ordering::Acquire);
        let unchanged = self
            .loaded_digest
            .lock()
            .is_ok_and(|loaded| !fresh && *loaded == Some(digest));
        if unchanged {
            // Every row the scan produced is already in the file, byte for byte.
            return Ok(false);
        }
        let latest = self.generation.load(Ordering::SeqCst);
        let mut guard = self
            .entries
            .lock()
            .map_err(|_| "cache lock poisoned".to_string())?;
        if token.generation == latest {
            guard.retain(|_, entry| entry.generation == token.generation);
        }
        let bytes = write_cache(
            &guard,
            self.config_fingerprint.load(Ordering::SeqCst),
            digest,
        );
        drop(guard);
        write_atomic(&self.path, &bytes)?;
        if let Ok(mut loaded) = self.loaded_digest.lock() {
            *loaded = Some(digest);
        }
        self.dirty.store(false, Ordering::Release);
        Ok(true)
    }
}

/// Default cache file for a folder: `$XDG_CACHE_HOME/music-tui/<folder>-<hash>.bin`.
///
/// The folder's canonical path and the scanner flags that decide *which rows
/// exist* (`--extensions`, the caps, hidden files) are hashed into the name, so
/// two different configurations of the same folder keep two independent — both
/// warm — caches instead of invalidating each other on every start.
#[must_use]
pub fn default_cache_file(root: &Path, cfg: &ScanConfig) -> PathBuf {
    let dir = cache_dir().join("music-tui");
    let stem = root
        .file_name()
        .map(|name| sanitize(name.to_string_lossy().as_ref()))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "library".to_string());
    dir.join(format!(
        "{stem}-{:016x}-s{SORT_ALGORITHM}.bin",
        config_fingerprint(root, cfg)
    ))
}

/// Where cache files live (XDG first, then a last-resort temp directory).
#[must_use]
pub fn cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_CACHE_HOME") {
        let path = PathBuf::from(dir);
        if path.is_absolute() {
            return path;
        }
    }
    for key in ["HOME", "USERPROFILE"] {
        if let Some(home) = std::env::var_os(key) {
            let path = PathBuf::from(home);
            if path.is_absolute() {
                return path.join(".cache");
            }
        }
    }
    std::env::temp_dir()
}

/// Hash of everything that changes the *set* of rows a scan produces.
#[must_use]
pub fn config_fingerprint(root: &Path, cfg: &ScanConfig) -> u64 {
    let mut hash = HASH_SEED;
    hash = hash_str(hash, &root.to_string_lossy());
    hash = hash_str(hash, &SORT_ALGORITHM.to_string());
    let mut extensions = cfg.extensions.clone();
    extensions.sort_unstable();
    for extension in &extensions {
        hash = hash_str(hash, extension);
    }
    hash = hash_u64(hash, u64::try_from(cfg.max_files).unwrap_or(u64::MAX));
    hash = hash_u64(hash, u64::try_from(cfg.max_entries).unwrap_or(u64::MAX));
    hash = hash_u64(hash, u64::try_from(cfg.max_depth).unwrap_or(u64::MAX));
    hash_u64(hash, u64::from(cfg.include_hidden))
}

/// Order-independent hash of the whole library: the scanner folds every
/// [`row_digest`] into these two accumulators, and two scans of the same folder
/// agree only when every row (and the row count) agrees.
#[derive(Debug, Clone, Copy, Default)]
pub struct LibraryDigest {
    sum: u64,
    xor: u64,
    count: u64,
}

impl LibraryDigest {
    /// An empty digest.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild from lock-free accumulators (the scanner folds rows into two
    /// atomics rather than sharing a mutex).
    #[must_use]
    pub fn from_accumulators(sum: u64, xor: u64, count: u64) -> Self {
        Self { sum, xor, count }
    }

    /// Fold one row in.
    pub fn add(&mut self, row: u64) {
        self.sum = self.sum.wrapping_add(row);
        self.xor ^= row;
        self.count += 1;
    }

    /// Number of rows folded in.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.count
    }

    /// The value stored in [`crate::scan::ScanReport::digest`].
    #[must_use]
    pub fn finish(&self) -> u64 {
        // Mixing sum + xor + count keeps a swap ("one row replaced by another
        // with the same hash") from being invisible; a hash collision on 64 bits
        // remains possible and only ever costs a stale row until `R`.
        self.sum ^ self.xor.rotate_left(23) ^ self.count.wrapping_mul(0x9E37_79B9_7F4A_7C15)
    }
}

/// Canonical digest input for one row: path, file fingerprint and every field
/// the UI shows. Uses the same encoding as the cache file, so the two can never
/// disagree about what "the same library" means.
#[must_use]
pub fn row_digest(track: &Track, fingerprint: Fingerprint) -> u64 {
    let mut buf = Vec::with_capacity(160);
    write_row(&mut buf, track, fingerprint);
    hash_bytes(HASH_SEED, &buf)
}

// ---------------------------------------------------------------------------
// encoding
// ---------------------------------------------------------------------------

fn hash_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(HASH_PRIME);
    }
    hash
}

fn hash_str(hash: u64, text: &str) -> u64 {
    hash_bytes(hash, text.as_bytes())
}

fn hash_u64(hash: u64, value: u64) -> u64 {
    hash_bytes(hash, &value.to_le_bytes())
}

fn put_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(buf: &mut Vec<u8>, value: u64) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn put_str(buf: &mut Vec<u8>, value: &str) {
    put_u32(buf, value.len() as u32);
    buf.extend_from_slice(value.as_bytes());
}

fn put_opt_str(buf: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            buf.push(1);
            put_str(buf, value);
        }
        None => buf.push(0),
    }
}

fn put_opt_u32(buf: &mut Vec<u8>, value: Option<u32>) {
    match value {
        Some(value) => {
            buf.push(1);
            put_u32(buf, value);
        }
        None => buf.push(0),
    }
}

fn put_opt_u64(buf: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(value) => {
            buf.push(1);
            put_u64(buf, value);
        }
        None => buf.push(0),
    }
}

/// Write one row in canonical order: fingerprint first (it is what invalidates
/// the row), then the path, then every tag field.
fn write_row(buf: &mut Vec<u8>, track: &Track, fingerprint: Fingerprint) {
    put_u64(buf, fingerprint.size);
    put_u64(buf, fingerprint.mtime_nanos);
    put_str(buf, &track.path.to_string_lossy());
    put_str(buf, &track.format);
    put_str(buf, &track.title);
    put_opt_str(buf, track.artist.as_deref());
    put_opt_str(buf, track.album_artist.as_deref());
    put_opt_str(buf, track.album.as_deref());
    put_opt_u32(buf, track.track_no);
    put_opt_u32(buf, track.track_total);
    put_opt_u32(buf, track.disc_no);
    put_opt_u32(buf, track.disc_total);
    put_opt_u32(
        buf,
        track
            .year
            .map(|year| u32::try_from(year).unwrap_or(u32::MAX)),
    );
    put_opt_str(buf, track.date.as_deref());
    put_opt_u64(
        buf,
        track
            .duration
            .map(|d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX)),
    );
    put_opt_u32(buf, track.bitrate);
    buf.push(u8::from(track.corrupt));
}

/// The row fields as a `Track` (without a path: the caller has the map key).
fn read_row(reader: &mut Reader) -> Option<(Fingerprint, Track)> {
    let fingerprint = Fingerprint {
        size: reader.u64()?,
        mtime_nanos: reader.u64()?,
    };
    let path = reader.path()?;
    let mut track = Track {
        path,
        format: reader.string()?,
        title: reader.string()?,
        artist: reader.opt_string()?,
        album_artist: reader.opt_string()?,
        album: reader.opt_string()?,
        track_no: reader.opt_u32()?,
        track_total: reader.opt_u32()?,
        disc_no: reader.opt_u32()?,
        disc_total: reader.opt_u32()?,
        year: reader
            .opt_u32()?
            .map(|year| i32::try_from(year).unwrap_or(i32::MAX)),
        date: reader.opt_string()?,
        duration: reader.opt_u64()?.map(Duration::from_micros),
        bitrate: reader.opt_u32()?,
        corrupt: reader.boolean()?,
        key: SortKey::default(),
    };
    // Derived, never trusted from the file (see the module docs).
    track.refresh_key();
    Some((fingerprint, track))
}

fn write_cache(entries: &HashMap<PathBuf, Entry>, config_fingerprint: u64, digest: u64) -> Vec<u8> {
    let mut buf = Vec::with_capacity(entries.len().saturating_mul(192));
    buf.extend_from_slice(MAGIC);
    put_u32(&mut buf, SCHEMA_VERSION);
    put_u32(&mut buf, SORT_ALGORITHM);
    put_u64(&mut buf, config_fingerprint);
    put_u64(&mut buf, digest);
    put_u64(&mut buf, entries.len() as u64);
    // Sorted so that two runs produce byte-identical files (handy for tests and
    // for spotting accidental churn in a diff).
    let mut paths: Vec<&PathBuf> = entries.keys().collect();
    paths.sort();
    for path in paths {
        let entry = &entries[path];
        let mut row = Vec::with_capacity(192);
        write_row(&mut row, &entry.track, entry.fingerprint);
        put_u32(&mut buf, row.len() as u32);
        buf.extend_from_slice(&row);
    }
    let checksum = hash_bytes(HASH_SEED, &buf);
    put_u64(&mut buf, checksum);
    buf
}

fn read_cache(
    path: &Path,
    config_fingerprint: u64,
) -> Result<(HashMap<PathBuf, Entry>, u64), String> {
    let mut file = File::open(path).map_err(|err| err.to_string())?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).map_err(|err| err.to_string())?;
    decode_cache(&buf, config_fingerprint)
        .ok_or_else(|| "not usable (foreign, truncated or corrupt)".to_string())
}

/// Decode a cache file; `None` for anything that is not a complete, matching
/// cache. Every failure means the same thing to the caller: cold scan.
fn decode_cache(buf: &[u8], config_fingerprint: u64) -> Option<(HashMap<PathBuf, Entry>, u64)> {
    if buf.len() < 8 {
        return None;
    }
    let body_len = buf.len() - 8;
    let stored = u64::from_le_bytes(buf[body_len..].try_into().ok()?);
    if hash_bytes(HASH_SEED, &buf[..body_len]) != stored {
        return None;
    }
    let mut reader = Reader::new(&buf[..body_len]);
    if reader.bytes(4)? != MAGIC {
        return None;
    }
    if reader.u32()? != SCHEMA_VERSION {
        return None;
    }
    if reader.u32()? != SORT_ALGORITHM {
        return None;
    }
    if reader.u64()? != config_fingerprint {
        return None;
    }
    let digest = reader.u64()?;
    let count = reader.u64()?;
    if count > 10_000_000 {
        return None;
    }
    let mut entries = HashMap::with_capacity(usize::try_from(count).unwrap_or(0));
    for _ in 0..count {
        let len = usize::try_from(reader.u32()?).ok()?;
        let row = reader.bytes(len)?;
        let mut inner = Reader::new(row);
        let (fingerprint, track) = read_row(&mut inner)?;
        if !inner.done() {
            return None;
        }
        // Entries are written with generation 0; the first scan re-stamps the
        // ones it actually confirms.
        entries.insert(
            track.path.clone(),
            Entry {
                fingerprint,
                track,
                generation: 0,
            },
        );
    }
    if !reader.done() {
        return None;
    }
    Some((entries, digest))
}

/// Cursor over the encoded file; any under-run or invalid UTF-8 is `None`, which
/// the loader turns into "cold cache" rather than an error.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn done(&self) -> bool {
        self.pos >= self.data.len()
    }

    fn bytes(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(len)?;
        if end > self.data.len() {
            return None;
        }
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Some(slice)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.bytes(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.bytes(8)?.try_into().ok()?))
    }

    fn boolean(&mut self) -> Option<bool> {
        Some(self.bytes(1)?[0] != 0)
    }

    fn string(&mut self) -> Option<String> {
        let len = self.u32()?;
        String::from_utf8(self.bytes(usize::try_from(len).ok()?)?.to_vec()).ok()
    }

    fn path(&mut self) -> Option<PathBuf> {
        Some(PathBuf::from(self.string()?))
    }

    fn opt_string(&mut self) -> Option<Option<String>> {
        Some(if self.boolean()? {
            Some(self.string()?)
        } else {
            None
        })
    }

    fn opt_u32(&mut self) -> Option<Option<u32>> {
        Some(if self.boolean()? {
            Some(self.u32()?)
        } else {
            None
        })
    }

    fn opt_u64(&mut self) -> Option<Option<u64>> {
        Some(if self.boolean()? {
            Some(self.u64()?)
        } else {
            None
        })
    }
}

/// Write `bytes` to `path` atomically, creating the parent directory, and leave
/// the file readable only by its owner (a library listing can name files the
/// user would rather not share with the other accounts on a machine).
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(format!(".tmp-{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)
            .map_err(|err| err.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _chmod = file.set_permissions(fs::Permissions::from_mode(0o600));
        }
        file.write_all(bytes).map_err(|err| err.to_string())?;
        file.flush().map_err(|err| err.to_string())?;
    }
    fs::rename(&tmp, path).map_err(|err| {
        let _remove = fs::remove_file(&tmp);
        err.to_string()
    })
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .take(32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(path: &str) -> Track {
        let mut track = Track::from_path(PathBuf::from(path));
        track.artist = Some("Alpha Beta".into());
        track.album = Some("Album 1".into());
        track.track_no = Some(2);
        track.year = Some(1998);
        track.duration = Some(Duration::from_secs(198));
        track.bitrate = Some(320_000);
        track.refresh_key();
        track
    }

    const FP: Fingerprint = Fingerprint {
        size: 4096,
        mtime_nanos: 1_700_000_000_000_000_000,
    };

    #[test]
    fn round_trip_survives_every_field() {
        let mut entries = HashMap::new();
        for path in ["/m/a/one.mp3", "/m/a/two.flac", "/m/üñî/三.mp3"] {
            entries.insert(
                PathBuf::from(path),
                Entry {
                    fingerprint: FP,
                    track: track(path),
                    generation: 1,
                },
            );
        }
        let bytes = write_cache(&entries, 1234, 0xbeef);
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("cache.bin");
        fs::write(&file, &bytes).expect("write");
        let (loaded, digest) = read_cache(&file, 1234).expect("cache loads");
        assert_eq!(digest, 0xbeef, "the digest travels with the file");
        assert_eq!(loaded.len(), 3);
        let loaded = loaded.get(Path::new("/m/üñî/三.mp3")).expect("row");
        assert_eq!(loaded.fingerprint, FP);
        assert_eq!(loaded.track, track("/m/üñî/三.mp3"));
        assert_eq!(loaded.track.key.artist, "alpha beta", "keys are rebuilt");
    }

    #[test]
    fn wrong_config_or_version_is_a_cold_cache() {
        let mut entries = HashMap::new();
        entries.insert(
            PathBuf::from("/m/a/one.mp3"),
            Entry {
                fingerprint: FP,
                track: track("/m/a/one.mp3"),
                generation: 1,
            },
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("cache.bin");
        fs::write(&file, write_cache(&entries, 999, 7)).expect("write");
        assert!(read_cache(&file, 1000).is_err(), "different folder/flags");
        assert!(read_cache(&file, 999).is_ok());
        let mut truncated = fs::read(&file).expect("read");
        truncated.truncate(truncated.len() - 4);
        fs::write(&file, &truncated).expect("write truncated");
        assert!(
            read_cache(&file, 999).is_err(),
            "a truncated file must not load"
        );
    }

    #[test]
    fn fingerprint_changed_and_digest_changed() {
        let track = track("/m/a/one.mp3");
        let before = row_digest(&track, FP);
        assert_eq!(before, row_digest(&track, FP), "stable");
        assert_ne!(
            before,
            row_digest(&track, Fingerprint { size: 5000, ..FP }),
            "a size change is visible"
        );
        let mut retagged = track.clone();
        retagged.title = String::from("New Title");
        assert_ne!(row_digest(&retagged, FP), before, "a tag change is visible");

        let mut one = LibraryDigest::new();
        one.add(before);
        let mut other = LibraryDigest::new();
        other.add(row_digest(&retagged, FP));
        assert_ne!(one.finish(), other.finish());

        let mut empty = LibraryDigest::new();
        assert_eq!(empty.count(), 0);
        assert_eq!(empty.finish(), 0);
        empty.add(before);
        assert_ne!(empty.finish(), 0);
    }

    #[test]
    fn cache_file_name_follows_folder_and_flags() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("Music");
        let cfg = ScanConfig::default();
        let one = default_cache_file(&root, &cfg);
        assert!(
            one.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("Music-"),
            "the file is named after the folder: {}",
            one.display()
        );
        assert!(
            one.to_string_lossy()
                .contains(&format!("-s{SORT_ALGORITHM}")),
            "the sort version is part of the name: {}",
            one.display()
        );
        let wider = ScanConfig {
            extensions: vec!["mid".into()],
            ..Default::default()
        };
        assert_ne!(one, default_cache_file(&root, &wider), "flags matter");
        assert_ne!(one, default_cache_file(&dir.path().join("Other"), &cfg));
    }

    #[test]
    fn lookup_requires_a_matching_fingerprint_and_generation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("cache.bin");
        let cache = LibraryCache::open(file.clone(), 7, false);
        let token = cache.begin_scan();
        assert!(cache.lookup(&token, Path::new("/m/a.mp3"), FP).is_none());
        cache.insert(&token, PathBuf::from("/m/a.mp3"), FP, &track("/m/a.mp3"));
        assert!(cache.lookup(&token, Path::new("/m/a.mp3"), FP).is_some());
        assert!(
            cache
                .lookup(&token, Path::new("/m/a.mp3"), Fingerprint { size: 1, ..FP })
                .is_none(),
            "a changed file is a miss"
        );

        // A save prunes rows the scan did not confirm, and records the digest…
        assert!(cache.save(&token, 0x1234).expect("save"), "wrote");
        assert!(file.exists());
        let next = cache.begin_scan();
        assert!(
            cache.lookup(&next, Path::new("/m/a.mp3"), FP).is_some(),
            "confirmed rows survive into the next scan"
        );
        // …so a scan that changes nothing does not rewrite the file at all.
        let after = cache.begin_scan();
        assert!(
            !cache.save(&after, 0x1234).expect("save"),
            "an unchanged save is skipped"
        );
        // A different digest must write, and writing prunes to what that scan
        // confirmed.
        let third = cache.begin_scan();
        assert!(
            cache.lookup(&third, Path::new("/m/a.mp3"), FP).is_some(),
            "the scan confirms the row it walked"
        );
        assert!(
            cache.save(&third, 0x9999).expect("save"),
            "a different digest must write"
        );

        // ...and a forced rescan ignores the cache without disabling it.
        cache.force_full_scan();
        let bypass = cache.begin_scan();
        assert!(
            bypass.is_bypassed(),
            "the flag is handed to exactly one scan"
        );
        assert!(cache.lookup(&bypass, Path::new("/m/a.mp3"), FP).is_none());
        let after = cache.begin_scan();
        assert!(!after.is_bypassed(), "and consumed");
        assert!(cache.lookup(&after, Path::new("/m/a.mp3"), FP).is_some());
    }

    #[test]
    fn disabled_cache_does_nothing() {
        let cache = LibraryCache::disabled();
        let token = cache.begin_scan();
        assert!(!cache.is_enabled());
        assert!(cache.lookup(&token, Path::new("/m/a.mp3"), FP).is_none());
        cache.insert(&token, PathBuf::from("/m/a.mp3"), FP, &track("/m/a.mp3"));
        assert!(
            !cache.save(&token, 0).expect("no-op save"),
            "a disabled cache writes nothing"
        );
    }

    #[test]
    fn atomic_write_leaves_a_private_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("nested").join("cache.bin");
        write_atomic(&file, b"payload").expect("write");
        assert_eq!(fs::read(&file).expect("read"), b"payload");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&file).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "not world-readable");
        }
    }
}
