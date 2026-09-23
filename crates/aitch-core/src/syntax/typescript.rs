//! A hand-written TypeScript lexer: `.ts`/`.tsx` files are ordinary
//! JavaScript plus type annotations, interfaces, generics and a few other
//! TS-only bits, so this implements the full JS surface (line/block
//! comments, strings, template literals with `${...}` interpolation,
//! numbers, a regex-vs-division heuristic, keywords, and naming heuristics
//! for [`Token::Function`]/[`Token::Type`]/[`Token::Constant`]) plus the
//! TS-only additions layered on top of it.
//!
//! No syntax tree, so nothing here tracks brackets or nesting beyond what a
//! [`LineState`] can carry across one line boundary — see `syntax.rs`'s
//! module docs for what that trades away.
//!
//! Design calls, since several of these are genuinely ambiguous without a
//! real parser:
//!
//! - **Type annotations.** After a `:` this lexer expects the next bareword
//!   to be a type and colours it [`Token::Type`] even if it is not
//!   capitalized (`: myAlias`) — a real type checker would know for sure,
//!   this is a honest, one-token-lookahead simplification, matching what
//!   `syntax/typescript.rs`'s task notes call out explicitly. It does not
//!   attempt to colour the rest of a type expression (`Foo<Bar>[]`) beyond
//!   that first word; nested capitalized names inside it still pick up
//!   [`Token::Type`] via the ordinary capitalized-identifier heuristic
//!   below, so `Bar` still lands right without any extra bookkeeping.
//! - **Generics.** `<`/`>` are always [`Token::Operator`] — the same
//!   context-free ambiguity as regex-vs-division, but with a much worse
//!   failure mode if guessed wrong (a stray `<` swallowing the rest of the
//!   line looking for a `>` that is actually a less-than). Not worth it.
//! - **Template interpolation.** `${...}` is carved out of the surrounding
//!   template literal as its own region (mirrors `bash.rs`'s `$var`
//!   carving), but — like `bash.rs`'s choice for `$(...)` — its contents
//!   are left unstyled rather than fully tokenized as expression tokens,
//!   *except* for nested strings/comments/template literals, which this
//!   still has to scan past correctly to find the interpolation's matching
//!   `}` in the first place, so colouring them too is free. A nested
//!   template literal inside an interpolation does not get its own
//!   interpolation colouring (it is scanned as one opaque
//!   [`Token::String`]) and cannot span a line boundary — `LineState`'s 32
//!   bits track one level of "inside a template, and if so where inside
//!   it", not an unbounded nesting stack, so anything nested deeper than
//!   that degrades gracefully (colours what it can, then resumes the next
//!   line as if nothing were open) rather than trying to represent
//!   arbitrary nesting.
//! - **Regex vs. division.** A lightweight "does the token before this `/`
//!   look like it just finished a value" check, same problem class as the
//!   type/generic calls above.

use std::ops::Range;

use super::{Lexer, LineState, Token};

pub(crate) struct TypeScriptLexer;

// `LineState`'s bit layout: 1 means "inside a `/* */` block comment" (these
// do not nest in JS/TS, unlike Rust's). Otherwise bit 31 set means "inside a
// template literal"; bit 30 further set means "inside that template's
// `${...}` interpolation" (with the low 30 bits its brace-nesting depth,
// always >= 1 while set) rather than its plain text.
const BLOCK_COMMENT: LineState = LineState(1);
const TEMPLATE_FLAG: u32 = 1 << 31;
const EXPR_FLAG: u32 = 1 << 30;
const DEPTH_MASK: u32 = (1 << 30) - 1;

fn template_text_state() -> LineState {
    LineState(TEMPLATE_FLAG)
}

fn template_expr_state(depth: u32) -> LineState {
    LineState(TEMPLATE_FLAG | EXPR_FLAG | (depth & DEPTH_MASK))
}

const KEYWORDS: &[&str] = &[
    // JS.
    "var",
    "let",
    "const",
    "function",
    "return",
    "if",
    "else",
    "for",
    "while",
    "do",
    "switch",
    "case",
    "default",
    "break",
    "continue",
    "class",
    "extends",
    "super",
    "this",
    "new",
    "delete",
    "typeof",
    "instanceof",
    "in",
    "of",
    "try",
    "catch",
    "finally",
    "throw",
    "async",
    "await",
    "yield",
    "import",
    "export",
    "from",
    "static",
    "get",
    "set",
    "void",
    "null",
    "undefined",
    "true",
    "false",
    // TypeScript-only.
    "interface",
    "type",
    "enum",
    "implements",
    "private",
    "public",
    "protected",
    "readonly",
    "abstract",
    "declare",
    "namespace",
    "module",
    "as",
    "is",
    "keyof",
    "infer",
    "satisfies",
];

