//! Behaviour of the `music-tui` binary: argument validation, exit codes,
//! `--dump-sort`, log file placement and "does the UI pollute stdout".

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_music-tui"))
}

fn run<I, S>(args: I, stdin_empty: bool) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut command = bin();
    command.args(args);
    if stdin_empty {
        command.stdin(Stdio::null());
    }
    command.output().expect("binary must run")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn fixture_library() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    music_tui::testing::write_synthetic_library(dir.path()).expect("write fixtures");
    dir
}

/// Width of the fixed-width columns before the path in `--dump-sort` output.
const PATH_COLUMN: usize = 69;

/// The path column of a `--dump-sort` table.
fn dumped_paths(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter(|line| line.len() > PATH_COLUMN)
        .map(|line| {
            line.chars()
                .skip(PATH_COLUMN)
                .collect::<String>()
                .trim()
                .to_string()
        })
        .collect()
}

/// Scan and sort a folder the way the binary does.
fn scanned(folder: &Path) -> Vec<music_tui::model::Track> {
    // `cli::prepare` canonicalises the folder argument, so the binary always prints
    // resolved paths. On macOS `tempfile` hands out `/var/folders/…`, which is a
    // symlink to `/private/var/folders/…` — comparing without resolving the same
    // way makes every row differ by that prefix. On Linux the two are equal.
    let folder = &std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
    let (tx, rx) = std::sync::mpsc::channel();
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    music_tui::scan::scan(
        folder,
        &music_tui::scan::ScanConfig::default(),
        &tx,
        &cancel,
    );
    let mut tracks = Vec::new();
    while let Ok(message) = rx.recv() {
        match message {
            music_tui::scan::ScanMessage::Track(track) => tracks.push(track),
            // `Progress` may arrive anywhere in the stream; `Done` ends it.
            music_tui::scan::ScanMessage::Done(_) => break,
            music_tui::scan::ScanMessage::Progress(_) => {}
        }
    }
    music_tui::model::sort_tracks(&mut tracks);
    tracks
}

#[test]
fn help_and_version_work() {
    let out = run(["--help"], true);
    assert!(out.status.success(), "--help must succeed");
    let help = text(&out.stdout);
    assert!(help.contains("<FOLDER>"), "{help}");
    assert!(help.contains("--dump-sort"), "{help}");

    let out = run(["--version"], true);
    assert!(out.status.success());
    assert!(text(&out.stdout).contains("music-tui"));
}

#[test]
fn folder_argument_is_required() {
    let out = run(Vec::<String>::new(), true);
    assert_eq!(out.status.code(), Some(2), "clap usage error");
    assert!(text(&out.stderr).contains("FOLDER"));
}

#[test]
fn missing_folder_exits_with_two() {
    let out = run(["/definitely/not/here"], true);
    assert_eq!(out.status.code(), Some(2));
    let stderr = text(&out.stderr);
    assert!(stderr.contains("does not exist"), "{stderr}");
}

#[test]
fn a_file_instead_of_a_folder_exits_with_two() {
    let file = tempfile::NamedTempFile::new().expect("tempfile");
    let out = run([file.path().as_os_str()], true);
    assert_eq!(out.status.code(), Some(2));
    let stderr = text(&out.stderr);
    assert!(stderr.contains("not a directory"), "{stderr}");
}

