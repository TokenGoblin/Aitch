//! Syntax highlighting: which language a file is, and what colour each run of
//! it should be.
//!
//! Parsing is tree-sitter's, incremental, and belongs off the UI thread — see
//! [`crate::highlighter`] for the thread and [`Highlighter`] for the work it
//! does. This module is the part that has no timing in it: language detection,
//! the grammars, and the mapping from a grammar's capture names to the small
//! set of tokens a theme actually colours.
//!
//! That mapping is deliberately coarse. Grammars disagree about detail —
//! `@variable.parameter.builtin` in one, `@parameter` in another — and a theme
//! with forty colours is a theme nobody can read. Captures are matched by
//! their leading component, so an unfamiliar refinement lands on the general
//! token rather than falling through to unstyled text.

use std::ops::Range;
use std::path::Path;

use ropey::Rope;
use tree_sitter::{InputEdit, Language as Grammar, Parser, Query, QueryCursor, StreamingIterator};
use tree_sitter::{TextProvider, Tree};

/// The languages Aitch ships grammars for, as PLAN.md Phase 5 lists them.
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

    fn grammar(self) -> Grammar {
        match self {
            Language::Bash => tree_sitter_bash::LANGUAGE.into(),
            Language::C => tree_sitter_c::LANGUAGE.into(),
            Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Language::Css => tree_sitter_css::LANGUAGE.into(),
            Language::Html => tree_sitter_html::LANGUAGE.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::Json => tree_sitter_json::LANGUAGE.into(),
            Language::Markdown => tree_sitter_md::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::Yaml => tree_sitter_yaml::LANGUAGE.into(),
        }
    }

    /// The highlight query to run against this grammar.
    ///
    /// Two wrinkles. Crates disagree on whether the constant is singular or
    /// plural. And some queries only hold what a language adds to the one it
    /// descends from — C++'s is 939 bytes of C++-isms that expect C's 1432
    /// bytes underneath, so using it alone highlights ordinary C++ not at all.
    /// Those are concatenated here, which is what `inherits:` means everywhere
    /// else in the tree-sitter world.
    fn highlight_query(self) -> String {
        match self {
            Language::Bash => tree_sitter_bash::HIGHLIGHT_QUERY.to_string(),
            Language::C => tree_sitter_c::HIGHLIGHT_QUERY.to_string(),
            Language::Cpp => format!(
                "{}
{}",
                tree_sitter_c::HIGHLIGHT_QUERY,
                tree_sitter_cpp::HIGHLIGHT_QUERY
            ),
            Language::Css => tree_sitter_css::HIGHLIGHTS_QUERY.to_string(),
            Language::Html => tree_sitter_html::HIGHLIGHTS_QUERY.to_string(),
            Language::JavaScript => tree_sitter_javascript::HIGHLIGHT_QUERY.to_string(),
            Language::Json => tree_sitter_json::HIGHLIGHTS_QUERY.to_string(),
            // Markdown ships two grammars, block and inline. Aitch parses the
            // block one, so emphasis and links inside a paragraph are not
            // coloured; that needs an injection, which is not this phase.
            Language::Markdown => tree_sitter_md::HIGHLIGHT_QUERY_BLOCK.to_string(),
            Language::Python => tree_sitter_python::HIGHLIGHTS_QUERY.to_string(),
            Language::Rust => tree_sitter_rust::HIGHLIGHTS_QUERY.to_string(),
            Language::Toml => tree_sitter_toml_ng::HIGHLIGHTS_QUERY.to_string(),
            Language::TypeScript => format!(
                "{}
{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY
            ),
            Language::Yaml => tree_sitter_yaml::HIGHLIGHTS_QUERY.to_string(),
        }
    }
}

/// What a theme colours. Small on purpose: see the module docs.
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

