//! A hand-written C lexer: line and (non-nesting) block comments,
//! preprocessor directives, strings, char literals, numbers, and a couple of
//! naming heuristics (`_t`/capitalized → [`Token::Type`], `name(` →
//! [`Token::Function`]) standing in for what a real parser and its symbol
//! table would tell us for free.
//!
//! No syntax tree, so nothing here tracks brackets or nesting beyond what a
//! [`LineState`] can carry across one line boundary — see `syntax.rs`'s
//! module docs for what that trades away. Unlike Rust's `/* */`, C's block
//! comments do not nest, so [`LineState`] only ever needs a flag for "inside
//! one", not a depth.

use std::ops::Range;

use super::{Lexer, LineState, Token};

pub(crate) struct CLexer;

// `LineState`'s bit layout for C: the low two bits are a small "what am I in
// the middle of" mode (normal, block comment, string, char literal — never
// more than one at once, so two bits is plenty), and bit 2 is a standalone
// flag for "the previous line was a preprocessor directive ending in `\`",
// which only ever applies when the mode is `NORMAL`.
const MODE_MASK: u32 = 0b11;
const NORMAL: u32 = 0;
const IN_BLOCK_COMMENT: u32 = 1;
const IN_STRING: u32 = 2;
const IN_CHAR: u32 = 3;
const DIRECTIVE_CONTINUATION: u32 = 1 << 2;

const KEYWORDS: &[&str] = &[
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
    "restrict",
    "return",
    "sizeof",
    "static",
    "struct",
    "switch",
    "typedef",
    "union",
    "volatile",
    "while",
    "_Complex",
    "_Imaginary",
];

// The primitive-type subset of the C keyword list, kept separate and mapped
// to `Token::Type` instead of `Token::Keyword` — the same split `rust.rs`
// makes between its control-flow keywords and `PRIMITIVE_TYPES`. `signed`
// and `unsigned` are grouped in here too: they modify a type, not control
// flow, so `Type` reads truer than `Keyword` for them.
const PRIMITIVE_TYPES: &[&str] = &[
    "char", "double", "float", "int", "long", "short", "signed", "unsigned", "void", "_Bool",
];

