# Changelog

All notable changes to `music-tui` are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), the project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html), and the sort and
playback semantics are fixed by `docs/adr/0002-sort-and-artist-run.md`.

## [Unreleased]

### Changed

* **The blue is softer and now readable on a black terminal** (`mutu-eo1`). The
  footer clock was drawn in ANSI `Blue` — `#0000EE` on the stock palette, 2.2:1
  against black, i.e. invisible — and the scrollbar thumb in `LightBlue`
  (4.4:1, fully saturated, and a solid block, which is what read as harsh). Both
  now use one desaturated steel blue, `#87AFD7`: 9.14:1 on black, 7.61:1 on the
  footer bar, 5.89:1 on the selected-row background, and exactly xterm-256
  index 110, so a terminal without 24-bit colour renders it un-dithered. All UI
  colours moved out of the widgets into `src/ui/palette.rs`, where they are named
  by meaning, and that module's tests measure WCAG 2.1 contrast so a harsh or
  illegible colour cannot creep back. No other hue changed.

### Fixed

* **An artist's albums are now ordered by release year, not by album title**
  (`mutu-172`). The sort key is artist → album year → album → disc → track →
  row year → file name → path, so a 1990 record comes before a 1995 one even
  when its title sorts later. The *album year* is the earliest year written
  anywhere on that album, which is what keeps an album in one piece: deriving the
  year per file would let a partly untagged 3-CD set shatter, its undated rows
  (`0`) jumping to the front of the artist. Amends `docs/adr/0002` §3/§3.2 (the
  §6 worked example changed with it, and `tests/adr_spec.rs` still checks code
  against the document); `tests/album_year_order.rs` covers the scanned case and
  a generated library where every artist, album and year combination is
  scrambled. An album with no year at all sorts first inside its artist, and a
  library with no years sorts exactly as it did before.

## [0.1.0]

First release. Implements the epic `mutu-2h8`.

### Added

* Recursive folder scanner on worker threads: streams rows while it walks, reads
  tags with `lofty`, respects `--max-files` / `--max-entries`, skips hidden
  trees, reports unreadable files instead of failing, and is cancellable.
* Total-order sort: artist (album artist first) → album → disc → track → year →
  file name → path, with natural number ordering and `u32::MAX` for missing
  track numbers. Normative spec plus a 22-row worked example in ADR 0002, and a
  test that parses the markdown table and compares it with the comparator.
* Playlist model with an explicit cursor and artist-run boundaries
  (`run`, `advance`, `prev`, `at_run_end`, re-anchoring after a rescan).
* Click-to-play with auto-advance limited to the clicked artist's run, plus the
  `End of artist: <name>` footer message; stale `Finished` events are dropped,
  corrupt files are skipped once, and two consecutive failures stop the run.
* Single scrollable list pane with one row per song, selection and playing
  markers, corrupt-file marker, and windowed rendering whose cost is
  independent of library size.
* Scrollbar (proportional thumb, wheel, drag, track click) and clamped offset
  math shared by mouse and keyboard.
* Audio worker thread with three backends: external command players
  (`mpv`, `ffplay`, `mpg123`, `mpg321`, `madplay`, `afplay`, `mplayer`),
  a silent simulated backend, and a mock backend for tests.
* Keyboard parity: quit, play, pause/resume, navigation, page scroll, jump to
  first/last, next/previous inside the run, mute, volume, rescan, help overlay.
* Footer status line: state, now playing, position/duration, run position,
  library count, active backend and the latest error or notice.
* Terminal lifecycle: raw mode + alternate screen + mouse capture, panic hook
  that restores the screen, `SIGINT` handling, guaranteed restore on quit.
* File-only diagnostics with typed errors and documented exit codes
  (`2` usage/folder, `3` not a terminal, `4` terminal failure).
* CLI: required folder argument with validation, `--extensions`,
  `--scan-threads`, `--max-files`, `--max-entries`, `--wheel-rows`,
  `--backend`, `--ascii`, `--no-mouse`, `--log-file`, `--debug`, `--dump-sort`.
* Test suite (~150 tests) that needs no speakers and no TTY: unit tests, ADR
  consistency tests, scanner fixtures on synthetic WAV/MP3/FLAC files,
  headless event-loop tests against a real audio worker, snapshot tests of the
  rendered screen, CLI tests including a pty run, and hard performance budgets.
* `cargo bench` benchmarks for sorting and frame rendering.

### Known limitations

* Pause/resume depends on `SIGSTOP`/`SIGCONT`, so it is unavailable when a native
  Windows player is used (the app reports the refusal instead of guessing).
* No gapless playback and no seeking from the UI (the backend supports `Seek`,
  the key binding is not exposed yet).
* Playlists are per run: there is no persistent queue file and no M3U import.
