//! Folder mode: the sidebar, quick open, and several buffers at once.
//!
//! nano has no equivalent, so these are the invented parts of the editor and
//! the tests read like someone using them: toggle the tree, walk it, open a
//! file, cycle buffers, find a file by typing part of its name.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use aitch_core::{Context, Workspace};
use aitch_harness::Harness;

/// Tests run in parallel, so each folder needs its own name.
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "aitch-folder-{name}-{}-{unique}",
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
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A small project, and a harness rooted at it.
fn project(name: &str) -> (Scratch, Harness) {
    let scratch = Scratch::new(name);
    scratch.file("README.md", "# readme\n");
    scratch.file("Cargo.toml", "[package]\n");
    scratch.file("src/main.rs", "fn main() {}\n");
    scratch.file("src/lib.rs", "pub fn go() {}\n");
    scratch.file("src/deep/nested.rs", "// nested\n");
    scratch.file("target/junk.o", "build output");
    scratch.file(".gitignore", "target/\n");

    let harness = Harness::nano().with_workspace(Workspace::with_root(scratch.0.clone()));
    (scratch, harness)
}

// -- M-T, the sidebar -------------------------------------------------------

#[test]
fn the_tree_toggles_on_and_off_and_takes_focus() {
    let (_scratch, mut h) = project("toggle");
    assert!(h.editor().tree().is_none());

    h.feed("M-T").unwrap();
    assert!(h.editor().tree().is_some());
    assert_eq!(h.context(), Context::Tree, "the sidebar takes the keys");
    // And advertises its own keys, as every context does.
    assert!(h.footer_cells().iter().any(|c| c.contains("Open")));

    h.feed("M-T").unwrap();
    assert!(h.editor().tree().is_none());
    assert_eq!(h.context(), Context::Editor);
}

#[test]
fn the_tree_shows_folders_first_and_hides_what_git_ignores() {
    let (_scratch, mut h) = project("listing");
    h.feed("M-T").unwrap();

    assert_eq!(h.tree_rows(), ["> src/", "  Cargo.toml", "  README.md"]);
    assert!(
        !h.tree_rows().iter().any(|row| row.contains("target")),
        "target/ is in .gitignore"
    );
}

#[test]
fn enter_opens_a_folder_and_then_a_file() {
    let (_scratch, mut h) = project("open");
    h.feed("M-T Enter").unwrap();

    assert_eq!(
        h.tree_rows(),
        [
            "v src/",
            "  > deep/",
            "    lib.rs",
            "    main.rs",
            "  Cargo.toml",
            "  README.md",
        ]
    );

    // Walk to src/lib.rs and open it.
    h.feed("Down Down Enter").unwrap();
    assert_eq!(h.text(), "pub fn go() {}\n");
    assert_eq!(h.context(), Context::Editor, "focus follows the file");
    assert_eq!(h.editor().document().display_name(), "lib.rs");
}

#[test]
fn escape_leaves_the_tree_without_closing_it() {
    let (_scratch, mut h) = project("escape");
    h.feed("M-T").unwrap();
    h.feed("Escape").unwrap();

    assert_eq!(h.context(), Context::Editor);
    assert!(h.editor().tree().is_some(), "the sidebar is still showing");
}

#[test]
fn the_tree_selection_stops_at_both_ends() {
    let (_scratch, mut h) = project("bounds");
    h.feed("M-T").unwrap();
    assert_eq!(h.editor().tree().unwrap().selected_index(), 0);

    h.feed("Up").unwrap();
    assert_eq!(h.editor().tree().unwrap().selected_index(), 0);

    for _ in 0..20 {
        h.feed("Down").unwrap();
    }
    let tree = h.editor().tree().unwrap();
    assert_eq!(tree.selected_index(), tree.len() - 1);
}

// -- ^T, quick open ---------------------------------------------------------

#[test]
fn quick_open_finds_a_file_by_typing_part_of_its_name() {
    let (_scratch, mut h) = project("quick");
    h.feed("^T").unwrap();

    assert_eq!(h.context(), Context::Prompt);
    assert_eq!(h.prompt_line().as_deref(), Some("Open file: "));
    assert!(!h.results().is_empty(), "it opens with a list, not a blank");

    h.type_text("nested");
    assert_eq!(
        h.results().first().map(String::as_str),
        Some("src/deep/nested.rs")
    );

    h.feed("Enter").unwrap();
    assert_eq!(h.text(), "// nested\n");
    assert_eq!(h.editor().document().display_name(), "nested.rs");
}

