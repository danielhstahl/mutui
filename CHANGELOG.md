# Changelog

All notable changes to `music-tui` are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), the project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html), and the sort and
playback semantics are fixed by `docs/adr/0002-sort-and-artist-run.md`.

## [Unreleased]

### Added

* **The library is indexed, so starting up stops reading every file**
  (`docs/adr/0003-library-cache.md`). Reading tags — not walking, not sorting —
  is what start-up waits on (~70% of a scan, and it barely parallelises because
  it is syscall-latency bound). `music-tui` now keeps an index of the library in
  `$XDG_CACHE_HOME/music-tui/<folder>-<hash>.bin`, and compares each file's size
  and mtime before opening it: unchanged files are answered from the index and
  never opened, while the walk still happens so additions and deletions are
  always found. Measured on a 14 400-file library: **1.07 s → 60 ms** cold, 137 ms
  → 58 ms warm (`cargo run --release --example cache_bench -- ~/Music` prints it
  for your own folder). The index is versioned against the sort rules, so a
  release that changes ordering is never hidden by a stale cache; unreadable
  files are never cached, so a broken file is retried every scan; a scan that hit
  a cap or was cancelled never rewrites it; and a file that changed size or mtime
  is re-read. New flags: `--no-cache`, `--refresh`, `--cache-file`,
  `--sync-interval`, and shift-`R` to re-read every tag once.
* **Background library sync.** Every 60 seconds (`--sync-interval`, `0` turns it
  off) the app re-syncs in the background: new songs appear, deleted ones go
  away, retagged ones update, and the footer reports `Library updated: 2 new, 1
  removed`. Because a scan carries a digest of the whole library, a sync that
  finds nothing changes nothing at all — no re-sort, no rows moving under the
  mouse, no rewrite of the index file. Syncs never stack, and none starts while
  you are dragging the scrollbar.

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
* `docs/benchmarks.md` said the sort runs on the scanner threads, which was never
  true: it runs on the main thread when a scan *changed* the library (175 ms per
  14 400 rows with many shared artist/album prefixes). The page now says where it
  really happens, why it is acceptable, and that an unchanged sync skips it.

### Fixed

* **`Space` after a long pause brings back the paused song instead of the next
  one** (`mutu-1sg`, `mutu-nb3`, and the report that outlived both of them).
  `SIGSTOP` stops a player process but not the clock it measures itself against,
  and a stopped process cannot exit — so a song paused for minutes can already be
  over (or its decoder killed by the audio server for idling) and the app can only
  reap it once `SIGCONT` wakes it, by which time the reducer has already put the
  state back to `Playing`. The end then looked like an ordinary `Finished`, the
  artist run advanced in the same instant, and the key pressed for one song
  started its neighbour. `audio::PlayerEvent::EndedWhilePaused` now carries that
  distinction: `CommandBackend` remembers that the user was holding a track paused
  and that it never became audible again after the wake-up (and checks for a child
  that was already gone *before* signalling it, because `kill()` on a zombie
  succeeds), and the reducer answers that event by restarting the paused song and
  saying `X ended while paused — starting it again` rather than by advancing.
  A track that ends at or past its tagged length is still a real end, so the run
  still moves on at the end of a song paused near it. Tests:
  `tests/pause_outlives_decoder.rs` drives the real event loop, the real audio
  worker and a **real decoder process** that runs out of song behind the stop
  (it fails with "the next song started" without the fix), two more process-level
  cases in `src/audio/command_backend.rs`, and two reducer cases for both halves
  of the event.