#[test]
fn unreadable_folder_is_reported_not_crashed() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o000))
            .expect("chmod");
    }
    if std::fs::read_dir(dir.path()).is_ok() {
        // Running as root: permissions do not apply, nothing to assert.
        return;
    }
    let out = run([dir.path().as_os_str()], true);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("readable"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn refusing_to_run_without_a_terminal_exits_with_three() {
    let dir = fixture_library();
    let out = run([dir.path().as_os_str()], true);
    assert_eq!(out.status.code(), Some(3), "not a terminal");
    let stderr = text(&out.stderr);
    assert!(stderr.contains("terminal"), "{stderr}");
    assert!(
        stderr.contains("log:"),
        "the error must point at the log file"
    );
    // Nothing may be written to stdout when the UI cannot start.
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
}

#[test]
fn dump_sort_prints_the_spec_order() {
    let dir = fixture_library();
    let out = run(["--dump-sort", dir.path().to_str().expect("utf8")], true);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    // The binary's table must be exactly what the library produces.
    assert_eq!(
        stdout,
        music_tui::model::dump_sorted_order(&scanned(dir.path())),
        "binary output must match the library"
    );
    let paths = dumped_paths(&stdout);
    assert_eq!(paths.len(), 10, "{stdout}");
    let names: Vec<String> = paths
        .iter()
        .map(|p| {
            Path::new(p)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(
        names,
        vec![
            "broken.mp3",
            "untagged.wav",
            "one.wav",
            "two.wav",
            "ten.mp3",
            "disc1-a.flac",
            "disc2-a.flac",
            "日本語 — «live» take.wav",
            "track3.mp3",
            "track1.mp3",
        ],
        "{stdout}"
    );
    // Header/footer noise would break scripting: every line is a row.
    assert!(stdout.lines().all(|line| !line.trim().is_empty()));
    // No ANSI escapes in the machine-readable dump.
    assert!(!stdout.contains('\u{1b}'));
}

#[test]
fn dump_sort_honours_extension_filters_and_caps() {
    let dir = fixture_library();
    let folder = dir.path().to_str().expect("utf8");
    let out = run(["--dump-sort", "--extensions", "wav", folder], true);
    assert!(out.status.success());
    let paths = dumped_paths(&text(&out.stdout));
    assert!(!paths.is_empty());
    assert!(
        paths.iter().all(|p| p.ends_with(".wav")),
        "only wav rows: {paths:?}"
    );

    let out = run(["--dump-sort", "--max-files", "3", folder], true);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let paths = dumped_paths(&text(&out.stdout));
    assert!(paths.len() <= 3, "cap must be honoured: {paths:?}");
}

#[test]
fn dump_sort_on_an_empty_folder_is_silent_on_stdout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = run(["--dump-sort", dir.path().to_str().expect("utf8")], true);
    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
    let stderr = text(&out.stderr);
    assert!(stderr.contains("no audio files"), "{stderr}");
    assert!(stderr.contains("mp3"), "the message lists the extensions");
}

#[test]
fn log_file_is_created_and_stdout_stays_clean() {
    let dir = fixture_library();
    let log = tempfile::tempdir()
        .expect("logdir")
        .path()
        .join("nested.log");
    let out = run(
        [
            "--dump-sort",
            "--log-file",
            log.to_str().expect("utf8"),
            dir.path().to_str().expect("utf8"),
        ],
        true,
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let contents = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        contents.contains("music-tui"),
        "the log file must carry the banner: {contents:?}"
    );
    // All diagnostics went to the file, nothing to stdout/stderr beyond the dump.
    assert!(out.stderr.is_empty(), "{}", text(&out.stderr));
}

/// The strongest "no stdout pollution" check: run the real UI inside a pty and
/// inspect the bytes it wrote to the terminal.
#[test]
fn running_the_ui_in_a_pty_never_pollutes_the_screen() {
    let dir = fixture_library();
    // An index file of its own: a test must never write into the developer's real
    // `$XDG_CACHE_HOME/music-tui/`.
    let store = tempfile::tempdir().expect("cache dir");
    let index = store.path().join("index.bin");
    let index_arg = index.to_str().expect("utf8 cache path");
    let capture = pty_screen_retry(
        dir.path(),
        &["--cache-file", index_arg],
        "clean",
        |screen| {
            let visible = strip_ansi(screen);
            visible.contains("End of artist") || visible.contains("help")
        },
    );
    if capture.is_empty() {
        return; // `script` is not installed here.
    }
    assert!(
        capture.contains("\u{1b}[?1049h"),
        "the alternate screen must be entered; raw typescript: {capture:?}"
    );
    assert!(
        capture.contains("\u{1b}[?1049l"),
        "the alternate screen must be left again (clean restore)"
    );
    assert!(
        capture.contains("\u{1b}[?1000h") && capture.contains("\u{1b}[?1000l"),
        "mouse tracking must be enabled and disabled again"
    );
    assert!(
        !capture.contains("\u{1b}[?1003h"),
        "any-motion tracking would steal native text selection"
    );
    let visible = strip_ansi(&capture);
    assert!(
        !visible.contains("music-tui:"),
        "diagnostics must never reach the screen: {visible}"
    );
    assert!(!visible.contains("panicked"), "no panic output: {visible}");
    assert!(
        visible.contains("End of artist") || visible.contains("help"),
        "the footer must be rendered: {visible}"
    );
}

struct CopyCleanup(PathBuf);

impl Drop for CopyCleanup {
    fn drop(&mut self) {
        let _remove = std::fs::remove_file(&self.0);
    }
}

fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // CSI sequence: ESC [ params final-byte, or ESC [ ? params final-byte.
        if chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() || c == '~' {
                    break;
                }
            }
        } else {
            chars.next();
        }
    }
    out
}

/// The footer's library count in a captured screen (the last `N songs` wins,
/// because the footer is redrawn while the scan fills in). A pty writes cells, so
/// the space between the number and the word may not survive: skip whitespace.
fn song_count(visible: &str) -> Option<usize> {
    let at = visible.rfind("songs")?;
    let digits: String = visible[..at]
        .trim_end()
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    digits.parse().ok()
}

