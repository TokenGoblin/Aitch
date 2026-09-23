//! A hand-written CSS lexer.
//!
//! CSS has exactly one comment form (`/* ... */`, non-nesting — there is no
//! `//` line comment) and, because it has no expressions, its "highlighting"
//! is really about *position*: the same bareword means a wildly different
//! thing depending on whether it sits in a selector or inside a declaration
//! block. This lexer tracks that with a single "are we inside `{ }`" flag
//! carried in [`LineState`] (bit 0) rather than a full brace-nesting depth —
//! at-rule blocks (`@media { ... }`) therefore toggle the same flag as a
//! plain rule's braces do, which is indistinguishable from a rule directly
//! at the top level. That is a deliberate simplification (documented in the
//! Wave 2 task notes): CSS blocks never meaningfully nest more than one
//! level deep for the purposes of "am I looking at a selector or a
//! declaration", so a flag is enough and a depth counter would buy nothing
//! a real parser wouldn't already need to spend on.
//!
//! Outside braces (selector position): a bare word is an element selector
//! ([`Token::Type`]), `.name` a class ([`Token::Property`]), `#name` an id
//! ([`Token::Constant`]), `:hover`/`::before` a pseudo-class/element
//! ([`Token::Keyword`]).
//!
//! Inside braces (declaration position): a word immediately followed by
//! (optionally-spaced) `:` is a property ([`Token::Property`]); otherwise it
//! is a value — a known keyword, a named colour ([`Token::Constant`]), a
//! `--custom-property` reference ([`Token::Variable`]), or, followed
//! directly by `(`, a function call's name ([`Token::Function`]). Numbers
//! carry their unit in the same span (`10px`, `1.5em`, `100%`); hex colours
//! (`#fff`) are [`Token::Constant`], same as an id selector — a unique
//! identifier reads closer to a constant than a property in both cases.
//!
//! Cross-line state (bits 31/30/29 of [`LineState`]) covers a block comment
//! or a string left open at the end of a line, mirroring `rust.rs`.

use std::ops::Range;

use super::{Lexer, LineState, Token};

pub(crate) struct CssLexer;

// `LineState` bit layout: bit 0 is "currently inside a `{ }` block" (see the
// module docs for why this is a flag, not a depth). Bits 31/30/29 carry
// cross-line comment/string state, the same way `rust.rs` does, and never
// overlap with bit 0 since they are combined with `|`.
const IN_BRACES: u32 = 1 << 0;
const IN_STRING: u32 = 1 << 31;
const STRING_SINGLE: u32 = 1 << 30;
const IN_COMMENT: u32 = 1 << 29;

fn brace_bit(in_braces: bool) -> u32 {
    if in_braces {
        IN_BRACES
    } else {
        0
    }
}

const KEYWORDS: &[&str] = &[
    "inherit",
    "initial",
    "unset",
    "revert",
    "none",
    "auto",
    "normal",
    "bold",
    "bolder",
    "lighter",
    "italic",
    "oblique",
    "underline",
    "overline",
    "line-through",
    "uppercase",
    "lowercase",
    "capitalize",
    "solid",
    "dashed",
    "dotted",
    "double",
    "groove",
    "ridge",
    "inset",
    "outset",
    "hidden",
    "visible",
    "collapse",
    "block",
    "inline",
    "inline-block",
    "flex",
    "inline-flex",
    "grid",
    "inline-grid",
    "table",
    "contents",
    "absolute",
    "relative",
    "fixed",
    "static",
    "sticky",
    "left",
    "right",
    "center",
    "top",
    "bottom",
    "middle",
    "baseline",
    "wrap",
    "nowrap",
    "wrap-reverse",
    "row",
    "row-reverse",
    "column",
    "column-reverse",
    "pointer",
    "default",
    "not-allowed",
    "crosshair",
    "text",
    "move",
    "border-box",
    "content-box",
    "cover",
    "contain",
    "no-repeat",
    "repeat",
    "repeat-x",
    "repeat-y",
    "space-between",
    "space-around",
    "space-evenly",
    "flex-start",
    "flex-end",
    "stretch",
    "ease",
    "ease-in",
    "ease-out",
    "ease-in-out",
    "linear",
    "step-start",
    "step-end",
    "forwards",
    "backwards",
    "infinite",
    "alternate",
    "both",
    "smooth",
    "scroll",
    "local",
];

