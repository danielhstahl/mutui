# Release process and compatibility matrix

Part of `mutu-2h8.21`. Everything here describes what is actually true of this
tree today, not an aspiration.

## Versioning and release

* The version lives in `Cargo.toml` (`[package] version`) and is reported by
  `music-tui --version`. It follows semantic versioning; `0.x` releases are
  pre-1.0 and may change CLI flags between minor versions.
* `rust-version = "1.85"` is the declared MSRV. The CI runs on stable.
* Build the release artefact:

  ```sh
  cargo build --release            # lto + codegen-units=1 + strip (see Cargo.toml)
  target/release/music-tui --version
  ```

* Package it (one tarball per platform, no install script needed — the binary is
  self-contained apart from an external audio player at run time):

  ```sh
  VERSION=$(cargo pkgid | cut -d'#' -f2 | cut -d: -f2)
  TARGET=$(rustc -vV | sed -n 's|host: ||p')
  tar -C target/release -czf "music-tui-${VERSION}-${TARGET}.tar.gz" music-tui
  ```

* Release checklist: `cargo fmt --check`, `cargo clippy --all-targets
  --all-features -- -D warnings`, `cargo test`, `cargo bench --bench
  sort_and_window` (compare with `docs/benchmarks.md`), manual smoke run in a
  real terminal (`cargo run -- --example gen_fixtures` first), `CHANGELOG.md`
  updated, tag `v$VERSION`.

## Compatibility matrix

Legend: **yes** = works and is exercised by the test-suite or a manual smoke
test; **mostly** = works with the stated caveat; **no** = not supported.

### Terminal emulators

| Terminal / environment | Alt screen | Wheel + drag (`1000/1002/1006`) | Unicode glyphs | Notes |
| --- | --- | --- | --- | --- |
| GNOME Terminal / VTE | yes | yes | yes | reference environment |
| Konsole | yes | yes | yes | |
| xterm | yes | yes | mostly | xterm needs `--wheel-rows 1` if the wheel reports single steps |
| alacritty | yes | yes | yes | |
| kitty | yes | yes | yes | |
| foot (Wayland) | yes | yes | yes | |
| WezTerm | yes | yes | yes | |
| Windows Terminal | yes | yes | yes | |
| macOS Terminal.app | yes | mostly | yes | drag reporting is limited; the wheel and click work |
| iTerm2 | yes | yes | yes | |
| tmux | mostly | mostly | yes | needs `set -g mouse on`, otherwise the terminal eats the wheel |
| GNU screen | mostly | mostly | yes | same caveat |
| SSH session | yes | yes | yes | run inside a terminal that supports the above |
| `screen`/`script` pty capture (CI) | yes | n/a | mostly | the pty has no size unless you `stty rows/cols`; see `tests/cli_binary.rs` |
| `less`/pipe/redirect (`stdout` not a TTY) | no | no | n/a | refuses to start with exit code 3 |

If a terminal cannot do mouse reporting, start with `--no-mouse`: the keyboard
bindings cover everything the mouse can do except dragging the scrollbar.

### Colours

The palette lives in `src/ui/palette.rs` and is tuned for a **black** terminal:
the accent is a desaturated steel blue `#87AFD7` — 9.1:1 on black — because the
ANSI `Blue` it replaced is `#0000EE` (2.2:1, unreadable) and `LightBlue` is
4.4:1 and glaring on a solid thumb. Every text colour is measured with
`palette::contrast` (WCAG 2.1) and asserted in that module's tests; colour-only
changes belong in that file and nowhere else.

| Terminal colour support | Result |
| --- | --- |
| 24-bit (`COLORTERM=truecolor`) | the palette exactly as designed |
| 256 colours (`TERM=xterm-256color` &c.) | identical for text: every RGB text colour is an exact 6×6×6 cube entry (`#87AFD7` is index 110), so nothing is dithered; the two tinted backgrounds land on their nearest cube grey |
| 16 colours (Linux console, `TERM=vt100`) | mostly — most text colours are plain ANSI names and follow the theme, but `ratatui` does not downscale RGB, so the surfaces and the accent are whatever the terminal decides |

### Character sets

| Locale | Glyphs used |
| --- | --- |
| `*.UTF-8` (any `LC_CTYPE`/`LANG` containing `utf-8`) | `▶ • │ █ ─` box drawing and markers |
| anything else, or `--ascii` | `> * | #` and plain ASCII |

### Operating systems / audio

| Platform | `--backend auto` behaviour | Pause/resume | Volume |
| --- | --- | --- | --- |
| Linux (X11 or Wayland) | `mpv` → `ffplay` → `mpg123` → `mpg321` → `madplay` → `mplayer` | `SIGSTOP`/`SIGCONT` | player switch |
| FreeBSD | same lookup, via `which` | `SIGSTOP`/`SIGCONT` | player switch |
| macOS | `afplay` (or a Homebrew `mpv`) | `SIGSTOP`/`SIGCONT` | `afplay -v` |
| Windows (native) | works if a listed player is on `PATH` | **no** — `SIGSTOP` is POSIX; the footer shows "not supported" and playback continues | player switch |
| Windows under WSL | as Linux | yes | yes |
| Container / CI / SSH without audio | falls back to `silent` with a footer note | simulated | simulated |

`--backend silent` never touches a sound device: it emits `Started`, a ticking
position and `Finished` after the duration read from the file, so the whole
auto-advance behaviour is testable and demonstrable without audio hardware.

### Rust toolchain

| rustc | Result |
| --- | --- |
| 1.85 (declared MSRV) and newer stable | builds and passes the suite |
| nightly | builds; nothing nightly-specific is used |
| < 1.85 | may fail (`std` features used in `app`/`ui`) |
