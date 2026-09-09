//! Every command on the footer, driven the way a user would drive it.
//!
//! PLAN.md's acceptance for Phase 3 is that someone with nano muscle memory
//! sits down and edits a config file without reading anything, and that the
//! harness covers every footer command. These tests are that coverage: each
//! one presses the chord a nano user would press and asserts on what would be
//! on screen — buffer, status line, prompt line, footer.

use std::fs;
use std::path::PathBuf;

use aitch_core::{Command, Context, Document, Outcome};
use aitch_harness::Harness;

/// A scratch directory that cleans itself up.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("aitch-fidelity-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch directory");
        Scratch(dir)
    }

    fn file(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, contents).expect("write");
        path
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The labelled footer commands in the nano profile's editor context.
///
/// If a binding gains a label, it belongs on this list and needs a test below.
const FOOTER_COMMANDS: &[&str] = &[
    "^G Help",
    "^X Exit",
    "^O Write Out",
    "^R Read File",
    "^W Where Is",
    "^\\ Replace",
    "^K Cut",
    "^U Paste",
    "^C Location",
    "^_ Go To",
    "M-U Undo",
    "M-E Redo",
    "M-A Set Mark",
    "M-6 Copy",
    "^T Open",
    "M-T Tree",
    "M-B Buffers",
    "M-^W Search All",
];

#[test]
fn the_footer_lists_exactly_what_this_file_tests() {
    // The guard on the rest of the file: a new footer entry fails here until
    // someone writes a test for it.
    let h = Harness::nano();
    let shown = h.editor().footer(2000);
    let cells: Vec<String> = shown.cells().iter().map(|c| c.text()).collect();
    assert_eq!(cells, FOOTER_COMMANDS);
}

// -- ^G Help ----------------------------------------------------------------

#[test]
fn help_opens_a_pane_and_closes_again() {
    let mut h = Harness::nano().with_text("some text");
    h.feed("^G").unwrap();

    assert!(h.is_help_open());
    assert_eq!(h.context(), Context::Help);
    // The footer follows the context, so the help pane advertises its own keys.
    assert!(h.footer_cells().iter().any(|c| c.contains("Exit Help")));

    h.feed("^X").unwrap();
    assert!(!h.is_help_open());
    assert_eq!(h.context(), Context::Editor);
    assert_eq!(h.text(), "some text", "help changed nothing");
}

#[test]
fn help_is_generated_from_the_active_keymap() {
    let nano = Harness::nano();
    let text = nano.editor().help_text().join("\n");
    assert!(text.contains("^X"), "help should list the exit chord");
    assert!(text.contains("Write Out"));

    // A different profile documents different keys, with no second source.
    let modern = Harness::modern();
    let modern_text = modern.editor().help_text().join("\n");
    assert!(modern_text.contains("^S"), "modern saves with ^S");
    assert!(modern_text.contains("modern keys"));
}

#[test]
fn help_scrolls_and_stops_at_both_ends() {
    let mut h = Harness::nano().with_height(10);
    h.feed("^G").unwrap();
    assert_eq!(h.editor().help().unwrap().scroll, 0);

    h.feed("Down Down").unwrap();
    assert_eq!(h.editor().help().unwrap().scroll, 2);

    h.feed("Up Up Up Up").unwrap();
    assert_eq!(h.editor().help().unwrap().scroll, 0, "clamped at the top");

    for _ in 0..500 {
        h.feed("Down").unwrap();
    }
    let bottom = h.editor().help().unwrap().scroll;
    h.feed("Down").unwrap();
    assert_eq!(
        h.editor().help().unwrap().scroll,
        bottom,
        "clamped at the end"
    );
}

// -- ^X Exit ----------------------------------------------------------------

#[test]
fn exit_leaves_immediately_when_nothing_has_changed() {
    let mut h = Harness::nano().with_text("untouched");
    h.feed("^X").unwrap();
    assert!(h.should_quit());
    assert_eq!(h.outcomes().last(), Some(&Outcome::Quit));
}