impl Lexer for CLexer {
    fn lex_line(&self, line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        let bytes = line.as_bytes();
        let len = bytes.len();
        let mut spans = Vec::new();
        let mut i = 0usize;

        let mode = state.0 & MODE_MASK;
        let directive_active = state.0 & DIRECTIVE_CONTINUATION != 0;

        match mode {
            IN_BLOCK_COMMENT => match scan_block_comment_end(bytes, 0) {
                Some(end) => {
                    spans.push((0..end, Token::Comment));
                    i = end;
                }
                None => return (vec![(0..len, Token::Comment)], LineState(IN_BLOCK_COMMENT)),
            },
            IN_STRING => match scan_quoted_body(bytes, 0, b'"') {
                Some(end) => {
                    spans.push((0..end, Token::String));
                    i = end;
                }
                None => return (vec![(0..len, Token::String)], LineState(IN_STRING)),
            },
            IN_CHAR => match scan_quoted_body(bytes, 0, b'\'') {
                Some(end) => {
                    spans.push((0..end, Token::String));
                    i = end;
                }
                None => return (vec![(0..len, Token::String)], LineState(IN_CHAR)),
            },
            _ => {}
        }

        let mut in_directive = directive_active;

        if i == 0 {
            let mut j = 0;
            while j < len && matches!(bytes[j], b' ' | b'\t') {
                j += 1;
            }
            if !directive_active && bytes.get(j) == Some(&b'#') {
                in_directive = true;
                let hash_pos = j;
                let mut k = j + 1;
                while k < len && matches!(bytes[k], b' ' | b'\t') {
                    k += 1;
                }
                let name_start = k;
                while k < len && bytes[k].is_ascii_alphabetic() {
                    k += 1;
                }
                let name_end = k;
                if name_end > name_start {
                    spans.push((hash_pos..name_end, Token::Keyword));
                } else {
                    spans.push((hash_pos..hash_pos + 1, Token::Keyword));
                }
                i = name_end.max(hash_pos + 1);

                if &line[name_start..name_end] == "include" {
                    let mut m = i;
                    while m < len && matches!(bytes[m], b' ' | b'\t') {
                        m += 1;
                    }
                    if bytes.get(m) == Some(&b'<') {
                        let start = m;
                        let mut e = m + 1;
                        while e < len && bytes[e] != b'>' {
                            e += 1;
                        }
                        let end = if e < len { e + 1 } else { len };
                        spans.push((start..end, Token::String));
                        i = end;
                    }
                    // A `"header.h"` form falls through to the ordinary
                    // string handling below.
                }
            }
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
                            return (spans, LineState(IN_BLOCK_COMMENT));
                        }
                    }
                }
                b'"' => {
                    let start = i;
                    match scan_quoted_body(bytes, i + 1, b'"') {
                        Some(end) => {
                            spans.push((start..end, Token::String));
                            i = end;
                        }
                        None => {
                            spans.push((start..len, Token::String));
                            return (spans, LineState(IN_STRING));
                        }
                    }
                }
                b'\'' => {
                    let start = i;
                    match scan_quoted_body(bytes, i + 1, b'\'') {
                        Some(end) => {
                            spans.push((start..end, Token::String));
                            i = end;
                        }
                        None => {
                            spans.push((start..len, Token::String));
                            return (spans, LineState(IN_CHAR));
                        }
                    }
                }
                b'0'..=b'9' => {
                    let start = i;
                    i = scan_number(bytes, i);
                    spans.push((start..i, Token::Number));
                }
                _ if is_ident_start(byte) => {
                    let start = i;
                    i += 1;
                    while i < len && is_ident_continue(bytes[i]) {
                        i += 1;
                    }
                    let word = &line[start..i];
                    spans.push((start..i, classify_word(word, bytes, i)));
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

        let next_state = if in_directive && line_ends_with_continuation(line) {
            LineState(DIRECTIVE_CONTINUATION)
        } else {
            LineState(NORMAL)
        };
        (spans, next_state)
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// From just after an opening quote (or the start of a line already inside
/// one), the byte past the matching closing quote. `None` if it is still
/// open at the end of the line. Shared by both `"strings"` and `'c'` char
/// literals — the escaping rule is the same, only the terminator differs.
fn scan_quoted_body(bytes: &[u8], mut i: usize, quote: u8) -> Option<usize> {
    let len = bytes.len();
    while i < len {
        match bytes[i] {
            b'\\' => i += 2,
            b if b == quote => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// From `i` (already past any opening `/*`), the byte past a `*/`. `None` if
/// it is still open at the end of the line. C block comments never nest, so
/// unlike `rust.rs`'s equivalent this needs no depth counter.
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

/// A pragmatic number scan: hex literals (`0x...`), and otherwise every
/// digit, at most one `.`, an optional signed exponent, and a trailing run of
/// suffix letters (`u`, `U`, `l`, `L`, `f`, `F` in any combination — this
/// does not validate that the combination is one a C compiler would accept,
/// only that it is highlighted as part of the same number).
fn scan_number(bytes: &[u8], start: usize) -> usize {
    let len = bytes.len();
    let mut i = start;

    if bytes[i] == b'0' && matches!(bytes.get(i + 1), Some(b'x') | Some(b'X')) {
        i += 2;
        while i < len && (bytes[i].is_ascii_hexdigit() || bytes[i] == b'_') {
            i += 1;
        }
        while i < len && matches!(bytes[i], b'u' | b'U' | b'l' | b'L') {
            i += 1;
        }
        return i;
    }

    i += 1;
    let mut seen_dot = false;
    let mut seen_exp = false;
    while i < len {
        match bytes[i] {
            b'0'..=b'9' => i += 1,
            b'.' if !seen_dot && !seen_exp => {
                seen_dot = true;
                i += 1;
            }
            b'e' | b'E' if !seen_exp => {
                seen_exp = true;
                i += 1;
                if i < len && matches!(bytes[i], b'+' | b'-') {
                    i += 1;
                }
            }
            _ => break,
        }
    }
    while i < len && matches!(bytes[i], b'u' | b'U' | b'l' | b'L' | b'f' | b'F') {
        i += 1;
    }
    i
}

/// Whether a directive continues onto the next physical line: real C splices
/// a `\` immediately followed by a line break before anything else even
/// looks at the source, and `#define`d macros lean on that constantly.
fn line_ends_with_continuation(line: &str) -> bool {
    let without_break = line
        .strip_suffix('\n')
        .map(|s| s.strip_suffix('\r').unwrap_or(s))
        .unwrap_or(line);
    without_break.ends_with('\\')
}

fn classify_word(word: &str, bytes: &[u8], next: usize) -> Token {
    if KEYWORDS.contains(&word) {
        return Token::Keyword;
    }
    if PRIMITIVE_TYPES.contains(&word) {
        return Token::Type;
    }
    // A capitalized identifier or one ending `_t` is a common-enough C
    // typedef convention (`FILE`, `size_t`) to be worth a heuristic, checked
    // before the call heuristic below so e.g. `FILE(` still reads as a type.
    let is_capitalized = word.chars().next().is_some_and(char::is_uppercase);
    if is_capitalized || word.ends_with("_t") {
        return Token::Type;
    }
    if bytes.get(next) == Some(&b'(') {
        return Token::Function;
    }
    Token::Variable
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        CLexer.lex_line(line, state)
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
        // Unlike Rust, a second `/*` inside an open comment means nothing:
        // the first `*/` closes it.
        let line = "/* outer /* still one comment */ int x;\n";
        let (spans, state) = lex(line, LineState::INITIAL);
        let end = line.find("*/").unwrap() + 2;
        assert!(spans.contains(&(0..end, Token::Comment)));
        assert_eq!(state, LineState::INITIAL);
        assert_eq!(find(&spans, "int", line), Some(&Token::Type));
    }

    #[test]
    fn a_block_comment_spans_lines_and_resumes() {
        let (spans1, state) = lex("int x = /* start\n", LineState::INITIAL);
        assert!(spans1
            .iter()
            .any(|(r, t)| *t == Token::Comment && r.start == "int x = ".len()));
        assert_ne!(state, LineState::INITIAL, "still inside the comment");

        let (spans2, state2) = lex("still commented\n", state);
        assert_eq!(spans2, vec![(0.."still commented\n".len(), Token::Comment)]);

        let (spans3, state3) = lex("end */ int y = 2;\n", state2);
        let close = "end */".len();
        assert!(spans3.contains(&(0..close, Token::Comment)));
        assert_eq!(state3, LineState::INITIAL);
        assert!(spans3.iter().any(|(_, t)| *t == Token::Number));
    }

    #[test]
    fn a_preprocessor_directive_colours_its_name_as_a_keyword() {
        let line = "#define MAX 100\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "#define", line), Some(&Token::Keyword));
        assert!(spans.iter().any(|(_, t)| *t == Token::Number));
    }

    #[test]
    fn an_angle_include_header_reads_as_a_string() {
        let line = "#include <stdio.h>\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "#include", line), Some(&Token::Keyword));
        let start = line.find('<').unwrap();
        let end = line.find('>').unwrap() + 1;
        assert!(spans.contains(&(start..end, Token::String)));
    }

    #[test]
    fn a_quoted_include_header_is_an_ordinary_string() {
        let line = "#include \"local.h\"\n";
        let spans = tokens(line);
        let start = line.find('"').unwrap();
        let end = line.rfind('"').unwrap() + 1;
        assert!(spans.contains(&(start..end, Token::String)));
    }

    #[test]
    fn indented_directives_are_still_recognized() {
        let line = "  #ifdef DEBUG\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "#ifdef", line), Some(&Token::Keyword));
    }

    #[test]
    fn a_plain_string_colours_as_a_string() {
        let line = "char *s = \"hello\";\n";
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
    fn char_literals_are_strings_including_escapes() {
        let line = "char c = 'x'; char nl = '\\n'; char z = '\\0';\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "'x'", line), Some(&Token::String));
        assert_eq!(find(&spans, "'\\n'", line), Some(&Token::String));
        assert_eq!(find(&spans, "'\\0'", line), Some(&Token::String));
    }

    #[test]
    fn an_unterminated_string_spans_lines_and_resumes() {
        let (spans1, state) = lex("char *s = \"still going\n", LineState::INITIAL);
        assert!(spans1.iter().any(|(_, t)| *t == Token::String));
        assert_ne!(state, LineState::INITIAL);

        let (spans2, state2) = lex("closes here\";\n", state);
        let close = "closes here\"".len();
        assert!(spans2.contains(&(0..close, Token::String)));
        assert_eq!(state2, LineState::INITIAL);
    }

    #[test]
    fn keywords_come_out_as_themselves() {
        let line = "for (;;) { return; }\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "for", line), Some(&Token::Keyword));
        assert_eq!(find(&spans, "return", line), Some(&Token::Keyword));
    }

    #[test]
    fn primitive_types_are_told_apart_from_control_keywords() {
        let line = "unsigned long int x;\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "unsigned", line), Some(&Token::Type));
        assert_eq!(find(&spans, "long", line), Some(&Token::Type));
        assert_eq!(find(&spans, "int", line), Some(&Token::Type));
    }

    #[test]
    fn a_t_suffix_or_capitalized_name_is_a_type() {
        let line = "size_t len; FILE *f;\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "size_t", line), Some(&Token::Type));
        assert_eq!(find(&spans, "FILE", line), Some(&Token::Type));
    }

    #[test]
    fn a_bareword_call_is_a_function() {
        let line = "int total = compute(a, b);\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "compute", line), Some(&Token::Function));
    }

    #[test]
    fn numbers_include_hex_octal_float_and_suffixes() {
        let line = "int a = 0x1F; int b = 010; double c = 3.5f; long d = 100UL;\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "0x1F", line), Some(&Token::Number));
        assert_eq!(find(&spans, "010", line), Some(&Token::Number));
        assert_eq!(find(&spans, "3.5f", line), Some(&Token::Number));
        assert_eq!(find(&spans, "100UL", line), Some(&Token::Number));
    }

    #[test]
    fn broken_source_still_lexes_what_it_can() {
        let spans = tokens("if (x( {\n    int y =\n");
        assert!(spans.iter().any(|(_, t)| *t == Token::Keyword));
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
        let line = "char *s = \"abc\";\n";
        for (range, _) in tokens(line) {
            assert!(range.end <= line.len());
        }
    }
}