impl Token {
    /// Map a grammar's capture name onto a token.
    ///
    /// Matched on the leading component, so `variable.parameter.builtin` and
    /// `variable` land in the same place. `None` means the capture is not
    /// something a theme colours, and the text is left alone.
    pub fn from_capture(capture: &str) -> Option<Token> {
        let mut parts = capture.split('.');
        let head = parts.next().unwrap_or(capture);
        let tail = parts.next().unwrap_or("");

        // The `text.*` family comes from the nvim-treesitter queries several
        // grammars ship — markdown's whole query is written in it, and
        // ignoring it leaves a document with nothing but its punctuation
        // coloured.
        if head == "text" {
            return match tail {
                "title" => Some(Token::Heading),
                "literal" | "quote" => Some(Token::String),
                "uri" | "reference" | "link" => Some(Token::Constant),
                "emphasis" | "strong" => Some(Token::Attribute),
                _ => None,
            };
        }

        let token = match head {
            "keyword" => Token::Keyword,
            "string" | "character" => Token::String,
            "title" | "heading" => Token::Heading,
            // An explicit "colour this as nothing", which some queries use to
            // punch a hole in a broader match.
            "none" => return None,
            "comment" => Token::Comment,
            "function" | "method" => Token::Function,
            "type" | "constructor" | "class" | "struct" | "interface" | "enum" => Token::Type,
            "number" | "float" | "integer" => Token::Number,
            "constant" | "boolean" | "null" => Token::Constant,
            "variable" | "parameter" | "identifier" => Token::Variable,
            "property" | "field" | "attribute" if head != "attribute" => Token::Property,
            "operator" => Token::Operator,
            "punctuation" | "delimiter" | "bracket" => Token::Punctuation,
            "attribute" | "annotation" | "decorator" | "label" => Token::Attribute,
            "module" | "namespace" => Token::Namespace,
            // `tag` is HTML/JSX element names, which read best as types.
            "tag" => Token::Type,
            _ => return None,
        };
        Some(token)
    }
}

/// A run of text that should be coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Byte offsets into the document, which is what tree-sitter works in.
    pub start: usize,
    pub end: usize,
    pub token: Token,
}

/// A parser and a query for one language, holding the tree between edits.
pub struct Highlighter {
    language: Language,
    parser: Parser,
    query: Query,
    tree: Option<Tree>,
    /// Capture index to token, so the query's names are resolved once rather
    /// than on every match.
    tokens: Vec<Option<Token>>,
}

impl Highlighter {
    /// Build a highlighter, or `None` if the grammar and query disagree —
    /// which happens when a grammar crate is upgraded and its query is not.
    pub fn new(language: Language) -> Option<Highlighter> {
        let grammar = language.grammar();
        let mut parser = Parser::new();
        parser.set_language(&grammar).ok()?;

        let query = Query::new(&grammar, &language.highlight_query()).ok()?;
        let tokens = query
            .capture_names()
            .iter()
            .map(|name| Token::from_capture(name))
            .collect();

        Some(Highlighter {
            language,
            parser,
            query,
            tree: None,
            tokens,
        })
    }

    pub fn language(&self) -> Language {
        self.language
    }

    /// Tell the tree what changed, so the next parse can reuse it.
    ///
    /// Without this the parse still succeeds — it just does all the work
    /// again, which is the difference between typing being free and not.
    /// Throw the tree away, so the next parse starts from nothing.
    ///
    /// For when the next text is a different document rather than a later
    /// version of this one: reusing the tree then colours the new file at the
    /// old file's offsets.
    pub fn forget(&mut self) {
        self.tree = None;
    }

    pub fn edit(&mut self, edit: &InputEdit) {
        if let Some(tree) = self.tree.as_mut() {
            tree.edit(edit);
        }
    }

    /// Parse, reusing the previous tree where the text has not changed.
    pub fn parse(&mut self, text: &Rope) {
        // Read straight from the rope rather than flattening it to a string:
        // a 10k-line file would otherwise be copied on every keystroke.
        let mut callback = |byte: usize, _: tree_sitter::Point| -> &[u8] {
            if byte >= text.len_bytes() {
                return &[];
            }
            let (chunk, chunk_start, _, _) = text.chunk_at_byte(byte);
            &chunk.as_bytes()[byte - chunk_start..]
        };
        self.tree = self
            .parser
            .parse_with_options(&mut callback, self.tree.as_ref(), None);
    }

    /// Coloured runs overlapping a byte range, in order and non-overlapping.
    ///
    /// tree-sitter's matches can overlap and nest — an identifier inside a
    /// macro inside an attribute. The later, more specific match wins, which
    /// is the convention every tree-sitter theme is written against.
    pub fn spans(&self, text: &Rope, range: Range<usize>) -> Vec<Span> {
        let Some(tree) = &self.tree else {
            return Vec::new();
        };

        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range.clone());

        let mut spans: Vec<Span> = Vec::new();
        let provider = RopeText(text);
        let mut matches = cursor.matches(&self.query, tree.root_node(), provider);

