//! Phase 7: what `aitchrc.toml` changes, what a session remembers, and what
//! happens to work that was never saved.
//!
//! These go through the editor rather than the config parser, because the
//! question is never "did the TOML parse" — the unit tests answer that — but
//! "does pressing Tab now put in two spaces".

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aitch_core::{Config, Document, OpenFile, Position, Recovery, Session, ThemeChoice, Workspace};
use aitch_harness::Harness;

/// Tests run in parallel, so each folder needs its own name.
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "aitch-settings-{name}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch directory");
        Scratch(dir)
    }

    fn file(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }

    /// A config file with these settings, read back as the editor would.
    fn config(&self, src: &str) -> Config {
        let path = self.file("aitchrc.toml", src);
        let (config, error) = Config::load_from(&path);
        assert!(error.is_none(), "{src:?} did not load: {error:?}");
        config
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// -- what the config changes ------------------------------------------------

#[test]
fn expanded_tabs_put_in_spaces_instead() {
    let scratch = Scratch::new("expand");
    let mut harness = Harness::nano().with_text("");
    harness
        .editor_mut()
        .apply_config(scratch.config("expand_tabs = true\ntab_width = 2\n"));

    harness.feed("Tab").unwrap();
    assert_eq!(harness.text(), "  ", "two spaces, as the config asked");
    assert_eq!(harness.cursor(), Position::new(0, 2));
}

#[test]
fn a_literal_tab_is_still_available() {
    let scratch = Scratch::new("literal-tab");
    let mut harness = Harness::nano().with_text("");
    harness
        .editor_mut()
        .apply_config(scratch.config("expand_tabs = false\ntab_width = 8\n"));

    harness.feed("Tab").unwrap();
    assert_eq!(harness.text(), "\t", "a tab is a tab when asked for");
}

#[test]
fn tab_expansion_is_one_undo_step() {
    // Four spaces that take four backspaces to remove would be a nasty
    // surprise; the whole insertion is one edit.
    let scratch = Scratch::new("tab-undo");
    let mut harness = Harness::nano().with_text("");
    harness
        .editor_mut()
        .apply_config(scratch.config("expand_tabs = true\ntab_width = 4\n"));

    harness.feed("Tab").unwrap();
    assert_eq!(harness.text(), "    ");
    harness.feed("M-U").unwrap();
    assert_eq!(harness.text(), "", "undone in one go");
}

#[test]
fn the_config_can_turn_on_line_numbers_and_whitespace() {
    let scratch = Scratch::new("view");
    let mut harness = Harness::nano().with_text("hi\n");
    assert!(!harness.editor().view().line_numbers, "off by default");

    harness
        .editor_mut()
        .apply_config(scratch.config("line_numbers = true\nwhitespace = true\n"));

    assert!(harness.editor().view().line_numbers);
    assert!(harness.editor().view().whitespace);
}

#[test]
fn the_config_can_choose_the_keymap() {
    let scratch = Scratch::new("keymap");
    let mut harness = Harness::nano().with_text("");
    assert_eq!(harness.keymap_profile(), "nano");

    harness
        .editor_mut()
        .apply_config(scratch.config("keymap = \"modern\"\n"));
    assert_eq!(harness.keymap_profile(), "modern");
    // And the footer follows, because it is generated from the keymap.
    let footer = harness.footer_cells().join(" ");
    assert!(footer.contains("^S"), "modern saves with ^S: {footer}");
}

#[test]
fn a_keymap_that_does_not_exist_leaves_the_keys_working() {
    // Better the keys you had than no keys at all.
    let scratch = Scratch::new("bad-keymap");
    let mut harness = Harness::nano().with_text("");
    harness
        .editor_mut()
        .apply_config(scratch.config("keymap = \"dvorak-emacs-9000\"\n"));

    assert_eq!(harness.keymap_profile(), "nano", "unchanged");
    assert!(
        harness.status().contains("keymap"),
        "and says so: {}",
        harness.status()
    );
    // And hands the complaint back, so a live reload prints it rather than
    // printing "settings reloaded" over the top of it.
    let complaint = harness
        .editor_mut()
        .apply_config(scratch.config("keymap = \"still-not-a-keymap\"\n"));
    assert!(
        complaint.is_some_and(|c| c.contains("keymap")),
        "the caller is told, not just the status line"
    );
    harness.type_text("still typing");
    assert_eq!(harness.text(), "still typing");
}

#[test]
fn a_broken_config_never_stops_the_editor_opening() {
    let scratch = Scratch::new("broken");
    let path = scratch.file("aitchrc.toml", "theme = = = dark\n");

    let (config, error) = Config::load_from(&path);
    let error = error.expect("a syntax error is reported");
    assert!(!error.message.is_empty(), "with something to read");
    assert_eq!(
        config.theme,
        ThemeChoice::Dark,
        "and everything falls back to the defaults"
    );
}

#[test]
fn an_absent_config_is_not_an_error() {
    let scratch = Scratch::new("absent");
    let (config, error) = Config::load_from(&scratch.0.join("nothing-here.toml"));
    assert!(error.is_none(), "not having settings is normal");
    assert_eq!(config.tab_width, 4);
}

/// A folder with something worth ignoring in it, opened with `rules` in the
/// config. The rules are applied before the tree or the index is built,
/// because that is when the walker reads them.
fn project(name: &str, rules: &str) -> (Scratch, Harness) {
    let scratch = Scratch::new(name);
    scratch.file("keep.txt", "needle here\n");
    scratch.file("notes.bak", "needle here too\n");
    scratch.file("vendor/thing.js", "needle in vendor\n");
    scratch.file("vendor/other.js", "needle in vendor too\n");

    let config = scratch.config(rules);
    let mut harness = Harness::nano().with_workspace(Workspace::with_root(scratch.0.clone()));
    harness.editor_mut().apply_config(config);
    (scratch, harness)
}

/// Wait for the project search to deliver everything it is going to.
///
/// Not just the first hit: the walker runs on several threads, so a test that
/// counted results the moment one arrived would race the rest of them.
fn settle(harness: &mut Harness) {
    let start = Instant::now();
    let mut steady = 0;
    let mut last = usize::MAX;
    while start.elapsed() < Duration::from_secs(10) {
        harness.editor_mut().poll_search();
        let now = harness.results().len();
        if now > 0 && now == last {
            steady += 1;
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
fn the_config_can_ignore_what_gitignore_does_not() {
    let (_scratch, mut harness) = project("ignore-tree", "ignore = [\"*.bak\", \"vendor\"]\n");
    harness.feed("M-T").unwrap();

    let rows = harness.tree_rows().join(" ");
    assert!(rows.contains("keep.txt"), "{rows}");
    assert!(!rows.contains("notes.bak"), "*.bak is ignored: {rows}");
    assert!(!rows.contains("vendor"), "vendor is ignored: {rows}");
}

#[test]
fn quick_open_honours_the_config_ignore() {
    let (_scratch, mut harness) = project("ignore-open", "ignore = [\"*.bak\"]\n");
    harness.feed("^T").unwrap();
    harness.type_text("bak");

    assert!(
        harness.results().is_empty(),
        "notes.bak is ignored: {:?}",
        harness.results()
    );
}

#[test]
fn project_search_honours_the_config_ignore() {
    let (_scratch, mut harness) = project("ignore-search", "ignore = [\"*.bak\", \"vendor\"]\n");
    harness.feed("M-^W").unwrap();
    harness.type_text("needle");
    settle(&mut harness);

    let results = harness.results().to_vec();
    assert!(!results.is_empty(), "keep.txt still matches");
    assert!(
        results.iter().all(|hit| hit.contains("keep.txt")),
        "only the file that was not ignored: {results:?}"
    );
}

#[test]
fn a_config_ignore_rule_can_be_taken_back() {
    // The same syntax as a .gitignore, negation included, because two
    // almost-the-same syntaxes is one too many to remember.
    let (_scratch, mut harness) = project(
        "ignore-negated",
        "ignore = [\"vendor/*\", \"!vendor/thing.js\"]\n",
    );
    harness.feed("^T").unwrap();
    harness.type_text("js");

    let results = harness.results().to_vec();
    assert!(
        results.iter().any(|hit| hit.contains("thing.js")),
        "the exception is put back: {results:?}"
    );
    assert!(
        !results.iter().any(|hit| hit.contains("other.js")),
        "and the rest of the folder stays out: {results:?}"
    );
}

#[test]
fn an_excluded_folder_cannot_have_one_file_pulled_back_out() {
    // Git's own rule: once a directory is excluded nothing under it is
    // looked at, so there is no file left to re-include. Matching git here
    // beats being cleverer than the thing people already know.
    let (_scratch, mut harness) = project(
        "ignore-pruned",
        "ignore = [\"vendor\", \"!vendor/thing.js\"]\n",
    );
    harness.feed("^T").unwrap();
    harness.type_text("js");

    assert!(
        harness.results().is_empty(),
        "the whole folder is gone: {:?}",
        harness.results()
    );
}

#[test]
fn an_ignore_rule_with_a_path_in_it_means_the_same_thing_everywhere() {
    // The rules are anchored at the project root. Anchoring them at whatever
    // folder is being expanded made `src/generated` resolve to
    // `src/src/generated` in the tree and match nothing.
    let scratch = Scratch::new("ignore-anchor");
    scratch.file("src/keep.rs", "fn main() {}\n");
    scratch.file("src/generated/machine.rs", "// generated\n");

    let config = scratch.config("ignore = [\"src/generated\"]\n");
    let mut harness = Harness::nano().with_workspace(Workspace::with_root(scratch.0.clone()));
    harness.editor_mut().apply_config(config);

    // Quick open walks from the root.
    harness.feed("^T").unwrap();
    harness.type_text("machine");
    assert!(
        harness.results().is_empty(),
        "the index leaves it out: {:?}",
        harness.results()
    );
    harness.feed("^C").unwrap();

    // The tree walks one folder at a time, and must agree.
    harness.feed("M-T").unwrap();
    harness.feed("Enter").unwrap();
    let rows = harness.tree_rows().join(" ");
    assert!(rows.contains("keep.rs"), "src is expanded: {rows}");
    assert!(
        !rows.contains("generated"),
        "and agrees with the index: {rows}"
    );
}

#[test]
fn a_nonsense_ignore_rule_does_not_take_the_tree_with_it() {
    let (_scratch, mut harness) = project("ignore-bad", "ignore = [\"[\"]\n");
    harness.feed("M-T").unwrap();

    let rows = harness.tree_rows().join(" ");
    assert!(rows.contains("keep.txt"), "still listing: {rows}");
}

// -- what a session remembers ----------------------------------------------

#[test]
fn a_session_records_what_is_open_and_where_the_cursor_was() {
    let scratch = Scratch::new("session-record");
    let one = scratch.file("one.txt", "alpha\nbeta\ngamma\n");
    let two = scratch.file("two.txt", "second\nfile\n");

    let mut workspace = Workspace::new(Document::open(&one).unwrap());
    workspace.set_root(scratch.0.clone());
    workspace.open(&two).unwrap();
    let mut harness = Harness::nano().with_workspace(workspace);
    harness.feed("Down Right Right").unwrap();

    let session = harness.editor().session();
    assert_eq!(session.files.len(), 2);
    assert_eq!(session.active, 1, "the one being looked at");
    assert_eq!(session.files[1].line, 1);
    assert_eq!(session.files[1].column, 2);
}

#[test]
fn the_active_buffer_survives_a_file_going_missing() {
    // `session.active` indexes the saved list; anything deleted since is
    // skipped, so the two stop agreeing and the wrong buffer comes up.
    let scratch = Scratch::new("session-active");
    let second = scratch.file("second.txt", "second\n");
    let third = scratch.file("third.txt", "third\n");

    let mut harness = Harness::nano().with_text("");
    harness.editor_mut().restore_session(&Session {
        root: None,
        files: vec![
            OpenFile {
                path: scratch.0.join("deleted.txt"),
                line: 0,
                column: 0,
            },
            OpenFile {
                path: second,
                line: 0,
                column: 0,
            },
            OpenFile {
                path: third,
                line: 0,
                column: 0,
            },
        ],
        // The third file in the saved list, which is now the second open.
        active: 2,
    });

    assert_eq!(harness.text(), "third\n", "the one that was in front");
}

#[test]
fn a_session_survives_the_round_trip_to_disk() {
    let scratch = Scratch::new("session-disk");
    let path = scratch.0.join("session.toml");
    let session = Session {
        root: Some(scratch.0.clone()),
        files: vec![OpenFile {
            path: scratch.0.join("notes.txt"),
            line: 12,
            column: 3,
        }],
        active: 0,
    };
    session.save_to(&path);

    let read = Session::load_from(&path).expect("it reads back");
    assert_eq!(read.files, session.files);
    assert_eq!(read.root, session.root);
}

#[test]
fn restoring_a_session_reopens_the_files_at_their_cursors() {
    let scratch = Scratch::new("session-restore");
    let path = scratch.file("notes.txt", "one\ntwo\nthree\n");

    let mut harness = Harness::nano().with_text("");
    harness.editor_mut().restore_session(&Session {
        root: Some(scratch.0.clone()),
        files: vec![OpenFile {
            path: path.clone(),
            line: 2,
            column: 1,
        }],
        active: 1,
    });

    assert_eq!(harness.text(), "one\ntwo\nthree\n");
    assert_eq!(harness.cursor(), Position::new(2, 1));
    assert_eq!(
        harness.editor().workspace().root(),
        Some(scratch.0.as_path())
    );
}

#[test]
fn a_file_that_has_since_been_deleted_is_quietly_skipped() {
    // A session is a convenience. Nagging about last week's scratch file
    // every time the editor starts is not one.
    let scratch = Scratch::new("session-gone");
    let mut harness = Harness::nano().with_text("");
    harness.editor_mut().restore_session(&Session {
        root: None,
        files: vec![OpenFile {
            path: scratch.0.join("gone.txt"),
            line: 0,
            column: 0,
        }],
        active: 0,
    });

    assert_eq!(harness.text(), "", "still the empty buffer");
    assert!(
        !harness.status().contains("gone.txt"),
        "and no complaint: {}",
        harness.status()
    );
}

#[test]
fn a_cursor_past_the_end_of_a_shortened_file_lands_inside_it() {
    let scratch = Scratch::new("session-shrunk");
    let path = scratch.file("shrunk.txt", "only one line\n");

    let mut harness = Harness::nano().with_text("");
    harness.editor_mut().restore_session(&Session {
        root: None,
        files: vec![OpenFile {
            path,
            line: 900,
            column: 0,
        }],
        active: 1,
    });

    assert!(harness.cursor().line < harness.editor().buffer().len_lines());
}

// -- work that was never saved ---------------------------------------------

fn recovery_for(path: &std::path::Path, text: &str) -> Recovery {
    Recovery::detached(Some(path.to_path_buf()), "2026-01-01 12:00 UTC", text)
}

#[test]
fn unsaved_work_from_a_crash_is_offered_back() {
    let scratch = Scratch::new("recover-yes");
    let path = scratch.file("draft.txt", "what was saved\n");

    let mut harness = Harness::nano().with_document(Document::open(&path).unwrap());
    let offered = harness
        .editor_mut()
        .offer_recovery_from(vec![recovery_for(&path, "what was typed\n")]);

    assert!(offered, "there is something to offer");
    let prompt = harness.prompt_line().expect("a question is asked");
    assert!(prompt.contains("Restore"), "{prompt}");

    harness.type_text("y");
    assert_eq!(
        harness.text(),
        "what was typed\n",
        "the typed version is back"
    );
    assert!(
        harness.editor().workspace().active().is_dirty(),
        "and is unsaved, because it is"
    );
}

#[test]
fn declining_recovery_leaves_the_file_as_it_was() {
    let scratch = Scratch::new("recover-no");
    let path = scratch.file("draft.txt", "what was saved\n");

    let mut harness = Harness::nano().with_document(Document::open(&path).unwrap());
    harness
        .editor_mut()
        .offer_recovery_from(vec![recovery_for(&path, "what was typed\n")]);
    harness.type_text("n");

    assert_eq!(harness.text(), "what was saved\n");
    assert!(
        !harness.editor().workspace().active().is_dirty(),
        "nothing was changed, so nothing is unsaved"
    );
}

#[test]
fn recovered_text_can_be_undone_in_one_step() {
    // Saying yes by accident should cost one keystroke to put right.
    let scratch = Scratch::new("recover-undo");
    let path = scratch.file("draft.txt", "original\n");

    let mut harness = Harness::nano().with_document(Document::open(&path).unwrap());
    harness
        .editor_mut()
        .offer_recovery_from(vec![recovery_for(&path, "recovered\n")]);
    harness.type_text("y");
    assert_eq!(harness.text(), "recovered\n");

    harness.feed("M-U").unwrap();
    assert_eq!(harness.text(), "original\n");
}

#[test]
fn recovery_that_matches_the_file_is_not_worth_asking_about() {
    let scratch = Scratch::new("recover-same");
    let path = scratch.file("draft.txt", "identical\n");

    let mut harness = Harness::nano().with_document(Document::open(&path).unwrap());
    let offered = harness
        .editor_mut()
        .offer_recovery_from(vec![recovery_for(&path, "identical\n")]);

    assert!(!offered, "the crash cost nothing, so say nothing");
    assert!(harness.prompt_line().is_none());
}

#[test]
fn recovery_for_a_different_file_is_left_alone() {
    // It stays on disk until its own buffer is opened, rather than being
    // pushed in front of someone who asked for this file.
    let scratch = Scratch::new("recover-other");
    let mine = scratch.file("mine.txt", "mine\n");
    let theirs = scratch.0.join("theirs.txt");

    let mut harness = Harness::nano().with_document(Document::open(&mine).unwrap());
    let offered = harness
        .editor_mut()
        .offer_recovery_from(vec![recovery_for(&theirs, "someone else's work\n")]);

    assert!(!offered);
    assert_eq!(harness.text(), "mine\n");
}

#[test]
fn an_unnamed_buffer_is_never_offered_a_recovery() {
    let mut harness = Harness::nano().with_text("scratch\n");
    let offered = harness
        .editor_mut()
        .offer_recovery_from(vec![recovery_for(std::path::Path::new("x.txt"), "other\n")]);
    assert!(!offered, "there is nothing to match it against");
}

#[test]
fn cancelling_the_question_keeps_the_work() {
    // Esc while thinking about it must not be read as "throw it away". The
    // recovery file may be the only copy of that text in the world.
    let scratch = Scratch::new("recover-cancel");
    let path = scratch.file("draft.txt", "what was saved\n");
    let directory = scratch.0.join("recovery");
    Recovery::write_in(&directory, Some(&path), 0, "what was typed\n");

    let mut harness = Harness::nano().with_document(Document::open(&path).unwrap());
    harness
        .editor_mut()
        .offer_recovery_from(Recovery::pending_in(&directory));
    harness.feed("^C").unwrap();

    assert_eq!(
        harness.text(),
        "what was saved\n",
        "the buffer is untouched"
    );
    assert_eq!(
        Recovery::pending_in(&directory).len(),
        1,
        "and the file is still there to be offered again"
    );
}

#[test]
fn saying_no_is_the_one_answer_that_deletes_it() {
    let scratch = Scratch::new("recover-no-deletes");
    let path = scratch.file("draft.txt", "what was saved\n");
    let directory = scratch.0.join("recovery");
    Recovery::write_in(&directory, Some(&path), 0, "what was typed\n");

    let mut harness = Harness::nano().with_document(Document::open(&path).unwrap());
    harness
        .editor_mut()
        .offer_recovery_from(Recovery::pending_in(&directory));
    harness.type_text("n");

    assert!(
        Recovery::pending_in(&directory).is_empty(),
        "turned down while looking at it"
    );
}

#[test]
fn restoring_it_also_clears_the_file_away() {
    let scratch = Scratch::new("recover-yes-clears");
    let path = scratch.file("draft.txt", "what was saved\n");
    let directory = scratch.0.join("recovery");
    Recovery::write_in(&directory, Some(&path), 0, "what was typed\n");

    let mut harness = Harness::nano().with_document(Document::open(&path).unwrap());
    harness
        .editor_mut()
        .offer_recovery_from(Recovery::pending_in(&directory));
    harness.type_text("y");

    assert_eq!(harness.text(), "what was typed\n");
    assert!(
        Recovery::pending_in(&directory).is_empty(),
        "the text is in front of them now"
    );
}

#[test]
fn work_typed_into_a_pipe_is_offered_back_too() {
    // `git log | aitch`, edited, then a crash. There is no file to key on,
    // so this is the case where the recovery file is the only copy.
    let scratch = Scratch::new("recover-unnamed");
    let directory = scratch.0.join("recovery");
    Recovery::write_in(&directory, None, 0x1234_0000, "notes on the log\n");

    let mut harness = Harness::nano().with_text("");
    let offered = harness
        .editor_mut()
        .offer_recovery_from(Recovery::pending_in(&directory));

    assert!(offered, "an empty unnamed buffer can take it");
    harness.type_text("y");
    assert_eq!(harness.text(), "notes on the log\n");
}

#[test]
fn an_unnamed_recovery_never_lands_on_top_of_something() {
    let scratch = Scratch::new("recover-unnamed-busy");
    let directory = scratch.0.join("recovery");
    Recovery::write_in(&directory, None, 0x1234_0000, "old work\n");

    let mut harness = Harness::nano().with_text("something already here\n");
    let offered = harness
        .editor_mut()
        .offer_recovery_from(Recovery::pending_in(&directory));

    assert!(!offered, "not over the top of a buffer in use");
    assert_eq!(harness.text(), "something already here\n");
    assert_eq!(
        Recovery::pending_in(&directory).len(),
        1,
        "and it is left on disk for a buffer that can take it"
    );
}

#[test]
fn quitting_asks_about_a_buffer_that_is_not_the_active_one() {
    // ^X checked only the buffer in front of you, and then quit deleted the
    // recovery files of every buffer -- so editing one file, switching to
    // another, and leaving threw the edits away without a question and took
    // the copy that would have got them back.
    let scratch = Scratch::new("quit-other-dirty");
    let edited = scratch.file(
        "edited.txt",
        "before
",
    );
    let clean = scratch.file(
        "clean.txt",
        "untouched
",
    );

    let mut harness = Harness::nano().with_document(Document::open(&edited).unwrap());
    harness.type_text("changed");
    harness.editor_mut().workspace_mut().open(&clean).unwrap();

    assert!(
        !harness.editor().workspace().active().is_dirty(),
        "the buffer in front of us is the clean one"
    );
    assert!(
        harness.editor().workspace().any_dirty(),
        "but something still needs saving"
    );

    harness.feed("^X").unwrap();
    assert!(
        !harness.should_quit(),
        "quit without asking, and the unsaved buffer went with it"
    );
    let line = harness.prompt_line().unwrap_or_default();
    assert!(
        line.starts_with("Save modified buffer?"),
        "it should ask, the way it does for the active buffer: {line:?}"
    );
    assert_eq!(
        harness.editor().document().display_name(),
        "edited.txt",
        "and about the buffer it means, brought to the front to be looked at"
    );
}

#[test]
fn saying_yes_walks_every_unsaved_buffer_before_leaving() {
    // Two unsaved buffers, one ^X: answering the question should save this
    // one and then ask about the next, not save one and leave with the other.
    let scratch = Scratch::new("quit-save-all");
    let first = scratch.file(
        "first.txt",
        "one
",
    );
    let second = scratch.file(
        "second.txt",
        "two
",
    );

    let mut harness = Harness::nano().with_document(Document::open(&first).unwrap());
    harness.type_text("A");
    harness.editor_mut().workspace_mut().open(&second).unwrap();
    harness.type_text("B");
    assert_eq!(harness.editor().workspace().dirty_count(), 2);

    harness.feed("^X").unwrap();
    assert!(harness
        .prompt_line()
        .unwrap_or_default()
        .contains("1 more unsaved"));

    harness.type_text("y");
    assert!(!harness.should_quit(), "one saved, one still to answer for");
    assert_eq!(harness.editor().workspace().dirty_count(), 1);

    harness.type_text("y");
    assert!(harness.should_quit(), "both answered for, now it may leave");
    assert_eq!(harness.editor().workspace().dirty_count(), 0);

    assert!(std::fs::read_to_string(&first).unwrap().contains('A'));
    assert!(std::fs::read_to_string(&second).unwrap().contains('B'));
}

#[test]
fn a_deliberate_quit_takes_the_recovery_files_with_it() {
    // Answering "no" to save-before-quit is a decision to throw the edits
    // away. Being offered them back tomorrow would undo that decision.
    let scratch = Scratch::new("recover-abandon");
    let path = scratch.file("draft.txt", "saved\n");

    let mut harness = Harness::nano().with_document(Document::open(&path).unwrap());
    harness.type_text("edited");
    assert!(harness.editor().needs_recovery());

    harness.editor().write_recovery();
    harness.editor().abandon_recovery();

    // Whatever the real state directory is, nothing this run wrote is left.
    assert!(
        Recovery::pending()
            .iter()
            .all(|r| r.path.as_deref() != Some(path.as_path())),
        "the dirty buffer's file goes too"
    );
}

#[test]
fn recovery_is_wanted_exactly_while_something_is_unsaved() {
    // The UI arms its timer off this, so a wrong answer here is either lost
    // work or a wakeup every two seconds forever.
    let scratch = Scratch::new("recover-needed");
    let path = scratch.file("draft.txt", "saved\n");

    let mut harness = Harness::nano().with_document(Document::open(&path).unwrap());
    assert!(!harness.editor().needs_recovery(), "nothing typed yet");

    harness.type_text("edited");
    assert!(harness.editor().needs_recovery());

    harness.feed("^O").unwrap();
    assert!(!harness.editor().needs_recovery(), "saved, so stood down");
}
