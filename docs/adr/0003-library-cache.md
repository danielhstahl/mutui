# ADR 0003 — Library index: fingerprinting, invalidation and background sync

Status: accepted (2026-10-13)
Decides: cached library startup and periodic background sync.
Implemented by `src/library_cache.rs` (index, digest, file format),
`src/scan.rs` (consulting it), `src/app/reducer.rs` (applying a scan),
specified here normatively. Constrained by
[`0001`](0001-stack-and-architecture.md) (no new dependencies, threading
contract) and [`0002`](0002-sort-and-artist-run.md) (what a row *means*).

## 1. Why

Measured on 14 400 files in the development container (8 vCPU, release build,
warm page cache; see `docs/benchmarks.md`):

| step                                        | cost       | per file |
|---------------------------------------------|-----------:|---------:|
| `readdir` over the whole tree                |     15 ms  |   1 µs   |
| `stat()` of every audio file                 |     23 ms  |   2 µs   |
| `open()` + read the first 4 KiB of each file |    140 ms  |  10 µs   |
| read tags with `lofty` (the scan today)      |    ~200 ms |  14 µs   |

Reading tags is ~70% of a scan, and it barely parallelises (8 threads buy only
1.7×), because it is syscall-latency bound rather than CPU bound. So the fix is
to stop opening files that have not changed, not to make the scanner cleverer.

The index does that: the walk still happens (it is what notices additions and
deletions) but every unchanged audio file is answered from the index instead of
being opened. Measured on the same tree (`cargo run --release --example
cache_bench -- <folder>`): **1 s → 60 ms cold page cache, 137 ms → 58 ms warm**.

## 2. Vocabulary

* **index** — the on-disk record of one folder's library: one row per audio file.
* **fingerprint** — the cheap identity of a file's contents: `size` plus mtime at
  nanosecond resolution (`Fingerprint`).
* **row** — one `Track`: path plus the tags read from the file.
* **digest** — an order-independent 64-bit hash of every row a scan produced
  (`LibraryDigest`), i.e. of the whole library.
* **generation** — a counter stamped on every index entry by the scan that last
  confirmed it; used to prune rows for files that no longer exist.
* **cold scan** — every tag read from disk. **Warm scan** — every row answered
  from the index. **Sync** — a scan started while the app is running (the
  periodic one, `r`, or shift-`R`).

## 3. The trust rule

A cached row may be reused **iff** the file's current fingerprint equals the one
stored with it. Consequences, normatively:

1. The scan walks the whole tree every time, unconditionally. The index never
   short-circuits the walk, so new, renamed and deleted files are always found.
2. A file whose fingerprint differs is re-read and its row replaced.
3. A file that is not confirmed by the scan (deleted, or outside the extension
   filter) is pruned from the index at the next successful write.
4. **Failures are never stored.** A file whose tags could not be read is not an
   answer; it is retried by every scan, so a half-finished download or a
   permission blip cannot become a permanent `!` row.
5. A file that cannot be `stat`ed is neither looked up nor stored.

What the fingerprint cannot see is a rewrite that preserves both size and mtime
(a tagger that restores mtimes, `cp -a` over a file). That is accepted, and the
answer is `R` / `--refresh` (§7), not a stronger hash: hashing content would
mean reading the file, which is the cost being removed. On filesystems where
mtime is unreliable (some NFS/SMB mounts, `noatime`-style oddities) the answer is
`--no-cache`.

## 4. What the index stores

Per row: the fingerprint and the **raw** fields of `Track` — path, format, title,
artist, album artist, album, track/disc numbers and totals, year, date,
duration, bitrate, `corrupt`.

It never stores a derived value. `SortKey` is rebuilt on load
(`Track::refresh_key`) and `album_year` is recomputed by `sort_tracks`, because
it is a whole-library property (§3.2 of `0002`). A cache that stored derived keys
would freeze the sort at the moment it was written.

## 5. Invalidation

The header carries three versions, and a mismatch is a cold scan, never an error:

| field                | bump when                                                            |
|----------------------|----------------------------------------------------------------------|
| `SCHEMA_VERSION`     | the byte layout of a row or of the header changes                     |
| `SORT_ALGORITHM`     | the *meaning* of a row changes (`src/model/sort.rs`, ADR 0002)       |
| config fingerprint   | the folder path, or `--extensions` / `--max-files` / `--max-entries` / `--max-depth` / hidden-file flags change |

`SORT_ALGORITHM` exists because a cached library must never hide a sorting fix:
without it, the release that changed album ordering (`mutu-172`) would have been
invisible to every user whose library was cached. It is a public constant in
`src/model/sort.rs` and is stamped into the file name too, so a stale file is
simply never read.

Two more rules:

* **A partial scan never writes.** If the scan hit `--max-files` or
  `--max-entries`, or was cancelled, the index is left exactly as it was: the
  scan did not see the whole library, so pruning to what it saw would lose rows.
* **An unchanged scan writes nothing.** If nothing was read from disk during the
  scan and the resulting digest equals the digest of the file on disk, the file
  is already byte-identical and is not rewritten. Without this, a periodic sync
  would rewrite a multi-megabyte index every minute to say "nothing happened".

