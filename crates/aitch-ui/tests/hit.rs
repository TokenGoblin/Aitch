//! Where a click lands.
//!
//! The document does not start at the left edge of the window: the file tree
//! takes a fixed column when it is open, and the line-number gutter takes
//! another when it is on. Drawing knows that. Hit-testing has to know the same
//! thing, or every click lands to the right of where it was aimed — by the
//! width of the gutter with `M-N`, and by twenty-eight characters with `M-T`.
//!
//! No GPU here. Laying text out and asking which character is under a point
//! are both CPU work, which is the whole reason this is testable at all.

use aitch_core::{Buffer, Command, Document, Editor, Position, Workspace};
use aitch_ui::render::screen;
use aitch_ui::render::text::TextRenderer;

const TEXT: &str = "hello world, this is a line with plenty of columns in it\n\
                    second line\n\
                    third line\n";

/// An editor over [`TEXT`], with a renderer prepared to draw it.
fn editor_and_text() -> (Editor, TextRenderer) {
    build(false)
}

/// The same, rooted at a folder, so the tree has something to show.
fn editor_and_text_in_a_folder() -> (Editor, TextRenderer) {
    build(true)
}

fn build(rooted: bool) -> (Editor, TextRenderer) {
    let mut document = Document::blank();
    document.buffer = Buffer::from_str(TEXT);
    let mut workspace = Workspace::new(document);
    if rooted {
        workspace.set_root(std::env::temp_dir());
    }
    let mut editor = Editor::with_workspace(workspace);
    editor.set_wake(|| {});

    let mut text = TextRenderer::new(14.0, 1.0);
    text.prepare(editor.buffer(), 0, (1600.0, 600.0), 0);
    (editor, text)
}

/// A point inside `column` on the first row, as drawn.
///
/// A quarter of the way in, not half: the midpoint of a character is where the
/// caret stops belonging to it and starts belonging to the next one, which is
/// right for a text editor and useless for aiming a test.
fn aim(text: &TextRenderer, editor: &Editor, column: usize) -> (f32, f32) {
    let x = screen::text_origin_x(text, editor) + (column as f32 + 0.25) * text.cell_width();
    (x, text.line_height() * 0.5)
}

#[test]
fn a_click_lands_on_the_character_under_it() {
    let (editor, text) = editor_and_text();
    assert_eq!(
        screen::text_origin_x(&text, &editor),
        0.0,
        "with no gutter and no tree the text starts at the edge"
    );

    let (x, y) = aim(&text, &editor, 7);
    assert_eq!(
        screen::hit(&text, &editor, x, y, 0.0),
        Some(Position::new(0, 7))
    );
}

#[test]
fn the_line_number_gutter_does_not_shift_the_click() {
    let (mut editor, mut text) = editor_and_text();
    editor.run(&Command::ToggleLineNumbers);
    text.prepare(editor.buffer(), 0, (1600.0, 600.0), 1);

    let origin = screen::text_origin_x(&text, &editor);
    assert!(origin > 0.0, "line numbers inset the text");

    let (x, y) = aim(&text, &editor, 7);
    assert_eq!(
        screen::hit(&text, &editor, x, y, 0.0),
        Some(Position::new(0, 7)),
        "a click aimed at column 7 must not land at 7 + the gutter width"
    );
}

#[test]
fn a_click_in_the_gutter_is_not_in_the_document() {
    let (mut editor, mut text) = editor_and_text();
    editor.run(&Command::ToggleLineNumbers);
    text.prepare(editor.buffer(), 0, (1600.0, 600.0), 1);

    let origin = screen::text_origin_x(&text, &editor);
    let y = text.line_height() * 0.5;
    assert_eq!(
        screen::hit(&text, &editor, origin - 1.0, y, 0.0),
        None,
        "the line number is not a character of the document"
    );
}

#[test]
fn the_file_tree_does_not_shift_the_click() {
    let (mut editor, mut text) = editor_and_text_in_a_folder();
    editor.run(&Command::ToggleTree);
    assert!(editor.tree().is_some(), "the tree should be showing");
    text.prepare(editor.buffer(), 0, (1600.0, 600.0), 1);

    let origin = screen::text_origin_x(&text, &editor);
    assert!(
        origin >= screen::SIDEBAR_COLUMNS as f32 * text.cell_width(),
        "the sidebar takes {} columns",
        screen::SIDEBAR_COLUMNS
    );

    let (x, y) = aim(&text, &editor, 3);
    assert_eq!(
        screen::hit(&text, &editor, x, y, 0.0),
        Some(Position::new(0, 3))
    );
}

#[test]
fn a_click_in_the_sidebar_is_not_in_the_document() {
    let (mut editor, mut text) = editor_and_text_in_a_folder();
    editor.run(&Command::ToggleTree);
    text.prepare(editor.buffer(), 0, (1600.0, 600.0), 1);

    let y = text.line_height() * 0.5;
    assert_eq!(
        screen::hit(&text, &editor, 4.0, y, 0.0),
        None,
        "clicking a file in the tree must not move the text cursor"
    );
}
