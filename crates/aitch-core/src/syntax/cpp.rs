//! A hand-written C++ lexer. This covers ordinary C-style code as well —
//! `.cpp`/`.hpp` files are full of it — plus the C++ additions: `class`,
//! `namespace`, `template`, and friends; `::` scope resolution; and, as a
//! nice-to-have rather than a strict requirement, C++11 raw strings
//! (`R"(...)"`, with an optional delimiter) and `[[attribute]]` syntax.
//!
//! No syntax tree, so nothing here tracks brackets or nesting beyond what a
//! [`LineState`] can carry across one line boundary — see `syntax.rs`'s
//! module docs for what that trades away.
//!
//! Design calls, since C++ leaves several of these open:
//! - `char, int, float, double, bool, void, short, long, signed, unsigned`
//!   come out as [`Token::Type`] rather than [`Token::Keyword`] — they name a
//!   type, the same reasoning `rust.rs`'s `PRIMITIVE_TYPES` split follows.
//! - `true`/`false`/`nullptr` are [`Token::Constant`]; `this` is
//!   [`Token::Variable`] (it names an object, just like `self` in
//!   `rust.rs`). Everything else in the keyword lists is
//!   [`Token::Keyword`].
//! - Raw strings do not carry state across a line boundary: the delimiter
//!   (up to 16 arbitrary bytes) does not fit in a `LineState`'s 32 bits
//!   alongside everything else it already has to track, and a raw string
//!   spanning multiple lines is rare enough in practice that this was not
//!   worth the complexity. An unterminated one on a single line still
//!   colours as a string to the end of that line — the "plain `"..."` scan
//!   as a fallback" the brief allows for — it just does not resume on the
//!   next line as a fully faithful implementation would.

use std::ops::Range;

use super::{Lexer, LineState, Token};

pub(crate) struct CppLexer;

// `LineState`'s bit layout for C++: unlike Rust, `/* */` does not nest here
// (that is C's rule), so a block comment only ever needs a flag, not a
// depth. Plain (non-raw) strings can also span a line boundary — mostly
// useful for "broken source still lexes what it can" once an editor is
// mid-edit — and get their own flag. Raw strings deliberately do not: see
// the module docs.
const NORMAL: LineState = LineState(0);
const IN_BLOCK_COMMENT: LineState = LineState(1);
const IN_STRING: LineState = LineState(2);

const KEYWORDS: &[&str] = &[
    // C.
    "auto",
    "break",
    "case",
    "const",
    "continue",
    "default",
    "do",
    "else",
    "enum",
    "extern",
    "for",
    "goto",
    "if",
    "inline",
    "register",
    "return",
    "sizeof",
    "static",
    "struct",
    "switch",
    "typedef",
    "union",
    "volatile",
    "while",
    // C++ additions.
    "class",
    "public",
    "private",
    "protected",
    "namespace",
    "template",
    "typename",
    "using",
    "new",
    "delete",
    "try",
    "catch",
    "throw",
    "virtual",
    "override",
    "final",
    "constexpr",
    "operator",
    "friend",
    "mutable",
    "explicit",
    "noexcept",
    "decltype",
    "static_assert",
    "and",
    "or",
    "not",
    "export",
    "module",
    "import",
    "concept",
    "requires",
    "co_await",
    "co_return",
    "co_yield",
];

const PRIMITIVE_TYPES: &[&str] = &[
    "char", "int", "float", "double", "bool", "void", "short", "long", "signed", "unsigned",
];

const CONSTANTS: &[&str] = &["true", "false", "nullptr"];