        while let Some(matched) = matches.next() {
            for capture in matched.captures {
                let Some(Some(token)) = self.tokens.get(capture.index as usize).copied() else {
                    continue;
                };
                let node = capture.node;
                let start = node.start_byte().max(range.start);
                let end = node.end_byte().min(range.end);
                if start >= end {
                    continue;
                }
                spans.push(Span { start, end, token });
            }
        }

        flatten(spans)
    }
}

impl std::fmt::Debug for Highlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Highlighter({})", self.language.name())
    }
}

/// Resolve overlapping spans into a flat, ordered list.
///
/// Later matches win over earlier ones where they overlap, so a specific
/// capture layered over a general one shows the specific colour.
fn flatten(mut spans: Vec<Span>) -> Vec<Span> {
    if spans.is_empty() {
        return spans;
    }
    // Stable, so equal ranges keep query order and the last still wins.
    spans.sort_by_key(|span| span.start);

    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for span in spans {
        while let Some(last) = out.last().copied() {
            if last.end <= span.start {
                break;
            }
            // Overlap: the newcomer is more specific, so it takes the ground.
            out.pop();
            if last.start < span.start {
                out.push(Span {
                    start: last.start,
                    end: span.start,
                    token: last.token,
                });
                break;
            }
        }
        if let Some(last) = out.last() {
            if last.end > span.start {
                continue;
            }
        }
        out.push(span);
    }
    out
}

/// Lets tree-sitter read a rope without flattening it.
struct RopeText<'a>(&'a Rope);

impl<'a> TextProvider<&'a [u8]> for RopeText<'a> {
    type I = RopeChunks<'a>;

    fn text(&mut self, node: tree_sitter::Node) -> Self::I {
        let range = node.byte_range();
        let slice = self.0.byte_slice(range);
        RopeChunks(slice.chunks())
    }
}

struct RopeChunks<'a>(ropey::iter::Chunks<'a>);

