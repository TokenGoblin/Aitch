//! The documentation has to agree with the keymap.
//!
//! `docs/guide.md` lists keys, and a guide that names a key which does
//! something else is worse than no guide. The footer is generated from the
//! keymap for exactly this reason; the prose cannot be, so it is checked
//! instead.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use aitch_core::{Chord, Context, Keymap};

fn repository_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/aitch-harness.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root")
        .to_path_buf()
}

fn read_doc(relative: &str) -> String {
    let path = repository_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Every `` `chord` `` in the file, in order.
fn quoted_chords(markdown: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = markdown;
    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('`') else { break };
        let candidate = &rest[..end];
        rest = &rest[end + 1..];

        // A chord, as opposed to a filename or a snippet: no spaces, and it
        // starts the way the two notations do.
        let looks_like_a_chord = !candidate.contains(' ')
            && !candidate.is_empty()
            && (candidate.starts_with('^')
                || candidate.starts_with("M-")
                || candidate.starts_with("Ctrl+")
                || candidate.starts_with("Shift+")
                || candidate.starts_with('F') && candidate[1..].parse::<u8>().is_ok());
        if looks_like_a_chord {
            found.push(candidate.to_string());
        }
    }
    found
}

#[test]
fn every_chord_the_guide_names_can_be_parsed() {
    // A typo like `M-^W ` or `Ctrl-T` would read fine and be wrong.
    let guide = read_doc("docs/guide.md");
    let chords = quoted_chords(&guide);
    assert!(chords.len() > 30, "found only {} chords", chords.len());

    for chord in &chords {
        assert!(
            Chord::parse(chord).is_ok(),
            "docs/guide.md writes `{chord}`, which is not a chord the keymap \
             parser understands"
        );
    }
}

#[test]
fn every_chord_the_guide_names_does_something() {
    // The guide is written against the default profile.
    let keymap = Keymap::by_name("nano").expect("the nano keymap");
    let guide = read_doc("docs/guide.md");

    // Chords the guide mentions as belonging to the other profile, in the
    // paragraph that says so. They are real keys, just not in this keymap.
    let modern_only: HashSet<&str> = [
        "^S",
        "^F",
        "^C",
        "^X",
        "^V",
        "^Z",
        "^Y",
        "^Q",
        "^A",
        "^P",
        "^B",
        "Ctrl+Shift+F",
    ]
    .into_iter()
    .collect();

    let contexts = [
        Context::Editor,
        Context::Prompt,
        Context::Search,
        Context::Tree,
        Context::Help,
    ];

    for chord in quoted_chords(&guide) {
        if modern_only.contains(chord.as_str()) {
            continue;
        }
        let parsed = Chord::parse(&chord).expect("already checked");
        let bound = contexts
            .iter()
            .any(|context| keymap.resolve(*context, parsed).is_some());
        assert!(
            bound,
            "docs/guide.md offers `{chord}`, which is not bound to anything in \
             the nano keymap"
        );
    }
}

#[test]
fn the_guide_covers_the_keys_that_do_the_work() {
    // Not every binding needs prose — the help screen lists them all — but
    // anything someone has to be told about should be in here somewhere.
    let guide = read_doc("docs/guide.md");
    for chord in [
        "^O", "^X", "^W", "^K", "^U", "^R", "^G", "^T", "^6", "^_", "M-T", "M-U", "M-E", "M-W",
        "M-^W", "M-B", "M-N", "M-P", "M-M",
    ] {
        assert!(
            guide.contains(&format!("`{chord}`")),
            "docs/guide.md never mentions {chord}"
        );
    }
}

#[test]
fn the_guide_only_claims_languages_that_are_highlighted() {
    let guide = read_doc("docs/guide.md");
    let (_, languages) = guide
        .split_once("thirteen\nlanguages:")
        .expect("the highlighting paragraph names its languages");
    let languages = languages.split('.').next().expect("a sentence");

    // Whole names, not substrings: "Java" is inside "JavaScript".
    let named: Vec<&str> = languages
        .split([',', ' ', '\n'])
        .map(|word| word.trim())
        .filter(|word| !word.is_empty())
        .collect();

    for absent in ["Go", "Java", "Ruby", "Perl", "Swift", "Kotlin"] {
        assert!(
            !named.contains(&absent),
            "docs/guide.md promises {absent} highlighting, which does not exist"
        );
    }
    for present in ["Rust", "Python", "TypeScript", "Bash", "YAML"] {
        assert!(
            named.contains(&present),
            "docs/guide.md leaves out {present}"
        );
    }
}

#[test]
fn the_documentation_the_installer_ships_is_all_there() {
    // packaging/windows/aitch.wxs names these; a missing one fails the build
    // there, but late, on a machine that may not be to hand.
    let root = repository_root();
    for shipped in [
        "README.md",
        "LICENSE",
        "docs/guide.md",
        "docs/config.md",
        "docs/keymap.md",
    ] {
        assert!(
            root.join(shipped).is_file(),
            "the installer ships {shipped}, which is not in the repository"
        );
    }
}

#[test]
fn every_image_the_readme_shows_is_in_the_repository() {
    // The screenshots are the pitch (PLAN.md Phase 8), and a README whose
    // images are broken boxes makes a worse first impression than one with no
    // images at all. They are generated rather than captured -- see
    // docs/screenshots.md -- so it is easy to move one and not notice.
    let root = repository_root();
    let readme = read_doc("README.md");

    let mut found = 0;
    let mut rest = readme.as_str();
    while let Some(start) = rest.find("](") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find(')') else { break };
        let target = &rest[..end];
        rest = &rest[end + 1..];

        // Only local images. External URLs are someone else's problem, and a
        // link to another document is checked by following it, not by us.
        if target.starts_with("http") {
            continue;
        }
        if !Path::new(target)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("png"))
        {
            continue;
        }

        found += 1;
        assert!(
            root.join(target).is_file(),
            "README.md shows {target}, which is not in the repository"
        );
    }

    assert!(found >= 2, "found only {found} images in the README");
}