/// Run the real UI inside a pty for `hold_ms`, quit it with `q`, and return the
/// typescript — the raw bytes it wrote to the terminal, escape sequences included
/// (call [`strip_ansi`] when reading it as text). Returns `String::new()` when
/// `script` is not installed, and the test that uses it then skips itself.
///
/// There is nothing to poll here: `script` buffers the typescript and writes it
/// only when the child exits, and the app's log file is buffered too — so a caller
/// that needs the scan to have finished can only wait. [`pty_screen_retry`] shows
/// how the tests that depend on this keep that cheap.
fn pty_screen(folder: &Path, extra: &[&str], hold_ms: u64, tag: &str) -> String {
    let stem = folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "x".to_string());
    let typescript = std::env::temp_dir().join(format!(
        "music-tui-pty-{}-{}-{}.log",
        std::process::id(),
        stem,
        tag
    ));
    let log =
        std::env::temp_dir().join(format!("music-tui-pty-{}-{}.log", std::process::id(), tag));
    let mut inner = format!(
        "stty rows 24 cols 80; {} --backend silent --log-file {}",
        env!("CARGO_BIN_EXE_music-tui"),
        log.display()
    );
    for arg in extra {
        inner.push(' ');
        inner.push_str(arg);
    }
    inner.push(' ');
    inner.push_str(&folder.display().to_string());
    let mut child = script_command(&inner, &typescript)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let child = match child.as_mut() {
        Ok(child) => child,
        Err(_) => return String::new(),
    };
    std::thread::sleep(std::time::Duration::from_millis(hold_ms));
    if let Some(mut stdin) = child.stdin.take() {
        let _write = stdin.write_all(b"q");
        let _flush = stdin.flush();
    }
    let _status = child.wait().expect("script must finish");
    let capture = std::fs::read_to_string(&typescript).unwrap_or_default();
    let _remove = std::fs::remove_file(&typescript);
    capture
}

/// `script` is not one program: util-linux takes `-q -e -c CMD FILE`, while the BSD
/// version on macOS has neither `-e` nor `-c` and takes `FILE CMD …`. Probe once and
/// use the right spelling, or the pty tests fail on anything that is not Linux.
fn script_command(inner: &str, typescript: &Path) -> Command {
    static GNU: OnceLock<bool> = OnceLock::new();
    let gnu = *GNU.get_or_init(|| {
        Command::new("script")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    });
    let mut command = Command::new("script");
    if gnu {
        command.args(["-q", "-e", "-c", inner, &typescript.to_string_lossy()]);
    } else {
        command.args(["-q", &typescript.to_string_lossy(), "sh", "-c", inner]);
    }
    command
}

/// [`pty_screen`] again with a longer hold while `want` is not satisfied.
///
/// A fixed hold is the only option (see above), and a fixed hold is exactly what
/// fails on a machine that is busy elsewhere — a real source of failed tests in
/// this suite. So try short, and only pay longer waits when the capture came back
/// useless: on an idle machine this costs the first hold and nothing more.
fn pty_screen_retry(
    folder: &Path,
    extra: &[&str],
    tag: &str,
    want: impl Fn(&str) -> bool,
) -> String {
    let mut capture = String::new();
    for hold in [1_200_u64, 6_000, 20_000] {
        capture = pty_screen(folder, extra, hold, tag);
        if capture.is_empty() || want(&capture) {
            break;
        }
    }
    capture
}

/// A library with no deliberately-broken file, so the footer shows the library
/// count instead of a warning.
fn clean_library() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    music_tui::testing::FixtureSet::new(dir.path())
        .song("alpha/one.wav", "One", "Alpha Beta", "Album 1", 1, 1, 1998)
        .song("alpha/two.wav", "Two", "Alpha Beta", "Album 1", 1, 2, 1998)
        .song("beta/one.wav", "One", "Beta Wave", "Double", 1, 1, 1994)
        .song("beta/two.wav", "Two", "Beta Wave", "Double", 1, 2, 1994)
        .write_all()
        .expect("fixtures");
    dir
}

#[test]
fn cold_start_background_syncs_and_warm_start_all_show_one_library() {
    let dir = clean_library();
    let cache =
        std::env::temp_dir().join(format!("music-tui-pty-index-{}.bin", std::process::id()));
    let _cleanup = CopyCleanup(cache.clone());
    let cache_arg = cache.to_str().expect("utf8 cache path");
    let folder = dir.path();

    // First start: no index yet, it is written by this run.
    let cold_raw = pty_screen_retry(folder, &["--cache-file", cache_arg], "cold", |screen| {
        song_count(&strip_ansi(screen)).is_some()
    });
    if cold_raw.is_empty() {
        return; // `script` is not installed here.
    }
    let cold = strip_ansi(&cold_raw);
    assert!(
        song_count(&cold).is_some(),
        "the footer never appeared in the pty: {cold}"
    );
    assert!(cache.exists(), "the first run must write the index");
    assert!(
        !cold.contains("music-tui:") && !cold.contains("panicked"),
        "diagnostics must never reach the screen: {cold}"
    );
    let cold_count = song_count(&cold).expect("footer shows a library count");
    assert!(cold_count > 0, "the folder has audio: {cold}");

    // Restart with the index it just wrote, and a background sync every second:
    // three syncs of an unchanged folder must not add a single row.
    let warm_raw = pty_screen_retry(
        folder,
        &["--cache-file", cache_arg, "--sync-interval", "1"],
        "warm",
        |screen| song_count(&strip_ansi(screen)).is_some(),
    );
    let warm = strip_ansi(&warm_raw);
    let warm_count = song_count(&warm).expect("footer shows a library count");
    assert_eq!(
        cold_count, warm_count,
        "an indexed start and three background syncs must show exactly the same library"
    );
}
