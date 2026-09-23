//! Syntax highlighting: which language a file is, and what colour each run
//! of it should be.
//!
//! Per [`PLAN-ZERO-DEP.md`](../../../../PLAN-ZERO-DEP.md) §2/§4 Phase 5, this
//! is no longer tree-sitter's job. Each language gets a hand-written
//! [`Lexer`]: a line-at-a-time state machine that saves its state at the end
//! of a line and resumes from it at the start of the next, so an edit only
//! re-lexes from the changed line onward rather than the whole document (see
//! [`Highlighter::parse`]). That is a real fidelity trade-off, not a free
//! lunch: this is a lexer, not a parser, so it has no syntax tree and no
//! nesting. Things tree-sitter gave for free — bracket matching across
//! nested constructs (`editor.rs` still does this itself, using
//! [`Token::String`]/[`Token::Comment`] spans to skip the ones that don't
//! count, unaffected by this rewrite), highlighting inside Markdown code
//! fences — need their own pass or stay unsupported. See `PLAN-ZERO-DEP.md`
//! §3 for the fidelity trade-offs this was signed off against.
//!
//! [`Highlighter`] is the engine: it walks a document line by line through a
//! [`Lexer`], keeps the state at every line boundary so a later edit knows
//! where it can safely resume, and flattens the result into byte-ordered,
//! non-overlapping [`Span`]s. A [`Lexer`] itself is pure, stateless logic —
//! see its own docs — which is what makes each one unit-testable with no
//! document, thread, or engine involved.

use std::ops::Range;
use std::path::Path;

use crate::buffer::TextEdit;

mod bash;
mod c;
mod cpp;
mod css;
mod html;
mod javascript;
mod json;
mod markdown;
mod python;
mod rust;
mod toml;
mod typescript;
mod yaml;

/// The languages Aitch ships a lexer for. PLAN.md Phase 5 lists all thirteen,
/// and `PLAN-ZERO-DEP.md`'s Wave 1/Wave 2 split landed them a few at a time
/// (each still its own file under `syntax/`, with its own tests) rather than
/// all at once — [`Language::lexer`] would have returned `None` for one not
/// yet written, the same graceful "no colour" fallback an unknown file
/// extension gets, but every variant here has one now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Bash,
    C,
    Cpp,
    Css,
    Html,
    JavaScript,
    Json,
    Markdown,
    Python,
    Rust,
    Toml,
    TypeScript,
    Yaml,
}