impl Lexer for CppLexer {
    fn lex_line(&self, line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        let bytes = line.as_bytes();
        let len = bytes.len();
        let mut spans = Vec::new();
        let mut i = 0usize;

        if state == IN_BLOCK_COMMENT {
            match scan_block_comment_end(bytes, 0) {
                Some(end) => {
                    spans.push((0..end, Token::Comment));
                    i = end;
                }
                None => return (vec![(0..len, Token::Comment)], IN_BLOCK_COMMENT),
            }
        } else if state == IN_STRING {
            match scan_string_end(bytes, 0) {
                Some(end) => {
                    spans.push((0..end, Token::String));
                    i = end;
                }
                None => return (vec![(0..len, Token::String)], IN_STRING),
            }
        } else if let Some(range) = scan_preprocessor_directive(bytes) {
            spans.push((range.clone(), Token::Keyword));
            i = range.end;
        }

        while i < len {
            let byte = bytes[i];
            match byte {
                b'/' if bytes.get(i + 1) == Some(&b'/') => {
                    spans.push((i..len, Token::Comment));
                    i = len;
                }
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    let start = i;
                    match scan_block_comment_end(bytes, i + 2) {
                        Some(end) => {
                            spans.push((start..end, Token::Comment));
                            i = end;
                        }
                        None => {
                            spans.push((start..len, Token::Comment));
                            return (spans, IN_BLOCK_COMMENT);
                        }
                    }
                }
                b'"' => {
                    let start = i;
                    match scan_string_end(bytes, i + 1) {
                        Some(end) => {
                            spans.push((start..end, Token::String));
                            i = end;
                        }
                        None => {
                            spans.push((start..len, Token::String));
                            return (spans, IN_STRING);
                        }
                    }
                }
                b'\'' => {
                    let start = i;
                    match scan_char_literal_end(bytes, i + 1) {
                        Some(end) => {
                            spans.push((start..end, Token::String));
                            i = end;
                        }
                        None => {
                            spans.push((start..len, Token::String));
                            i = len;
                        }
                    }
                }
                b'[' if bytes.get(i + 1) == Some(&b'[') => {
                    let start = i;
                    i = scan_attribute(bytes, i);
                    spans.push((start..i, Token::Attribute));
                }
                b'0'..=b'9' => {
                    let start = i;
                    i = scan_number(bytes, i);
                    spans.push((start..i, Token::Number));
                }
                b'.' if bytes.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                    let start = i;
                    i = scan_number(bytes, i);
                    spans.push((start..i, Token::Number));
                }
                _ if is_ident_start(byte) => {
                    if let Some(quote_at) = parse_raw_string_prefix(bytes, i) {
                        let start = i;
                        match scan_raw_string_end(bytes, quote_at) {
                            Some(end) => {
                                spans.push((start..end, Token::String));
                                i = end;
                            }
                            None => {
                                // Unterminated on this line: fall back to a
                                // plain scan to the end, per the module
                                // docs — no cross-line state for raw
                                // strings.
                                spans.push((start..len, Token::String));
                                i = len;
                            }
                        }
                    } else {
                        let start = i;
                        i += 1;
                        while i < len && is_ident_continue(bytes[i]) {
                            i += 1;
                        }
                        let word = &line[start..i];
                        let mut token = classify_word(word, bytes, i);
                        // A name immediately followed by `::` reads as a
                        // namespace or class qualifier regardless of how it
                        // would otherwise have been classified (`std` is
                        // lowercase and would otherwise be a plain
                        // Variable).
                        if bytes.get(i) == Some(&b':') && bytes.get(i + 1) == Some(&b':') {
                            token = Token::Namespace;
                        }
                        spans.push((start..i, token));
                    }
                }
                b':' if bytes.get(i + 1) == Some(&b':') => {
                    spans.push((i..i + 2, Token::Punctuation));
                    i += 2;
                }
                b'-' if bytes.get(i + 1) == Some(&b'>') => {
                    spans.push((i..i + 2, Token::Operator));
                    i += 2;
                }
                b'+' | b'-' | b'*' | b'/' | b'%' | b'=' | b'<' | b'>' | b'!' | b'&' | b'|'
                | b'^' | b'~' | b'?' => {
                    spans.push((i..i + 1, Token::Operator));
                    i += 1;
                }
                b'(' | b')' | b'[' | b']' | b'{' | b'}' | b',' | b'.' | b';' | b':' => {
                    spans.push((i..i + 1, Token::Punctuation));
                    i += 1;
                }
                _ => i += 1,
            }
        }

        (spans, NORMAL)
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// A line starting with `#` (leading spaces/tabs allowed) is a preprocessor
/// directive; the range returned covers the `#` and the directive name
/// (`#include`, `#define`, `#ifdef`, `#pragma`, ...), which is all that gets
/// coloured — the rest of the line is still lexed as ordinary C++ so a
/// `#define FOO(x) ...` macro body's strings and comments still work.
fn scan_preprocessor_directive(bytes: &[u8]) -> Option<Range<usize>> {
    let len = bytes.len();
    let mut j = 0;
    while j < len && matches!(bytes[j], b' ' | b'\t') {
        j += 1;
    }
    if bytes.get(j) != Some(&b'#') {
        return None;
    }
    let start = j;
    j += 1;
    while j < len && matches!(bytes[j], b' ' | b'\t') {
        j += 1;
    }
    while j < len && is_ident_continue(bytes[j]) {
        j += 1;
    }
    Some(start..j)
}

