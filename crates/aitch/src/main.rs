//! The `aitch` binary: argument parsing and wiring, and nothing else.

// A GUI program, so that launching it from the Start menu or Explorer opens a
// window and not a window plus an empty console. Windows decides that from the
// subsystem in the executable header and nothing else: as a console program it
// gets a console whether or not it wants one, which is what shipped in 0.1.0
// and 0.1.1.
//
// The cost is that a GUI program starts with no console at all, even when it
// was run from a terminal -- so `attach_to_parent_console` below puts that
// back, because `aitch --help` has to print.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use aitch_core::{Buffer, Config, ConfigError, Document, Position, Session, Workspace};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
Aitch — a GUI text editor with nano's interaction model

Usage:
    aitch [OPTIONS] [+LINE[:COLUMN]] [FILE|FOLDER]

Options:
    -h, --help          Show this message
    -V, --version       Show the version
    --config PATH       Read settings from PATH instead of the usual place
    --no-config         Ignore aitchrc.toml entirely
    --no-session        Do not reopen what was open last time

A FILE that does not exist yet opens as an empty buffer under that name.
A FOLDER opens a workspace: M-T shows the tree, ^T finds a file by name.
+LINE opens at that line, so `aitch +42 notes.txt` starts at line 42.

Text piped in is opened as an unnamed buffer, so `git log | aitch` works.

Aitch runs until its window closes, which is what $EDITOR requires:
    export EDITOR=aitch
";

/// What the command line asked for.
#[derive(Debug, Default)]
struct Arguments {
    path: Option<PathBuf>,
    /// From `+LINE` or `+LINE:COLUMN`, one-based as the user writes it.
    at: Option<(usize, usize)>,
    config: Option<PathBuf>,
    no_config: bool,
    no_session: bool,
}

fn main() -> ExitCode {
    attach_to_parent_console();
    record_panics();

    let arguments = match parse(std::env::args().skip(1)) {
        Ok(Some(arguments)) => arguments,
        // --help and --version have already printed.
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => {
            complain(&format!("aitch: {message}"));
            complain("Try `aitch --help`.");
            return ExitCode::FAILURE;
        }
    };

    // A broken config never stops the editor opening; the message is shown on
    // the status line once there is one to show it on.
    let config_path = if arguments.no_config {
        None
    } else {
        arguments.config.clone().or_else(Config::default_path)
    };
    let (config, mut config_error) = match &config_path {
        Some(path) => Config::load_from(path),
        None => (Config::default(), None),
    };

    // Not having a config is the ordinary state of a fresh install, so
    // `load_from` says nothing about a missing file. A path the user typed is
    // different: silence there means a mistyped `--config` looks like it
    // worked, and none of the settings they were testing appear.
    if let Some(path) = &arguments.config {
        if config_error.is_none() && !path.exists() {
            config_error = Some(ConfigError {
                path: path.clone(),
                message: "no such file".to_string(),
            });
        }
    }

    // Only when there is nothing else to open. Reading a pipe that stays
    // open would otherwise hold the editor closed, and `aitch notes.txt` in a
    // script with stdin attached would silently open the pipe instead.
    let piped = if arguments.path.is_some() {
        None
    } else {
        read_stdin()
    };

    let workspace = match build_workspace(&arguments, piped) {
        Ok(workspace) => workspace,
        Err(message) => {
            complain(&format!("aitch: {message}"));
            return ExitCode::FAILURE;
        }
    };

    // Only restore a session when nothing else was asked for. Opening a file
    // and getting yesterday's five as well would be a surprise.
    let session = if arguments.no_session || arguments.path.is_some() {
        None
    } else {
        Session::load()
    };

    let startup = aitch_ui::Startup {
        workspace,
        config,
        config_error,
        config_path,
        session,
        cursor: arguments.at,
    };

    if let Err(e) = aitch_ui::run(startup) {
        complain(&format!("aitch: {e}"));
        aitch_core::log_failure(&e.to_string());
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

fn parse(args: impl Iterator<Item = String>) -> Result<Option<Arguments>, String> {
    let mut parsed = Arguments::default();
    let mut args = args.peekable();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "-h" | "--help" => {
                tell(USAGE);
                return Ok(None);
            }
            "-V" | "--version" => {
                tell(&format!(
                    "aitch {VERSION}
"
                ));
                return Ok(None);
            }
            "--no-config" => parsed.no_config = true,
            "--no-session" => parsed.no_session = true,
            "--config" => {
                let path = args
                    .next()
                    .ok_or_else(|| "--config needs a path".to_string())?;
                parsed.config = Some(PathBuf::from(path));
            }

            // `+42` or `+42:8`, as every editor since vi has taken it.
            plus if plus.starts_with('+') && plus.len() > 1 => {
                parsed.at = Some(parse_position(&plus[1..])?);
            }

            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("unrecognized option `{other}`"));
            }

            other => {
                if parsed.path.is_some() {
                    return Err("only one file at a time for now".to_string());
                }
                parsed.path = Some(PathBuf::from(other));
            }
        }
    }

    Ok(Some(parsed))
}