impl Language {
    /// Every language, for tests that must cover all of them.
    pub const ALL: &'static [Language] = &[
        Language::Bash,
        Language::C,
        Language::Cpp,
        Language::Css,
        Language::Html,
        Language::JavaScript,
        Language::Json,
        Language::Markdown,
        Language::Python,
        Language::Rust,
        Language::Toml,
        Language::TypeScript,
        Language::Yaml,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Language::Bash => "Shell",
            Language::C => "C",
            Language::Cpp => "C++",
            Language::Css => "CSS",
            Language::Html => "HTML",
            Language::JavaScript => "JavaScript",
            Language::Json => "JSON",
            Language::Markdown => "Markdown",
            Language::Python => "Python",
            Language::Rust => "Rust",
            Language::Toml => "TOML",
            Language::TypeScript => "TypeScript",
            Language::Yaml => "YAML",
        }
    }

    /// Work out a file's language from its name.
    ///
    /// Extension first, then the whole filename for the ones that have no
    /// extension worth the name — a `Makefile` is not a `.mk` file and
    /// `.bashrc` is not a `.rc` file.
    pub fn from_path(path: &Path) -> Option<Language> {
        let name = path.file_name()?.to_str()?.to_ascii_lowercase();

        // Whole-name matches win: `CMakeLists.txt` is not plain text.
        let by_name = match name.as_str() {
            ".bashrc" | ".bash_profile" | ".profile" | ".zshrc" => Some(Language::Bash),
            "cargo.lock" | "cargo.toml" => Some(Language::Toml),
            "dockerfile" | "makefile" => None,
            _ => None,
        };
        if by_name.is_some() {
            return by_name;
        }

        let extension = name.rsplit_once('.').map(|(_, ext)| ext)?;
        let language = match extension {
            "sh" | "bash" | "zsh" => Language::Bash,
            "c" | "h" => Language::C,
            "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => Language::Cpp,
            "css" => Language::Css,
            "htm" | "html" | "xhtml" => Language::Html,
            "cjs" | "js" | "jsx" | "mjs" => Language::JavaScript,
            "json" | "jsonc" => Language::Json,
            "markdown" | "md" => Language::Markdown,
            "py" | "pyi" | "pyw" => Language::Python,
            "rs" => Language::Rust,
            "toml" => Language::Toml,
            "mts" | "ts" | "tsx" => Language::TypeScript,
            "yaml" | "yml" => Language::Yaml,
            _ => return None,
        };
        Some(language)
    }

    /// This language's lexer, or `None` if nobody has written one yet.
    fn lexer(self) -> Option<Box<dyn Lexer>> {
        match self {
            // Wave 1, PLAN-ZERO-DEP.md Phase 5.
            Language::Rust => Some(Box::new(rust::RustLexer)),
            Language::Json => Some(Box::new(json::JsonLexer)),
            Language::Toml => Some(Box::new(toml::TomlLexer)),
            Language::Markdown => Some(Box::new(markdown::MarkdownLexer)),
            Language::Bash => Some(Box::new(bash::BashLexer)),
            // Wave 2, PLAN-ZERO-DEP.md Phase 5.
            Language::C => Some(Box::new(c::CLexer)),
            Language::Cpp => Some(Box::new(cpp::CppLexer)),
            Language::Css => Some(Box::new(css::CssLexer)),
            Language::Html => Some(Box::new(html::HtmlLexer)),
            Language::JavaScript => Some(Box::new(javascript::JavaScriptLexer)),
            Language::Python => Some(Box::new(python::PythonLexer)),
            Language::TypeScript => Some(Box::new(typescript::TypeScriptLexer)),
            Language::Yaml => Some(Box::new(yaml::YamlLexer)),
        }
    }
}

/// What a theme colours. Small on purpose: a theme with forty colours is a
/// theme nobody can read, and every lexer maps its own vocabulary onto this
/// same short list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Token {
    Keyword,
    /// Strings, characters, and literal blocks.
    String,
    Comment,
    Function,
    Type,
    Number,
    /// Booleans, `nil`, named constants.
    Constant,
    Variable,
    /// Fields and object keys.
    Property,
    Operator,
    Punctuation,
    /// Rust attributes, decorators, annotations.
    Attribute,
    /// Modules and namespaces.
    Namespace,
    /// Markdown headings and anything else meant to stand out as a title.
    Heading,
}

/// A run of text that should be coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Byte offsets into the document.
    pub start: usize,
    pub end: usize,
    pub token: Token,
}

/// State carried from the end of one line to the start of the next.
///
/// Opaque to the engine: it only ever compares two for equality and holds a
/// default one for the very first line. Each [`Lexer`] packs whatever it
/// needs to resume mid-construct — a block-comment nesting depth, "inside a
/// string", "inside a fenced code block" — into its own bit layout and is
/// the only code that interprets it. 32 bits is deliberately generous for a
/// line-state lexer's needs; nothing here should ever need to spill out of
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineState(pub u32);

impl LineState {
    /// The state at the very start of a document.
    pub const INITIAL: LineState = LineState(0);
}

