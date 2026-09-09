//! The safety net: load a file, edit it, save it, and check the bytes.
//!
//! PLAN.md's acceptance criterion for Phase 2 is that a load-edit-save cycle
//! produces a file byte-identical to the original except for the edit. These
//! fixtures are the evidence. Never delete them — every phase after this one
//! is built on the assumption that files survive being opened.
//!
//! The fixtures are marked `-text` in `.gitattributes`, so git will not
//! normalize their line endings on checkout. A normalized CRLF fixture is a
//! test that has quietly stopped testing anything.

use std::fs;
use std::path::{Path, PathBuf};

use aitch_core::fileio::{self, Charset, Encoding};
use aitch_core::{Buffer, Position, Viewport};

const FIXTURES: &[&str] = &[
    "unix-lf.txt",
    "dos-crlf.txt",
    "mac-cr.txt",
    "mixed-endings.txt",
    "no-trailing-newline.txt",
    "empty.txt",
    "utf8-plain.txt",
    "utf8-bom.txt",
    "utf16le-bom.txt",
    "utf16be-bom.txt",
    "utf16le-nobom.txt",
    "utf16be-nobom.txt",
    "utf16le-astral.txt",
    "utf16le-crlf.txt",
    "latin1.txt",
];

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn fixture(name: &str) -> Vec<u8> {
    let path = fixture_dir().join(name);
    fs::read(&path).unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", path.display()))
}

/// A scratch directory that cleans itself up.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("aitch-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("scratch directory");
        Scratch(dir)
    }

    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).expect("write fixture copy");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Show a byte difference in a way that can be read at 2am.
fn assert_bytes_eq(actual: &[u8], expected: &[u8], what: &str) {
    if actual == expected {
        return;
    }
    let at = actual
        .iter()
        .zip(expected.iter())
        .position(|(a, b)| a != b)
        .unwrap_or(actual.len().min(expected.len()));
    panic!(
        "{what}\n  first difference at byte {at}\n  \
         expected {} bytes: {:02x?}\n  got      {} bytes: {:02x?}",
        expected.len(),
        &expected[at.saturating_sub(4)..expected.len().min(at + 8)],
        actual.len(),
        &actual[at.saturating_sub(4)..actual.len().min(at + 8)],
    );
}