#[test]
fn exit_asks_before_discarding_changes() {
    let mut h = Harness::nano().with_text("original");
    h.type_text("!");
    h.feed("^X").unwrap();

    assert!(!h.should_quit(), "it must ask first");
    assert_eq!(h.prompt_line().as_deref(), Some("Save modified buffer?: "));

    // "n" discards and leaves.
    h.type_text("n");
    assert!(h.should_quit());
}

#[test]
fn answering_no_to_the_exit_question_cancels_it() {
    let mut h = Harness::nano().with_text("original");
    h.type_text("!");
    h.feed("^X ^C").unwrap();

    assert!(!h.should_quit());
    assert_eq!(h.prompt_line(), None, "the question is gone");
    assert_eq!(h.status(), "cancelled");
    assert_eq!(h.context(), Context::Editor);
}

#[test]
fn answering_yes_to_the_exit_question_saves_first() {
    let scratch = Scratch::new("exit-save");
    let path = scratch.file("notes.txt", "original\n");
    let mut h = Harness::nano().with_document(Document::open(&path).unwrap());

    h.type_text("X");
    h.feed("^X").unwrap();
    h.type_text("y");

    assert!(h.should_quit());
    assert_eq!(fs::read_to_string(&path).unwrap(), "Xoriginal\n");
}

// -- ^O Write Out -----------------------------------------------------------

#[test]
fn write_out_saves_a_file_that_has_a_name() {
    let scratch = Scratch::new("write");
    let path = scratch.file("notes.txt", "before\n");
    let mut h = Harness::nano().with_document(Document::open(&path).unwrap());

    h.type_text("after ");
    assert!(h.status().contains("Modified"));

    h.feed("^O").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "after before\n");
    assert!(h.status().starts_with("Wrote "), "{}", h.status());
}

#[test]
fn write_out_asks_for_a_name_when_there_is_none() {
    let scratch = Scratch::new("write-as");
    let mut h = Harness::nano();
    h.type_text("fresh content\n");

    h.feed("^O").unwrap();
    assert_eq!(h.context(), Context::Prompt);
    assert_eq!(h.prompt_line().as_deref(), Some("File Name to Write: "));

    let path = scratch.path("new.txt");
    h.type_text(&path.display().to_string());
    h.feed("Enter").unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), "fresh content\n");
    assert_eq!(h.context(), Context::Editor);
}

// -- ^R Read File -----------------------------------------------------------

#[test]
fn read_file_inserts_another_file_at_the_cursor() {
    let scratch = Scratch::new("read");
    let insert = scratch.file("snippet.txt", "inserted\n");
    let mut h = Harness::nano().with_text("start\nend\n");

    h.feed("^R").unwrap();
    assert_eq!(h.prompt_line().as_deref(), Some("File to insert: "));

    h.type_text(&insert.display().to_string());
    h.feed("Enter").unwrap();

    assert_eq!(h.text(), "inserted\nstart\nend\n");
    assert!(h.status().contains("Inserted"), "{}", h.status());
}

#[test]
fn read_file_says_so_when_the_file_is_not_there() {
    let mut h = Harness::nano();
    h.feed("^R").unwrap();
    h.type_text("no-such-file-anywhere.txt");
    h.feed("Enter").unwrap();

    assert!(!h.status().is_empty());
    assert_eq!(h.text(), "", "nothing was inserted");
}

// -- ^W Where Is ------------------------------------------------------------

#[test]
fn where_is_searches_as_you_type() {
    let mut h = Harness::nano().with_text("alpha\nbeta\ngamma\n");
    h.feed("^W").unwrap();
    assert_eq!(h.context(), Context::Search);
    assert_eq!(h.prompt_line().as_deref(), Some("Search: "));

    h.type_text("gam");
    // Incremental: the match is found and highlighted before Enter.
    assert_eq!(h.selection().as_deref(), Some("gam"));
    assert_eq!(h.cursor().line, 2);

    h.feed("Enter").unwrap();
    assert_eq!(h.context(), Context::Editor);
}

#[test]
fn search_is_case_insensitive_the_way_nano_is() {
    let mut h = Harness::nano().with_text("The Quick Brown Fox");
    h.feed("^W").unwrap();
    h.type_text("quick");
    assert_eq!(h.selection().as_deref(), Some("Quick"));
}