impl<'a> Iterator for RopeChunks<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        self.0.next().map(str::as_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rope(text: &str) -> Rope {
        Rope::from_str(text)
    }

    fn highlight(language: Language, source: &str) -> Vec<(Token, String)> {
        let text = rope(source);
        let mut highlighter = Highlighter::new(language).expect("a highlighter");
        highlighter.parse(&text);
        highlighter
            .spans(&text, 0..text.len_bytes())
            .into_iter()
            .map(|span| {
                (
                    span.token,
                    text.byte_slice(span.start..span.end).to_string(),
                )
            })
            .collect()
    }

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

    // -- the grammars themselves -------------------------------------------

    #[test]
    fn every_shipped_grammar_loads_and_its_query_compiles() {
        // The failure this guards against is a grammar crate upgrading past
        // its own highlight query, which shows up as no colour at all rather
        // than as an error.
        for language in Language::ALL {
            assert!(
                Highlighter::new(*language).is_some(),
                "{} failed to build a highlighter",
                language.name()
            );
        }
    }

    #[test]
    fn every_shipped_grammar_colours_something() {
        let samples: &[(Language, &str)] = &[
            (Language::Bash, "# comment\necho \"hi\"\n"),
            (Language::C, "int main(void) { return 0; }\n"),
            (
                Language::Cpp,
                "#include <string>\nint main() { return 0; }\n",
            ),
            (Language::Css, "body { color: red; }\n"),
            (Language::Html, "<p class=\"x\">hi</p>\n"),
            (Language::JavaScript, "const x = 1; // note\n"),
            (Language::Json, "{\"key\": 42}\n"),
            (Language::Markdown, "# Title\n\nSome *text*.\n"),
            (Language::Python, "def go(x):\n    return x  # note\n"),
            (Language::Rust, "fn main() { let x = 1; }\n"),
            (Language::Toml, "[package]\nname = \"aitch\"\n"),
            (Language::TypeScript, "const x: number = 1;\n"),
            (Language::Yaml, "key: value\n"),
        ];
        assert_eq!(
            samples.len(),
            Language::ALL.len(),
            "a language has no sample"
        );

        for (language, source) in samples {
            let spans = highlight(*language, source);
            let kinds: std::collections::HashSet<Token> =
                spans.iter().map(|(token, _)| *token).collect();
            // More than one kind: a grammar whose query is only its own
            // additions can still match a stray node and look like it works.
            assert!(
                kinds.len() >= 2,
                "{} highlighted {:?} as only {kinds:?}",
                language.name(),
                source
            );
        }
    }

    // -- what the spans say ------------------------------------------------

    #[test]
    fn rust_keywords_strings_and_comments_come_out_as_themselves() {
        let spans = highlight(Language::Rust, "// note\nfn main() { let s = \"hi\"; }\n");

        assert!(
            spans
                .iter()
                .any(|(t, text)| *t == Token::Comment && text.contains("note")),
            "{spans:?}"
        );
        assert!(
            spans
                .iter()
                .any(|(t, text)| *t == Token::Keyword && text == "fn"),
            "{spans:?}"
        );
        assert!(
            spans
                .iter()
                .any(|(t, text)| *t == Token::String && text.contains("hi")),
            "{spans:?}"
        );
    }

    #[test]
    fn spans_are_ordered_and_never_overlap() {
        let source = "fn main() {\n    let x: Vec<String> = vec![\"a\", \"b\"];\n}\n";
        let text = rope(source);
        let mut highlighter = Highlighter::new(Language::Rust).unwrap();
        highlighter.parse(&text);

        let spans = highlighter.spans(&text, 0..text.len_bytes());
        for pair in spans.windows(2) {
            assert!(
                pair[0].end <= pair[1].start,
                "overlapping spans: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
        for span in &spans {
            assert!(span.start < span.end, "empty span {span:?}");
        }
    }

    #[test]
    fn spans_stay_inside_the_range_they_were_asked_for() {
        let source = "fn one() {}\nfn two() {}\nfn three() {}\n";
        let text = rope(source);
        let mut highlighter = Highlighter::new(Language::Rust).unwrap();
        highlighter.parse(&text);

        // Only the middle line, as a viewport would ask.
        let range = 12..24;
        for span in highlighter.spans(&text, range.clone()) {
            assert!(
                span.start >= range.start && span.end <= range.end,
                "{span:?} escaped {range:?}"
            );
        }
    }

    #[test]
    fn an_unparsed_highlighter_produces_nothing_rather_than_panicking() {
        let highlighter = Highlighter::new(Language::Rust).unwrap();
        assert!(highlighter.spans(&rope("fn main() {}"), 0..12).is_empty());
    }

    #[test]
    fn broken_source_still_highlights_what_it_can() {
        // Half-typed code is the normal state of a file being edited.
        let spans = highlight(Language::Rust, "fn main( {\n    let x =\n");
        assert!(
            spans.iter().any(|(token, _)| *token == Token::Keyword),
            "{spans:?}"
        );
    }

    #[test]
    fn multibyte_text_keeps_its_span_boundaries() {
        let source = "// naïve café → 日本語\nfn main() {}\n";
        let text = rope(source);
        let mut highlighter = Highlighter::new(Language::Rust).unwrap();
        highlighter.parse(&text);

        for span in highlighter.spans(&text, 0..text.len_bytes()) {
            // Slicing would panic if a span split a character.
            let _ = text.byte_slice(span.start..span.end);
        }
    }

    // -- capture mapping ---------------------------------------------------

    #[test]
    fn capture_names_map_by_their_leading_component() {
        assert_eq!(Token::from_capture("keyword"), Some(Token::Keyword));
        assert_eq!(Token::from_capture("keyword.control"), Some(Token::Keyword));
        assert_eq!(
            Token::from_capture("variable.parameter.builtin"),
            Some(Token::Variable)
        );
        assert_eq!(Token::from_capture("string.special"), Some(Token::String));
    }

    #[test]
    fn an_unknown_capture_is_left_uncoloured() {
        assert_eq!(Token::from_capture("something.invented"), None);
        assert_eq!(Token::from_capture(""), None);
    }

    // -- incremental parsing -----------------------------------------------

    #[test]
    fn an_edited_tree_reparses_to_the_same_answer_as_a_fresh_one() {
        // The whole point of incremental parsing is that it is a shortcut,
        // not a different result.
        let before = "fn main() { let x = 1; }\n";
        let after = "fn main() { let xy = 1; }\n";

        let mut incremental = Highlighter::new(Language::Rust).unwrap();
        incremental.parse(&rope(before));
        incremental.edit(&InputEdit {
            start_byte: 17,
            old_end_byte: 17,
            new_end_byte: 18,
            start_position: tree_sitter::Point::new(0, 17),
            old_end_position: tree_sitter::Point::new(0, 17),
            new_end_position: tree_sitter::Point::new(0, 18),
        });
        let text = rope(after);
        incremental.parse(&text);

        let mut fresh = Highlighter::new(Language::Rust).unwrap();
        fresh.parse(&text);

        assert_eq!(
            incremental.spans(&text, 0..text.len_bytes()),
            fresh.spans(&text, 0..text.len_bytes())
        );
    }
}
