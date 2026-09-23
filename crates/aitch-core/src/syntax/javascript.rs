//! A hand-written JavaScript lexer: `//` and `/* */` comments (the latter
//! not nested — JS has no nested block comments, unlike Rust), single- and
//! double-quoted strings (escapes, no interpolation, no cross-line state —
//! see the note below), template literals (`` `...` ``, which *do* carry
//! cross-line [`LineState`] and support `${expr}` interpolation), keywords
//! with a handful of special-cased words, numbers (hex/octal/binary,
//! exponents, BigInt `n`, `_` separators), a light regex-vs-division
//! heuristic, and the same "capitalized → [`Token::Type`], `name(` →
//! [`Token::Function`]" naming heuristics `rust.rs` uses.
//!
//! No syntax tree, so nothing here tracks brackets or nesting beyond what a
//! [`LineState`] can carry across one line boundary — see `syntax.rs`'s
//! module docs for what that trades away.
//!
//! ## Design calls
//!
//! **Plain strings never carry cross-line state.** `'...'`/`"..."` are not
//! meant to contain a literal newline in real JavaScript; an unterminated
//! one is styled as [`Token::String`] to the end of its line and the lexer
//! resumes the next line in [`Mode::Normal`], rather than pretending it
//! knows how to resume mid-string. Template literals are different — they
//! routinely span many lines — so they get real [`LineState`] bits.
//!
//! **Template interpolation (`${...}`) is carved out of the surrounding
//! [`Token::String`] as its own region, but its *contents are left
//! unstyled* rather than re-lexed as JS.** Only the `${` and the matching
//! `}` are given spans (as [`Token::Punctuation`], the same convention
//! `bash.rs` uses for `$( ... )`). Finding the matching `}` is done by
//! counting `{`/`}` bytes with a depth counter carried in [`LineState`] —
//! it does *not* understand that a `}` inside a nested string, comment, or
//! backtick within the interpolation doesn't count. A real
//! `` `${obj['}']}` `` would therefore close early. This is the simplified
//! option the task explicitly allows over fully re-lexing arbitrary nested
//! expressions (including nested template literals, which would need an
//! unbounded state stack `LineState`'s 32 bits can't give it).
//!
//! **Regex vs. division.** `/` opens a regex literal only when the previous
//! non-whitespace *token* was one of `( , = : ; ! & | ? { } [` or the
//! keyword `return`, or nothing has been seen yet this line — otherwise `/`
//! is [`Token::Operator`]. This is deliberately a light heuristic, not a
//! real parser: `a / b` after any other expression is division, but so is
//! (incorrectly, and unavoidably without a parser) a regex that happens to
//! follow something not on that list, e.g. a `typeof`/`case`/`in` keyword.
//! As an extension beyond that literal list, `=>` also permits a regex to
//! follow it (`() => /x/.test(s)` is common enough to be worth the special
//! case).
//!
//! **`:` is [`Token::Punctuation`], not [`Token::Operator`]**, matching
//! `rust.rs`'s own split, even though the task's operator list happens to
//! mention `:` too — ternary/label/object-literal colons read better
//! grouped with the other separators.

use std::ops::Range;

use super::{Lexer, LineState, Token};

pub(crate) struct JavaScriptLexer;

// `LineState`'s bit layout for JavaScript: 0 is "normal top-level code", 1
// is "inside an unterminated /* */ block comment", 2 is "inside a template
// literal's text", and anything with `& 0xFF == 3` is "inside a `${ ... }`
// interpolation", with the brace depth (always >= 1) packed into the
// remaining upper bits.
const NORMAL: LineState = LineState(0);
const IN_BLOCK_COMMENT: LineState = LineState(1);
const IN_TEMPLATE_TEXT: LineState = LineState(2);
const INTERP_TAG: u32 = 3;

fn interp_state(depth: u32) -> LineState {
    LineState(INTERP_TAG | (depth << 8))
}

fn interp_depth(state: LineState) -> Option<u32> {
    (state.0 & 0xFF == INTERP_TAG).then_some(state.0 >> 8)
}

const KEYWORDS: &[&str] = &[
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
];