* **A scan can no longer put a progress message ahead of the first row.** The
  progress throttle was seeded with "never sent", so its very first call always
  passed the gate; whenever the walker won the race against the tagging workers —
  which is what CPU starvation on a busy runner does — that first `Progress`
  landed in front of every `Track`, and a consumer that stops reading at the first
  message that is not a row then saw an *empty library*. Three test helpers did
  exactly that, which is why `rapid_clicks_leave_exactly_one_track_playing` and
  `stale_finished_event_from_the_worker_is_ignored` reported "no plays at all" on
  GitHub Actions but never locally. The throttle is now seeded with the start of
  the scan, so the first progress tick waits for `PROGRESS_INTERVAL` to have
  actually elapsed (which is when progress reporting starts to say something), the
  helpers read a scan in any order (`Done` ends the stream, `Progress` is
  ignored), and `tests/scan_fixtures.rs::the_first_message_of_a_scan_is_a_row_not_progress`
  pins the contract.
* **Rescanning (`r`) no longer duplicates the library.** The streamed rows were
  appended to the rows already on screen and everything was re-sorted together,
  so a rescan of an unchanged folder showed the library twice (and moved the
  selection while doing it). Later scans are now staged and swapped in once, and
  a scan whose digest matches the library on screen is discarded outright.
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
* **`Space` brings back the song you paused, minutes later or not** (`mutu-1sg`).
  "After a few minutes, pressing space when paused doesn't unpause, it moves to
  the next song." Three separate faults added up to that sentence. (1) The reducer
  auto-advanced the run on a `Finished`/`Failed` for the *paused* track, so a
  decoder that reached the end of the file a beat after the stop landed — or one
  that died, or a backend that refuses to pause — silently started the next
  song, and the next `Space` then landed on *that* song. A paused run now never
  advances by itself: the row is held, the footer says
  `<title> ended while paused — Space replays it`, and `Space` replays the song
  that was paused instead of whatever row the cursor sits on. (2) In the
  external-player backend, `set_volume` (and `seek`) are implemented by killing
  the decoder and launching it again at the current position — and that respawn
  cleared the path, so it reported `no track selected` and left the track dead,
  and it also dropped `paused_at`, so a mute or volume press while paused left
  the *screen* saying `Paused` over a decoder that was playing again: a few
  minutes later that song really did end and the run walked on. Respawn keeps
  the track and re-applies the `SIGSTOP` to the new child, so `+`/`-`/`m` can no
  longer quietly unpause anything. Spawning the freshly-killed binary can also
  briefly fail with `ETXTBSY` ("Text file busy"), which is now retried instead of
  ending the track. (3) A background library sync (a minute apart) could shift
  the paused song to a different row while `playback` kept the old index, so the
  replay — and the playing marker — pointed at a neighbour; the swap-in now goes
  through `AppState::rescan_set_library`, which re-anchors the row. Covered by
  `tests/reducer_playback.rs`, two end-to-end runs in `tests/headless_loop.rs`
  (five virtual minutes of ticking, and an event injected through the real audio
  worker) and real-process tests in `src/audio/command_backend.rs` that read the
  child's state out of `/proc`.
* **A refresh between pause and unpause no longer skips to the next song**
  (`mutu-nb3`). The same sentence as `mutu-1sg`, one layer down: `playback`
  remembered the paused song as a *row index*, and a refresh moves rows. Two ways
  in: the first scan sorts every row it streamed, so a song clicked out of the
  half-filled list sat somewhere else entirely once the scan finished
  (`Playlist::finish_scan` re-anchored the cursor and left `playback` on the
  number it had), and a sync re-anchored `playback` from the *cursor* — so a
  paused song the user had browsed away from lost its row too. `Space`, which
  replays the paused row when that row's player already ended, then started
  whichever neighbour the old number now named. The state machine owns a song, not
  a row: `request_play` records `AppState::playback_path`, and every library
  replacement (`finish_first_scan`, `rescan_set_library`) re-derives the row from
  it in `reconcile_playback_row`. The `▶`/`▮` marker, the scroll-follow and the
  replay all resolve that path, so a song the sync *deleted* marks no row at all
  and `Space` answers `<title> is no longer in the library — nothing to resume`
  rather than starting a stranger. Covered by `tests/pause_across_refresh.rs`,
  including two end-to-end runs that replay a whole scan into the real event loop
  between the pause and the unpause.

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