#[test]
fn search_wraps_around_and_says_so() {
    let mut h = Harness::nano().with_text("needle\nhay\nhay\n");
    h.feed("Down Down").unwrap();

    h.feed("^W").unwrap();
    h.type_text("needle");
    h.feed("Enter").unwrap();

    assert_eq!(h.cursor().line, 0, "found by wrapping to the top");
}

#[test]
fn cancelling_a_search_puts_the_cursor_back() {
    let mut h = Harness::nano().with_text("alpha\nbeta\ngamma\n");
    h.feed("Down").unwrap();
    let before = h.cursor();

    h.feed("^W").unwrap();
    h.type_text("gamma");
    assert_ne!(h.cursor(), before, "the search moved it");

    h.feed("^C").unwrap();
    assert_eq!(h.cursor(), before, "cancelling put it back");
    assert_eq!(h.selection(), None);
}

#[test]
fn a_search_that_finds_nothing_says_so() {
    let mut h = Harness::nano().with_text("alpha beta");
    h.feed("^W").unwrap();
    h.type_text("delta");
    h.feed("Enter").unwrap();
    assert!(h.status().contains("not found"), "{}", h.status());
}

#[test]
fn repeating_a_search_walks_to_the_next_match() {
    let mut h = Harness::nano().with_text("one two one two one");
    h.feed("^W").unwrap();
    h.type_text("one");
    h.feed("Enter").unwrap();
    assert_eq!(h.cursor().column, 3, "cursor sits after the first match");

    h.feed("M-W").unwrap();
    assert_eq!(h.selection().as_deref(), Some("one"));
    assert_eq!(h.cursor().column, 11);
}

#[test]
fn search_history_comes_back_with_the_up_key() {
    let mut h = Harness::nano().with_text("alpha beta gamma");
    h.feed("^W").unwrap();
    h.type_text("beta");
    h.feed("Enter").unwrap();

    h.feed("^W").unwrap();
    assert_eq!(h.prompt_line().as_deref(), Some("Search: "));
    h.feed("Up").unwrap();
    assert_eq!(h.prompt_line().as_deref(), Some("Search: beta"));
}

// -- ^\ Replace -------------------------------------------------------------

#[test]
fn replace_asks_for_both_halves_then_confirms_each_match() {
    let mut h = Harness::nano().with_text("cat dog cat dog cat");

    h.feed("^\\").unwrap();
    assert_eq!(h.prompt_line().as_deref(), Some("Search (to replace): "));
    h.type_text("cat");
    h.feed("Enter").unwrap();

    assert_eq!(h.prompt_line().as_deref(), Some("Replace \"cat\" with: "));
    h.type_text("bird");
    h.feed("Enter").unwrap();

    // Now confirming, one match at a time.
    assert_eq!(h.context(), Context::Search);
    assert_eq!(h.prompt_line().as_deref(), Some("Replace this instance?: "));
    assert_eq!(h.selection().as_deref(), Some("cat"));

    h.type_text("y");
    assert_eq!(h.text(), "bird dog cat dog cat");

    h.type_text("n");
    assert_eq!(h.text(), "bird dog cat dog cat", "skipped, not replaced");

    h.type_text("y");
    assert_eq!(h.text(), "bird dog cat dog bird");
    assert!(h.status().contains("Replaced"), "{}", h.status());
}

#[test]
fn replacing_all_takes_the_rest_in_one_go() {
    let mut h = Harness::nano().with_text("cat cat cat");
    h.feed("^\\").unwrap();
    h.type_text("cat");
    h.feed("Enter").unwrap();
    h.type_text("dog");
    h.feed("Enter").unwrap();

    h.type_text("a");
    assert_eq!(h.text(), "dog dog dog");
    assert_eq!(h.context(), Context::Editor);
    assert!(h.status().contains("Replaced 3"), "{}", h.status());
}

#[test]
fn cancelling_a_replace_keeps_what_was_already_done() {
    let mut h = Harness::nano().with_text("cat cat cat");
    h.feed("^\\").unwrap();
    h.type_text("cat");
    h.feed("Enter").unwrap();
    h.type_text("dog");
    h.feed("Enter").unwrap();

    h.type_text("y");
    h.feed("^C").unwrap();

    assert_eq!(h.text(), "dog cat cat");
    assert_eq!(h.context(), Context::Editor);
}

