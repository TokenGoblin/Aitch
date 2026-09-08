//! The `nib` binary: argument parsing and wiring, and nothing else.
//!
//! Phase 7 grows this into the real command line (`+LINE`, stdin piping,
//! `$EDITOR` compatibility). Phase 0 needs enough to open the window.

use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
nib — a GUI text editor with nano's interaction model

Usage:
    nib [OPTIONS]

Options:
    -h, --help       Show this message
    -V, --version    Show the version

Phase 0 opens an empty window. Opening files comes with Phase 2, folders with
Phase 4; see PLAN.md.
";

fn main() -> ExitCode {
    // Phase 0 takes no positional arguments, so the first one settles it.
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("-h" | "--help") => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("-V" | "--version") => {
            println!("nib {VERSION}");
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("nib: unrecognized argument `{other}`");
            eprintln!("Try `nib --help`.");
            return ExitCode::FAILURE;
        }
    }

    if let Err(e) = nib_ui::run() {
        eprintln!("nib: {e}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