const NAMED_COLORS: &[&str] = &[
    "red",
    "blue",
    "green",
    "black",
    "white",
    "gray",
    "grey",
    "yellow",
    "orange",
    "purple",
    "pink",
    "brown",
    "cyan",
    "magenta",
    "transparent",
    "currentColor",
    "currentcolor",
];

impl Lexer for CssLexer {
    fn lex_line(&self, line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        let bytes = line.as_bytes();
        let len = bytes.len();
        let mut spans = Vec::new();
        let mut i = 0usize;
        let mut in_braces = state.0 & IN_BRACES != 0;

        if state.0 & IN_STRING != 0 {
            let quote = if state.0 & STRING_SINGLE != 0 {
                b'\''
            } else {
                b'"'
            };
            match scan_string_body(bytes, 0, quote) {
                Some(end) => {
                    spans.push((0..end, Token::String));
                    i = end;
                }
                None => return (vec![(0..len, Token::String)], state),
            }
        } else if state.0 & IN_COMMENT != 0 {
            match scan_comment_body(bytes, 0) {
                Some(end) => {
                    spans.push((0..end, Token::Comment));
                    i = end;
                }
                None => return (vec![(0..len, Token::Comment)], state),
            }
        }

        while i < len {
            let byte = bytes[i];
            match byte {
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    let start = i;
                    match scan_comment_body(bytes, i + 2) {
                        Some(end) => {
                            spans.push((start..end, Token::Comment));
                            i = end;
                        }
                        None => {
                            spans.push((start..len, Token::Comment));
                            return (spans, LineState(brace_bit(in_braces) | IN_COMMENT));
                        }
                    }
                }
                b'"' | b'\'' => {
                    let quote = byte;
                    let start = i;
                    match scan_string_body(bytes, i + 1, quote) {
                        Some(end) => {
                            spans.push((start..end, Token::String));
                            i = end;
                        }
                        None => {
                            spans.push((start..len, Token::String));
                            let quote_bit = if quote == b'\'' { STRING_SINGLE } else { 0 };
                            return (
                                spans,
                                LineState(brace_bit(in_braces) | IN_STRING | quote_bit),
                            );
                        }
                    }
                }
                b'@' => {
                    let start = i;
                    i += 1;
                    while i < len && is_ident_continue(bytes[i]) {
                        i += 1;
                    }
                    spans.push((start..i, Token::Keyword));
                }
                b'{' => {
                    spans.push((i..i + 1, Token::Punctuation));
                    in_braces = true;
                    i += 1;
                }
                b'}' => {
                    spans.push((i..i + 1, Token::Punctuation));
                    in_braces = false;
                    i += 1;
                }
                b'(' | b')' | b'[' | b']' | b',' | b';' => {
                    spans.push((i..i + 1, Token::Punctuation));
                    i += 1;
                }
                b':' => {
                    let mut j = i + 1;
                    if bytes.get(j) == Some(&b':') {
                        j += 1;
                    }
                    let ident_start = j;
                    if bytes
                        .get(j)
                        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'-')
                    {
                        while j < len && is_ident_continue(bytes[j]) {
                            j += 1;
                        }
                    }
                    if !in_braces && j > ident_start {
                        spans.push((i..j, Token::Keyword));
                        i = j;
                    } else {
                        spans.push((i..i + 1, Token::Punctuation));
                        i += 1;
                    }
                }
                b'#' => {
                    let start = i;
                    i += 1;
                    while i < len
                        && (bytes[i].is_ascii_alphanumeric()
                            || bytes[i] == b'-'
                            || bytes[i] == b'_')
                    {
                        i += 1;
                    }
                    if i > start + 1 {
                        spans.push((start..i, Token::Constant));
                    } else {
                        spans.push((start..i, Token::Punctuation));
                    }
                }
                b'.' if bytes.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                    let start = i;
                    i = scan_number(bytes, i);
                    spans.push((start..i, Token::Number));
                }
                b'.' if bytes
                    .get(i + 1)
                    .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'-' || *b == b'_') =>
                {
                    let start = i;
                    i += 1;
                    while i < len && is_ident_continue(bytes[i]) {
                        i += 1;
                    }
                    spans.push((start..i, Token::Property));
                }
                b'.' => {
                    spans.push((i..i + 1, Token::Punctuation));
                    i += 1;
                }
                b'0'..=b'9' => {
                    let start = i;
                    i = scan_number(bytes, i);
                    spans.push((start..i, Token::Number));
                }
                b'-' => {
                    let looks_like_number = bytes.get(i + 1).is_some_and(u8::is_ascii_digit)
                        || (bytes.get(i + 1) == Some(&b'.')
                            && bytes.get(i + 2).is_some_and(u8::is_ascii_digit));
                    if looks_like_number {
                        let start = i;
                        i = scan_number(bytes, i);
                        spans.push((start..i, Token::Number));
                    } else if bytes.get(i + 1).is_some_and(|&b| is_ident_continue(b)) {
                        let start = i;
                        i = scan_word(bytes, start);
                        push_word(&mut spans, line, bytes, start, i, in_braces);
                    } else {
                        spans.push((i..i + 1, Token::Operator));
                        i += 1;
                    }
                }
                b'!' if line[i..].starts_with("!important") => {
                    spans.push((i..i + "!important".len(), Token::Attribute));
                    i += "!important".len();
                }
                b'>' | b'~' | b'*' | b'+' => {
                    spans.push((i..i + 1, Token::Operator));
                    i += 1;
                }
                _ if is_ident_start(byte) => {
                    let start = i;
                    i = scan_word(bytes, start);
                    push_word(&mut spans, line, bytes, start, i, in_braces);
                }
                _ => i += 1,
            }
        }

        (spans, LineState(brace_bit(in_braces)))
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b >= 0x80
}

