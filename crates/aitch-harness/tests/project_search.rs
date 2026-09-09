//! Project-wide search and replace, driven the way a user would drive them.
//!
//! Searching runs on its own threads, so these tests wait for results. The
//! editor never waits: the pane fills as hits arrive.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aitch_core::{Context, Workspace};
use aitch_harness::Harness;

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "aitch-psearch-h-{name}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn file(&self, relative: &str, contents: &str) {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.0.join(relative)).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn project(name: &str) -> (Scratch, Harness) {
    let scratch = Scratch::new(name);
    scratch.file("src/main.rs", "fn main() {\n    let needle = 1;\n}\n");
    scratch.file("src/lib.rs", "// needle in a comment\npub fn go() {}\n");
    scratch.file("README.md", "Nothing to see.\n");
    scratch.file("target/junk.rs", "let needle = 99;\n");
    scratch.file(".gitignore", "target/\n");

    let harness = Harness::nano().with_workspace(Workspace::with_root(scratch.0.clone()));
    (scratch, harness)
}

/// Wait for the search to deliver everything it is going to.
///
/// Not just the first hit: the walker runs on several threads, so a test that
/// counted or indexed results the moment one arrived would race the rest of
/// them. Waiting for the count to stop moving is what makes that
/// deterministic.
fn settle(h: &mut Harness) {
    let start = Instant::now();
    let mut steady = 0;
    let mut last = usize::MAX;
    while start.elapsed() < Duration::from_secs(10) {
        h.editor_mut().poll_search();
        let now = h.results().len();
        if now > 0 && now == last {
            steady += 1;
            // Three quiet polls in a row: everything that is coming is here.
            if steady == 3 {
                return;
            }
        } else {
            steady = 0;
        }
        last = now;
        std::thread::sleep(Duration::from_millis(30));
    }
}

#[test]
fn a_project_search_opens_a_prompt_and_lists_what_it_finds() {
    let (_scratch, mut h) = project("basic");

    h.feed("M-^W").unwrap();
    // The search context, where the arrows walk results and ^\ replaces.
    assert_eq!(h.context(), Context::Search);
    assert_eq!(h.prompt_line().as_deref(), Some("Search in folder: "));

    h.type_text("needle");
    settle(&mut h);

    let results = h.results().to_vec();
    assert_eq!(results.len(), 2, "{results:?}");
    assert!(
        results.iter().any(|r| r.starts_with("src/main.rs:2:")),
        "{results:?}"
    );
    assert!(
        results.iter().any(|r| r.starts_with("src/lib.rs:1:")),
        "{results:?}"
    );
}

#[test]
fn ignored_folders_stay_out_of_the_results() {
    let (_scratch, mut h) = project("ignored");
    h.feed("M-^W").unwrap();
    h.type_text("needle");
    settle(&mut h);

    assert!(
        !h.results().iter().any(|r| r.contains("target/")),
        "{:?}",
        h.results()
    );
}

#[test]
fn enter_jumps_to_the_matching_line() {
    let (_scratch, mut h) = project("jump");
    h.feed("M-^W").unwrap();
    h.type_text("needle");
    settle(&mut h);

    // Pick the main.rs hit, wherever it landed in the list.
    let index = h
        .results()
        .iter()
        .position(|r| r.starts_with("src/main.rs:"))
        .expect("a main.rs hit");
    for _ in 0..index {
        h.feed("Down").unwrap();
    }

    h.feed("Enter").unwrap();
    assert_eq!(h.editor().document().display_name(), "main.rs");
    assert_eq!(h.cursor().line, 1, "line 2, zero-based");
    assert_eq!(h.context(), Context::Editor);
}

#[test]
fn a_short_pattern_is_not_searched_for() {
    // One or two characters match most of a tree and say nothing.
    let (_scratch, mut h) = project("short");
    h.feed("M-^W").unwrap();
    h.type_text("ne");
    std::thread::sleep(Duration::from_millis(200));
    h.editor_mut().poll_search();
    assert!(h.results().is_empty(), "{:?}", h.results());
}