/// `42` or `42:8`, counted from one.
fn parse_position(text: &str) -> Result<(usize, usize), String> {
    let (line, column) = match text.split_once(':') {
        Some((line, column)) => (line, column),
        None => (text, "1"),
    };
    let line: usize = line
        .parse()
        .map_err(|_| format!("`+{text}` is not a line number"))?;
    let column: usize = column
        .parse()
        .map_err(|_| format!("`+{text}` is not a line:column"))?;
    Ok((line, column))
}

/// Whatever was piped in, if anything.
///
/// Only when stdin is not a terminal: otherwise starting `aitch` with no
/// arguments would sit there reading from the keyboard instead of opening.
fn read_stdin() -> Option<String> {
    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut text = String::new();
    match std::io::stdin().read_to_string(&mut text) {
        Ok(0) => None,
        Ok(_) => Some(text),
        // Binary on stdin is not text; opening it as a buffer would be worse
        // than ignoring it.
        Err(_) => None,
    }
}

fn build_workspace(arguments: &Arguments, piped: Option<String>) -> Result<Workspace, String> {
    // A named file wins over a pipe: `echo hi | aitch notes.txt` asked for
    // notes.txt, and quietly opening the pipe instead loses the argument and
    // any `+LINE` with it.
    if arguments.path.is_none() {
        if let Some(text) = piped {
            let mut document = Document::blank();
            document.buffer = Buffer::from_str(&text);
            return Ok(Workspace::new(document));
        }
    }

    let Some(path) = &arguments.path else {
        return Ok(Workspace::new(Document::blank()));
    };

    if path.is_dir() {
        let root = path
            .canonicalize()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        return Ok(Workspace::with_root(root));
    }

    // A name that is not on disk yet is a new file, not an error — that is
    // how every editor is used to start one.
    if !path.exists() {
        return Ok(Workspace::new(Document::new_at(path)));
    }

    let document = Document::open(path).map_err(|e| e.to_string())?;
    let mut workspace = Workspace::new(document);
    // Opening a file inside a project still wants the project, so quick open
    // and the tree have somewhere to look.
    if let Some(parent) = path
        .canonicalize()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
    {
        workspace.set_root(parent);
    }
    Ok(workspace)
}

/// Where the cursor should start, given `+LINE:COLUMN`.
pub fn starting_position(at: (usize, usize)) -> Position {
    Position::new(at.0.saturating_sub(1), at.1.saturating_sub(1))
}