// Primitive type names that always read as `Token::Type`, regardless of
// position. `void`/`null`/`undefined` are not here: those three are also
// ordinary JS keywords/values, so they need `expect_type` context — see
// `classify_word`.
const PRIMITIVE_TYPES: &[&str] = &[
    "any", "unknown", "never", "boolean", "number", "string", "object", "bigint", "symbol",
];

impl Lexer for TypeScriptLexer {
    fn lex_line(&self, line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        let bytes = line.as_bytes();
        let len = bytes.len();
        let mut spans = Vec::new();
        let mut i = 0usize;
        let mut expect_value = true;

        if state == BLOCK_COMMENT {
            match scan_block_comment(bytes, 0) {
                Some(end) => {
                    spans.push((0..end, Token::Comment));
                    i = end;
                }
                None => return (vec![(0..len, Token::Comment)], BLOCK_COMMENT),
            }
        } else if state.0 & TEMPLATE_FLAG != 0 {
            let in_expr = state.0 & EXPR_FLAG != 0;
            let depth = state.0 & DEPTH_MASK;
            match scan_template(line, 0, 0, in_expr, depth, &mut spans) {
                Ok(end) => i = end,
                Err(next_state) => return (spans, next_state),
            }
            expect_value = false; // a template literal just closed: it's a value.
        }

        let mut expect_type = false;

        while i < len {
            let byte = bytes[i];
            match byte {
                b'/' if bytes.get(i + 1) == Some(&b'/') => {
                    spans.push((i..len, Token::Comment));
                    i = len;
                }
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    let start = i;
                    match scan_block_comment(bytes, i + 2) {
                        Some(end) => {
                            spans.push((start..end, Token::Comment));
                            i = end;
                        }
                        None => {
                            spans.push((start..len, Token::Comment));
                            return (spans, BLOCK_COMMENT);
                        }
                    }
                    expect_value = true;
                    expect_type = false;
                }
                b'/' if expect_value => {
                    let start = i;
                    match scan_regex(bytes, i) {
                        Some(end) => {
                            spans.push((start..end, Token::String));
                            i = end;
                        }
                        None => {
                            spans.push((start..start + 1, Token::Operator));
                            i += 1;
                        }
                    }
                    expect_value = false;
                    expect_type = false;
                }
                b'\'' | b'"' => {
                    let start = i;
                    let end = scan_string_body(bytes, i + 1, byte).unwrap_or(len);
                    spans.push((start..end, Token::String));
                    i = end;
                    expect_value = false;
                    expect_type = false;
                }
                b'`' => {
                    let start = i;
                    match scan_template(line, i + 1, start, false, 0, &mut spans) {
                        Ok(end) => i = end,
                        Err(next_state) => return (spans, next_state),
                    }
                    expect_value = false;
                    expect_type = false;
                }
                b'@' if bytes.get(i + 1).is_some_and(|&b| is_ident_start(b)) => {
                    let start = i;
                    i += 1;
                    while i < len && is_ident_continue(bytes[i]) {
                        i += 1;
                    }
                    spans.push((start..i, Token::Attribute));
                    expect_value = false;
                    expect_type = false;
                }
                b'0'..=b'9' => {
                    let start = i;
                    i = scan_number(bytes, i);
                    spans.push((start..i, Token::Number));
                    expect_value = false;
                    expect_type = false;
                }
                _ if is_ident_start(byte) => {
                    let start = i;
                    i += 1;
                    while i < len && is_ident_continue(bytes[i]) {
                        i += 1;
                    }
                    let word = &line[start..i];
                    let token = classify_word(word, bytes, i, expect_type);
                    if let Some(token) = token {
                        spans.push((start..i, token));
                    }
                    expect_value = !matches!(token, Some(Token::Keyword));
                    expect_type = word == "as";
                }
                b':' => {
                    spans.push((i..i + 1, Token::Punctuation));
                    i += 1;
                    expect_value = true;
                    expect_type = true;
                    continue;
                }
                b'+' | b'-' | b'*' | b'%' | b'=' | b'<' | b'>' | b'!' | b'&' | b'|' | b'^'
                | b'~' | b'?' => {
                    spans.push((i..i + 1, Token::Operator));
                    i += 1;
                    expect_value = true;
                    expect_type = false;
                }
                b')' | b']' => {
                    spans.push((i..i + 1, Token::Punctuation));
                    i += 1;
                    expect_value = false;
                    expect_type = false;
                }
                b'(' | b'[' | b'{' | b',' | b';' => {
                    spans.push((i..i + 1, Token::Punctuation));
                    i += 1;
                    expect_value = true;
                    expect_type = false;
                }
                b'}' => {
                    // Ambiguous without a parser: `}` can close a block
                    // (next `/` would start a statement, i.e. a regex is
                    // plausible) or an object/expression (next `/` would be
                    // division). Treated as the former, matching the more
                    // common case in real code.
                    spans.push((i..i + 1, Token::Punctuation));
                    i += 1;
                    expect_value = true;
                    expect_type = false;
                }
                b'.' => {
                    spans.push((i..i + 1, Token::Punctuation));
                    i += 1;
                    expect_value = true;
                    expect_type = false;
                }
                _ => i += 1,
            }
        }

