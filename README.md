# music-tui

A terminal music player for a folder of audio files.

It does two things particularly well, and everything else supports them:

1. **It sorts the folder into a sensible listening order** — artist, then each
   album in release-year order, then disc, then track. An artist's records come
   out the way they came out (a 1990 album before a 1995 one), and ties inside a
   year use natural number ordering (`Album 2` before `Album 10`).
2. **Clicking a song plays the rest of that artist, then stops.** After a track
   ends, the next song *of the same artist* starts automatically; when the last
   song of that artist is over the app stops and says so in the footer. It never
   wraps around to the top and never crosses into the next artist.

```
$ music-tui ~/Music
  d1 t1  Alpha Beta         Album 1            First Song        3:12
▶ d1 t2  Alpha Beta         Album 1            Second Song       2:58   <- playing
  d1 t1  Alpha Beta         Album 10           Tenth Song        4:04
  d1 t1  Beta Wave          Double             Disc One Track 1  1:59
Idle │ No audio files found in /home/you/Music │ [?] help
```

Full semantics (normative, with worked examples) live in
[`docs/adr/0002-sort-and-artist-run.md`](docs/adr/0002-sort-and-artist-run.md).
The stack and threading decisions are in
[`docs/adr/0001-stack-and-architecture.md`](docs/adr/0001-stack-and-architecture.md).

## Requirements

