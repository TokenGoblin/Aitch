//! The `aitch` binary: argument parsing and wiring, and nothing else.
//!
//! Phase 7 grows this into the real command line (`+LINE`, stdin piping,
//! `$EDITOR` compatibility).

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::ExitCode;

use aitch_core::Buffer;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
Aitch — a GUI text editor with nano's interaction model

Usage:
    aitch [OPTIONS] [FILE]

Options:
    -h, --help       Show this message
    -V, --version    Show the version

Phase 1 opens a file and lets you move around it. Editing comes with Phase 2,
folders with Phase 4; see PLAN.md.
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

    let buffer = match &path {
        Some(path) => match load(path) {
            Ok(buffer) => buffer,
            Err(e) => {
                eprintln!("aitch: {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        },
        None => Buffer::new(),
    };

    if let Err(e) = aitch_ui::run(buffer, path) {
        eprintln!("aitch: {e}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

/// Read a file into a buffer, streaming rather than slurping so a large file
/// does not need twice its size in memory on the way in.
///
/// Encoding detection and line-ending preservation are Phase 2; this reads
/// UTF-8 and fails on anything else.
fn load(path: &PathBuf) -> std::io::Result<Buffer> {
    let file = File::open(path)?;
    Buffer::from_reader(BufReader::new(file))
}