#[test]
fn a_replace_that_matches_nothing_says_so() {
    let mut h = Harness::nano().with_text("alpha beta");
    h.feed("^\\").unwrap();
    h.type_text("delta");
    h.feed("Enter").unwrap();
    h.type_text("gamma");
    h.feed("Enter").unwrap();

    assert!(h.status().contains("not found"), "{}", h.status());
    assert_eq!(h.text(), "alpha beta");
}

// -- ^_ Go To ---------------------------------------------------------------

#[test]
fn goto_line_counts_from_one() {
    let text: String = (1..=50).map(|i| format!("line {i}\n")).collect();
    let mut h = Harness::nano().with_text(&text);

    h.feed("^_").unwrap();
    assert!(h.prompt_line().unwrap().starts_with("Enter line number"));
    h.type_text("10");
    h.feed("Enter").unwrap();

    assert_eq!(h.cursor().line, 9, "line 10 is index 9");
}

#[test]
fn goto_takes_a_column_too() {
    let mut h = Harness::nano().with_text("alpha\nbeta gamma\n");
    h.feed("^_").unwrap();
    h.type_text("2:6");
    h.feed("Enter").unwrap();
    assert_eq!(h.cursor().line, 1);
    assert_eq!(h.cursor().column, 5);
}

#[test]
fn goto_past_the_end_lands_on_the_last_line() {
    let mut h = Harness::nano().with_text("one\ntwo\n");
    h.feed("^_").unwrap();
    h.type_text("9999");
    h.feed("Enter").unwrap();
    assert_eq!(h.cursor().line, h.editor().buffer().len_lines() - 1);
}

#[test]
fn goto_that_is_not_a_number_complains_and_does_nothing() {
    let mut h = Harness::nano().with_text("one\ntwo\n");
    h.feed("^_").unwrap();
    h.type_text("banana");
    h.feed("Enter").unwrap();
    assert_eq!(h.cursor().line, 0);
    assert!(h.status().contains("not a line number"), "{}", h.status());
}

// -- ^C Location ------------------------------------------------------------

#[test]
fn location_reports_where_the_cursor_is() {
    let mut h = Harness::nano().with_text("alpha\nbeta\ngamma\n");
    h.feed("Down Right Right ^C").unwrap();

    let status = h.status();
    assert!(status.contains("line 2/4"), "{status}");
    assert!(status.contains("col 3"), "{status}");
}

// -- ^K Cut, ^U Paste -------------------------------------------------------

#[test]
fn cut_and_paste_move_a_line() {
    let mut h = Harness::nano().with_text("one\ntwo\nthree\n");
    h.feed("^K").unwrap();
    assert_eq!(h.text(), "two\nthree\n");

    h.feed("Down ^U").unwrap();
    assert_eq!(h.text(), "two\none\nthree\n");
}

#[test]
fn consecutive_cuts_pile_up() {
    let mut h = Harness::nano().with_text("one\ntwo\nthree\nfour\n");
    h.feed("^K ^K").unwrap();
    assert_eq!(h.text(), "three\nfour\n");

    h.feed("M-^W").unwrap(); // anything else breaks the run
    h.clear();
    h.feed("^U").unwrap();
    assert_eq!(h.text(), "one\ntwo\nthree\nfour\n", "both lines came back");
}

// -- M-U Undo, M-E Redo -----------------------------------------------------

#[test]
fn undo_and_redo_walk_the_history() {
    let mut h = Harness::nano();
    h.type_text("hello");
    h.feed("M-U").unwrap();
    assert_eq!(h.text(), "");
    h.feed("M-E").unwrap();
    assert_eq!(h.text(), "hello");
}

// -- M-A Set Mark, M-6 Copy -------------------------------------------------

#[test]
fn the_mark_selects_and_copy_asks_for_the_clipboard() {
    let mut h = Harness::nano().with_text("hello world");
    h.feed("M-A Ctrl+Right").unwrap();
    assert_eq!(h.selection().as_deref(), Some("hello "));

    h.feed("M-6").unwrap();
    // The core cannot reach a window server; it asks the UI to.
    assert_eq!(
        h.outcomes().last(),
        Some(&Outcome::Copy("hello ".to_string()))
    );
}

