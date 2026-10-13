# ADR 0001 — Stack, crate layout and threading model

Status: accepted (2026-10-13)
Decides: `mutu-2h8.1`. Constrains: `mutu-2h8.8`, `mutu-2h8.12`, `mutu-2h8.16`.

## 1. Decision

Single Rust package, one binary (`music-tui`) plus a library (`music_tui`) that
holds every piece of logic, so the whole app is testable without a terminal or a
sound device.edition 2021, `rust-version = 1.85`, stable toolchain.

## 2. Dependencies (as locked)

| crate                | version  | why                                                                        | rejected alternative                                                            |
|----------------------|---------:|----------------------------------------------------------------------------|---------------------------------------------------------------------------------|
| `ratatui`            | `0.30.2` | immediate-mode widget rendering, `List`/`Scrollbar`/`Paragraph`, `TestBackend` for snapshot tests | raw ANSI, `tui-rust` predecessor, `egui`/`termion` (termion is Unix-only and unmaintained) |
| `crossterm`          | `0.29.0` | cross-platform raw mode, alternate screen, and **mouse** events (click, drag, wheel) | `termion` (no Windows, no drag events we need), `tui-input`                    |
| `lofty`              | `0.25.4` | one crate reading/writing tags for mp3/flac/ogg/opus/m4a/wav/aac/aiff/wma  | `id3` + `metaflac` + … (one crate per format), `symphonia`-only (decoder, weaker tag coverage) |
| `walkdir`            | `2.5.0`  | recursive traversal with depth limit, no symlink following, error-per-entry | `ignore` (gitignore semantics we do not want), manual `read_dir` recursion       |
| `clap` (derive)      | `4.6.7`  | positional folder + flags, `--help`/`--version`, exit code 2 on misuse      | `argh`, hand-rolled `env::args` parsing                                          |
| `thiserror`          | `2.0.21` | typed error enums in the library (`error::Error`)                           | stringly errors, `snafu` (fine, but heavier for our surface)                     |
| `anyhow`             | `1.0.104`| declared acceptable for the binary edge — in practice `main.rs` only needs `Error::exit_code()`, so it is kept as a declared-but-unused option for future plugins | — |
| `tracing` + `tracing-subscriber` | `0.1.44` / `0.3.23` | file-only diagnostics with `RUST_LOG` override                  | `log`+`env_logger` to stderr (forbidden once the alt screen is up), `fern`       |
| `unicode-width`      | `0.2.2`  | cell-accurate truncation of CJK/emoji titles                                | `str::chars().count()` (wrong width)                                            |
| `libc`               | `0.2.190`| `SIGSTOP`/`SIGCONT` to pause an external player                             | spawning a second process per pause                                             |
| dev: `insta`         | `1.49.0` | snapshot tests of the rendered buffer                                       | manual golden files                                                             |
| dev: `tempfile`      | `3.27.0` | scratch fixture trees in tests                                              | shared `/tmp` names                                                             |
| dev: `criterion`     | `0.5.1`  | benchmarks for sorting and frame rendering (`cargo bench`)                 | `#[bench]` (nightly-only), timing inside unit tests (flaky)                      |

`rayon` is not used — the scanner uses plain `std::thread` workers over a bounded
`std::sync::mpsc::sync_channel`, which is all the parallelism tagging needs and
keeps the dependency graph small.

## 3. Audio: why not `rodio`/`cpal`

The ticket recommended `rodio` (cpal + symphonia). We rejected it after measuring:

* `cpal` → `alsa-sys` requires `pkg-config` **and** `libasound2-dev` headers at
  build time. On a container or a CI runner without them (and on any host where
  the user has only the runtime library), `cargo build --release` fails in a C
  build script — a hard dependency the acceptance criteria forbid.
* Headless CI has no sound device at all, so an in-process sink cannot be tested.

Decision: playback is a **pluggable backend** (`audio::Backend`), chosen at
runtime with `--backend`:

| backend   | what it does                                                                                   |
|-----------|------------------------------------------------------------------------------------------------|
| `auto`    | pick the first known external player found on `PATH`; otherwise fall back to `silent` and say so in the footer |
| `command` | `audio::CommandBackend`: spawn the first of `mpv`/`ffplay`/`mpg123`/`mpg321`/`madplay`/`afplay`/`mplayer` found on `PATH`, end-of-track detected by reaping the child, pause = `SIGSTOP`, resume = `SIGCONT` |
| `silent`  | `audio::NullBackend`: no sound, perfect timing events (drives the headless tests and machines without audio) |
| `mock`    | `audio::MockBackend`: records commands and replays scripted events (test support)              |

Child processes always get `stdin`/`stdout`/`stderr` from `/dev/null`, and the
backend kills its child on `Drop`, so the terminal can never be corrupted by a
player and no player outlives us.

A `rodio` backend remains a drop-in addition: implement `audio::Backend`
(`play/pause/resume/stop/seek/set_volume/poll`) and add it to
`run::make_backend`; no other module changes.

## 4. Threading and channel contract

```
main thread                      player thread                    scanner threads
----------             ------------------------                   ---------------
terminal + AppState      owns one Backend                         walkdir + N taggers
   |   ^                        |   |
   |   | PlayerCommand (mpsc)   |   | PlayerEvent (mpsc)
   +---|------------------------+   |
       +----------------------------+
   ScanMessage (mpsc, swapped on rescan) <------------------------------+
```

* **Main thread** owns the terminal, `AppState` and the reducer. It never blocks:
  `crossterm::event::poll(50 ms)`, then `try_recv` on both channels, then one
  draw if anything changed.