* Rust 1.85 or newer (`rustc --version`).
* An interactive terminal (stdin **and** stdout must be a TTY).
* Sound comes from an external command-line player. `music-tui` uses the first
  of these that is on `PATH`: `mpv`, `ffplay`, `mpg123`, `mpg321`, `madplay`,
  `afplay` (macOS), `mplayer`. Install one if you want audio.
  Without one, the app still runs and behaves exactly the same, but silently
  (see [Backends](#backends)).

No `libasound`/ALSA headers, no GStreamer, no `rodio`/`cpal`: that is deliberate,
see ADR 0001 §3.

## Build and run

```sh
cargo build --release
./target/release/music-tui ~/Music
```

Development builds work the same way (`cargo run -- ~/Music`).

Try it without your real library:

```sh
cargo run --example gen_fixtures -- /tmp/music-demo   # writes ~10 tagged files
cargo run -- --dump-sort /tmp/music-demo             # print the order, no UI
cargo run -- /tmp/music-demo                          # the UI
```

## Usage

### Mouse

| Action | Result |
| --- | --- |
| Click a row | play that song, then the rest of its artist |
| Wheel anywhere in the list | scroll three rows (configurable, `--wheel-rows`) |
| Drag the bar on the right | scroll |
| Click the bar (not the thumb) | jump to that position |

Native terminal text selection keeps working: the app asks for click, drag and
wheel reporting (`1000`/`1002`/`1006`) but never any-motion reporting (`1003`),
and `Shift`-drag still selects in every mainstream terminal. Use `--no-mouse`
to keep the mouse entirely to the terminal.

### Keyboard

| Key | Action |
| --- | --- |
| `q`, `Esc`, `Ctrl-C` | quit |
| `Enter` | play the selected row |
| `Space` | pause / resume |
| `Up`/`Down`, `j`/`k` | move the selection |
| `PageUp`/`PageDown` | scroll by one page |
| `Home` / `End` | jump to the first / last song |
| `n` / `p` | next / previous song **inside the current artist run** (refuses at the boundary) |
| `m` | mute / unmute |
| `+` / `-` | volume up / down |
| `r` | rescan the folder |
| `?` or `F1` | help overlay |

### Colours

The palette is chosen for a black terminal and lives in `src/ui/palette.rs` —
named by meaning (`ACCENT`, `MUTED`, `STATE_PLAYING`, …) instead of scattered
`Color::` literals. The accent (scrollbar thumb, footer clock) is a soft steel
blue `#87AFD7`, 9.1:1 on black; ANSI `Blue` was 2.2:1 and unreadable there.
Every text colour is checked against WCAG AA by the tests in that module, and
colour-only changes should touch that file alone.

### Command line

```
Usage: music-tui [OPTIONS] <FOLDER>

  --extensions <EXT>      replace the audio extensions (comma separated)
  --scan-threads <N>      tag-reading threads (0 = auto)
  --log-file <PATH>       where to write diagnostics
  --debug                 verbose logging into the log file
  --max-files <N>         refuse to index more than N files (default 50000)
  --max-entries <N>       refuse to walk more than N directory entries (default 500000)
  --wheel-rows <N>        rows per wheel notch (default 3)
  --backend <BACKEND>     auto | command | silent | mock | none
  --ascii                 plain-ASCII glyphs
  --no-mouse              keyboard only
  --dump-sort             print the sorted library and exit
  -V, --version           print version
```

`--dump-sort` is the quickest way to see *exactly* the order the player will
follow, one artist run at a time:

```
$ music-tui --dump-sort ~/Music
   1                                    d 1 t-           /home/you/Misc/mystery.mp3
   2  alpha beta               Album 1  d 1 t1    1998   /home/you/Music/alpha/one.wav
   3  alpha beta               Album 1  d 1 t2    1998   /home/you/Music/alpha/two.wav
   ...
```

Exit codes: `0` success, `2` bad folder/usage, `3` no interactive terminal,
`4` terminal failure, `1` anything else.

## Supported formats

Extensions considered audio (override with `--extensions`):
`mp3, flac, ogg, oga, opus, m4a, m4b, aac, wav, aiff, aif, wma, ape, mpc, wv`.

Tags are read with [lofty](https://crates.io/crates/lofty): title, artist,
album, album artist, track, disc, year. Missing tags are never an error — a file
with no tags still gets a row (sorted by artist `""`, then file name) and is
played like any other. An album with no year anywhere sorts first inside its
artist. Hidden directories and dot-files are skipped.

## Backends

`--backend` chooses how sound happens:

| Value | Behaviour |
| --- | --- |
| `auto` (default) | use the first installed player (`mpv`, `ffplay`, `mpg123`, `mpg321`, `madplay`, `afplay`, `mplayer`); fall back to `silent` with a footer note |
| `command` | require an external player, fail if none is found |
| `silent` (alias `none`) | no sound: timing is simulated from the tag duration (useful over SSH, in containers, and in CI) |
| `mock` | records commands and replays scripted events; used by the test-suite |

The player runs on its own worker thread. End of track is detected by reaping
the child process; pause/resume use `SIGSTOP`/`SIGCONT`, so pausing really stops
the clock and no position events are emitted while paused.

## Diagnostics

Everything is written to a log file — never to the screen, which is the app's
canvas. Default path: `$XDG_STATE_HOME/music-tui/music-tui.log`, else
`$HOME/.local/state/music-tui.log`, else `$TMPDIR/music-tui.log`. Use
`--log-file /path/to/log` and `--debug` for more detail.

If a file cannot be read, the row is marked `!` in the list and the footer says
`n of m files could not be read`. If a file fails *while playing*, the app shows
`Skipping <title>: <reason>` and continues with the next song; two failures in a
row stop the run rather than flashing past ten broken files.

Common problems:

* **`music-tui: stdout is not a terminal`** (exit 3) — you piped or redirected
  the output, or ran it from a non-interactive shell. Run it from a real
  terminal; use `--dump-sort` if you wanted the sorted list.
* **Nothing is audible** — no decoder is installed (see the footer, which names
  the active backend), or the player failed to open the file (check the log).
* **The mouse selects text instead of clicking** — you started with
  `--no-mouse`, or your terminal captures the drag (tmux: `set -g mouse on`).
* **Garbled glyphs** — `--ascii` for terminals without Unicode.

Compatibility details (terminals, tmux/screen, SSH, Windows) are in
[`docs/release-and-compatibility.md`](docs/release-and-compatibility.md).

## Development

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test                       # ~150 tests, no speakers and no TTY needed
cargo bench --bench sort_and_window
```

Layout:

```
src/
  app/        state + pure reducer + 50 ms event loop (no I/O in the reducer)
  audio/      worker thread, command backend, silent and mock backends
  model/      Track, sort keys, playlist cursor and artist runs
  scan/       recursive scanner (worker threads, streaming progress)
  ui/         layout, list, scrollbar math, mouse hit-testing, footer, palette, render
  terminal.rs raw mode / alternate screen / panic hook / SIGINT
docs/adr/     the two normative documents the tests check themselves against
tests/        unit + integration + snapshot + CLI/pty + performance budgets
examples/     gen_fixtures: write a small synthetic library to disk
```

The test-suite is the contract: the sort table in
`docs/adr/0002-sort-and-artist-run.md` is parsed by `tests/adr_spec.rs` and
compared against the real comparator, so documentation and code cannot drift.

## Release

See [`CHANGELOG.md`](CHANGELOG.md) and
[`docs/release-and-compatibility.md`](docs/release-and-compatibility.md).

## License

MIT