/// A hand-written, line-at-a-time lexer for one language.
///
/// Pure logic: no document, no thread, no incremental re-lex bookkeeping —
/// that is [`Highlighter`]'s job, not this trait's. Implementors are one
/// self-contained file each under `syntax/`, unit-tested by feeding
/// [`Lexer::lex_line`] fixture lines directly and checking the tokens (and,
/// for anything with cross-line state, that state saved from one call and
/// fed into the next produces the same answer as lexing both lines
/// together).
pub trait Lexer: Send {
    /// Lex one line, including its line break if it has one (a lexer that
    /// does not care about line breaks can simply never match `\r`/`\n`, and
    /// they end up in no span, same as any other unstyled byte).
    ///
    /// `state` is what [`Lexer::lex_line`] returned for the previous line,
    /// or [`LineState::INITIAL`] for the first line of the document.
    ///
    /// Returns byte-range spans local to `line` — `0..line.len()`, never
    /// crossing it — in order and non-overlapping (a lexer's own output is
    /// fully under its control, unlike tree-sitter's captures were, so
    /// there is no flattening step above this), and the state to resume
    /// with on the line after this one.
    fn lex_line(&self, line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState);
}

/// A lexer, and the running state that lets it re-lex only what an edit
/// touched.
pub struct Highlighter {
    language: Language,
    lexer: Box<dyn Lexer>,
    /// Whether [`Highlighter::parse`] has ever completed. Distinct from
    /// `states.len() <= 1` (an empty document is a legitimate parsed state).
    parsed: bool,
    /// State at every line boundary, oldest first: `states[i]` is what
    /// [`Highlighter::parse`] saw entering line `i`, and `states.len()` is
    /// always the number of lines last seen, plus one for the boundary past
    /// the last line. Index 0 is always [`LineState::INITIAL`].
    states: Vec<LineState>,
    /// Byte offset where line `i` starts, parallel to `states`.
    line_starts: Vec<usize>,
    /// Spans for the whole document, in byte order and non-overlapping.
    spans: Vec<Span>,
}

impl Highlighter {
    /// Build a highlighter, or `None` if nobody has written this language's
    /// lexer yet.
    pub fn new(language: Language) -> Option<Highlighter> {
        let lexer = language.lexer()?;
        Some(Highlighter {
            language,
            lexer,
            parsed: false,
            states: vec![LineState::INITIAL],
            line_starts: vec![0],
            spans: Vec::new(),
        })
    }

    pub fn language(&self) -> Language {
        self.language
    }

    /// Throw away everything parsed so far, so the next [`Highlighter::parse`]
    /// starts from nothing.
    ///
    /// For when the next text is a different document rather than a later
    /// version of this one: without this, a buffer switch between two files
    /// of the same language reuses this document's line states for the new
    /// one, at offsets that mean nothing in it.
    pub fn forget(&mut self) {
        self.parsed = false;
        self.states.truncate(1);
        self.line_starts.truncate(1);
        self.spans.clear();
    }

    /// Lex `lines`, reusing whatever the last call already worked out.
    ///
    /// `edits` describes what changed since the last call (empty if this is
    /// only a request for a different range of an unchanged document, in
    /// which case this does nothing — the previous answer is still exactly
    /// right). Re-lexing resumes at the earliest line any edit touched,
    /// using the state saved there, and runs to the end of `lines` — not
    /// stopping early even if the state it computes would match what was
    /// there before, since knowing that without tree-sitter's persistent
    /// tree would need the very shift bookkeeping a lexer this simple is
    /// meant to avoid. An edit at the top of a very large file is therefore
    /// the case this pays for; see `PLAN-ZERO-DEP.md` Phase 5's acceptance
    /// note for the budget this was measured against.
    ///
    /// Which line an edit "touched" only needs to be a safe lower bound, not
    /// exact: every edit in `edits` was recorded against the document state
    /// this highlighter had actually parsed at the time (the worker in
    /// `highlighter.rs` never drops an edit between two `parse` calls, only
    /// between two *requests* — see `Job::superseded_by`), so the smallest
    /// start line across all of them is always a line this parse's stored
    /// state still describes correctly.
    pub fn parse(&mut self, lines: &[String], edits: &[TextEdit]) {
        if self.parsed && edits.is_empty() {
            return;
        }

        let known_lines = self.states.len() - 1;
        let start = if self.parsed {
            edits
                .iter()
                .map(|edit| edit.start_point.0)
                .min()
                .unwrap_or(known_lines)
                .min(known_lines)
                .min(lines.len())
        } else {
            0
        };

        self.states.truncate(start + 1);
        self.line_starts.truncate(start + 1);
        let mut state = self.states[start];
        let mut offset = self.line_starts[start];
        self.spans.retain(|span| span.end <= offset);

        for line in &lines[start..] {
            let (line_spans, next_state) = self.lexer.lex_line(line, state);
            for (range, token) in line_spans {
                debug_assert!(range.end <= line.len(), "a span escaped its own line");
                self.spans.push(Span {
                    start: offset + range.start,
                    end: offset + range.end,
                    token,
                });
            }
            offset += line.len();
            state = next_state;
            self.states.push(state);
            self.line_starts.push(offset);
        }

        self.parsed = true;
    }