## 6. Location, format and concurrency

* Path: `$XDG_CACHE_HOME/music-tui/<folder>-<config fingerprint>-s<SORT_ALGORITHM>.bin`,
  else `$HOME/.cache/…`, else `$TMPDIR/…`. `--cache-file` overrides. The config
  fingerprint is in the name as well as in the header, so two different flag
  combinations of one folder keep two independent — both warm — indexes instead
  of invalidating each other.
* Format: hand-rolled little-endian binary (magic, the three header versions,
  the digest, the row count, length-prefixed rows, a trailing FNV-1a checksum of
  the whole body). No dependency: `serde` is only a dev-dependency today and
  ADR 0001 keeps the shipped dependency graph small.
* Any problem reading the file — missing, truncated, foreign, unknown version,
  bad checksum — means "start cold" and is logged at `debug`. Startup never
  fails because of the index.
* Writes go to `<path>.tmp-<pid>` then `rename()`, mode `0600`. Two instances on
  one folder can therefore never read a half-written file, and neither can any
  other account on the machine. There is no lock: the last complete writer wins,
  which is safe because a row is only ever written after reading that file.

## 7. Applying a scan, and the periodic sync

Two scan shapes, decided when the scan starts:

* **Streaming** (the first scan only): rows are appended to the live library as
  they arrive, so the list fills in while the walker runs.
* **Staging** (every later scan): rows accumulate in `AppState::scan_stage` and
  are sorted and swapped in once, when the scan ends. Appending a second copy of
  the library to the first — which is what the previous rescan path did — is the
  bug this rule forbids.

At the end of a scan the digest decides:

| condition                          | required behaviour                                                                 |
|------------------------------------|-------------------------------------------------------------------------------------|
| digest equals the digest on screen | **discard** the rows. No re-sort, no re-anchor, no row moves, no visible change.     |
| digest differs                     | sort the staged rows, replace the library, re-anchor the cursor (`path` then artist), and tell the user what changed (`N new, M removed`). |

The first row is what makes an idle background sync free: no re-sort (the one
part of a rescan that costs real time on the UI thread) and no rows jumping under
the mouse.

The periodic sync (`--sync-interval`, default 60 s) is issued by the reducer on a
`Tick` and must not fire when:

* the interval is zero,
* a scan is already in flight (`phase != Ready`) — syncs never stack,
* the user is dragging the scrollbar,
and it resets its own timer when it fires, so a slow scan cannot make every
following tick fire.

## 8. Escape hatches

| control                | effect                                                              |
|------------------------|---------------------------------------------------------------------|
| `r`                    | sync using the index (cheap)                                        |
| shift-`R`              | next scan re-reads every tag, then refreshes the index               |
| `--refresh`            | same, for this process only                                          |
| `--no-cache`           | never read or write the index                                        |
| `--cache-file <path>`  | store the index somewhere else                                       |
| `--sync-interval <s>`  | background sync period; `0` turns syncs off                          |

`--dump-sort` never touches the index: a one-shot command must not write files.
`--no-cache` combined with a non-zero `--sync-interval` re-reads every file once
per interval — that is the slow path, chosen deliberately by the flags.

## 9. Deliberately not done

* **File-system notifications** (`notify`/inotify). Watch counts scale with
  directories (default `max_user_watches` is easily exceeded by a large tree),
  editors write through temporary files so events need debouncing, and network
  mounts do not deliver events at all. A sync that costs one `stat` per file does
  not need them.
* **SQLite or `serde`** for the format (ADR 0001 dependency discipline).
* **Content hashing** (§3) and per-entry time-to-live: the unconditional walk
  already guarantees that additions and deletions are found, so a TTL would only
  add staleness, not correctness.
* **Moving the sort off the UI thread.** `sort_tracks` runs on the main thread
  when a scan *changed* something (~175 ms per 14 000 rows, pre-existing). The
  index makes that rare; moving it would mean buffering every row in the scanner
  thread and losing the streaming first paint.
* **Cross-instance coordination** beyond atomic writes, and any sharing of one
  index between two different folders.

## 10. Where this is enforced

* `src/library_cache.rs` unit tests: round-trip of every field, bad config and
  truncated file are cold scans, fingerprint and generation rules, unchanged save
  is skipped, `0600` and atomicity of the write.
* `tests/library_cache.rs`: cold writes / warm reads without opening files, warm
  rows identical to cold rows, retag picked up, deletion prunes, failures are
  retried, capped scans do not write, garbage file is repaired, forced rescan,
  per-flag index files, unchanged sync leaves the file untouched — plus the
  reducer half: a rescan never duplicates, an unchanged sync keeps cursor/run/
  scroll, a changed sync reports `N new, M removed`, periodic sync timing.
* `tests/perf_budgets.rs::a_warm_scan_is_much_cheaper_than_a_cold_one` fails if
  the index stops paying for itself (budget: warm < cold / 2).
* `tests/headless_loop.rs::periodic_syncs_and_shift_r_reach_the_scan_job_through_the_loop`
  drives the real event loop and checks the sync and the forced rescan arrive.