fn scan_word(bytes: &[u8], start: usize) -> usize {
    let len = bytes.len();
    let mut i = start + 1;
    while i < len && is_ident_continue(bytes[i]) {
        i += 1;
    }
    i
}

/// Whether, skipping spaces and tabs from `j`, the next byte is `:` — a
/// property name's tell, e.g. `color:` or `color :`.
fn peek_is_colon(bytes: &[u8], mut j: usize) -> bool {
    let len = bytes.len();
    while j < len && matches!(bytes[j], b' ' | b'\t') {
        j += 1;
    }
    bytes.get(j) == Some(&b':')
}

fn push_word(
    spans: &mut Vec<(Range<usize>, Token)>,
    line: &str,
    bytes: &[u8],
    start: usize,
    end: usize,
    in_braces: bool,
) {
    let word = &line[start..end];
    let followed_by_colon = peek_is_colon(bytes, end);
    let followed_by_paren = bytes.get(end) == Some(&b'(');
    if let Some(token) = classify_word(word, in_braces, followed_by_colon, followed_by_paren) {
        spans.push((start..end, token));
    }
}

/// Selector position (`in_braces == false`) has no other reading for a
/// bareword than "element selector". Declaration position tells a property
/// name from a value by whether `:` follows; a value is then a known
/// keyword, a named colour, a `--custom-property` reference, or — followed
/// directly by `(` — a function call's name. An unrecognized value word
/// (a font family, a custom identifier) comes back `None`: left unstyled
/// rather than guessed at, the same call `rust.rs` makes for `_`.
fn classify_word(
    word: &str,
    in_braces: bool,
    followed_by_colon: bool,
    followed_by_paren: bool,
) -> Option<Token> {
    if !in_braces {
        return Some(Token::Type);
    }
    if followed_by_colon {
        return Some(Token::Property);
    }
    if followed_by_paren {
        return Some(Token::Function);
    }
    if word.starts_with("--") {
        return Some(Token::Variable);
    }
    if NAMED_COLORS.contains(&word) {
        return Some(Token::Constant);
    }
    if KEYWORDS.contains(&word) {
        return Some(Token::Keyword);
    }
    None
}