#[derive(Clone, Copy)]
enum Mode {
    Normal,
    BlockComment,
    TemplateText,
    Interp(u32),
}

enum TemplateStep {
    /// Found the closing backtick; the byte just past it.
    Closed(usize),
    /// Found an unescaped `${`: where the text before it ends, and where
    /// the interpolation's content begins (just past the `{`).
    Interp(usize, usize),
    /// Still open at the end of the line; the line's length.
    Open(usize),
}

enum InterpStep {
    /// Found the matching `}`; its own position.
    Closed(usize),
    /// Still open at the end of the line, with the depth to resume with.
    StillOpen(u32),
}

impl Lexer for JavaScriptLexer {
    fn lex_line(&self, line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        let bytes = line.as_bytes();
        let len = bytes.len();
        let mut spans = Vec::new();
        let mut i = 0usize;
        // Whether a `/` seen right now would open a regex literal rather
        // than being division — see the module doc's heuristic note. `true`
        // covers both "start of line" and "resuming mid multi-line
        // construct that produced no value" (a block comment, or the far
        // side of a template literal that is about to close); the first
        // real operand this line sets it to `false`.
        let mut regex_allowed = true;

        let mut mode = if state == IN_BLOCK_COMMENT {
            Mode::BlockComment
        } else if state == IN_TEMPLATE_TEXT {
            Mode::TemplateText
        } else if let Some(depth) = interp_depth(state) {
            Mode::Interp(depth)
        } else {
            Mode::Normal
        };

        while i < len {
            match mode {
                Mode::BlockComment => match scan_block_comment_end(bytes, i) {
                    Some(end) => {
                        spans.push((i..end, Token::Comment));
                        i = end;
                        mode = Mode::Normal;
                    }
                    None => {
                        spans.push((i..len, Token::Comment));
                        return (spans, IN_BLOCK_COMMENT);
                    }
                },
                Mode::TemplateText => match scan_template_text_step(bytes, i) {
                    TemplateStep::Closed(end) => {
                        if end > i {
                            spans.push((i..end, Token::String));
                        }
                        i = end;
                        mode = Mode::Normal;
                        regex_allowed = false;
                    }
                    TemplateStep::Interp(text_end, interp_start) => {
                        if text_end > i {
                            spans.push((i..text_end, Token::String));
                        }
                        spans.push((text_end..interp_start, Token::Punctuation));
                        i = interp_start;
                        mode = Mode::Interp(1);
                    }
                    TemplateStep::Open(end) => {
                        if end > i {
                            spans.push((i..end, Token::String));
                        }
                        return (spans, IN_TEMPLATE_TEXT);
                    }
                },
                Mode::Interp(depth) => match scan_interp_step(bytes, i, depth) {
                    InterpStep::Closed(close_at) => {
                        spans.push((close_at..close_at + 1, Token::Punctuation));
                        i = close_at + 1;
                        mode = Mode::TemplateText;
                    }
                    InterpStep::StillOpen(new_depth) => {
                        return (spans, interp_state(new_depth));
                    }
                },
                Mode::Normal => {
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
                        b'/' if regex_allowed => {
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
                            regex_allowed = false;
                        }
                        b'/' => {
                            spans.push((i..i + 1, Token::Operator));
                            i += 1;
                            regex_allowed = false;
                        }
                        b'\'' | b'"' => {
                            let start = i;
                            let end = scan_quoted_string(bytes, i + 1, byte);
                            spans.push((start..end, Token::String));
                            i = end;
                            regex_allowed = false;
                        }
                        b'`' => {
                            let start = i;
                            match scan_template_text_step(bytes, i + 1) {
                                TemplateStep::Closed(end) => {
                                    spans.push((start..end, Token::String));
                                    i = end;
                                    regex_allowed = false;
                                }
                                TemplateStep::Interp(text_end, interp_start) => {
                                    spans.push((start..text_end, Token::String));
                                    spans.push((text_end..interp_start, Token::Punctuation));
                                    i = interp_start;
                                    mode = Mode::Interp(1);
                                }
                                TemplateStep::Open(end) => {
                                    spans.push((start..end, Token::String));
                                    return (spans, IN_TEMPLATE_TEXT);
                                }
                            }
                        }
                        b'0'..=b'9' => {
                            let start = i;
                            i = scan_number(bytes, i);
                            spans.push((start..i, Token::Number));
                            regex_allowed = false;
                        }
                        _ if is_ident_start(byte) => {
                            let start = i;
                            i += 1;
                            while i < len && is_ident_continue(bytes[i]) {
                                i += 1;
                            }
                            let word = &line[start..i];
                            regex_allowed = word == "return";
                            if let Some(token) = classify_word(word, bytes, i) {
                                spans.push((start..i, token));
                            }
                        }
                        b'=' if bytes.get(i + 1) == Some(&b'>') => {
                            spans.push((i..i + 2, Token::Operator));
                            i += 2;
                            regex_allowed = true;
                        }
                        b'+' | b'-' | b'*' | b'%' | b'=' | b'<' | b'>' | b'!' | b'&' | b'|'
                        | b'^' | b'~' | b'?' => {
                            spans.push((i..i + 1, Token::Operator));
                            regex_allowed = allows_regex(byte);
                            i += 1;
                        }
                        b'(' | b')' | b'[' | b']' | b'{' | b'}' | b',' | b'.' | b';' | b':' => {
                            spans.push((i..i + 1, Token::Punctuation));
                            regex_allowed = allows_regex(byte);
                            i += 1;
                        }
                        _ => i += 1,
                    }
                }
            }
        }

