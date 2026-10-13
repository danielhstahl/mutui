# Performance evidence (`mutu-2h8.18`)

Two questions matter for a big library: does start-up take forever, and does the
UI stop responding? Both are answered by a machine-readable test
(`tests/perf_budgets.rs`, run by `cargo test`) plus benchmarks
(`cargo bench --bench sort_and_window`).

## Benchmark results

Measured on the development container (8 vCPU, release build, `criterion`):

```
sort/unsorted/1000          time:   [986 µs  988 µs  990 µs]
sort/unsorted/10000         time:   [29.80 ms 29.87 ms 29.96 ms]
sort/unsorted/50000         time:   [240.7 ms 241.5 ms 242.6 ms]
render/frame/100            time:   [364.2 µs 364.9 µs 365.8 µs]
render/frame/10000          time:   [406.9 µs 407.3 µs 408.1 µs]
render/frame/50000          time:   [407.7 µs 408.9 µs 411.3 µs]
scroll_scroll_by_rows       time:   [136.1 ns 136.6 ns 136.7 ns]  (100 scrolls)
scroll_thumb                time:   [1.49 ns 1.49 ns 1.49 ns]
```

The important line is `render/frame`: 100 rows and 50 000 rows cost the same
(~0.4 ms), because only the visible window is built into widgets. A frame budget
of 50 ms (one tick) is therefore not remotely at risk, even at 50 000 files.

Sorting 50 000 rows takes ~0.24 s in release builds. Rows stream in from the
scanner threads while it walks (so the first frame is drawn immediately and the
list fills in as it goes — asserted by
`first_frames_are_drawn_before_the_scan_finishes` in `tests/headless_loop.rs`),
and the one sort that puts them in final order runs when the scan finishes, on the
main thread. See "What is deliberately *not* fast" below.

## Enforced budgets (`tests/perf_budgets.rs`, debug build)

| Budget | Threshold |
| --- | --- |
| one frame with 50 000 songs | < 60 ms (and within 3× the cost of a 100-song frame) |
| sorting 50 000 songs | < 20 s, and < 6× the time for 25 000 (no accidental O(n²)) |
| 20 000 wheel notches on a 50 000-row library | < 10 s, offset clamps at the end |
| scanning 5 000 real files end to end | first paint < 1.5 s, whole scan < 60 s (`tests/scan_fixtures.rs`; measured ≈ 1 s) |
| scanner progress messages | coalesced: ≤ 1 per 100 ms or per 256 files |
| a warm (indexed) scan of 2 000 tagged files | < ½ the cold scan (measured 4× faster, see below) |

## Startup: cold vs indexed

The library index (`docs/adr/0003-library-cache.md`) exists because reading tags
— not walking, not sorting — is what you wait for. Where the time goes, 14 400
files, release build, warm page cache:

| step | cost | per file |
| --- | ---: | ---: |
| `readdir` over the whole tree | 15 ms | 1 µs |
| `stat()` of every audio file | 23 ms | 2 µs |
| `open()` + read the first 4 KiB of each | 140 ms | 10 µs |
| reading tags with `lofty`, 1 thread | 465 ms | 32 µs |
| reading tags with `lofty`, 8 threads | 205 ms | 14 µs |

Note the 8 threads buying only 1.7×: tagging is syscall-latency bound, which is
exactly the cost an index removes instead of spreading.

Measured end to end (`cargo run --release --example cache_bench -- <folder>`, same
14 400 files):

```
cold   :      1.07s   (14400 rows read from disk)
warm   :     60.1ms   (14400 rows served from the index)
sort   :    175.4ms   for 14400 rows (this part runs on the UI thread)
speedup:      17.7x
```

…which is 17.7× with a cold page cache and 2.4× (137 ms → 58 ms) once everything
is in the cache. A background sync that finds nothing new costs the warm number
and skips the sort entirely.

## Memory

The model is one `Track` per file (path + a handful of tags + the computed sort
key). At 50 000 files that is tens of megabytes, and the renderer never holds
more than `visible + 1` widgets, which is why frame time is flat.

## What is deliberately *not* fast

Natural comparison is not cheap (it scans digit runs), so the sort is
O(n log n) with a heavy constant: 175 ms for 14 400 rows whose artists and album
titles mostly tie, and it runs on the main thread when a scan *changed* the
library. That is accepted: the index means a rescan usually changes nothing, and
in that case the sort is skipped entirely (ADR 0003 §7). Moving it to the scanner
thread would mean buffering every row there and losing the streaming first paint.
If a library ever grows past ~200 000 files, the right next step is to sort inside
the index file rather than to micro-optimize the comparator.