/// Write panics down as well as printing them.
///
/// A window opens before the GPU surface is built, so a failure there looks
/// like the editor starting and vanishing. The message goes to stderr, but
/// `aitch` is a console-subsystem binary: launched from the Start menu,
/// Windows gives it a console that closes with the process, so the
/// explanation exists for a frame. Started from a terminal it is visible, and
/// that is the difference between a bug report that says "it crashes" and one
/// that names the cause.
///
/// Panics matter more here than returned errors do. wgpu reports a surface it
/// cannot configure by panicking — `Surface::configure` returns `()` — so the
/// one failure that has actually shipped never reached the error path at all.
///
/// The default hook still runs, so nothing that worked before stops working.
fn record_panics() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // `info` already carries the file and line, so it is not repeated.
        aitch_core::log_failure(&info.to_string());
        previous(info);
    }));

    // Deliberately says nothing here. Pointing at the log because the file
    // exists means one transient failure months ago prefixes every `aitch`,
    // `aitch --help` and `aitch --version` from then on. The log is for
    // whoever goes looking after something went wrong, and the thing that
    // went wrong is what names it.
}

/// Print to stdout, and say nothing if there is nowhere to print to.
///
/// `println!` panics when the write fails, and for a GUI-subsystem program
/// that is not a remote possibility: a shell does not wait for one, so it can
/// close the pipe it was reading before the program gets round to writing.
/// `aitch --version` then died with a panic and exit code 101 instead of
/// printing a version — which is what it did to the release workflow.
///
/// Nothing here is worth taking the process down for. If the output went
/// nowhere, it went nowhere.
fn tell(text: &str) {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

/// The same, for stderr.
fn complain(text: &str) {
    use std::io::Write;
    let mut err = std::io::stderr();
    let _ = err.write_all(text.as_bytes());
    let _ = err.write_all(
        b"
",
    );
    let _ = err.flush();
}

/// Borrow the console of whatever started us, if it had one.
///
/// A GUI-subsystem program is given no console, which is the point -- being
/// given one unasked is why a shortcut used to open two windows. But this is
/// also a command-line program: `--help` and `--version` print, a bad argument
/// explains itself, and `git log | aitch` reads a pipe. Run from a terminal,
/// all of that has to land in that terminal.
///
/// `cmd` and PowerShell both hand a GUI child their own standard handles, so
/// in practice attaching is enough and the loop below changes nothing. It is
/// there for the launchers that do not: attaching makes the console reachable,
/// but a process holding no handle to it still writes into a void, and the
/// symptom of that would be `aitch --help` printing nothing at all.
///
/// A handle that is already valid is never touched, so a pipe or a redirect --
/// `git log | aitch`, `aitch --help > notes.txt` -- keeps pointing where the
/// shell pointed it.
///
/// Called before anything writes, so Rust settles on the right handles.
///
/// One consequence is worth knowing: `cmd` does not wait for a GUI program, so
/// `aitch --version` returns the prompt and prints a moment later. That is the
/// price of not opening a console nobody asked for.
#[cfg(windows)]
fn attach_to_parent_console() {
    use std::ffi::c_void;

    // Hand-rolled rather than a dependency for four calls.
    const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
    const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6;
    const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5;
    const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const OPEN_EXISTING: u32 = 3;

    extern "system" {
        fn AttachConsole(process: u32) -> i32;
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn SetStdHandle(which: u32, handle: *mut c_void) -> i32;
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *mut c_void,
            disposition: u32,
            flags: u32,
            template: *mut c_void,
        ) -> *mut c_void;
    }

    let invalid = usize::MAX as *mut c_void;

    // Failure means the parent had no console: an Explorer double-click, and
    // nothing to do about it. The startup log is the fallback there.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return;
    }

    // "CONOUT$" and "CONIN$" name the attached console's own ends.
    let wide = |name: &str| name.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let conout = wide("CONOUT$");
    let conin = wide("CONIN$");

    for (which, name) in [
        (STD_OUTPUT_HANDLE, &conout),
        (STD_ERROR_HANDLE, &conout),
        (STD_INPUT_HANDLE, &conin),
    ] {
        // Already pointing somewhere real -- a pipe or a redirect -- so leave
        // it exactly as the shell set it up.
        let existing = unsafe { GetStdHandle(which) };
        if !existing.is_null() && existing != invalid {
            continue;
        }

        let opened = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if !opened.is_null() && opened != invalid {
            unsafe {
                SetStdHandle(which, opened);
            }
        }
    }
}