        (spans, NORMAL)
    }
}

/// Whether a `/` immediately after this punctuation/operator byte should be
/// read as opening a regex literal rather than as division — see the module
/// doc's heuristic note.
fn allows_regex(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b',' | b'=' | b':' | b';' | b'!' | b'&' | b'|' | b'?' | b'{' | b'}' | b'['
    )
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$' || b >= 0x80
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

/// From just past a comment's opening `/*` (or from wherever a resumed
/// block comment should keep looking), the byte just past its closing `*/`.
/// `None` if it is still open at the end of the line. JS block comments do
/// not nest, unlike Rust's.
fn scan_block_comment_end(bytes: &[u8], start: usize) -> Option<usize> {
    let len = bytes.len();
    let mut i = start;
    while i < len {
        if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

/// From just past a string's opening quote, the byte just past its matching
/// closing quote. Unterminated at the end of the line just returns `len` —
/// see the module doc: plain strings carry no cross-line state.
fn scan_quoted_string(bytes: &[u8], start: usize, quote: u8) -> usize {
    let len = bytes.len();
    let mut i = start;
    while i < len {
        match bytes[i] {
            b'\\' if i + 1 < len => i += 2,
            b'\\' => i += 1,
            b if b == quote => return i + 1,
            _ => i += 1,
        }
    }
    len
}

/// Scan a template literal's text from `start` (just past its opening
/// backtick, or wherever a resumed one/one past a closed interpolation
/// should keep looking) for either an unescaped closing backtick or an
/// unescaped `${`.
fn scan_template_text_step(bytes: &[u8], start: usize) -> TemplateStep {
    let len = bytes.len();
    let mut i = start;
    while i < len {
        match bytes[i] {
            b'\\' if i + 1 < len => i += 2,
            b'\\' => i += 1,
            b'`' => return TemplateStep::Closed(i + 1),
            b'$' if bytes.get(i + 1) == Some(&b'{') => return TemplateStep::Interp(i, i + 2),
            _ => i += 1,
        }
    }
    TemplateStep::Open(len)
}

/// Scan a `${...}` interpolation's body from `start` for its matching `}`,
/// counting brace depth (already at `depth`, always >= 1 while inside). See
/// the module doc: this does not understand strings, comments, or nested
/// template literals inside the interpolation, so a `}` inside any of those
/// closes it early — a documented, deliberate simplification.
fn scan_interp_step(bytes: &[u8], start: usize, mut depth: u32) -> InterpStep {
    let len = bytes.len();
    let mut i = start;
    while i < len {
        match bytes[i] {
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return InterpStep::Closed(i);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    InterpStep::StillOpen(depth)
}

/// A regex literal from its opening `/` at `start`: the byte just past its
/// closing `/` and any trailing flag letters. `None` if no unescaped,
/// out-of-character-class closing `/` is found before the line ends — the
/// caller then falls back to treating the opening `/` as division.
fn scan_regex(bytes: &[u8], start: usize) -> Option<usize> {
    let len = bytes.len();
    let mut i = start + 1;
    let mut in_class = false;
    while i < len {
        match bytes[i] {
            b'\\' if i + 1 < len => i += 2,
            b'\n' | b'\r' => return None,
            b'[' => {
                in_class = true;
                i += 1;
            }
            b']' if in_class => {
                in_class = false;
                i += 1;
            }
            b'/' if !in_class => {
                let mut j = i + 1;
                while j < len && bytes[j].is_ascii_alphabetic() {
                    j += 1;
                }
                return Some(j);
            }
            _ => i += 1,
        }
    }
    None
}

/// A pragmatic number scan covering integers, floats, `0x`/`0o`/`0b`
/// radixes, exponents, a trailing BigInt `n`, and `_` separators.
fn scan_number(bytes: &[u8], start: usize) -> usize {
    let len = bytes.len();
    let mut i = start;
    if bytes[i] == b'0'
        && matches!(
            bytes.get(i + 1),
            Some(b'x' | b'X' | b'o' | b'O' | b'b' | b'B')
        )
    {
        i += 2;
        while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        return i;
    }
    i += 1;
    while i < len {
        let b = bytes[i];
        if b.is_ascii_digit()
            || b == b'_'
            || (b == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit))
        {
            i += 1;
        } else if (b == b'e' || b == b'E')
            && matches!(bytes.get(i + 1), Some(b'0'..=b'9' | b'+' | b'-'))
        {
            i += 1;
            if matches!(bytes.get(i), Some(b'+' | b'-')) {
                i += 1;
            }
        } else if b == b'n' {
            i += 1;
            break;
        } else {
            break;
        }
    }
    i
}

fn classify_word(word: &str, bytes: &[u8], next: usize) -> Option<Token> {
    if KEYWORDS.contains(&word) {
        return Some(Token::Keyword);
    }
    match word {
        "null" | "undefined" | "true" | "false" => return Some(Token::Constant),
        "this" => return Some(Token::Variable),
        _ => {}
    }
    let first = word.chars().next()?;
    // Checked before the function-call heuristic below, the same order
    // `rust.rs`'s `classify_word` uses: a capitalized identifier is
    // conventionally a JS class/constructor (PascalCase), never a plain
    // function (those are camelCase by convention) — even one immediately
    // followed by `(`, as in `new Foo()`.
    if first.is_uppercase() {
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
        JavaScriptLexer.lex_line(line, state)
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
    fn a_line_comment_runs_to_the_end_of_the_line() {
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
    fn single_and_double_quoted_strings() {
        let line = "let a = 'x\\'y'; let b = \"p\\\"q\";\n";
        let spans = tokens(line);
        let s1 = line.find('\'').unwrap();
        let e1 = line[s1 + 1..].find('\'').unwrap() + s1 + 1;
        // Confirm the escaped quote did not end the string early: the
        // matching close is the *last* quote before the semicolon, not the
        // first one after the escape.
        let real_end = line.find("';").unwrap() + 1;
        assert!(spans.contains(&(s1..real_end, Token::String)));
        let _ = e1;

        let s2 = line.find('"').unwrap();
        let real_end2 = line.find("\";").unwrap() + 1;
        assert!(spans.contains(&(s2..real_end2, Token::String)));
    }

    #[test]
    fn a_template_literal_spans_lines_and_resumes() {
        let (spans1, state) = lex("let s = `hello\n", LineState::INITIAL);
        assert!(spans1.iter().any(|(_, t)| *t == Token::String));
        assert_ne!(state, LineState::INITIAL);

        let (spans2, state2) = lex("world`;\n", state);
        let close = "world`".len();
        assert!(spans2.contains(&(0..close, Token::String)));
        assert_eq!(state2, LineState::INITIAL);
        assert!(spans2.iter().any(|(_, t)| *t == Token::Punctuation));
    }

    #[test]
    fn a_template_literal_carves_out_an_interpolation() {
        let line = "let s = `sum: ${a + b}!`;\n";
        let spans = tokens(line);
        let open = line.find("${").unwrap();
        assert!(spans.contains(&(open..open + 2, Token::Punctuation)));
        let close = line.find('}').unwrap();
        assert!(spans.contains(&(close..close + 1, Token::Punctuation)));
        // The interpolation's contents are deliberately left unstyled (see
        // the module doc): no Variable/Operator spans inside `a + b`.
        let inside = open + 2..close;
        assert!(
            !spans
                .iter()
                .any(|(r, _)| r.start >= inside.start && r.end <= inside.end),
            "{spans:?}: interpolation contents should be unstyled"
        );
        // Text before and after the interpolation is still String.
        let backtick = line.find('`').unwrap();
        assert!(spans.contains(&(backtick..open, Token::String)));
    }

    #[test]
    fn an_interpolation_can_span_lines() {
        let (spans1, state) = lex("let s = `x: ${a +\n", LineState::INITIAL);
        let open = "let s = `x: ${".len();
        assert!(spans1.contains(&(open - 2..open, Token::Punctuation)));
        assert_ne!(state, LineState::INITIAL);

        let (spans2, state2) = lex("b}`;\n", state);
        assert!(spans2.contains(&(1..2, Token::Punctuation)));
        assert_eq!(state2, LineState::INITIAL);
    }

    #[test]
    fn keywords_this_null_and_true_are_special_cased() {
        let line = "function f() { return this === null || true; }\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "function", line), Some(&Token::Keyword));
        assert_eq!(find(&spans, "return", line), Some(&Token::Keyword));
        assert_eq!(find(&spans, "this", line), Some(&Token::Variable));
        assert_eq!(find(&spans, "null", line), Some(&Token::Constant));
        assert_eq!(find(&spans, "true", line), Some(&Token::Constant));
    }

    #[test]
    fn a_slash_after_an_operand_is_division() {
        let line = "let x = a / b;\n";
        let spans = tokens(line);
        let at = line.find(" / ").unwrap() + 1;
        assert_eq!(
            spans.iter().find(|(r, _)| r.start == at).map(|(_, t)| t),
            Some(&Token::Operator)
        );
    }

    #[test]
    fn a_slash_after_an_operator_is_a_regex() {
        let line = "let re = /abc/;\n";
        let spans = tokens(line);
        let start = line.find('/').unwrap();
        let end = line.rfind('/').unwrap() + 1;
        assert!(spans.contains(&(start..end, Token::String)));
    }

    #[test]
    fn a_bareword_call_is_a_function() {
        let line = "doStuff(1, 2);\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "doStuff", line), Some(&Token::Function));
    }

    #[test]
    fn a_capitalized_identifier_is_a_type_even_when_called() {
        let line = "const p = new Point(1, 2);\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "Point", line), Some(&Token::Type));
    }

    #[test]
    fn numbers_cover_radixes_exponents_bigint_and_separators() {
        let line = "let a = 0xFF; let b = 1_000n; let c = 3.5e-2; let d = 0b101;\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "0xFF", line), Some(&Token::Number));
        assert_eq!(find(&spans, "1_000n", line), Some(&Token::Number));
        assert_eq!(find(&spans, "3.5e-2", line), Some(&Token::Number));
        assert_eq!(find(&spans, "0b101", line), Some(&Token::Number));
    }

    #[test]
    fn broken_source_still_lexes_what_it_can() {
        let spans = tokens("function f( {\n  let x =\n");
        assert!(spans.iter().any(|(_, t)| *t == Token::Keyword));
    }

    #[test]
    fn spans_never_cross_the_lines_length() {
        let line = "let s = `a${b}c`; let n = /x/g;\n";
        for (range, _) in tokens(line) {
            assert!(range.end <= line.len());
        }
    }

    #[test]
    fn multibyte_text_never_produces_a_span_that_is_not_at_a_char_boundary() {
        let line = "// naïve café → 日本語\nfunction main() {}\n";
        for chunk in line.split_inclusive('\n') {
            for (range, _) in tokens(chunk) {
                let _ = &chunk[range]; // panics if it split a character
            }
        }
    }
}