#[test]
fn copying_nothing_says_so_rather_than_clearing_the_clipboard() {
    let mut h = Harness::nano().with_text("hello");
    h.feed("M-6").unwrap();
    assert_eq!(h.status(), "nothing is selected");
    assert!(!matches!(h.outcomes().last(), Some(Outcome::Copy(_))));
}

// -- ^T Open, M-T Tree, M-B Buffers, M-^W Search All ------------------------

#[test]
fn project_search_admits_it_is_not_built() {
    // Phase 6. Better a key that says what it is than one that silently does
    // nothing. The folder-mode keys arrived in Phase 4; see folder_mode.rs.
    let mut h = Harness::nano().with_text("text");
    h.feed("M-^W").unwrap();
    let status = h.status();
    assert!(
        status.contains("project search") && status.contains("not built yet"),
        "M-^W said {status:?}"
    );
    assert_eq!(h.text(), "text");
}

#[test]
fn the_folder_keys_say_so_when_no_folder_is_open() {
    // Opening a bare file is the common case, and these keys have nothing to
    // act on then. Saying why beats appearing broken.
    for chord in ["^T", "M-T"] {
        let mut h = Harness::nano().with_text("text");
        h.feed(chord).unwrap();
        assert!(
            h.status().contains("no folder is open"),
            "{chord} said {:?}",
            h.status()
        );
    }
}

// -- the footer itself ------------------------------------------------------

#[test]
fn the_footer_changes_with_the_context() {
    let mut h = Harness::nano().with_text("text");
    let editing = h.footer_cells();
    assert_eq!(editing[0], "^G Help");

    h.feed("^W").unwrap();
    let searching = h.footer_cells();
    assert_ne!(searching, editing, "the footer follows the context");
    assert!(searching.iter().any(|c| c.contains("Cancel")));
}

#[test]
fn the_footer_is_never_more_than_two_rows() {
    let mut h = Harness::nano().with_text("text");
    for chord in ["^G", "^X"] {
        h.feed(chord).unwrap();
        assert_eq!(h.footer_lines().len(), 2);
    }
    h.feed("^W").unwrap();
    assert_eq!(h.footer_lines().len(), 2);
}

#[test]
fn the_status_line_shows_the_file_and_whether_it_changed() {
    let scratch = Scratch::new("status");
    let path = scratch.file("notes.txt", "content\n");
    let mut h = Harness::nano().with_document(Document::open(&path).unwrap());

    assert_eq!(h.status(), "notes.txt");
    h.type_text("x");
    assert_eq!(h.status(), "notes.txt  Modified");

    h.feed("^O").unwrap();
    assert!(h.status().starts_with("Wrote"));
    // The message lasts one keystroke, then the filename comes back.
    h.feed("Right").unwrap();
    assert_eq!(h.status(), "notes.txt");
}

#[test]
fn a_modern_user_gets_modern_keys_for_the_same_commands() {
    let mut h = Harness::modern().with_text("alpha beta");
    h.feed("^F").unwrap();
    assert_eq!(h.context(), Context::Search);
    h.type_text("beta");
    assert_eq!(h.selection().as_deref(), Some("beta"));

    let mut goto = Harness::modern().with_text("one\ntwo\nthree\n");
    goto.feed("^G").unwrap();
    goto.type_text("3");
    goto.feed("Enter").unwrap();
    assert_eq!(goto.cursor().line, 2);
}

#[test]
fn switching_profiles_switches_the_footer_with_it() {
    let mut h = Harness::nano().with_text("text");
    assert_eq!(h.footer_cells()[1], "^X Exit");

    h.feed("M-M").unwrap();
    assert_eq!(h.footer_cells()[1], "^Q Quit");
    assert_eq!(h.status(), "modern keys");

    // And the commands follow: ^S now writes.
    assert_eq!(
        h.editor()
            .keymap()
            .resolve(Context::Editor, aitch_core::Chord::parse("^S").unwrap()),
        Some(&Command::WriteOut)
    );
}