#[cfg(not(windows))]
fn attach_to_parent_console() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Option<Arguments>, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn a_bare_filename_is_the_file_to_open() {
        let parsed = parse_args(&["notes.txt"]).unwrap().unwrap();
        assert_eq!(parsed.path, Some(PathBuf::from("notes.txt")));
        assert_eq!(parsed.at, None);
    }

    #[test]
    fn plus_line_opens_at_that_line() {
        let parsed = parse_args(&["+42", "notes.txt"]).unwrap().unwrap();
        assert_eq!(parsed.at, Some((42, 1)));
        assert_eq!(parsed.path, Some(PathBuf::from("notes.txt")));
    }

    #[test]
    fn plus_line_and_column_both_count_from_one() {
        let parsed = parse_args(&["+10:5", "notes.txt"]).unwrap().unwrap();
        assert_eq!(parsed.at, Some((10, 5)));
        assert_eq!(starting_position((10, 5)), Position::new(9, 4));
        assert_eq!(starting_position((1, 1)), Position::new(0, 0));
    }

    #[test]
    fn the_position_may_come_after_the_filename() {
        let parsed = parse_args(&["notes.txt", "+7"]).unwrap().unwrap();
        assert_eq!(parsed.at, Some((7, 1)));
        assert_eq!(parsed.path, Some(PathBuf::from("notes.txt")));
    }

    #[test]
    fn a_plus_that_is_not_a_number_is_refused() {
        assert!(parse_args(&["+banana", "notes.txt"]).is_err());
        assert!(parse_args(&["+1:x"]).is_err());
    }

    #[test]
    fn a_file_named_like_an_option_is_still_refused_clearly() {
        let error = parse_args(&["--wat"]).unwrap_err();
        assert!(error.contains("unrecognized option"), "{error}");
    }

    #[test]
    fn config_flags_are_understood() {
        let parsed = parse_args(&["--config", "my.toml", "--no-session"])
            .unwrap()
            .unwrap();
        assert_eq!(parsed.config, Some(PathBuf::from("my.toml")));
        assert!(parsed.no_session);
        assert!(!parsed.no_config);

        assert!(parse_args(&["--config"]).is_err(), "--config needs a path");
    }

    #[test]
    fn help_and_version_print_and_stop() {
        assert!(parse_args(&["--help"]).unwrap().is_none());
        assert!(parse_args(&["-V"]).unwrap().is_none());
    }

    #[test]
    fn two_files_are_refused_for_now() {
        assert!(parse_args(&["one.txt", "two.txt"]).is_err());
    }

    #[test]
    fn a_lone_plus_is_treated_as_a_filename() {
        // `+` on its own is a strange file name, but it is a legal one, and
        // guessing that it meant a line number would be worse.
        let parsed = parse_args(&["+"]).unwrap().unwrap();
        assert_eq!(parsed.path, Some(PathBuf::from("+")));
    }

    #[test]
    fn a_named_file_wins_over_a_pipe() {
        // `echo hi | aitch notes.txt` asked for notes.txt.
        let arguments = Arguments {
            path: Some(PathBuf::from("notes.txt")),
            ..Arguments::default()
        };
        let workspace = build_workspace(&arguments, Some("piped in\n".to_string())).unwrap();
        assert_eq!(
            workspace.active().path(),
            Some(std::path::Path::new("notes.txt")),
            "the argument, not the pipe"
        );
        assert_eq!(workspace.active().buffer.text().to_string(), "");
    }

    #[test]
    fn piped_text_becomes_the_buffer() {
        let arguments = Arguments::default();
        let workspace = build_workspace(&arguments, Some("piped in\n".to_string())).unwrap();
        assert_eq!(workspace.active().buffer.text().to_string(), "piped in\n");
        assert!(workspace.active().path().is_none(), "and has no filename");
    }
}