#[test]
fn saving_an_untouched_file_changes_nothing() {
    let scratch = Scratch::new("untouched");

    for name in FIXTURES {
        let original = fixture(name);
        let path = scratch.file(name, &original);

        let loaded = fileio::load(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        fileio::save(&path, &loaded.text, loaded.encoding)
            .unwrap_or_else(|e| panic!("{name}: {e}"));

        assert_bytes_eq(
            &fs::read(&path).unwrap(),
            &original,
            &format!("{name} was rewritten by a save that changed nothing"),
        );
    }
}

#[test]
fn one_edit_changes_exactly_one_thing() {
    let scratch = Scratch::new("one-edit");

    for name in FIXTURES {
        let original = fixture(name);
        let path = scratch.file(name, &original);
        let loaded = fileio::load(&path).unwrap_or_else(|e| panic!("{name}: {e}"));

        // Insert at the very start, through the real editing path, so the
        // expected bytes are the original with the encoded word spliced in
        // after the BOM. Nothing else may move.
        let mut buffer = Buffer::from_str(&loaded.text);
        buffer.set_line_ending(loaded.line_ending);
        buffer.set_cursor(Position::new(0, 0));
        assert!(buffer.insert("EDIT"), "{name}: the edit did not apply");

        fileio::save(&path, &buffer.text().to_string(), loaded.encoding)
            .unwrap_or_else(|e| panic!("{name}: {e}"));

        let bom_len = bom_length(loaded.encoding);
        let inserted = fileio::encode("EDIT", strip_bom_flag(loaded.encoding)).unwrap();
        let mut expected = original[..bom_len].to_vec();
        expected.extend_from_slice(&inserted);
        expected.extend_from_slice(&original[bom_len..]);

        assert_bytes_eq(
            &fs::read(&path).unwrap(),
            &expected,
            &format!("{name} changed in more than the one place it was edited"),
        );
    }
}

fn bom_length(encoding: Encoding) -> usize {
    if !encoding.bom {
        return 0;
    }
    match encoding.charset {
        Charset::Utf8 => 3,
        Charset::Utf16Le | Charset::Utf16Be => 2,
        Charset::Latin1 => 0,
    }
}

fn strip_bom_flag(encoding: Encoding) -> Encoding {
    Encoding {
        bom: false,
        ..encoding
    }
}

#[test]
fn line_endings_are_detected_as_the_file_wrote_them() {
    use aitch_core::LineEnding;

    let cases = [
        ("unix-lf.txt", LineEnding::Lf),
        ("dos-crlf.txt", LineEnding::CrLf),
        ("mac-cr.txt", LineEnding::Cr),
        ("utf16le-crlf.txt", LineEnding::CrLf),
        // Two CRLF against one LF and one CR.
        ("mixed-endings.txt", LineEnding::CrLf),
        // Nothing to go on, so the sane default.
        ("no-trailing-newline.txt", LineEnding::Lf),
        ("empty.txt", LineEnding::Lf),
    ];

    for (name, expected) in cases {
        let loaded = fileio::load_bytes(&fixture(name)).unwrap();
        assert_eq!(loaded.line_ending, expected, "{name}");
    }
}

#[test]
fn encodings_are_detected_as_the_file_wrote_them() {
    let cases = [
        ("unix-lf.txt", Charset::Utf8, false),
        ("utf8-plain.txt", Charset::Utf8, false),
        ("utf8-bom.txt", Charset::Utf8, true),
        ("utf16le-bom.txt", Charset::Utf16Le, true),
        ("utf16be-bom.txt", Charset::Utf16Be, true),
        ("utf16le-nobom.txt", Charset::Utf16Le, false),
        ("utf16be-nobom.txt", Charset::Utf16Be, false),
        ("latin1.txt", Charset::Latin1, false),
    ];

    for (name, charset, bom) in cases {
        let loaded = fileio::load_bytes(&fixture(name)).unwrap();
        assert_eq!(loaded.encoding.charset, charset, "{name} charset");
        assert_eq!(loaded.encoding.bom, bom, "{name} bom");
    }
}

#[test]
fn a_typed_newline_matches_the_file_it_is_typed_into() {
    let scratch = Scratch::new("typed-newline");

    for (name, expected_break) in [
        ("unix-lf.txt", "\n"),
        ("dos-crlf.txt", "\r\n"),
        ("mac-cr.txt", "\r"),
    ] {
        let original = fixture(name);
        let path = scratch.file(name, &original);
        let loaded = fileio::load(&path).unwrap();

        let mut buffer = Buffer::from_str(&loaded.text);
        buffer.set_line_ending(loaded.line_ending);
        buffer.set_cursor(Position::new(0, 0));
        buffer.insert_newline();

        let text = buffer.text().to_string();
        assert!(
            text.starts_with(expected_break),
            "{name}: typed newline was {:?}, not {expected_break:?}",
            &text[..expected_break.len().min(text.len())]
        );

        // And the rest of the file is untouched.
        fileio::save(&path, &text, loaded.encoding).unwrap();
        let saved = fs::read(&path).unwrap();
        let bom = bom_length(loaded.encoding);
        let inserted = fileio::encode(expected_break, strip_bom_flag(loaded.encoding)).unwrap();
        assert_eq!(&saved[bom + inserted.len()..], &original[bom..], "{name}");
    }
}

#[test]
fn editing_a_crlf_file_never_splits_a_line_break() {
    let loaded = fileio::load_bytes(&fixture("dos-crlf.txt")).unwrap();
    let mut buffer = Buffer::from_str(&loaded.text);
    let viewport = Viewport::new(10);
    let _ = viewport;

    // Walk the cursor across the whole file one character at a time. It must
    // never come to rest between a CR and its LF, because ropey counts the
    // pair as one break and a cursor inside it has a column off the end of
    // its own line.
    buffer.set_cursor(Position::new(0, 0));
    for _ in 0..buffer.len_chars() + 2 {
        let position = buffer.cursor();
        assert!(
            position.column <= buffer.line_len(position.line),
            "cursor landed inside a CRLF at {position:?}"
        );
        buffer.move_right();
    }

    // And backspacing over one takes the whole pair, joining the lines
    // rather than leaving a naked CR behind.
    let mut buffer = Buffer::from_str(&loaded.text);
    buffer.set_cursor(Position::new(1, 0));
    assert!(buffer.delete_backward());
    assert_eq!(buffer.line_text(0), "first linesecond line");
    assert!(
        !buffer.text().to_string().contains("\rsecond"),
        "a stray CR was left behind"
    );
}

#[test]
fn a_latin1_file_that_gains_an_unencodable_character_refuses_to_save() {
    let scratch = Scratch::new("latin1-refuse");
    let original = fixture("latin1.txt");
    let path = scratch.file("latin1.txt", &original);

    let loaded = fileio::load(&path).unwrap();
    let mut buffer = Buffer::from_str(&loaded.text);
    buffer.set_cursor(Position::new(0, 0));
    buffer.insert("日本語");

    let result = fileio::save(&path, &buffer.text().to_string(), loaded.encoding);
    assert!(result.is_err(), "Latin-1 cannot hold this and must say so");
    assert_bytes_eq(
        &fs::read(&path).unwrap(),
        &original,
        "a refused save damaged the file",
    );
}
