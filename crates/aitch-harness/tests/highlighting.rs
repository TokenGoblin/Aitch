//! Syntax highlighting, bracket matching and the view toggles, driven the way
//! a user would drive them.
//!
//! Parsing happens on another thread, so these tests wait for it. The editor
//! never waits: it draws what it has and redraws when colour lands.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aitch_core::{Document, Language, Token};
use aitch_harness::Harness;

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("aitch-hl-{}-{unique}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn file(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Open a real file, since the language comes from its name.
fn open(name: &str, contents: &str) -> (Scratch, Harness) {
    let scratch = Scratch::new();
    let path = scratch.file(name, contents);
    let mut harness = Harness::nano().with_document(Document::open(&path).unwrap());
    harness.editor_mut().set_wake(|| {});
    (scratch, harness)
}

/// Wait for the parser to catch up with the buffer.
fn settle(harness: &mut Harness) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        harness.editor_mut().poll_highlights();
        let ready = harness
            .editor()
            .highlights()
            .is_some_and(|h| !h.spans.is_empty());
        if ready {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

/// The token at a byte offset, once the parser has answered.
fn token_at(harness: &Harness, byte: usize) -> Option<Token> {
    harness.editor().highlights()?.token_at(byte)
}

/// Put the cursor on a character index, counting from the start.
fn move_to(harness: &mut Harness, index: usize) {
    harness.feed("Ctrl+Home").unwrap();
    for _ in 0..index {
        harness.feed("Right").unwrap();
    }
}

#[test]
fn a_rust_file_gets_a_rust_parser() {
    let (_scratch, mut h) = open("main.rs", "fn main() {}\n");
    assert_eq!(h.editor().language(), Some(Language::Rust));
    assert!(settle(&mut h));

    assert_eq!(token_at(&h, 0), Some(Token::Keyword), "fn");
}

#[test]
fn a_file_with_no_grammar_shows_uncoloured_rather_than_wrong() {
    let (_scratch, h) = open("notes.txt", "just some prose\n");
    assert_eq!(h.editor().language(), None);
    assert!(h.editor().highlights().is_none());
}

#[test]
fn typing_updates_the_colours() {
    let (_scratch, mut h) = open("main.rs", "fn main() {}\n");
    assert!(settle(&mut h));

    // Type a comment at the start; it must come back as a comment.
    h.feed("Ctrl+Home").unwrap();
    h.type_text("// note");
    h.feed("Enter").unwrap();

    let start = Instant::now();
    let mut coloured = false;
    while start.elapsed() < Duration::from_secs(5) {
        h.editor_mut().poll_highlights();
        if token_at(&h, 2) == Some(Token::Comment) {
            coloured = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(coloured, "the new comment never got coloured");
}

#[test]
fn opening_another_language_starts_the_right_parser() {
    let scratch = Scratch::new();
    let rust = scratch.file("main.rs", "fn main() {}\n");
    // Written so quick open can find it; opened by name below, not by path.
    scratch.file("go.py", "def go():\n    return 1\n");

    let mut workspace = aitch_core::Workspace::with_root(scratch.0.clone());
    workspace.open(&rust).unwrap();
    let mut h = Harness::nano().with_workspace(workspace);
    h.editor_mut().set_wake(|| {});
    assert_eq!(h.editor().language(), Some(Language::Rust));

    h.feed("^T").unwrap();
    h.type_text("go.py");
    h.feed("Enter").unwrap();

    assert_eq!(
        h.editor().language(),
        Some(Language::Python),
        "the parser followed the buffer"
    );
    assert!(settle(&mut h));
    assert_eq!(token_at(&h, 0), Some(Token::Keyword), "def");
}

// -- bracket matching -------------------------------------------------------

#[test]
fn a_bracket_finds_its_partner() {
    let (_scratch, mut h) = open("main.rs", "fn main() { let x = (1 + 2); }\n");
    assert!(settle(&mut h));

    move_to(&mut h, 10);
    let (open, close) = h.editor().bracket_pair().expect("a matching brace");
    assert_eq!(open, 10);
    assert_eq!(close, 29, "the closing brace, not the inner paren at 26");
}

#[test]
fn nesting_is_counted_rather_than_taking_the_first_bracket_found() {
    let (_scratch, mut h) = open("main.rs", "fn f() { g(h(1)); }\n");
    assert!(settle(&mut h));

    move_to(&mut h, 10);
    let (open, close) = h.editor().bracket_pair().expect("a pair");
    assert_eq!(open, 10, "g's opening paren");
    assert_eq!(close, 15, "and its own closer, past h's");
}

#[test]
fn a_closing_bracket_looks_backwards_for_its_partner() {
    let (_scratch, mut h) = open("main.rs", "fn f() { }\n");
    assert!(settle(&mut h));

    move_to(&mut h, 9);
    let (at, partner) = h.editor().bracket_pair().expect("a pair");
    assert_eq!(at, 9);
    assert_eq!(partner, 7);
}

#[test]
fn a_bracket_inside_a_string_is_not_mistaken_for_code() {
    // This is what the syntax tree buys. Without it the brace in the string
    // is counted, and the search runs off after a partner that is not there.
    let source = "fn f() { let s = \"}\"; }\n";
    let (_scratch, mut h) = open("main.rs", source);
    assert!(settle(&mut h));

    move_to(&mut h, 7);
    let (open, close) = h.editor().bracket_pair().expect("a pair");
    assert_eq!(open, 7, "the opening brace of the body");
    assert_eq!(
        source.chars().nth(close),
        Some('}'),
        "the partner should be a closing brace"
    );
    assert_eq!(close, 22, "the real one, not the one inside the string");
}

#[test]
fn a_cursor_that_is_not_on_a_bracket_matches_nothing() {
    let (_scratch, mut h) = open("main.rs", "fn main() { }\n");
    assert!(settle(&mut h));
    move_to(&mut h, 1);
    assert_eq!(h.editor().bracket_pair(), None);
}

#[test]
fn an_unmatched_bracket_reports_nothing_rather_than_guessing() {
    let (_scratch, mut h) = open("main.rs", "fn main() { \n");
    assert!(settle(&mut h));
    move_to(&mut h, 10);
    assert_eq!(h.editor().bracket_pair(), None);
}

// -- the view toggles -------------------------------------------------------

#[test]
fn line_numbers_and_whitespace_toggle_on_and_off() {
    let (_scratch, mut h) = open("main.rs", "fn main() {}\n");
    assert!(!h.editor().view().line_numbers);
    assert!(!h.editor().view().whitespace);

    h.feed("M-N").unwrap();
    assert!(h.editor().view().line_numbers);
    h.feed("M-N").unwrap();
    assert!(!h.editor().view().line_numbers);

    h.feed("M-P").unwrap();
    assert!(h.editor().view().whitespace);
    h.feed("M-P").unwrap();
    assert!(!h.editor().view().whitespace);
}

#[test]
fn the_toggles_are_bound_in_both_profiles() {
    for mut h in [Harness::nano(), Harness::modern()] {
        let profile = h.keymap_profile();
        h.feed("M-N").unwrap();
        assert!(
            h.editor().view().line_numbers,
            "{profile} does not bind M-N"
        );
        h.feed("M-P").unwrap();
        assert!(h.editor().view().whitespace, "{profile} does not bind M-P");
    }
}