#[test]
fn cancelling_a_search_clears_it() {
    let (_scratch, mut h) = project("cancel");
    h.feed("M-^W").unwrap();
    h.type_text("needle");
    settle(&mut h);
    assert!(!h.results().is_empty());

    h.feed("^C").unwrap();
    assert_eq!(h.context(), Context::Editor);
    assert!(h.results().is_empty(), "the results went with the prompt");
}

#[test]
fn the_status_line_reports_how_the_search_is_going() {
    let (_scratch, mut h) = project("status");
    h.feed("M-^W").unwrap();
    h.type_text("needle");
    settle(&mut h);

    assert!(h.status().contains("matches"), "{}", h.status());
}

#[test]
fn a_search_with_no_folder_open_says_so() {
    let mut h = Harness::nano().with_text("some text");
    h.feed("M-^W").unwrap();
    assert!(h.status().contains("no folder is open"), "{}", h.status());
}

// -- project-wide replace ---------------------------------------------------

#[test]
fn replace_asks_before_changing_anything() {
    let (scratch, mut h) = project("replace-ask");
    h.feed("M-^W").unwrap();
    h.type_text("needle");
    settle(&mut h);

    h.feed("^\\").unwrap();
    assert_eq!(
        h.prompt_line().as_deref(),
        Some("Replace \"needle\" everywhere with: ")
    );

    h.type_text("pin");
    h.feed("Enter").unwrap();

    // The plan, not the change.
    let line = h.prompt_line().unwrap_or_default();
    assert!(
        line.starts_with("Replace 2 occurrences in 2 files?"),
        "{line:?}"
    );
    assert!(
        scratch.read("src/main.rs").contains("needle"),
        "nothing should be written while the question is open"
    );
}

#[test]
fn answering_yes_replaces_across_every_file() {
    let (scratch, mut h) = project("replace-yes");
    h.feed("M-^W").unwrap();
    h.type_text("needle");
    settle(&mut h);

    h.feed("^\\").unwrap();
    h.type_text("pin");
    h.feed("Enter").unwrap();
    h.type_text("y");

    assert!(scratch.read("src/main.rs").contains("let pin = 1;"));
    assert!(scratch.read("src/lib.rs").contains("// pin in a comment"));
    assert!(h.status().contains("Replaced 2"), "{}", h.status());

    // And nothing outside the search, including ignored folders.
    assert!(scratch.read("target/junk.rs").contains("needle"));
}

#[test]
fn answering_no_changes_nothing() {
    let (scratch, mut h) = project("replace-no");
    h.feed("M-^W").unwrap();
    h.type_text("needle");
    settle(&mut h);

    h.feed("^\\").unwrap();
    h.type_text("pin");
    h.feed("Enter").unwrap();
    h.type_text("n");

    assert!(scratch.read("src/main.rs").contains("needle"));
    assert!(scratch.read("src/lib.rs").contains("needle"));
    assert!(h.status().contains("nothing was changed"), "{}", h.status());
}

#[test]
fn replace_before_searching_says_to_search_first() {
    let (_scratch, mut h) = project("replace-empty");
    h.feed("M-^W").unwrap();
    h.feed("^\\").unwrap();
    assert!(
        h.status().contains("search for something first"),
        "{}",
        h.status()
    );
}

#[test]
fn a_project_replace_keeps_line_endings_and_encoding() {
    // Phase 2's guarantee, applied across a whole tree at once. This is the
    // single keystroke most able to undo it.
    let scratch = Scratch::new("replace-encoding");
    scratch.file("unix.txt", "needle here\nsecond\n");
    fs::write(scratch.0.join("dos.txt"), b"needle here\r\nsecond\r\n").unwrap();

    let mut h = Harness::nano().with_workspace(Workspace::with_root(scratch.0.clone()));
    h.feed("M-^W").unwrap();
    h.type_text("needle");
    settle(&mut h);

    h.feed("^\\").unwrap();
    h.type_text("pin");
    h.feed("Enter").unwrap();
    h.type_text("y");

    assert_eq!(
        fs::read(scratch.0.join("dos.txt")).unwrap(),
        b"pin here\r\nsecond\r\n",
        "CRLF endings were rewritten"
    );
    assert_eq!(
        fs::read(scratch.0.join("unix.txt")).unwrap(),
        b"pin here\nsecond\n"
    );
}