        (spans, LineState::INITIAL)
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$' || b >= 0x80
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

/// Scans from just after `/*` for the end of a `/* */` comment. JS/TS block
/// comments do not nest — unlike `rust.rs`'s — so this just looks for the
/// first `*/`. `None` if it is still open at the end of the line.
fn scan_block_comment(bytes: &[u8], mut i: usize) -> Option<usize> {
    let len = bytes.len();
    while i < len {
        if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

/// Scans a single-quoted or double-quoted string body from just after its
/// opening quote. `'...'`/`"..."` strings are single-line in JS/TS (a
/// literal newline inside one is a syntax error), so unlike `rust.rs`'s
/// strings or this same file's template literals, an unterminated one does
/// not carry any [`LineState`] across the line boundary — the caller just
/// colours the rest of the line as the best guess and resumes fresh.
fn scan_string_body(bytes: &[u8], mut i: usize, quote: u8) -> Option<usize> {
    let len = bytes.len();
    while i < len {
        match bytes[i] {
            b'\\' if i + 1 < len => i += 2,
            b'\\' => i += 1,
            b if b == quote => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// Scans a nested template literal found while already inside a `${...}`
/// interpolation, from just after its opening backtick. Unlike the
/// top-level `scan_template`, this does not colour its own interpolation
/// (see the module doc's design note) and — like `scan_string_body` — is
/// single-line only.
fn scan_simple_template(bytes: &[u8], mut i: usize) -> Option<usize> {
    let len = bytes.len();
    while i < len {
        match bytes[i] {
            b'\\' if i + 1 < len => i += 2,
            b'\\' => i += 1,
            b'`' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// Scans a template literal, alternating between its plain-text regions and
/// any `${...}` interpolations inside it, resuming mid-construct if
/// `in_expr`/`depth` say so. `content_start` is where scanning resumes —
/// just past the opening backtick, or 0 when resuming a line that started
/// already inside one — and `span_start` is where the first emitted span
/// should begin — the backtick's own offset, or 0 when resuming (mirrors
/// `bash.rs`'s `scan_double_quoted_body` convention).
///
/// Returns the byte offset just past the closing backtick if the whole
/// literal closes on this line, or `Err` with the [`LineState`] to resume
/// with if it is still open at the end of the line.
fn scan_template(
    line: &str,
    mut content_start: usize,
    mut span_start: usize,
    mut in_expr: bool,
    mut depth: u32,
    spans: &mut Vec<(Range<usize>, Token)>,
) -> Result<usize, LineState> {
    let bytes = line.as_bytes();
    let len = bytes.len();
    loop {
        if !in_expr {
            let mut i = content_start;
            let text_start = span_start;
            loop {
                if i >= len {
                    if text_start < len {
                        spans.push((text_start..len, Token::String));
                    }
                    return Err(template_text_state());
                }
                match bytes[i] {
                    b'\\' if i + 1 < len => i += 2,
                    b'\\' => i += 1,
                    b'`' => {
                        spans.push((text_start..i + 1, Token::String));
                        return Ok(i + 1);
                    }
                    b'$' if bytes.get(i + 1) == Some(&b'{') => {
                        if text_start < i {
                            spans.push((text_start..i, Token::String));
                        }
                        spans.push((i..i + 2, Token::Punctuation));
                        content_start = i + 2;
                        in_expr = true;
                        depth = 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
        } else {
            let mut i = content_start;
            loop {
                if i >= len {
                    return Err(template_expr_state(depth));
                }
                match bytes[i] {
                    b'{' => {
                        spans.push((i..i + 1, Token::Punctuation));
                        depth += 1;
                        i += 1;
                    }
                    b'}' => {
                        depth -= 1;
                        spans.push((i..i + 1, Token::Punctuation));
                        i += 1;
                        if depth == 0 {
                            in_expr = false;
                            content_start = i;
                            span_start = i;
                            break;
                        }
                    }
                    b'`' => {
                        let start = i;
                        match scan_simple_template(bytes, i + 1) {
                            Some(end) => {
                                spans.push((start..end, Token::String));
                                i = end;
                            }
                            None => {
                                // See the module doc's design note: this
                                // lexer cannot represent "still inside a
                                // nested template inside this
                                // interpolation" in `LineState`, so it
                                // closes everything out rather than
                                // mis-resuming.
                                spans.push((start..len, Token::String));
                                return Err(LineState::INITIAL);
                            }
                        }
                    }
                    b'\'' | b'"' => {
                        let start = i;
                        let quote = bytes[i];
                        match scan_string_body(bytes, i + 1, quote) {
                            Some(end) => {
                                spans.push((start..end, Token::String));
                                i = end;
                            }
                            None => {
                                spans.push((start..len, Token::String));
                                return Err(LineState::INITIAL);
                            }
                        }
                    }
                    b'/' if bytes.get(i + 1) == Some(&b'/') => {
                        spans.push((i..len, Token::Comment));
                        i = len;
                    }
                    b'/' if bytes.get(i + 1) == Some(&b'*') => {
                        let start = i;
                        match scan_block_comment(bytes, i + 2) {
                            Some(end) => {
                                spans.push((start..end, Token::Comment));
                                i = end;
                            }
                            None => {
                                spans.push((start..len, Token::Comment));
                                return Err(LineState::INITIAL);
                            }
                        }
                    }
                    _ => i += 1,
                }
            }
        }
    }
}

/// A pragmatic number scan, mirroring `rust.rs`'s: everything alphanumeric
/// after the first digit (covers hex/octal/binary prefixes, exponents, and
/// the bigint `n` suffix), underscores as a digit separator, plus a `.`
/// when followed by another digit, and a `+`/`-` right after an `e`/`E` for
/// a signed exponent.
fn scan_number(bytes: &[u8], mut i: usize) -> usize {
    let len = bytes.len();
    i += 1;
    while i < len {
        let b = bytes[i];
        let is_number_char = b.is_ascii_alphanumeric()
            || b == b'_'
            || (b == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit))
            || (matches!(b, b'+' | b'-')
                && matches!(bytes.get(i.wrapping_sub(1)), Some(b'e') | Some(b'E')));
        if !is_number_char {
            break;
        }
        i += 1;
    }
    i
}

/// Scans a regex literal from its opening `/` (only called where the
/// preceding token makes one plausible — see `expect_value`). `None` if no
/// closing `/` is found on this line: regex literals cannot span a line
/// break in real JS/TS either, and rather than guessing wrong and
/// mis-colouring the rest of the line, this just falls back to treating the
/// `/` as division.
fn scan_regex(bytes: &[u8], start: usize) -> Option<usize> {
    let len = bytes.len();
    let mut i = start + 1;
    let mut in_class = false;
    while i < len {
        match bytes[i] {
            b'\\' if i + 1 < len => i += 2,
            b'[' => {
                in_class = true;
                i += 1;
            }
            b']' => {
                in_class = false;
                i += 1;
            }
            b'/' if !in_class => {
                i += 1;
                while i < len && bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
                return Some(i);
            }
            b'\r' | b'\n' => return None,
            _ => i += 1,
        }
    }
    None
}

fn classify_word(word: &str, bytes: &[u8], next: usize, expect_type: bool) -> Option<Token> {
    if KEYWORDS.contains(&word) {
        return Some(match word {
            "this" => Token::Variable,
            "true" | "false" => Token::Constant,
            "null" | "undefined" => {
                if expect_type {
                    Token::Type
                } else {
                    Token::Constant
                }
            }
            "void" => {
                if expect_type {
                    Token::Type
                } else {
                    Token::Keyword
                }
            }
            _ => Token::Keyword,
        });
    }
    if PRIMITIVE_TYPES.contains(&word) {
        return Some(Token::Type);
    }
    // SCREAMING_CASE reads as a constant, checked before the capitalized-type
    // case below since it also starts with an uppercase letter (mirrors
    // rust.rs's own PRIMITIVE_TYPES/constant split).
    if word.chars().any(char::is_uppercase) && word.chars().all(|c| !c.is_lowercase()) {
        return Some(Token::Constant);
    }
    if word.chars().next().is_some_and(char::is_uppercase) {
        return Some(Token::Type);
    }
    // Design call: a bareword directly after `:` is assumed to be a type,
    // even lowercase and even though this is not full type-expression
    // parsing — see the module doc's design note.
    if expect_type {
        return Some(Token::Type);
    }
    if bytes.get(next) == Some(&b'(') {
        return Some(Token::Function);
    }
    Some(Token::Variable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        TypeScriptLexer.lex_line(line, state)
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
        assert!(spans.iter().any(|(_, t)| *t == Token::Number));
    }

    #[test]
    fn a_block_comment_spans_lines_and_resumes() {
        let (spans1, state) = lex("let x = /* start\n", LineState::INITIAL);
        assert!(spans1
            .iter()
            .any(|(r, t)| *t == Token::Comment && r.start == "let x = ".len()));
        assert_ne!(state, LineState::INITIAL);

        let (spans2, state2) = lex("still commented\n", state);
        assert_eq!(spans2, vec![(0.."still commented\n".len(), Token::Comment)]);

        let (spans3, state3) = lex("end */ let y = 2;\n", state2);
        let close = "end */".len();
        assert!(spans3.contains(&(0..close, Token::Comment)));
        assert_eq!(state3, LineState::INITIAL);
        assert!(spans3.iter().any(|(_, t)| *t == Token::Number));
    }

    #[test]
    fn plain_strings_colour_as_strings() {
        let line = "let s = \"hello\"; let t = 'bye';\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "\"hello\"", line), Some(&Token::String));
        assert_eq!(find(&spans, "'bye'", line), Some(&Token::String));
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
    fn a_template_literal_spans_lines_and_resumes_with_interpolation() {
        let (spans1, state1) = lex("const s = `line one\n", LineState::INITIAL);
        assert!(spans1.iter().any(|(_, t)| *t == Token::String));
        assert_ne!(state1, LineState::INITIAL);

        let line2 = "line two ${x} more\n";
        let (spans2, state2) = lex(line2, state1);
        // The interpolation delimiters are their own punctuation spans, and
        // `x` inside them is left unstyled (see the module doc's design
        // note) rather than coloured as a variable.
        let open = line2.find("${").unwrap();
        assert!(spans2.contains(&(open..open + 2, Token::Punctuation)));
        let close = line2.find('}').unwrap();
        assert!(spans2.contains(&(close..close + 1, Token::Punctuation)));
        assert!(
            !spans2
                .iter()
                .any(|(r, t)| *t == Token::Variable && line2[r.clone()] == *"x"),
            "{spans2:?}: interpolation contents are left unstyled"
        );
        // Text either side of the interpolation is still String.
        assert!(spans2.contains(&(0..open, Token::String)));
        assert_ne!(state2, LineState::INITIAL, "still inside the template");

        let line3 = "line three`;\n";
        let (spans3, state3) = lex(line3, state2);
        let backtick = line3.find('`').unwrap();
        assert!(spans3.contains(&(0..backtick + 1, Token::String)));
        assert_eq!(state3, LineState::INITIAL);
        assert!(spans3.iter().any(|(_, t)| *t == Token::Punctuation));
    }

    #[test]
    fn a_single_line_template_literal_with_interpolation() {
        let line = "const s = `hi ${name}!`;\n";
        let spans = tokens(line);
        let open = line.find("${").unwrap();
        let close = line.find('}').unwrap();
        assert!(spans.contains(&(open..open + 2, Token::Punctuation)));
        assert!(spans.contains(&(close..close + 1, Token::Punctuation)));
        let backtick_start = line.find('`').unwrap();
        assert!(spans.contains(&(backtick_start..open, Token::String)));
        let last_backtick = line.rfind('`').unwrap();
        assert!(spans.contains(&(close + 1..last_backtick + 1, Token::String)));
    }

    #[test]
    fn js_keywords_come_out_as_keywords() {
        let line = "function f() { return; }\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "function", line), Some(&Token::Keyword));
        assert_eq!(find(&spans, "return", line), Some(&Token::Keyword));
    }

    #[test]
    fn this_null_and_true_are_special_cased() {
        let line = "if (this.x === null || true) {}\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "this", line), Some(&Token::Variable));
        assert_eq!(find(&spans, "null", line), Some(&Token::Constant));
        assert_eq!(find(&spans, "true", line), Some(&Token::Constant));
    }

    #[test]
    fn ts_only_keywords_are_keywords() {
        let line = "interface Foo { }\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "interface", line), Some(&Token::Keyword));

        let line2 = "type Bar = number;\n";
        assert_eq!(find(&tokens(line2), "type", line2), Some(&Token::Keyword));

        let line3 = "enum Baz { A, B }\n";
        assert_eq!(find(&tokens(line3), "enum", line3), Some(&Token::Keyword));
    }

    #[test]
    fn a_type_annotation_after_a_colon_is_a_type() {
        let line = "let x: myAlias;\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "myAlias", line), Some(&Token::Type));
    }

    #[test]
    fn primitive_type_names_are_types() {
        let line = "function f(a: string, b: number, c: boolean): void {}\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "string", line), Some(&Token::Type));
        assert_eq!(find(&spans, "number", line), Some(&Token::Type));
        assert_eq!(find(&spans, "boolean", line), Some(&Token::Type));
        assert_eq!(find(&spans, "void", line), Some(&Token::Type));
    }

    #[test]
    fn a_function_call_is_a_function() {
        let line = "doStuff(1, 2);\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "doStuff", line), Some(&Token::Function));
    }

    #[test]
    fn a_capitalized_name_is_a_type_not_a_function() {
        let line = "const x = new Widget();\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "Widget", line), Some(&Token::Type));
    }

    #[test]
    fn numbers_include_hex_underscores_and_exponents() {
        let line = "let a = 1_000; let b = 0xFF; let c = 3.5e-10;\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "1_000", line), Some(&Token::Number));
        assert_eq!(find(&spans, "0xFF", line), Some(&Token::Number));
        assert_eq!(find(&spans, "3.5e-10", line), Some(&Token::Number));
    }

    #[test]
    fn a_decorator_name_is_an_attribute() {
        let line = "@Component({ selector: 'app' })\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "@Component", line), Some(&Token::Attribute));
    }

    #[test]
    fn a_regex_literal_is_not_mistaken_for_division() {
        let line = "const re = /ab+c/g;\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "/ab+c/g", line), Some(&Token::String));
    }

    #[test]
    fn a_slash_after_a_value_is_division() {
        let line = "const x = a / b;\n";
        let spans = tokens(line);
        let at = line.find(" / ").unwrap() + 1;
        assert_eq!(
            spans
                .iter()
                .find(|(r, _)| *r == (at..at + 1))
                .map(|(_, t)| t),
            Some(&Token::Operator)
        );
    }

    #[test]
    fn broken_source_still_lexes_what_it_can() {
        let spans = tokens("function f( {\n    let x =\n");
        assert!(spans.iter().any(|(_, t)| *t == Token::Keyword));
    }

    #[test]
    fn spans_never_cross_the_lines_length() {
        let line = "let x: string = `hi ${1 + 2}`;\n";
        for (range, _) in tokens(line) {
            assert!(range.end <= line.len());
        }
    }

    #[test]
    fn multibyte_text_never_produces_a_span_that_is_not_at_a_char_boundary() {
        let line = "// naïve café → 日本語\nconst s = `héllo 日本語 ${x}`;\n";
        for chunk in line.split_inclusive('\n') {
            for (range, _) in tokens(chunk) {
                let _ = &chunk[range]; // panics if it split a character
            }
        }
    }
}