    /// Coloured runs overlapping a byte range, in order and non-overlapping.
    pub fn spans(&self, range: Range<usize>) -> Vec<Span> {
        self.spans
            .iter()
            .filter_map(|span| {
                let start = span.start.max(range.start);
                let end = span.end.min(range.end);
                (start < end).then_some(Span {
                    start,
                    end,
                    token: span.token,
                })
            })
            .collect()
    }
}

impl std::fmt::Debug for Highlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Highlighter({})", self.language.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- detection ---------------------------------------------------------

    #[test]
    fn a_file_is_recognized_by_its_extension() {
        let cases = [
            ("main.rs", Language::Rust),
            ("lib.RS", Language::Rust),
            ("script.py", Language::Python),
            ("app.tsx", Language::TypeScript),
            ("index.html", Language::Html),
            ("style.css", Language::Css),
            ("data.json", Language::Json),
            ("config.yml", Language::Yaml),
            ("README.md", Language::Markdown),
            ("build.sh", Language::Bash),
            ("thing.hpp", Language::Cpp),
            ("thing.c", Language::C),
        ];
        for (name, expected) in cases {
            assert_eq!(
                Language::from_path(Path::new(name)),
                Some(expected),
                "{name}"
            );
        }
    }

    #[test]
    fn some_files_are_known_by_name_rather_than_extension() {
        assert_eq!(
            Language::from_path(Path::new(".bashrc")),
            Some(Language::Bash)
        );
        assert_eq!(
            Language::from_path(Path::new("Cargo.lock")),
            Some(Language::Toml),
            "a lock file is TOML however it is spelled"
        );
    }

    #[test]
    fn an_unknown_file_has_no_language_rather_than_a_wrong_one() {
        for name in ["notes.txt", "data.bin", "noextension", ""] {
            assert_eq!(Language::from_path(Path::new(name)), None, "{name}");
        }
    }

    #[test]
    fn a_path_is_recognized_by_its_last_component() {
        assert_eq!(
            Language::from_path(Path::new("src/deep/main.rs")),
            Some(Language::Rust)
        );
    }

    #[test]
    fn every_language_has_a_lexer() {
        // PLAN-ZERO-DEP.md Phase 5's Wave 1 and Wave 2 are both landed now;
        // this replaces the tree-sitter era's
        // `every_shipped_grammar_loads_and_its_query_compiles`, guarding
        // against the same failure shape a future language addition could
        // reintroduce: `Language::ALL` growing a variant that
        // `Language::lexer` forgets to wire up, which showed up back then as
        // silent no-colour rather than a build error.
        for language in Language::ALL {
            assert!(
                Highlighter::new(*language).is_some(),
                "{} has no lexer",
                language.name()
            );
        }
    }

    // -- the engine, against a fixture lexer --------------------------------
    //
    // A tiny lexer that has nothing to do with any real language, so these
    // tests are about `Highlighter`'s re-lex bookkeeping, not any one
    // language's rules. It colours runs of ASCII digits as `Token::Number`,
    // and `<` opens a multi-line "quoted" region (`Token::String`) that runs
    // until a matching `>`, to exercise state carried across a line boundary.
    struct FixtureLexer;

    const IN_QUOTE: LineState = LineState(1);

    impl Lexer for FixtureLexer {
        fn lex_line(
            &self,
            line: &str,
            state: LineState,
        ) -> (Vec<(Range<usize>, Token)>, LineState) {
            let mut spans = Vec::new();
            let bytes = line.as_bytes();
            let mut in_quote = state == IN_QUOTE;
            let mut i = 0;
            let mut run_start = if in_quote { Some(0) } else { None };

            while i < bytes.len() {
                let byte = bytes[i];
                if in_quote {
                    if byte == b'>' {
                        spans.push((run_start.unwrap()..i + 1, Token::String));
                        run_start = None;
                        in_quote = false;
                    }
                    i += 1;
                    continue;
                }
                if byte == b'<' {
                    run_start = Some(i);
                    in_quote = true;
                    i += 1;
                    continue;
                }
                if byte.is_ascii_digit() {
                    let start = i;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                    spans.push((start..i, Token::Number));
                    continue;
                }
                i += 1;
            }

            if let Some(start) = run_start {
                if in_quote {
                    spans.push((start..line.len(), Token::String));
                }
            }

            (
                spans,
                if in_quote {
                    IN_QUOTE
                } else {
                    LineState::INITIAL
                },
            )
        }
    }

    fn fixture() -> Highlighter {
        Highlighter {
            language: Language::Rust,
            lexer: Box::new(FixtureLexer),
            parsed: false,
            states: vec![LineState::INITIAL],
            line_starts: vec![0],
            spans: Vec::new(),
        }
    }

    fn lines_of(text: &str) -> Vec<String> {
        // Mirrors `Buffer::snapshot_lines`: each entry keeps its own `\n`.
        let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_string).collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        lines
    }

    fn edit_at_line(line: usize) -> TextEdit {
        TextEdit {
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: 0,
            start_point: (line, 0),
            old_end_point: (line, 0),
            new_end_point: (line, 0),
        }
    }

    #[test]
    fn a_fresh_parse_covers_every_line() {
        let mut highlighter = fixture();
        let lines = lines_of("12\nab\n34\n");
        highlighter.parse(&lines, &[]);

        let spans = highlighter.spans(0..lines.iter().map(String::len).sum());
        let numbers: Vec<Range<usize>> = spans
            .iter()
            .filter(|s| s.token == Token::Number)
            .map(|s| s.start..s.end)
            .collect();
        assert_eq!(numbers, vec![0..2, 6..8]);
    }

    #[test]
    fn requesting_again_with_no_edits_is_a_no_op() {
        let mut highlighter = fixture();
        let lines = lines_of("12\n34\n");
        highlighter.parse(&lines, &[]);
        let before = highlighter.spans(0..100);

        // Same lines handed back, no edits: nothing should change, and in
        // particular this must not panic re-reading an out-of-range line.
        highlighter.parse(&lines, &[]);
        assert_eq!(highlighter.spans(0..100), before);
    }

    #[test]
    fn an_edit_only_relexes_from_its_line_onward() {
        let mut highlighter = fixture();
        let before = lines_of("11\n22\n33\n");
        highlighter.parse(&before, &[]);

        // Change the middle line's digits; lines before and after are
        // untouched content, but still get re-included in the new spans
        // because the engine relexes to the end, not just the changed line.
        let after = lines_of("11\n99\n33\n");
        highlighter.parse(&after, &[edit_at_line(1)]);

        let spans = highlighter.spans(0..100);
        let numbers: Vec<(Range<usize>, Token)> = spans
            .into_iter()
            .filter(|s| s.token == Token::Number)
            .map(|s| (s.start..s.end, s.token))
            .collect();
        assert_eq!(
            numbers,
            vec![
                (0..2, Token::Number),
                (3..5, Token::Number),
                (6..8, Token::Number)
            ]
        );
    }

    #[test]
    fn state_carries_across_a_line_boundary() {
        // The `<` on line 0 is not closed until line 2, so `99` on line 1
        // must come back as part of the quote, not as a number.
        let mut highlighter = fixture();
        let lines = lines_of("a<b\n99\nc>d\n");
        highlighter.parse(&lines, &[]);

        let spans = highlighter.spans(0..100);
        assert!(
            !spans.iter().any(|s| s.token == Token::Number),
            "{spans:?}: the digits are inside the quote"
        );
        let quote: Vec<Range<usize>> = spans
            .iter()
            .filter(|s| s.token == Token::String)
            .map(|s| s.start..s.end)
            .collect();
        // One span per line, since a `Lexer` never returns a span crossing
        // its own line — each covers from where the quote opened (or the
        // line's own start, if it was already open) to where it closed (or
        // the line's own end, including its break, if it was still open).
        assert_eq!(quote, vec![1..4, 4..7, 7..9]);
    }

    #[test]
    fn an_edit_that_changes_line_count_still_relexes_correctly() {
        let mut highlighter = fixture();
        let before = lines_of("1\n2\n3\n");
        highlighter.parse(&before, &[]);

        // Insert two new lines after line 0 (a real edit would report this
        // as start_point.0 == 1, old_end == new_end on row 0, but only the
        // start row matters to this engine).
        let after = lines_of("1\n55\n66\n2\n3\n");
        highlighter.parse(&after, &[edit_at_line(1)]);

        let numbers: Vec<Range<usize>> = highlighter
            .spans(0..100)
            .into_iter()
            .filter(|s| s.token == Token::Number)
            .map(|s| s.start..s.end)
            .collect();
        assert_eq!(numbers, vec![0..1, 2..4, 5..7, 8..9, 10..11]);
    }

    #[test]
    fn multiple_coalesced_edits_relex_from_the_earliest_line() {
        let mut highlighter = fixture();
        let before = lines_of("1\n2\n3\n4\n");
        highlighter.parse(&before, &[]);

        let after = lines_of("9\n2\n3\n9\n");
        // Recorded out of line order, the way two coalesced edits from
        // different parts of the file would be; the engine must still find
        // the earliest one.
        highlighter.parse(&after, &[edit_at_line(3), edit_at_line(0)]);

        let numbers: Vec<Range<usize>> = highlighter
            .spans(0..100)
            .into_iter()
            .filter(|s| s.token == Token::Number)
            .map(|s| s.start..s.end)
            .collect();
        assert_eq!(numbers, vec![0..1, 2..3, 4..5, 6..7]);
    }

    #[test]
    fn spans_stay_inside_the_range_asked_for() {
        let mut highlighter = fixture();
        let lines = lines_of("111\n222\n333\n");
        highlighter.parse(&lines, &[]);

        let range = 4..7;
        for span in highlighter.spans(range.clone()) {
            assert!(
                span.start >= range.start && span.end <= range.end,
                "{span:?} escaped {range:?}"
            );
        }
    }

    #[test]
    fn forgetting_then_parsing_a_shorter_document_does_not_panic() {
        let mut highlighter = fixture();
        highlighter.parse(&lines_of("1\n2\n3\n4\n5\n"), &[]);
        highlighter.forget();
        highlighter.parse(&lines_of("9\n"), &[]);

        assert_eq!(highlighter.spans(0..100).len(), 1);
    }

    #[test]
    fn an_empty_document_parses_to_no_spans_without_panicking() {
        let mut highlighter = fixture();
        highlighter.parse(&[], &[]);
        assert!(highlighter.spans(0..0).is_empty());
    }

    #[test]
    fn an_unparsed_highlighter_produces_nothing() {
        let highlighter = fixture();
        assert!(highlighter.spans(0..100).is_empty());
    }
}
