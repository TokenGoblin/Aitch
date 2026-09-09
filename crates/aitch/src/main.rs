//! The `aitch` binary: argument parsing and wiring, and nothing else.
//!
//! Phase 7 grows this into the real command line (`+LINE`, stdin piping,
//! `$EDITOR` compatibility).

use std::path::PathBuf;
use std::process::ExitCode;

use aitch_core::{Document, Workspace};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
Aitch — a GUI text editor with nano's interaction model

Usage:
    aitch [OPTIONS] [FILE|FOLDER]

Options:
    -h, --help       Show this message
    -V, --version    Show the version

A FILE that does not exist yet opens as an empty buffer under that name.
A FOLDER opens a workspace: M-T shows the tree, ^T finds a file by name.
";

fn main() -> ExitCode {
    let mut path: Option<PathBuf> = None;

    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("aitch {VERSION}");
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') && other.len() > 1 => {
                eprintln!("aitch: unrecognized option `{other}`");
                eprintln!("Try `aitch --help`.");
                return ExitCode::FAILURE;
            }
            other => {
                if path.is_some() {
                    // Multiple buffers arrive in Phase 4, with the workspace.
                    eprintln!("aitch: only one file at a time for now");
                    return ExitCode::FAILURE;
                }
                path = Some(PathBuf::from(other));
            }
        }
    }

    let workspace = match &path {
        // A folder opens a workspace rather than a buffer.
        Some(path) if path.is_dir() => match path.canonicalize() {
            Ok(root) => Workspace::with_root(root),
            Err(e) => {
                eprintln!("aitch: {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        },
        // A name that is not on disk yet is a new file, not an error — that is
        // how every editor is used to start one.
        Some(path) if !path.exists() => Workspace::new(Document::new_at(path)),
        Some(path) => match Document::open(path) {
            Ok(document) => {
                let mut workspace = Workspace::new(document);
                // Opening a file inside a project still wants the project, so
                // quick open and the tree have somewhere to look.
                if let Some(parent) = path
                    .canonicalize()
                    .ok()
                    .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
                {
                    workspace.set_root(parent);
                }
                workspace
            }
            Err(e) => {
                eprintln!("aitch: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => Workspace::new(Document::blank()),
    };

    if let Err(e) = aitch_ui::run(workspace) {
        eprintln!("aitch: {e}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
