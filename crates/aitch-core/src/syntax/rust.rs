//! A hand-written Rust lexer: line comments, nested block comments, strings
//! (plain, raw, byte, C), char literals vs. lifetimes, numbers, attributes,
//! and a handful of naming heuristics (capitalized → [`Token::Type`],
//! `SCREAMING_CASE` → [`Token::Constant`], `name(`/`name!` → [`Token::Function`])
//! standing in for what a real type checker would tell a parser for free.
//!
//! No syntax tree, so nothing here tracks brackets or nesting beyond what a
//! [`LineState`] can carry across one line boundary — see `syntax.rs`'s
//! module docs for what that trades away.

use std::ops::Range;

use super::{Lexer, LineState, Token};

pub(crate) struct RustLexer;

// `LineState`'s bit layout for Rust: bit 31 set means "inside a string",
// with bit 30 as "it's a raw string" and the low byte as its `#` count (0
// for a non-raw string). Otherwise the whole value is a block-comment
// nesting depth (0 meaning "not in one").
const STRING_FLAG: u32 = 1 << 31;
const RAW_FLAG: u32 = 1 << 30;
const HASHES_MASK: u32 = 0xFF;

fn string_state(raw: bool, hashes: usize) -> LineState {
    let mut value = STRING_FLAG | (hashes as u32 & HASHES_MASK);
    if raw {
        value |= RAW_FLAG;
    }
    LineState(value)
}

const KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "static", "struct", "super", "trait", "type", "unsafe", "use", "where", "while", "async",
    "await", "abstract", "become", "box", "do", "final", "macro", "override", "priv", "typeof",
    "unsized", "virtual", "yield", "try", "union", "self", "Self", "true", "false",
];

const PRIMITIVE_TYPES: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32",
    "f64", "bool", "char", "str",
];