#[test]
fn quick_open_never_offers_an_ignored_file() {
    let (_scratch, mut h) = project("quick-ignore");
    h.feed("^T").unwrap();
    h.type_text("junk");
    assert!(
        h.results().is_empty(),
        "target/junk.o is ignored: {:?}",
        h.results()
    );
}

#[test]
fn the_arrows_move_through_the_results_rather_than_through_history() {
    let (_scratch, mut h) = project("quick-arrows");
    h.feed("^T").unwrap();
    h.type_text("rs");

    let results = h.results().to_vec();
    assert!(results.len() > 1, "need several to move between");
    assert_eq!(h.editor().result_index(), 0);

    h.feed("Down").unwrap();
    assert_eq!(h.editor().result_index(), 1);
    h.feed("Up Up").unwrap();
    assert_eq!(h.editor().result_index(), 0, "stops at the top");
}

#[test]
fn cancelling_quick_open_opens_nothing() {
    let (_scratch, mut h) = project("quick-cancel");
    let before = h.text();

    h.feed("^T").unwrap();
    h.type_text("main");
    h.feed("^C").unwrap();

    assert_eq!(h.context(), Context::Editor);
    assert_eq!(h.text(), before);
}

// -- M-, M-. M-B, several buffers ------------------------------------------

#[test]
fn opening_two_files_gives_two_buffers_to_cycle() {
    let (scratch, mut h) = project("cycle");
    let main = scratch.0.join("src/main.rs");
    let lib = scratch.0.join("src/lib.rs");

    h.editor_mut().workspace_mut().open(&main).unwrap();
    h.editor_mut().workspace_mut().open(&lib).unwrap();
    assert_eq!(h.editor().workspace().len(), 2);

    assert_eq!(h.text(), "pub fn go() {}\n");
    h.feed("M-,").unwrap();
    assert_eq!(h.text(), "fn main() {}\n", "cycled back to the other one");
    h.feed("M-.").unwrap();
    assert_eq!(h.text(), "pub fn go() {}\n");
}

#[test]
fn cycling_one_buffer_says_so_rather_than_doing_nothing() {
    let (_scratch, mut h) = project("cycle-one");
    h.feed("M-.").unwrap();
    assert_eq!(h.status(), "only one buffer is open");
}

#[test]
fn the_buffer_list_shows_them_all_and_switches_to_the_picked_one() {
    let (scratch, mut h) = project("list");
    h.editor_mut()
        .workspace_mut()
        .open(&scratch.0.join("src/main.rs"))
        .unwrap();
    h.editor_mut()
        .workspace_mut()
        .open(&scratch.0.join("src/lib.rs"))
        .unwrap();

    h.feed("M-B").unwrap();
    assert_eq!(h.prompt_line().as_deref(), Some("Switch to buffer: "));
    assert_eq!(h.results(), [" 1 main.rs", ">2 lib.rs"]);
    assert_eq!(h.editor().result_index(), 1, "starts on the active one");

    h.feed("Up Enter").unwrap();
    assert_eq!(h.text(), "fn main() {}\n");
}

#[test]
fn a_buffer_list_marks_the_ones_with_unsaved_changes() {
    let (scratch, mut h) = project("list-dirty");
    h.editor_mut()
        .workspace_mut()
        .open(&scratch.0.join("src/main.rs"))
        .unwrap();
    h.type_text("// edited\n");

    h.feed("M-B").unwrap();
    assert!(
        h.results().iter().any(|row| row.ends_with("main.rs*")),
        "{:?}",
        h.results()
    );
}

#[test]
fn opening_the_same_file_twice_switches_rather_than_duplicating() {
    let (scratch, mut h) = project("no-dupes");
    let main = scratch.0.join("src/main.rs");

    h.editor_mut().workspace_mut().open(&main).unwrap();
    h.editor_mut()
        .workspace_mut()
        .open(&scratch.0.join("src/lib.rs"))
        .unwrap();
    h.editor_mut().workspace_mut().open(&main).unwrap();

    assert_eq!(h.editor().workspace().len(), 2, "two files, two buffers");
    assert_eq!(h.text(), "fn main() {}\n");
}