* **Player thread** (`audio::worker`) owns the `Backend`. It receives
  `PlayerCommand`, sends `PlayerEvent`. Anything the backend reports
  asynchronously (end of track, spawn failure, position) is *only* sent as an
  event — the worker never touches `AppState`.
* **Scanner threads** (`scan::scan`) walk on the main scanner thread and tag on
  `--scan-threads` workers, streaming `ScanMessage` into one channel. A rescan
  cancels the old walk and swaps the `Receiver` (`run::ScanJob::restart`) so
  late messages from a cancelled scan disappear instead of duplicating rows.
  Before opening a file a worker asks the library index (`src/library_cache.rs`,
  [`0003`](0003-library-cache.md)) whether that file changed since the last scan;
  the walk itself is never skipped.

Message definitions (normative):

```rust
pub enum PlayerCommand {
    Play(PlayRequest),          // PlayRequest { path, track_id, duration }
    Pause,
    Resume,
    Stop,                       // silence; never produces Finished
    Seek(Duration),
    SetVolume(f32),             // 0.0..=1.0
    Quit,
}

pub enum PlayerEvent {
    Started  { track_id: u64 },
    Position { track_id: u64, position: Duration },   // <= ~4/s
    Finished { track_id: u64 },                       // track ended by itself
    Failed   { track_id: u64, reason: String },       // could not play
    Notice(String),                                   // control refused (volume/seek)
}

pub enum ScanMessage {
    Track(Track),                                     // streamed rows
    Progress(ScanProgress),                           // <= 10/s or 1 per 256 files
    Done(ScanReport),                                 // exactly once
}
```

`track_id` is a `u64` handed out by the reducer (`AppState::next_track_id`) and
is stamped on every play request. A `PlayerEvent` whose `track_id` is not the
currently playing one is **dropped** — that is how the app survives a `Finished`
that races a new click.

## 5. Module layout

```
src/lib.rs          module surface
src/main.rs         thin binary: parse, log, validate, dispatch, exit codes
src/cli.rs          clap flags + folder validation + default log path
src/error.rs        thiserror enums, footer text, exit codes
src/log.rs          file-only tracing-subscriber setup
src/terminal.rs     raw mode / alt screen / mouse capture RAII, panic + SIGINT hooks
src/model/          track.rs, sort.rs, playlist.rs, fixtures.rs (pure)
src/scan.rs         walker + taggers + ScanReport (pure w.r.t. UI)
src/library_cache.rs on-disk library index: fingerprints, digest, file format (0003)
src/audio/          mod.rs (contract + worker), command_backend.rs, null.rs, mock.rs
src/app/            state.rs, reducer.rs (pure), eventloop.rs, time.rs
src/ui/             layout.rs, scroll.rs, list.rs, footer.rs, mouse.rs, render.rs, truncate.rs
src/run.rs          bootstrap: backend choice, ScanJob, session
src/testing.rs      synthetic WAV/MP3/FLAC fixtures written by the tests
benches/            sort_and_window.rs (criterion); budgets in tests/perf_budgets.rs
```

The `model`, `ui::scroll`, `ui::mouse`, `ui::footer` and `app::reducer` modules
are **pure**: they take data and return data, which is what makes the reducer,
the sort and the hit-testing unit-testable.

## 6. Forbidden outputs

Once `TerminalGuard::enter()` has taken over the screen (i.e. while
`terminal::TERMINAL_ACTIVE` is true), **nothing may be written to stdout or
stderr**:

* `src/log.rs` always writes to a file (default `$XDG_STATE_HOME/music-tui/music-tui.log`,
  else `$HOME/.local/state/music-tui.log`, else `$TMPDIR/music-tui.log`;
  `--log-file` overrides).
* Every child process is spawned with `Stdio::null()` for all three streams.
* No `println!`/`eprintln!` may appear in `src/app`, `src/ui`, `src/audio`,
  `src/scan` or `src/model` — they are legal only in `main.rs` before
  `run::run()` is called and in the `--dump-sort` path, which runs before any
  terminal takeover.
* The panic hook restores the terminal first and only then lets the default hook
  print, together with the log path.

`tests/cli_binary.rs::running_the_ui_in_a_pty_never_pollutes_the_screen` runs
the binary under `script` (a real pty) and asserts that it enters and leaves the
alternate screen, never enables `?1003`, and never writes diagnostics
(`music-tui: …`, panic text) to the screen. `tests/headless_loop.rs` covers the
same rule without a pty.

## 7. Mouse capture

crossterm's `EnableMouseCapture` switches on `?1000` (press/release),
`?1002` (drag), `?1003` (any motion), `?1015` and `?1006`. `?1003` steals every
mouse gesture from the terminal, including the one users need to select and copy
text. We therefore send the sequences ourselves — `?1000`, `?1002`, `?1006` only
— which gives clicks, drags and wheel without permanently disabling native
selection (`--no-mouse` disables capture entirely). Consequence:
`MouseEventKind::Moved` is never delivered, and the UI does not use it.

## 8. OS / backend notes

* **Linux**: no build-time system dependencies at all (that was the point of §3).
  Playback needs one of the players in §3 at runtime; ALSA/Pulse/PipeWire is the
  player's problem, not ours.
* **macOS**: `afplay` is always present, so `auto` works out of the box.
  `SIGSTOP`/`SIGCONT` behave as on Linux.
* **Windows**: compiles (`Stdio::null`, process reaping are portable); pause is
  reported as unsupported because `SIGSTOP` does not exist (the footer shows the
  refusal and playback continues). Any of the known players works if it is on
  `PATH`. Mouse and rendering work via crossterm. See
  `docs/release-and-compatibility.md` for the full matrix.
