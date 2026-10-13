//! Behaviour of the `music-tui` binary: argument validation, exit codes,
//! `--dump-sort`, log file placement and "does the UI pollute stdout".

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

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
    let (tx, rx) = std::sync::mpsc::channel();
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    music_tui::scan::scan(
        folder,
        &music_tui::scan::ScanConfig::default(),
        &tx,
        &cancel,
    );
    let mut tracks = Vec::new();
    while let Ok(music_tui::scan::ScanMessage::Track(track)) = rx.recv() {
        tracks.push(track);
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
    let typescript = std::env::temp_dir().join(format!(
        "music-tui-pty-{}-{}.log",
        std::process::id(),
        file_name_of(&dir)
    ));
    let _remove = CopyCleanup(typescript.clone());
    let folder = dir.path().display().to_string();
    let log = std::env::temp_dir().join(format!("music-tui-pty-{}.log", std::process::id()));
    let inner = format!(
        "stty rows 24 cols 80; {} --backend silent --log-file {} {}",
        env!("CARGO_BIN_EXE_music-tui"),
        log.display(),
        folder
    );
    let mut child = Command::new("script")
        .args(["-q", "-e", "-c", &inner, typescript.to_str().expect("utf8")])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let child = match child.as_mut() {
        Ok(child) => child,
        Err(_) => return, // `script` is not installed here.
    };
    // Let it draw, then quit with 'q'.
    std::thread::sleep(std::time::Duration::from_millis(1_500));
    if let Some(mut stdin) = child.stdin.take() {
        let _write = stdin.write_all(b"q");
        let _flush = stdin.flush();
    }
    let status = child.wait().expect("script must finish");
    assert!(status.success(), "script exit: {status}");

    let capture = std::fs::read_to_string(&typescript).unwrap_or_default();
    assert!(
        capture.contains("\u{1b}[?1049h"),
        "the alternate screen must be entered"
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

fn file_name_of(dir: &tempfile::TempDir) -> String {
    dir.path()
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "x".to_string())
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