/// Track an (unnested) `/* */` from `i`. `None` if it is still open at the
/// end of the line.
fn scan_block_comment_end(bytes: &[u8], mut i: usize) -> Option<usize> {
    let len = bytes.len();
    while i + 1 < len {
        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

/// From just after an opening `"` (or the start of a line already inside
/// one), the byte past a closing `"`. `None` if it is still open.
fn scan_string_end(bytes: &[u8], mut i: usize) -> Option<usize> {
    let len = bytes.len();
    while i < len {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// From just after an opening `'`, the byte past a closing `'`, handling
/// escapes. Unlike Rust, C++ has no lifetimes to disambiguate against, and
/// permits (rarely used) multi-character literals like `'ab'`, so this just
/// looks for the next unescaped `'`.
fn scan_char_literal_end(bytes: &[u8], mut i: usize) -> Option<usize> {
    let len = bytes.len();
    while i < len {
        match bytes[i] {
            b'\\' => i += 2,
            b'\'' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// A C++11 raw-string prefix (`R`, or an encoding prefix `u8`/`u`/`U`/`L`
/// immediately followed by `R`) directly followed by `"`, starting at `i`.
/// Returns the index of that opening `"`.
fn parse_raw_string_prefix(bytes: &[u8], i: usize) -> Option<usize> {
    let mut j = i;
    if bytes.get(j) == Some(&b'u') && bytes.get(j + 1) == Some(&b'8') {
        j += 2;
    } else if matches!(bytes.get(j), Some(b'u') | Some(b'U') | Some(b'L')) {
        j += 1;
    }
    if bytes.get(j) == Some(&b'R') && bytes.get(j + 1) == Some(&b'"') {
        return Some(j + 1);
    }
    None
}

/// From a raw string's opening `"` at `quote_at`, the byte past its closing
/// `"` — `R"delim(...)delim"`, where `delim` is whatever sits between the
/// quote and the first `(`. `None` if the delimiter's `(` or the matching
/// `)delim"` is not found on this line (see the module docs: this does not
/// carry state to the next one).
fn scan_raw_string_end(bytes: &[u8], quote_at: usize) -> Option<usize> {
    let len = bytes.len();
    let delim_start = quote_at + 1;
    let mut j = delim_start;
    while j < len && bytes[j] != b'(' {
        j += 1;
    }
    if j >= len {
        return None;
    }
    let delim = &bytes[delim_start..j];
    let mut k = j + 1;
    while k < len {
        if bytes[k] == b')' {
            let after = k + 1;
            if let Some(candidate) = bytes.get(after..after + delim.len()) {
                if candidate == delim && bytes.get(after + delim.len()) == Some(&b'"') {
                    return Some(after + delim.len() + 1);
                }
            }
        }
        k += 1;
    }
    None
}

/// A `[[attribute]]` (C++11), from its opening `[[` at `i` to the byte past
/// its closing `]]`, or to the end of the line if it is never closed.
fn scan_attribute(bytes: &[u8], mut i: usize) -> usize {
    let len = bytes.len();
    i += 2;
    while i < len {
        if bytes[i] == b']' && bytes.get(i + 1) == Some(&b']') {
            return i + 2;
        }
        i += 1;
    }
    len
}

/// A pragmatic number scan, `rust.rs`'s `scan_number` with C++14 digit
/// separators (`'`) added: everything alphanumeric after the first digit
/// (covers hex `0x…`, octal, and suffixes `u`/`U`/`l`/`L`/`f`/`F`), plus `.`
/// when followed by another digit, plus `'` as a separator between digits.
fn scan_number(bytes: &[u8], mut i: usize) -> usize {
    let len = bytes.len();
    i += 1;
    while i < len {
        let b = bytes[i];
        let is_number_char = b.is_ascii_alphanumeric()
            || b == b'\''
            || (b == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit));
        if !is_number_char {
            break;
        }
        i += 1;
    }
    i
}

fn classify_word(word: &str, bytes: &[u8], next: usize) -> Token {
    if PRIMITIVE_TYPES.contains(&word) {
        return Token::Type;
    }
    if CONSTANTS.contains(&word) {
        return Token::Constant;
    }
    if word == "this" {
        return Token::Variable;
    }
    if KEYWORDS.contains(&word) {
        return Token::Keyword;
    }
    // Checked before the capitalized-type heuristic below, same ordering
    // `rust.rs`'s `classify_word` uses for its call/macro heuristic: a
    // bareword directly followed by `(` reads as a call, even one that
    // happens to be capitalized (a constructor call, idiomatically).
    if matches!(bytes.get(next), Some(b'(')) {
        return Token::Function;
    }
    // Checked before the plain-capitalized case: `SCREAMING_CASE` also
    // starts with an uppercase letter, but means a constant (a `#define` or
    // enumerator), not a type.
    if word.chars().any(char::is_uppercase) && word.chars().all(|c| !c.is_lowercase()) {
        return Token::Constant;
    }
    if word.chars().next().is_some_and(char::is_uppercase) {
        return Token::Type;
    }
    Token::Variable
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        CppLexer.lex_line(line, state)
    }

    fn tokens(line: &str) -> Vec<(Range<usize>, Token)> {
        lex(line, LineState::INITIAL).0
    }

    fn find<'a>(spans: &'a [(Range<usize>, Token)], text: &str, line: &str) -> Option<&'a Token> {
        let at = line.find(text)?;
        spans
            .iter()
            .find(|(range, _)| *range == (at..at + text.len()))
            .map(|(_, token)| token)
    }

    #[test]
    fn line_comments_run_to_the_end_of_the_line() {
        let line = "int x = 1; // trailing note\n";
        let spans = tokens(line);
        let at = line.find("//").unwrap();
        assert!(spans.contains(&(at..line.len(), Token::Comment)));
    }

    #[test]
    fn a_block_comment_can_close_on_the_same_line() {
        let line = "int x = /* inline */ 1;\n";
        let spans = tokens(line);
        let at = line.find("/*").unwrap();
        let end = line.find("*/").unwrap() + 2;
        assert!(spans.contains(&(at..end, Token::Comment)));
        assert!(spans.iter().any(|(_, t)| *t == Token::Number));
    }

    #[test]
    fn a_block_comment_does_not_nest() {
        // C's rule, not Rust's: the first `*/` closes it, so `still` and
        // the trailing `*/` come back as ordinary code.
        let line = "/* outer /* inner */ still outer */\n";
        let (spans, state) = lex(line, LineState::INITIAL);
        let first_close = line.find("*/").unwrap() + 2;
        assert!(spans.contains(&(0..first_close, Token::Comment)));
        assert_eq!(state, NORMAL);
    }

    #[test]
    fn a_block_comment_spans_lines_and_resumes() {
        let (spans1, state) = lex("int x = /* start\n", LineState::INITIAL);
        assert!(spans1
            .iter()
            .any(|(r, t)| *t == Token::Comment && r.start == "int x = ".len()));
        assert_ne!(state, NORMAL, "still inside the comment");
        assert_eq!(state, IN_BLOCK_COMMENT);

        let (spans2, state2) = lex("still commented\n", state);
        assert_eq!(spans2, vec![(0.."still commented\n".len(), Token::Comment)]);

        let (spans3, state3) = lex("end */ int y = 2;\n", state2);
        let close = "end */".len();
        assert!(spans3.contains(&(0..close, Token::Comment)));
        assert_eq!(state3, NORMAL);
        assert!(spans3.iter().any(|(_, t)| *t == Token::Number));
    }

    #[test]
    fn a_preprocessor_directive_is_a_keyword() {
        let line = "#include <stdio.h>\n";
        let spans = tokens(line);
        assert!(spans.contains(&(0.."#include".len(), Token::Keyword)));
    }

    #[test]
    fn an_indented_directive_is_still_recognized() {
        let line = "  #  define FOO 1\n";
        let spans = tokens(line);
        let end = line.find("FOO").unwrap() - 1;
        let start = line.find('#').unwrap();
        assert_eq!(spans[0], (start..end, Token::Keyword));
    }

    #[test]
    fn a_plain_string_colours_as_a_string() {
        let line = "const char *s = \"hello\";\n";
        let spans = tokens(line);
        let start = line.find('"').unwrap();
        let end = line.rfind('"').unwrap() + 1;
        assert!(spans.contains(&(start..end, Token::String)));
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string() {
        let line = "char *s = \"a\\\"b\";\n";
        let spans = tokens(line);
        let start = line.find('"').unwrap();
        let end = line.rfind('"').unwrap() + 1;
        assert!(spans.contains(&(start..end, Token::String)));
    }

    #[test]
    fn an_unterminated_string_spans_lines_and_resumes() {
        let (spans1, state) = lex("char *s = \"still going\n", LineState::INITIAL);
        assert!(spans1.iter().any(|(_, t)| *t == Token::String));
        assert_eq!(state, IN_STRING);

        let (spans2, state2) = lex("closes here\";\n", state);
        let close = "closes here\"".len();
        assert!(spans2.contains(&(0..close, Token::String)));
        assert_eq!(state2, NORMAL);
    }

    #[test]
    fn a_char_literal_is_a_string() {
        let line = "char c = 'x'; char nl = '\\n';\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "'x'", line), Some(&Token::String));
        assert_eq!(find(&spans, "'\\n'", line), Some(&Token::String));
    }

    #[test]
    fn a_raw_string_ignores_backslashes_and_respects_its_delimiter() {
        let line = "auto s = R\"(a\\b)raw\"c)\";\n";
        // Delimiter is empty, so the string ends at the first `)"`.
        let simple = "auto s = R\"(a\\b)\";\n";
        let spans = tokens(simple);
        let start = simple.find("R\"").unwrap();
        let end = simple.find(")\"").unwrap() + 2;
        assert!(spans.contains(&(start..end, Token::String)));

        // A named delimiter: `)"` alone inside the body does not close it.
        let delimited = "auto s = R\"delim(a)\"b)delim\";\n";
        let spans2 = tokens(delimited);
        let dstart = delimited.find("R\"").unwrap();
        let dend = delimited.find(")delim\"").unwrap() + ")delim\"".len();
        assert!(spans2.contains(&(dstart..dend, Token::String)));
        let _ = line;
    }

    #[test]
    fn c_keywords_come_out_as_themselves() {
        let line = "for (int i = 0; i < 10; i++) { return; }\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "for", line), Some(&Token::Keyword));
        assert_eq!(find(&spans, "return", line), Some(&Token::Keyword));
        assert_eq!(find(&spans, "int", line), Some(&Token::Type));
    }

    #[test]
    fn cpp_only_keywords_are_recognized() {
        let line = "class Foo : public Bar { template<typename T> void f(); };\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "class", line), Some(&Token::Keyword));
        assert_eq!(find(&spans, "public", line), Some(&Token::Keyword));
        assert_eq!(find(&spans, "template", line), Some(&Token::Keyword));
        assert_eq!(find(&spans, "typename", line), Some(&Token::Keyword));
    }

    #[test]
    fn a_namespace_block_uses_the_keyword() {
        let line = "namespace app { }\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "namespace", line), Some(&Token::Keyword));
    }

    #[test]
    fn true_false_and_nullptr_are_constants_and_this_is_a_variable() {
        let line = "bool ok = true; bool bad = false; auto *p = nullptr; return this;\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "true", line), Some(&Token::Constant));
        assert_eq!(find(&spans, "false", line), Some(&Token::Constant));
        assert_eq!(find(&spans, "nullptr", line), Some(&Token::Constant));
        assert_eq!(find(&spans, "this", line), Some(&Token::Variable));
        assert_eq!(find(&spans, "bool", line), Some(&Token::Type));
    }

    #[test]
    fn scope_resolution_is_recognized_and_colours_the_namespace() {
        let line = "std::vector<int> v = std::move(other);\n";
        let spans = tokens(line);
        let at = line.find("::").unwrap();
        assert!(spans.contains(&(at..at + 2, Token::Punctuation)));
        assert_eq!(find(&spans, "std", line), Some(&Token::Namespace));
    }

    #[test]
    fn a_call_is_a_function() {
        let line = "int result = compute(a, b);\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "compute", line), Some(&Token::Function));
    }

    #[test]
    fn a_capitalized_bareword_not_called_is_a_type() {
        let line = "Widget w;\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "Widget", line), Some(&Token::Type));
    }

    #[test]
    fn numbers_include_hex_suffixes_and_digit_separators() {
        let line = "long a = 1'000'000L; int b = 0xFF; double c = 3.5f;\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "1'000'000L", line), Some(&Token::Number));
        assert_eq!(find(&spans, "0xFF", line), Some(&Token::Number));
        assert_eq!(find(&spans, "3.5f", line), Some(&Token::Number));
    }

    #[test]
    fn an_attribute_is_its_own_token() {
        let line = "[[nodiscard]] int f();\n";
        let spans = tokens(line);
        let end = line.find("]]").unwrap() + 2;
        assert!(spans.contains(&(0..end, Token::Attribute)));
    }

    #[test]
    fn broken_source_still_lexes_what_it_can() {
        let spans = tokens("int main( {\n    int x =\n");
        assert!(spans
            .iter()
            .any(|(_, t)| *t == Token::Keyword || *t == Token::Type));
    }

    #[test]
    fn multibyte_text_never_produces_a_span_that_is_not_at_a_char_boundary() {
        let line = "// naïve café → 日本語\nint main() {}\n";
        for chunk in line.split_inclusive('\n') {
            for (range, _) in tokens(chunk) {
                let _ = &chunk[range]; // panics if it split a character
            }
        }
    }

    #[test]
    fn spans_never_cross_the_lines_length() {
        let line = "const char *s = \"abc\";\n";
        for (range, _) in tokens(line) {
            assert!(range.end <= line.len());
        }
    }
}
