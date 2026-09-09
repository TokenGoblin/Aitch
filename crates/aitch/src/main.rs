//! The `aitch` binary: argument parsing and wiring, and nothing else.

use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use aitch_core::{Buffer, Config, Document, Position, Session, Workspace};

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
    let arguments = match parse(std::env::args().skip(1)) {
        Ok(Some(arguments)) => arguments,
        // --help and --version have already printed.
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("aitch: {message}");
            eprintln!("Try `aitch --help`.");
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
    let (config, config_error) = match &config_path {
        Some(path) => Config::load_from(path),
        None => (Config::default(), None),
    };

    let piped = read_stdin();

    let workspace = match build_workspace(&arguments, piped) {
        Ok(workspace) => workspace,
        Err(message) => {
            eprintln!("aitch: {message}");
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
        eprintln!("aitch: {e}");
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
                print!("{USAGE}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("aitch {VERSION}");
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
    // Piped text wins: someone who ran `git log | aitch` wants the log.
    if let Some(text) = piped {
        let mut document = Document::blank();
        document.buffer = Buffer::from_str(&text);
        return Ok(Workspace::new(document));
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
    fn piped_text_becomes_the_buffer() {
        let arguments = Arguments::default();
        let workspace = build_workspace(&arguments, Some("piped in\n".to_string())).unwrap();
        assert_eq!(workspace.active().buffer.text().to_string(), "piped in\n");
        assert!(workspace.active().path().is_none(), "and has no filename");
    }
}