impl Lexer for RustLexer {
    fn lex_line(&self, line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        let bytes = line.as_bytes();
        let len = bytes.len();
        let mut spans = Vec::new();
        let mut i = 0usize;

        if state.0 & STRING_FLAG != 0 {
            let raw = state.0 & RAW_FLAG != 0;
            let hashes = (state.0 & HASHES_MASK) as usize;
            match scan_string_body(bytes, 0, raw, hashes) {
                Some(end) => {
                    spans.push((0..end, Token::String));
                    i = end;
                }
                None => return (vec![(0..len, Token::String)], state),
            }
        } else if state.0 != 0 {
            let mut depth = state.0;
            match scan_block_comment(bytes, 0, &mut depth) {
                Some(end) => {
                    spans.push((0..end, Token::Comment));
                    i = end;
                }
                None => return (vec![(0..len, Token::Comment)], LineState(depth)),
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
                    let mut depth = 1u32;
                    match scan_block_comment(bytes, i + 2, &mut depth) {
                        Some(end) => {
                            spans.push((start..end, Token::Comment));
                            i = end;
                        }
                        None => {
                            spans.push((start..len, Token::Comment));
                            return (spans, LineState(depth));
                        }
                    }
                }
                b'"' => {
                    let start = i;
                    match scan_string_body(bytes, i + 1, false, 0) {
                        Some(end) => {
                            spans.push((start..end, Token::String));
                            i = end;
                        }
                        None => {
                            spans.push((start..len, Token::String));
                            return (spans, string_state(false, 0));
                        }
                    }
                }
                b'\'' => match scan_char_literal(line, i) {
                    Some(end) => {
                        spans.push((i..end, Token::String));
                        i = end;
                    }
                    None => {
                        let start = i;
                        i += 1;
                        while i < len && is_ident_continue(bytes[i]) {
                            i += 1;
                        }
                        spans.push((start..i, Token::Variable));
                    }
                },
                b'#' => {
                    let start = i;
                    i += 1;
                    if bytes.get(i) == Some(&b'!') {
                        i += 1;
                    }
                    if bytes.get(i) == Some(&b'[') {
                        let mut depth = 0i32;
                        while i < len {
                            match bytes[i] {
                                b'[' => depth += 1,
                                b']' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        i += 1;
                                        break;
                                    }
                                }
                                _ => {}
                            }
                            i += 1;
                        }
                        spans.push((start..i, Token::Attribute));
                    }
                }
                b'0'..=b'9' => {
                    let start = i;
                    i = scan_number(bytes, i);
                    spans.push((start..i, Token::Number));
                }
                _ if is_ident_start(byte) => {
                    if let Some((raw, hashes, quote_at)) = parse_string_prefix(bytes, i) {
                        let start = i;
                        match scan_string_body(bytes, quote_at + 1, raw, hashes) {
                            Some(end) => {
                                spans.push((start..end, Token::String));
                                i = end;
                            }
                            None => {
                                spans.push((start..len, Token::String));
                                return (spans, string_state(raw, hashes));
                            }
                        }
                    } else {
                        let start = i;
                        i += 1;
                        while i < len && is_ident_continue(bytes[i]) {
                            i += 1;
                        }
                        if let Some(token) = classify_word(&line[start..i], bytes, i) {
                            spans.push((start..i, token));
                        }
                    }
                }
                b'+' | b'-' | b'*' | b'/' | b'%' | b'=' | b'<' | b'>' | b'!' | b'&' | b'|'
                | b'^' | b'~' | b'@' | b'?' => {
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

        (spans, LineState::INITIAL)
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Look for a string-literal prefix (`r`, `b`, `c`, `br`, `cr`, with any
/// number of `#` after an `r`) starting at `i`. Returns whether it is raw,
/// how many `#` it carries, and where its opening `"` sits.
fn parse_string_prefix(bytes: &[u8], i: usize) -> Option<(bool, usize, usize)> {
    let len = bytes.len();
    let mut j = i;
    let saw_bc = matches!(bytes.get(j), Some(b'b') | Some(b'c'));
    if saw_bc {
        j += 1;
    }
    if bytes.get(j) == Some(&b'r') {
        j += 1;
        let mut hashes = 0usize;
        while j < len && bytes[j] == b'#' {
            hashes += 1;
            j += 1;
        }
        if bytes.get(j) == Some(&b'"') {
            return Some((true, hashes, j));
        }
        return None;
    }
    if saw_bc && bytes.get(j) == Some(&b'"') {
        return Some((false, 0, j));
    }
    None
}

/// Scan from just after the opening `"` (or from the start of a line already
/// inside one) for the end of a string body. `None` means it is still open
/// at the end of the line.
fn scan_string_body(bytes: &[u8], mut i: usize, raw: bool, hashes: usize) -> Option<usize> {
    let len = bytes.len();
    while i < len {
        match bytes[i] {
            b'\\' if !raw => i += 2,
            b'"' => {
                if !raw {
                    return Some(i + 1);
                }
                let mut j = i + 1;
                let mut count = 0;
                while j < len && bytes[j] == b'#' && count < hashes {
                    j += 1;
                    count += 1;
                }
                if count == hashes {
                    return Some(j);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// Track nested `/* */` from `i`, `depth` already open. `None` and an
/// updated `depth` if it is still open at the end of the line.
fn scan_block_comment(bytes: &[u8], mut i: usize, depth: &mut u32) -> Option<usize> {
    let len = bytes.len();
    while i < len {
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            *depth += 1;
            i += 2;
        } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
            *depth -= 1;
            i += 2;
            if *depth == 0 {
                return Some(i);
            }
        } else {
            i += 1;
        }
    }
    None
}

/// A char literal, distinguished from a lifetime by having exactly one
/// (possibly escaped) character before a closing `'`.
fn scan_char_literal(line: &str, quote_at: usize) -> Option<usize> {
    let rest = line.get(quote_at + 1..)?;
    if let Some(after_backslash) = rest.strip_prefix('\\') {
        if let Some(after_u) = after_backslash.strip_prefix("u{") {
            let brace = after_u.find('}')?;
            let after_brace = after_u.get(brace + 1..)?;
            after_brace.strip_prefix('\'')?;
            return Some(quote_at + 1 + 1 + 2 + brace + 1 + 1);
        }
        let escaped = after_backslash.chars().next()?;
        let after_escaped = after_backslash.get(escaped.len_utf8()..)?;
        after_escaped.strip_prefix('\'')?;
        return Some(quote_at + 1 + 1 + escaped.len_utf8() + 1);
    }
    let c = rest.chars().next()?;
    if c == '\'' {
        return None;
    }
    let after = rest.get(c.len_utf8()..)?;
    after.strip_prefix('\'')?;
    Some(quote_at + 1 + c.len_utf8() + 1)
}

/// A pragmatic number scan: everything alphanumeric-or-`_` after the first
/// digit, plus a `.` when it is followed by another digit (so `1.5` is one
/// number but `1..5` and `1.to_string()` are not). Does not special-case a
/// signed exponent (`1e-5` comes back as `1e` then `-` then `5`) — a real
/// parser's precision for one extra byte class was not worth it here.
fn scan_number(bytes: &[u8], mut i: usize) -> usize {
    let len = bytes.len();
    i += 1;
    while i < len {
        let b = bytes[i];
        let is_number_char = b.is_ascii_alphanumeric()
            || b == b'_'
            || (b == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit));
        if !is_number_char {
            break;
        }
        i += 1;
    }
    i
}

fn classify_word(word: &str, bytes: &[u8], next: usize) -> Option<Token> {
    if KEYWORDS.contains(&word) {
        return Some(match word {
            "self" => Token::Variable,
            "Self" => Token::Type,
            "true" | "false" => Token::Constant,
            _ => Token::Keyword,
        });
    }
    if PRIMITIVE_TYPES.contains(&word) {
        return Some(Token::Type);
    }
    if word == "_" {
        return None;
    }
    // Checked before the plain-capitalized case below: `SCREAMING_CASE`
    // also starts with an uppercase letter, but means a constant, not a
    // type.
    if word.chars().any(char::is_uppercase) && word.chars().all(|c| !c.is_lowercase()) {
        return Some(Token::Constant);
    }
    let first = word.chars().next()?;
    // Checked before the call/macro heuristic below: a capitalized name
    // directly followed by `(` is idiomatically a tuple-struct or enum-
    // variant constructor (`Some(x)`, `Point(1, 2)`), never a plain
    // function — those are snake_case by convention.
    if first.is_uppercase() {
        return Some(Token::Type);
    }
    if matches!(bytes.get(next), Some(b'!') | Some(b'(')) {
        return Some(Token::Function);
    }
    Some(Token::Variable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        RustLexer.lex_line(line, state)
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
    fn keywords_come_out_as_themselves() {
        let spans = tokens("fn main() {}\n");
        assert_eq!(find(&spans, "fn", "fn main() {}\n"), Some(&Token::Keyword));
    }

    #[test]
    fn self_and_true_are_not_plain_keywords() {
        let line = "fn f(self) { true }\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "self", line), Some(&Token::Variable));
        assert_eq!(find(&spans, "true", line), Some(&Token::Constant));
    }

    #[test]
    fn capitalized_words_are_types() {
        let line = "let x: Option<String> = None;\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "Option", line), Some(&Token::Type));
        assert_eq!(find(&spans, "String", line), Some(&Token::Type));
        assert_eq!(find(&spans, "None", line), Some(&Token::Type));
    }

    #[test]
    fn screaming_case_is_a_constant() {
        let line = "const MAX_LEN: usize = 10;\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "MAX_LEN", line), Some(&Token::Constant));
        assert_eq!(find(&spans, "usize", line), Some(&Token::Type));
    }

    #[test]
    fn a_call_and_a_macro_are_functions() {
        let line = "println!(\"{}\", go());\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "println", line), Some(&Token::Function));
        assert_eq!(find(&spans, "go", line), Some(&Token::Function));
    }

    #[test]
    fn line_comments_run_to_the_end_of_the_line() {
        let line = "let x = 1; // trailing note\n";
        let spans = tokens(line);
        let at = line.find("//").unwrap();
        assert!(spans.contains(&(at..line.len(), Token::Comment)));
    }

    #[test]
    fn a_block_comment_can_close_on_the_same_line() {
        let line = "let x = /* inline */ 1;\n";
        let spans = tokens(line);
        let at = line.find("/*").unwrap();
        let end = line.find("*/").unwrap() + 2;
        assert!(spans.contains(&(at..end, Token::Comment)));
        assert!(
            spans.iter().any(|(_, t)| *t == Token::Number),
            "the 1 after the comment should still be lexed"
        );
    }

    #[test]
    fn a_block_comment_spans_lines_and_resumes() {
        let (spans1, state) = lex("let x = /* start\n", LineState::INITIAL);
        assert!(spans1
            .iter()
            .any(|(r, t)| *t == Token::Comment && r.start == "let x = ".len()));
        assert_ne!(state, LineState::INITIAL, "still inside the comment");

        let (spans2, state2) = lex("still commented\n", state);
        assert_eq!(spans2, vec![(0.."still commented\n".len(), Token::Comment)]);

        let (spans3, state3) = lex("end */ let y = 2;\n", state2);
        let close = "end */".len();
        assert!(spans3.contains(&(0..close, Token::Comment)));
        assert_eq!(state3, LineState::INITIAL);
        assert!(spans3.iter().any(|(_, t)| *t == Token::Number));
    }

    #[test]
    fn nested_block_comments_only_close_at_the_matching_depth() {
        let line = "/* outer /* inner */ still outer */\n";
        let (spans, state) = lex(line, LineState::INITIAL);
        let end = line.rfind("*/").unwrap() + 2;
        assert!(spans.contains(&(0..end, Token::Comment)));
        assert_eq!(state, LineState::INITIAL);
    }

    #[test]
    fn a_plain_string_colours_as_a_string() {
        let line = "let s = \"hello\";\n";
        let spans = tokens(line);
        let start = line.find('"').unwrap();
        let end = line.rfind('"').unwrap() + 1;
        assert!(spans.contains(&(start..end, Token::String)));
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string() {
        let line = "let s = \"a\\\"b\";\n";
        let spans = tokens(line);
        let start = line.find('"').unwrap();
        let end = line.rfind('"').unwrap() + 1;
        assert!(spans.contains(&(start..end, Token::String)));
    }

    #[test]
    fn an_unterminated_string_spans_lines_and_resumes() {
        let (spans1, state) = lex("let s = \"still going\n", LineState::INITIAL);
        assert!(spans1.iter().any(|(_, t)| *t == Token::String));
        assert_ne!(state, LineState::INITIAL);

        let (spans2, state2) = lex("closes here\";\n", state);
        let close = "closes here\"".len();
        assert!(spans2.contains(&(0..close, Token::String)));
        assert_eq!(state2, LineState::INITIAL);
    }

    #[test]
    fn a_raw_string_ignores_backslashes_and_counts_its_hashes() {
        let line = "let s = r#\"a\\b\"c\"#;\n";
        let spans = tokens(line);
        let start = line.find("r#\"").unwrap();
        let end = line.find("\"#;").unwrap() + 2;
        assert!(spans.contains(&(start..end, Token::String)));
    }

    #[test]
    fn a_char_literal_is_a_string_and_a_lifetime_is_a_variable() {
        let line = "fn f<'a>(c: char) { let x = 'x'; let y = '\\n'; }\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "'a", line), Some(&Token::Variable));
        assert_eq!(find(&spans, "'x'", line), Some(&Token::String));
        assert_eq!(find(&spans, "'\\n'", line), Some(&Token::String));
    }

    #[test]
    fn numbers_include_suffixes_hex_and_underscores() {
        let line = "let a = 1_000u32; let b = 0xFF; let c = 3.5f64;\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "1_000u32", line), Some(&Token::Number));
        assert_eq!(find(&spans, "0xFF", line), Some(&Token::Number));
        assert_eq!(find(&spans, "3.5f64", line), Some(&Token::Number));
    }

    #[test]
    fn a_range_does_not_get_swallowed_into_a_float() {
        let line = "let r = 1..5;\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "1", line), Some(&Token::Number));
        assert_eq!(find(&spans, "5", line), Some(&Token::Number));
    }

    #[test]
    fn an_attribute_is_its_own_token() {
        let line = "#[derive(Debug)]\n";
        let spans = tokens(line);
        let end = line.find(']').unwrap() + 1;
        assert!(spans.contains(&(0..end, Token::Attribute)));
    }

    #[test]
    fn broken_source_still_lexes_what_it_can() {
        let spans = tokens("fn main( {\n    let x =\n");
        assert!(spans.iter().any(|(_, t)| *t == Token::Keyword));
    }

    #[test]
    fn multibyte_text_never_produces_a_span_that_is_not_at_a_char_boundary() {
        let line = "// naïve café → 日本語\nfn main() {}\n";
        for chunk in line.split_inclusive('\n') {
            for (range, _) in tokens(chunk) {
                let _ = &chunk[range]; // panics if it split a character
            }
        }
    }

    #[test]
    fn spans_never_cross_the_lines_length() {
        let line = "let x = \"abc\";\n";
        for (range, _) in tokens(line) {
            assert!(range.end <= line.len());
        }
    }
}