/// From just after the opening quote (or the start of a line already inside
/// one) to the byte past a matching closing quote. `None` if it is still
/// open at the end of the line — CSS strings do not span lines unescaped in
/// real syntax, but this lexer is fault-tolerant rather than a validator, so
/// it just resumes on the next line the same way `rust.rs`'s strings do.
fn scan_string_body(bytes: &[u8], mut i: usize, quote: u8) -> Option<usize> {
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

/// From just after the opening `/*` (or the start of a line already inside
/// one) to the byte past a `*/`. `None` if still open. CSS block comments do
/// not nest, unlike Rust's.
fn scan_comment_body(bytes: &[u8], mut i: usize) -> Option<usize> {
    let len = bytes.len();
    while i + 1 < len {
        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

/// A number, including its unit or `%` in the same span (`10px`, `1.5em`,
/// `100%`, `-5px`). `i` may start at a leading `-`, at the first digit, or
/// at a leading `.` (for `.5em`-style values with no integer part).
fn scan_number(bytes: &[u8], mut i: usize) -> usize {
    let len = bytes.len();
    if bytes.get(i) == Some(&b'-') {
        i += 1;
    }
    while i < len && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if bytes.get(i) == Some(&b'.') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
        i += 1;
        while i < len && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }
    if bytes.get(i) == Some(&b'%') {
        i += 1;
    } else {
        while i < len && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        CssLexer.lex_line(line, state)
    }

    fn tokens(line: &str) -> Vec<(Range<usize>, Token)> {
        lex(line, LineState::INITIAL).0
    }

    /// A line's tokens as if it were already inside a `{ }` block — the
    /// state every real declaration line is lexed with once its rule's
    /// opening brace has been seen (see
    /// `brace_state_governs_selector_vs_declaration_reading_across_lines`
    /// for that carried-across-lines behaviour itself).
    fn declaration_tokens(line: &str) -> Vec<(Range<usize>, Token)> {
        lex(line, LineState(IN_BRACES)).0
    }

    fn find<'a>(spans: &'a [(Range<usize>, Token)], text: &str, line: &str) -> Option<&'a Token> {
        let at = line.find(text)?;
        spans
            .iter()
            .find(|(range, _)| *range == (at..at + text.len()))
            .map(|(_, token)| token)
    }

    #[test]
    fn a_block_comment_can_close_on_the_same_line() {
        let line = "div { /* note */ color: red; }\n";
        let spans = tokens(line);
        let at = line.find("/*").unwrap();
        let end = line.find("*/").unwrap() + 2;
        assert!(spans.contains(&(at..end, Token::Comment)));
    }

    #[test]
    fn a_block_comment_spans_lines_and_resumes() {
        let (spans1, state) = lex("/* start\n", LineState::INITIAL);
        assert_eq!(spans1, vec![(0.."/* start\n".len(), Token::Comment)]);
        assert_ne!(state, LineState::INITIAL, "still inside the comment");

        let (spans2, state2) = lex("still commented\n", state);
        assert_eq!(spans2, vec![(0.."still commented\n".len(), Token::Comment)]);

        let (spans3, state3) = lex("end */ div {}\n", state2);
        let close = "end */".len();
        assert!(spans3.contains(&(0..close, Token::Comment)));
        assert_eq!(state3, LineState::INITIAL);
    }

    #[test]
    fn there_is_no_line_comment_form() {
        // CSS has no `//` comment; a stray `//` is just two operators/junk,
        // never a comment, so nothing after it is swallowed.
        let line = "div { color: red; } // not a comment\n";
        let spans = tokens(line);
        assert!(!spans.iter().any(|(_, t)| *t == Token::Comment));
    }

    #[test]
    fn selectors_are_told_apart_by_kind() {
        let line = "div.card#main:hover::before {\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "div", line), Some(&Token::Type));
        assert_eq!(find(&spans, ".card", line), Some(&Token::Property));
        assert_eq!(find(&spans, "#main", line), Some(&Token::Constant));
        assert_eq!(find(&spans, ":hover", line), Some(&Token::Keyword));
        assert_eq!(find(&spans, "::before", line), Some(&Token::Keyword));
    }

    #[test]
    fn a_property_and_its_value_are_told_apart() {
        let line = "  color: inherit;\n";
        let spans = declaration_tokens(line);
        assert_eq!(find(&spans, "color", line), Some(&Token::Property));
        assert_eq!(find(&spans, "inherit", line), Some(&Token::Keyword));
    }

    #[test]
    fn a_number_keeps_its_unit_in_the_same_span() {
        let line = "  margin: 10px 1.5em 100%;\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "10px", line), Some(&Token::Number));
        assert_eq!(find(&spans, "1.5em", line), Some(&Token::Number));
        assert_eq!(find(&spans, "100%", line), Some(&Token::Number));
    }

    #[test]
    fn a_hex_colour_is_a_constant() {
        let line = "  color: #fff;\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "#fff", line), Some(&Token::Constant));

        let line2 = "  color: #112233;\n";
        let spans2 = tokens(line2);
        assert_eq!(find(&spans2, "#112233", line2), Some(&Token::Constant));
    }

    #[test]
    fn quoted_string_values_are_strings() {
        let line = "  font-family: \"Arial\";\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "\"Arial\"", line), Some(&Token::String));

        let line2 = "  font-family: 'Arial';\n";
        let spans2 = tokens(line2);
        assert_eq!(find(&spans2, "'Arial'", line2), Some(&Token::String));
    }

    #[test]
    fn an_unterminated_string_spans_lines_and_resumes() {
        let (spans1, state) = lex("  content: \"still going\n", LineState::INITIAL);
        assert!(spans1.iter().any(|(_, t)| *t == Token::String));
        assert_ne!(state, LineState::INITIAL);

        let (spans2, state2) = lex("closes here\";\n", state);
        let close = "closes here\"".len();
        assert!(spans2.contains(&(0..close, Token::String)));
        assert_eq!(state2, LineState::INITIAL);
    }

    #[test]
    fn a_function_value_names_the_function() {
        let line = "  color: rgb(255, 0, 0);\n";
        let spans = declaration_tokens(line);
        assert_eq!(find(&spans, "rgb", line), Some(&Token::Function));
        assert!(spans.iter().any(|(_, t)| *t == Token::Number));

        let line2 = "  width: calc(100% - 10px);\n";
        let spans2 = declaration_tokens(line2);
        assert_eq!(find(&spans2, "calc", line2), Some(&Token::Function));
    }

    #[test]
    fn a_custom_property_is_a_variable() {
        let line = "  width: var(--main-width);\n";
        let spans = declaration_tokens(line);
        assert_eq!(find(&spans, "var", line), Some(&Token::Function));
        assert_eq!(find(&spans, "--main-width", line), Some(&Token::Variable));
    }

    #[test]
    fn an_at_rule_is_a_keyword() {
        for (line, at_rule) in [
            ("@media (min-width: 600px) {\n", "@media"),
            ("@import \"reset.css\";\n", "@import"),
            ("@keyframes spin {\n", "@keyframes"),
            ("@font-face {\n", "@font-face"),
        ] {
            let spans = tokens(line);
            assert_eq!(find(&spans, at_rule, line), Some(&Token::Keyword), "{line}");
        }
    }

    #[test]
    fn important_is_marked() {
        let line = "  color: red !important;\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "!important", line), Some(&Token::Attribute));
    }

    #[test]
    fn brace_state_governs_selector_vs_declaration_reading_across_lines() {
        // `.card` opens a rule on one line; the property on the next line
        // must read as a declaration, not another selector, so the brace
        // flag has to survive the line boundary.
        let (spans1, state) = lex(".card {\n", LineState::INITIAL);
        assert_eq!(find(&spans1, ".card", ".card {\n"), Some(&Token::Property));
        assert_ne!(state, LineState::INITIAL, "still inside the braces");

        let line2 = "  color: blue;\n";
        let (spans2, state2) = lex(line2, state);
        assert_eq!(find(&spans2, "color", line2), Some(&Token::Property));
        assert_eq!(find(&spans2, "blue", line2), Some(&Token::Constant));

        let (_, state3) = lex("}\n", state2);
        assert_eq!(state3, LineState::INITIAL, "the closing brace resets it");
    }

    #[test]
    fn broken_source_still_lexes_what_it_can() {
        let spans = tokens(".card { color: \n");
        assert!(spans.iter().any(|(_, t)| *t == Token::Property));

        let spans2 = tokens("@media (min-width: 600\n");
        assert!(spans2.iter().any(|(_, t)| *t == Token::Keyword));
    }

    #[test]
    fn multibyte_text_never_produces_a_span_that_is_not_at_a_char_boundary() {
        let line = "/* naïve café → 日本語 */\n.card { content: \"日本語\"; }\n";
        for chunk in line.split_inclusive('\n') {
            for (range, _) in tokens(chunk) {
                let _ = &chunk[range]; // panics if it split a character
            }
        }
    }

    #[test]
    fn spans_never_cross_the_lines_length() {
        let line = "div.card#main:hover { color: rgb(1, 2, 3); }\n";
        for (range, _) in tokens(line) {
            assert!(range.end <= line.len());
        }
    }
}