#[test]
fn closing_a_buffer_refuses_while_it_has_unsaved_changes() {
    let (scratch, mut h) = project("close-dirty");
    h.editor_mut()
        .workspace_mut()
        .open(&scratch.0.join("src/main.rs"))
        .unwrap();
    h.editor_mut()
        .workspace_mut()
        .open(&scratch.0.join("src/lib.rs"))
        .unwrap();
    h.type_text("x");

    h.feed("Ctrl+Shift+W").unwrap();
    assert_eq!(h.editor().workspace().len(), 2, "nothing was closed");
    assert!(h.status().contains("unsaved"), "{}", h.status());
}

#[test]
fn closing_the_last_buffer_asks_to_quit() {
    let (_scratch, mut h) = project("close-last");
    h.feed("Ctrl+Shift+W").unwrap();
    // One clean buffer: closing it is leaving, and there is nothing to save.
    assert!(h.should_quit());
}

// -- the tree and the buffers together --------------------------------------

#[test]
fn opening_a_second_file_from_the_tree_keeps_the_first() {
    let (_scratch, mut h) = project("tree-buffers");

    h.feed("M-T Enter").unwrap(); // open src/
    h.feed("Down Down Enter").unwrap(); // src/lib.rs
    assert_eq!(
        h.editor().workspace().len(),
        1,
        "the scratch buffer was reused"
    );

    // The sidebar is still showing; M-T steps back into it, with the
    // selection where it was left.
    h.feed("M-T").unwrap();
    h.feed("Down Enter").unwrap(); // src/main.rs, the row below lib.rs
    assert_eq!(h.editor().workspace().len(), 2);
    assert_eq!(h.text(), "fn main() {}\n");

    h.feed("M-,").unwrap();
    assert_eq!(h.text(), "pub fn go() {}\n", "the first file is still open");
}

#[test]
fn refresh_picks_up_a_file_created_outside_the_editor() {
    let (scratch, mut h) = project("refresh");
    h.feed("M-T").unwrap();
    assert!(!h.tree_rows().iter().any(|row| row.contains("AAA-new")));

    scratch.file("AAA-new.txt", "made elsewhere");
    h.feed("^L").unwrap();

    assert!(
        h.tree_rows().iter().any(|row| row.contains("AAA-new.txt")),
        "{:?}",
        h.tree_rows()
    );
}

// -- external changes -------------------------------------------------------

#[test]
fn saving_over_a_file_something_else_changed_asks_first() {
    let (scratch, mut h) = project("overwrite");
    let path = scratch.0.join("src/main.rs");
    h.editor_mut().workspace_mut().open(&path).unwrap();
    h.type_text("// mine\n");

    // Something else writes the file while it is open.
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(&path, "// theirs\n").unwrap();

    h.feed("^O").unwrap();
    assert_eq!(
        h.prompt_line().as_deref(),
        Some("File changed on disk since you opened it. Save anyway?: ")
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "// theirs\n",
        "nothing was written while the question was open"
    );

    // Answering no leaves their version alone.
    h.type_text("n");
    assert_eq!(fs::read_to_string(&path).unwrap(), "// theirs\n");
    assert!(h.status().contains("not saved"), "{}", h.status());
}

#[test]
fn answering_yes_overwrites_deliberately() {
    let (scratch, mut h) = project("overwrite-yes");
    let path = scratch.0.join("src/main.rs");
    h.editor_mut().workspace_mut().open(&path).unwrap();
    h.type_text("// mine\n");

    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(&path, "// theirs\n").unwrap();

    h.feed("^O").unwrap();
    h.type_text("y");
    assert!(fs::read_to_string(&path).unwrap().starts_with("// mine"));
}

#[test]
fn an_untouched_file_saves_without_a_question() {
    let (scratch, mut h) = project("no-question");
    let path = scratch.0.join("src/main.rs");
    h.editor_mut().workspace_mut().open(&path).unwrap();
    h.type_text("// mine\n");

    h.feed("^O").unwrap();
    assert_eq!(h.prompt_line(), None, "no question for an unchanged file");
    assert!(fs::read_to_string(&path).unwrap().starts_with("// mine"));

    // And saving again straight away is still fine: we wrote it, so we know.
    h.type_text("x");
    h.feed("^O").unwrap();
    assert_eq!(h.prompt_line(), None);
}

#[test]
fn refresh_says_when_the_open_file_changed_underneath() {
    let (scratch, mut h) = project("refresh-stale");
    let path = scratch.0.join("src/main.rs");
    h.editor_mut().workspace_mut().open(&path).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(&path, "// theirs\n").unwrap();

    h.feed("^L").unwrap();
    assert!(h.status().contains("changed on disk"), "{}", h.status());
}
