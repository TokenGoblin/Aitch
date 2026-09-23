//! A hand-written Python lexer: `#` comments, single/double-quoted strings
//! and their triple-quoted (`'''...'''`/`"""..."""`) forms which genuinely
//! span multiple lines, string prefixes (`r`, `b`, `u`, `f`, and the
//! combinations `rb`/`br`/`rf`/`fr`, case-insensitive), decorators,
//! keywords, `None`/`True`/`False` as constants, numbers (including hex/
//! octal/binary radixes, underscores, exponents, and the `j`/`J` complex
//! suffix), and a handful of naming heuristics (`def name(` ->
//! [`Token::Function`], `class Name` -> [`Token::Type`], a capitalized
//! identifier elsewhere -> [`Token::Type`], the same ordering `rust.rs`'s
//! `classify_word` uses so a capitalized call like `SomeClass()` reads as a
//! type, not a function).
//!
//! No syntax tree, so nothing here tracks brackets or nesting beyond what a
//! [`LineState`] can carry across one line boundary — see `syntax.rs`'s
//! module docs for what that trades away.
//!
//! Design call — string prefixes: `r` disables escape processing entirely
//! (a raw string's `\` is a literal byte, so `r"x\" y"` closes right after
//! the literal `\"`, not at the end — see the test of the same name below).
//! `b`/`u` are lexed exactly like a plain string (their meaning is a
//! semantic, not lexical, difference). `f` enables interpolation (next
//! paragraph). An unrecognized combination (anything but `r`, `b`, `u`,
//! `f`, `rb`/`br`, `rf`/`fr`) is not treated as a prefix at all, so e.g. a
//! variable named `rb` used on its own still lexes as a plain identifier.
//!
//! Design call — f-string interpolation: an f-string's `{expr}` IS carved
//! out of the surrounding [`Token::String`] run as its own region, mirroring
//! `bash.rs`'s `$var` carving inside a double-quoted string (`{{`/`}}`, a
//! literal brace, is left as string text, matching Python's own escaping).
//! It is a lightweight, single-line sub-lex (see `lex_expression`):
//! identifiers/keywords/constants/numbers/operators, plus a simple nested
//! same-line quoted string with no prefix/raw support of its own — not the
//! full lexer recursively applied. An expression whose braces are not
//! balanced within the current physical line's remaining bytes (a nested
//! f-string spanning further lines, e.g.) is proportionate to a
//! token-colouring lexer with no parse tree to fall back on, not a bug.

use std::ops::Range;

use super::{Lexer, LineState, Token};

pub(crate) struct PythonLexer;

// `LineState`'s bit layout for Python: bit 31 set means "inside a string",
// with bit 30 as "it's a triple-quoted (multi-line) string", bit 29 as
// "raw", bit 28 as "f-string", and bit 27 as "the quote character is `'`
// rather than `\"`". A single-line string still open at the end of its line
// is not valid Python (except via a trailing `\` continuation, which this
// also covers), but is treated as continuing onto the next line anyway —
// the same error-recovery call `rust.rs`/`toml.rs` make: broken source
// still highlights something reasonable instead of a confusing false close.
const STRING_FLAG: u32 = 1 << 31;
const TRIPLE_FLAG: u32 = 1 << 30;
const RAW_FLAG: u32 = 1 << 29;
const FSTRING_FLAG: u32 = 1 << 28;
const SINGLE_QUOTE_FLAG: u32 = 1 << 27;

fn string_state(raw: bool, triple: bool, fstring: bool, single_quote: bool) -> LineState {
    let mut value = STRING_FLAG;
    if triple {
        value |= TRIPLE_FLAG;
    }
    if raw {
        value |= RAW_FLAG;
    }
    if fstring {
        value |= FSTRING_FLAG;
    }
    if single_quote {
        value |= SINGLE_QUOTE_FLAG;
    }
    LineState(value)
}

const KEYWORDS: &[&str] = &[
    "def", "class", "if", "elif", "else", "for", "while", "break", "continue", "return", "pass",
    "import", "from", "as", "try", "except", "finally", "raise", "with", "lambda", "yield",
    "global", "nonlocal", "del", "assert", "async", "await", "and", "or", "not", "in", "is",
];

/// What identifier is expected next, tracked across a handful of bytes
/// within one `lex_line` call only (Python always puts a `def`/`class`
/// name on the same physical line as the keyword) — not carried in
/// [`LineState`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    None,
    FunctionName,
    ClassName,
}

impl Lexer for PythonLexer {
    fn lex_line(&self, line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        let bytes = line.as_bytes();
        let len = bytes.len();
        let mut spans = Vec::new();
        let mut i = 0usize;

        if state.0 & STRING_FLAG != 0 {
            let triple = state.0 & TRIPLE_FLAG != 0;
            let raw = state.0 & RAW_FLAG != 0;
            let fstring = state.0 & FSTRING_FLAG != 0;
            let quote = if state.0 & SINGLE_QUOTE_FLAG != 0 {
                b'\''
            } else {
                b'"'
            };
            let kind = StringKind {
                raw,
                triple,
                fstring,
                quote,
            };
            match scan_string_body(line, 0, 0, kind, &mut spans) {
                Some(end) => i = end,
                None => return (spans, state),
            }
        }

        // A decorator (`@name` or `@mod.name`) can only be the first thing
        // on a line — an `i == 0` still here means no string resumed above,
        // so this line is free to start with one.
        if i == 0 {
            let trimmed = bytes
                .iter()
                .position(|b| !matches!(b, b' ' | b'\t'))
                .unwrap_or(len);
            if bytes.get(trimmed) == Some(&b'@')
                && bytes.get(trimmed + 1).is_some_and(|&b| is_ident_start(b))
            {
                let start = trimmed;
                let mut j = trimmed + 1;
                while j < len && (is_ident_continue(bytes[j]) || bytes[j] == b'.') {
                    j += 1;
                }
                spans.push((start..j, Token::Attribute));
                i = j;
            }
        }

        let mut expect = Expect::None;

        while i < len {
            let byte = bytes[i];
            match byte {
                b'#' => {
                    spans.push((i..len, Token::Comment));
                    i = len;
                }
                b'\'' | b'"' => {
                    let quote = byte;
                    let quote_at = i;
                    let (triple, content_start) = open_string(bytes, quote_at, quote);
                    let kind = StringKind {
                        raw: false,
                        triple,
                        fstring: false,
                        quote,
                    };
                    match scan_string_body(line, content_start, quote_at, kind, &mut spans) {
                        Some(end) => i = end,
                        None => return (spans, string_state(false, triple, false, quote == b'\'')),
                    }
                    expect = Expect::None;
                }
                b'.' if bytes.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                    let start = i;
                    i = scan_number(bytes, i);
                    spans.push((start..i, Token::Number));
                    expect = Expect::None;
                }
                b'0'..=b'9' => {
                    let start = i;
                    i = scan_number(bytes, i);
                    spans.push((start..i, Token::Number));
                    expect = Expect::None;
                }
                _ if is_ident_start(byte) => {
                    if let Some((raw, fstring, quote, quote_at)) = parse_string_prefix(bytes, i) {
                        let (triple, content_start) = open_string(bytes, quote_at, quote);
                        let kind = StringKind {
                            raw,
                            triple,
                            fstring,
                            quote,
                        };
                        match scan_string_body(line, content_start, i, kind, &mut spans) {
                            Some(end) => i = end,
                            None => {
                                return (spans, string_state(raw, triple, fstring, quote == b'\''))
                            }
                        }
                        expect = Expect::None;
                    } else {
                        let start = i;
                        i += 1;
                        while i < len && is_ident_continue(bytes[i]) {
                            i += 1;
                        }
                        let word = &line[start..i];
                        let token = if let Some(t) = classify_word(word) {
                            expect = match word {
                                "def" => Expect::FunctionName,
                                "class" => Expect::ClassName,
                                _ => Expect::None,
                            };
                            t
                        } else {
                            let t = match expect {
                                Expect::FunctionName => Token::Function,
                                Expect::ClassName => Token::Type,
                                Expect::None => {
                                    if word.chars().next().is_some_and(char::is_uppercase) {
                                        Token::Type
                                    } else if bytes.get(i) == Some(&b'(') {
                                        Token::Function
                                    } else {
                                        Token::Variable
                                    }
                                }
                            };
                            expect = Expect::None;
                            t
                        };
                        spans.push((start..i, token));
                    }
                }
                b':' => {
                    if bytes.get(i + 1) == Some(&b'=') {
                        spans.push((i..i + 2, Token::Operator));
                        i += 2;
                    } else {
                        spans.push((i..i + 1, Token::Punctuation));
                        i += 1;
                    }
                    expect = Expect::None;
                }
                b'+' | b'-' | b'*' | b'/' | b'%' | b'=' | b'<' | b'>' | b'!' | b'&' | b'|'
                | b'^' | b'~' | b'@' => {
                    let start = i;
                    i = scan_operator(bytes, i);
                    spans.push((start..i, Token::Operator));
                    expect = Expect::None;
                }
                b'(' | b')' | b'[' | b']' | b'{' | b'}' | b',' | b'.' | b';' => {
                    spans.push((i..i + 1, Token::Punctuation));
                    i += 1;
                    expect = Expect::None;
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

fn classify_word(word: &str) -> Option<Token> {
    if KEYWORDS.contains(&word) {
        return Some(Token::Keyword);
    }
    if matches!(word, "None" | "True" | "False") {
        return Some(Token::Constant);
    }
    None
}

/// Look for a string-literal prefix (`r`, `b`, `u`, `f`, or `rb`/`br`/
/// `rf`/`fr`, case-insensitive) starting at `i`, immediately followed by a
/// quote. Returns whether it is raw, whether it is an f-string, which quote
/// character, and where that quote sits. `None` for anything else (an
/// ordinary identifier, including one that merely starts with these
/// letters, like `rb_count` or a bare `r`).
fn parse_string_prefix(bytes: &[u8], i: usize) -> Option<(bool, bool, u8, usize)> {
    let len = bytes.len();
    let mut letters = [0u8; 2];
    let mut count = 0;
    let mut j = i;
    while count < 2 && j < len {
        let lower = bytes[j].to_ascii_lowercase();
        if matches!(lower, b'r' | b'b' | b'u' | b'f') {
            letters[count] = lower;
            count += 1;
            j += 1;
        } else {
            break;
        }
    }
    let quote = *bytes.get(j)?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let valid = matches!(
        &letters[..count],
        [] | [b'r']
            | [b'b']
            | [b'u']
            | [b'f']
            | [b'r', b'b']
            | [b'b', b'r']
            | [b'r', b'f']
            | [b'f', b'r']
    );
    if !valid {
        return None;
    }
    let raw = letters[..count].contains(&b'r');
    let fstring = letters[..count].contains(&b'f');
    Some((raw, fstring, quote, j))
}

/// From an opening quote at `quote_at`, whether it is the start of a
/// triple-quoted string and the byte offset just past its opening
/// delimiter.
fn open_string(bytes: &[u8], quote_at: usize, quote: u8) -> (bool, usize) {
    if bytes.get(quote_at + 1) == Some(&quote) && bytes.get(quote_at + 2) == Some(&quote) {
        (true, quote_at + 3)
    } else {
        (false, quote_at + 1)
    }
}

/// What kind of string [`scan_string_body`] is scanning: bundled into one
/// type rather than four separate `bool`/`u8` parameters.
#[derive(Clone, Copy)]
struct StringKind {
    raw: bool,
    triple: bool,
    fstring: bool,
    quote: u8,
}

/// Scans a string's body from `content_start` (just past its opening
/// quote(s), or 0 when resuming one already open at the start of this
/// line), pushing [`Token::String`] spans for its literal text directly
/// into `spans` — starting from `span_start`, which is the opening quote's
/// own position (or its prefix's, if it had one), or 0 when resuming, the
/// same convention `bash.rs`'s `scan_double_quoted_body` uses. If `kind.fstring`
/// is set, a same-line `{expr}` interpolation (but not a doubled `{{`/`}}`,
/// which is a literal brace) is carved out via [`lex_expression`] — see the
/// module docs' design-call note.
///
/// Returns the byte offset just past the closing quote(s), or `None` if the
/// string (and any `spans` already pushed for it) is still open at the end
/// of the line.
fn scan_string_body(
    line: &str,
    content_start: usize,
    span_start: usize,
    kind: StringKind,
    spans: &mut Vec<(Range<usize>, Token)>,
) -> Option<usize> {
    let StringKind {
        raw,
        triple,
        fstring,
        quote,
    } = kind;
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = content_start;
    let mut text_start = span_start;

    while i < len {
        let byte = bytes[i];
        if !raw && byte == b'\\' {
            i += if i + 1 < len { 2 } else { 1 };
            continue;
        }
        if fstring && byte == b'{' {
            if bytes.get(i + 1) == Some(&b'{') {
                i += 2;
                continue;
            }
            if text_start < i {
                spans.push((text_start..i, Token::String));
            }
            spans.push((i..i + 1, Token::Punctuation));
            i += 1;
            let expr_start = i;
            let mut depth = 1i32;
            while i < len && depth > 0 {
                match bytes[i] {
                    b'{' => {
                        depth += 1;
                        i += 1;
                    }
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                        i += 1;
                    }
                    _ => i += 1,
                }
            }
            lex_expression(line, expr_start, i, spans);
            if bytes.get(i) == Some(&b'}') {
                spans.push((i..i + 1, Token::Punctuation));
                i += 1;
            }
            text_start = i;
            continue;
        }
        if fstring && byte == b'}' && bytes.get(i + 1) == Some(&b'}') {
            i += 2;
            continue;
        }
        if byte == quote {
            if triple {
                if bytes.get(i + 1) == Some(&quote) && bytes.get(i + 2) == Some(&quote) {
                    spans.push((text_start..i + 3, Token::String));
                    return Some(i + 3);
                }
                i += 1;
            } else {
                spans.push((text_start..i + 1, Token::String));
                return Some(i + 1);
            }
        } else {
            i += 1;
        }
    }

    if text_start < len {
        spans.push((text_start..len, Token::String));
    }
    None
}

/// A lightweight, single-line sub-lex for an f-string's `{expr}` body — see
/// the module docs' design-call note for exactly what this does and does
/// not cover.
fn lex_expression(line: &str, start: usize, end: usize, spans: &mut Vec<(Range<usize>, Token)>) {
    let bytes = line.as_bytes();
    let mut i = start;
    while i < end {
        let byte = bytes[i];
        match byte {
            b'0'..=b'9' => {
                let s = i;
                i = scan_number(bytes, i).min(end);
                spans.push((s..i, Token::Number));
            }
            _ if is_ident_start(byte) => {
                let s = i;
                i += 1;
                while i < end && is_ident_continue(bytes[i]) {
                    i += 1;
                }
                let token = classify_word(&line[s..i]).unwrap_or_else(|| {
                    if line[s..i].chars().next().is_some_and(char::is_uppercase) {
                        Token::Type
                    } else {
                        Token::Variable
                    }
                });
                spans.push((s..i, token));
            }
            b'\'' | b'"' => {
                let quote = byte;
                let s = i;
                i += 1;
                while i < end {
                    if bytes[i] == b'\\' && i + 1 < end {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == quote {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                spans.push((s..i, Token::String));
            }
            b'.' | b',' | b':' | b'[' | b']' | b'(' | b')' => {
                spans.push((i..i + 1, Token::Punctuation));
                i += 1;
            }
            b'+' | b'-' | b'*' | b'/' | b'%' | b'=' | b'<' | b'>' | b'!' | b'&' | b'|' | b'^'
            | b'~' => {
                spans.push((i..i + 1, Token::Operator));
                i += 1;
            }
            _ => i += 1,
        }
    }
}

/// A pragmatic multi-char operator scan: 3-char augmented assignments
/// first, then 2-char (comparisons, `**`/`//`, `->`, `:=`, and the other
/// augmented assignments), else a single byte.
fn scan_operator(bytes: &[u8], i: usize) -> usize {
    let len = bytes.len();
    if i + 3 <= len {
        if let b"**=" | b"//=" | b">>=" | b"<<=" = &bytes[i..i + 3] {
            return i + 3;
        }
    }
    if i + 2 <= len {
        if let b"**" | b"//" | b"==" | b"!=" | b"<=" | b">=" | b"->" | b":=" | b"+=" | b"-="
        | b"*=" | b"/=" | b"%=" | b"&=" | b"|=" | b"^=" | b"<<" | b">>" | b"@=" =
            &bytes[i..i + 2]
        {
            return i + 2;
        }
    }
    i + 1
}

/// A number from its first byte (a digit, or a `.` already confirmed by the
/// caller to lead into one): decimal, `0x`/`0o`/`0b` radixes with
/// underscores, a float's fractional part and exponent, and a trailing
/// `j`/`J` complex suffix.
fn scan_number(bytes: &[u8], start: usize) -> usize {
    let len = bytes.len();
    let mut i = start;

    if bytes[i] == b'.' {
        i += 1;
        while i < len && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
            i += 1;
        }
        return finish_complex(bytes, i);
    }

    if bytes[i] == b'0' {
        match bytes.get(i + 1) {
            Some(b'x' | b'X') => {
                i += 2;
                while i < len && (bytes[i].is_ascii_hexdigit() || bytes[i] == b'_') {
                    i += 1;
                }
                return i;
            }
            Some(b'o' | b'O') => {
                i += 2;
                while i < len && (matches!(bytes[i], b'0'..=b'7') || bytes[i] == b'_') {
                    i += 1;
                }
                return i;
            }
            Some(b'b' | b'B') => {
                i += 2;
                while i < len && (matches!(bytes[i], b'0' | b'1') || bytes[i] == b'_') {
                    i += 1;
                }
                return i;
            }
            _ => {}
        }
    }

    while i < len && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
        i += 1;
    }
    if bytes.get(i) == Some(&b'.') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
        i += 1;
        while i < len && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
            i += 1;
        }
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(bytes.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        if bytes.get(j).is_some_and(u8::is_ascii_digit) {
            i = j;
            while i < len && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
                i += 1;
            }
        }
    }
    finish_complex(bytes, i)
}

fn finish_complex(bytes: &[u8], mut i: usize) -> usize {
    if bytes.get(i).is_some_and(|b| matches!(b, b'j' | b'J')) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        PythonLexer.lex_line(line, state)
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
    fn a_comment_runs_to_the_end_of_the_line() {
        let line = "x = 1  # trailing note\n";
        let spans = tokens(line);
        let at = line.find('#').unwrap();
        assert!(spans.contains(&(at..line.len(), Token::Comment)));
    }

    #[test]
    fn plain_single_and_double_quoted_strings() {
        let line = "a = 'hi'\nb = \"bye\"\n";
        for chunk in line.split_inclusive('\n') {
            let spans = tokens(chunk);
            assert!(spans.iter().any(|(_, t)| *t == Token::String), "{chunk:?}");
        }
    }

    #[test]
    fn an_escaped_quote_does_not_end_a_string() {
        let line = "s = \"a\\\"b\"\n";
        let spans = tokens(line);
        let start = line.find('"').unwrap();
        let end = line.rfind('"').unwrap() + 1;
        assert!(spans.contains(&(start..end, Token::String)));
    }

    #[test]
    fn a_raw_string_does_not_treat_backslash_as_an_escape() {
        // In a raw string a backslash is just a literal backslash, so the
        // quote right after it closes the string — unlike a plain string,
        // where `\"` would be an escaped quote that keeps it open.
        let line = "a = r\"x\\\" y\n";
        let spans = tokens(line);
        let start = line.find('r').unwrap();
        let close = line.find("\\\"").unwrap() + 2;
        assert!(spans.contains(&(start..close, Token::String)));
        // "y" after the string closed early is ordinary code, not a string.
        assert_eq!(find(&spans, "y", line), Some(&Token::Variable));
    }

    #[test]
    fn a_triple_double_quoted_string_spans_lines_and_resumes() {
        let (spans1, state) = lex("s = \"\"\"start\n", LineState::INITIAL);
        assert!(spans1.iter().any(|(_, t)| *t == Token::String));
        assert_ne!(state, LineState::INITIAL);

        let (spans2, state2) = lex("still going\n", state);
        assert_eq!(spans2, vec![(0.."still going\n".len(), Token::String)]);
        assert_eq!(state2, state, "still inside the string");

        let (spans3, state3) = lex("end\"\"\" # done\n", state2);
        let close = "end\"\"\"".len();
        assert!(spans3.contains(&(0..close, Token::String)));
        assert_eq!(state3, LineState::INITIAL);
        assert!(spans3.iter().any(|(_, t)| *t == Token::Comment));
    }

    #[test]
    fn a_triple_single_quoted_string_spans_lines_and_resumes() {
        let (spans1, state) = lex("s = '''start\n", LineState::INITIAL);
        assert!(spans1.iter().any(|(_, t)| *t == Token::String));
        assert_ne!(state, LineState::INITIAL);

        let (spans2, state2) = lex("c:\\still\\going\n", state);
        assert_eq!(
            spans2,
            vec![(0.."c:\\still\\going\n".len(), Token::String)],
            "no escapes in this non-raw string matter for whether it stays open"
        );
        assert_eq!(state2, state);

        let (spans3, state3) = lex("end'''\n", state2);
        assert!(spans3.contains(&(0.."end'''".len(), Token::String)));
        assert_eq!(state3, LineState::INITIAL);
    }

    #[test]
    fn keywords_come_out_as_keywords() {
        let line = "if x:\n    return\n";
        for chunk in line.split_inclusive('\n') {
            let spans = tokens(chunk);
            if chunk.contains("if") {
                assert_eq!(find(&spans, "if", chunk), Some(&Token::Keyword));
            }
            if chunk.contains("return") {
                assert_eq!(find(&spans, "return", chunk), Some(&Token::Keyword));
            }
        }
    }

    #[test]
    fn none_true_false_are_constants() {
        let line = "a = None\nb = True\nc = False\n";
        for chunk in line.split_inclusive('\n') {
            let spans = tokens(chunk);
            let value = chunk.split_once('=').unwrap().1.trim_end().trim();
            assert_eq!(
                find(&spans, value, chunk),
                Some(&Token::Constant),
                "{chunk:?}"
            );
        }
    }

    #[test]
    fn a_decorator_is_its_own_token() {
        let line = "@app.route(\"/\")\n";
        let spans = tokens(line);
        let end = line.find('(').unwrap();
        assert!(spans.contains(&(0..end, Token::Attribute)));
    }

    #[test]
    fn def_name_is_a_function() {
        let line = "def greet(name):\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "def", line), Some(&Token::Keyword));
        assert_eq!(find(&spans, "greet", line), Some(&Token::Function));
    }

    #[test]
    fn class_name_and_its_base_are_types() {
        let line = "class Greeter(Base):\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "class", line), Some(&Token::Keyword));
        assert_eq!(find(&spans, "Greeter", line), Some(&Token::Type));
        assert_eq!(find(&spans, "Base", line), Some(&Token::Type));
    }

    #[test]
    fn a_capitalized_identifier_elsewhere_is_a_type() {
        let line = "x = SomeClass()\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "SomeClass", line), Some(&Token::Type));
    }

    #[test]
    fn a_lowercase_call_is_a_function() {
        let line = "x = compute(1)\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "compute", line), Some(&Token::Function));
    }

    #[test]
    fn self_is_a_plain_variable() {
        let line = "def f(self, cls):\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "self", line), Some(&Token::Variable));
        assert_eq!(find(&spans, "cls", line), Some(&Token::Variable));
    }

    #[test]
    fn numbers_include_radixes_and_underscores() {
        let line = "a = 1_000\nb = 0xFF\nc = 0o17\nd = 0b1010\n";
        for chunk in line.split_inclusive('\n') {
            let spans = tokens(chunk);
            let value = chunk.split_once('=').unwrap().1.trim_end().trim();
            assert_eq!(
                find(&spans, value, chunk),
                Some(&Token::Number),
                "{chunk:?}"
            );
        }
    }

    #[test]
    fn a_float_with_an_exponent_is_a_number() {
        let line = "a = 6.02e23\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "6.02e23", line), Some(&Token::Number));
    }

    #[test]
    fn a_complex_number_literal_is_a_number() {
        let line = "z = 3+4j\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "3", line), Some(&Token::Number));
        assert_eq!(find(&spans, "+", line), Some(&Token::Operator));
        assert_eq!(find(&spans, "4j", line), Some(&Token::Number));
    }

    #[test]
    fn an_fstring_carves_out_an_interpolated_expression() {
        let line = "s = f\"hi {name}!\"\n";
        let spans = tokens(line);
        let open = line.find('{').unwrap();
        assert!(spans.contains(&(open..open + 1, Token::Punctuation)));
        let close = line.find('}').unwrap();
        assert!(spans.contains(&(close..close + 1, Token::Punctuation)));
        assert_eq!(find(&spans, "name", line), Some(&Token::Variable));
    }

    #[test]
    fn an_fstring_leaves_a_doubled_brace_as_literal_text() {
        let line = "s = f\"{{literal}}\"\n";
        let spans = tokens(line);
        assert!(
            !spans.iter().any(|(_, t)| *t == Token::Punctuation),
            "{spans:?}: a doubled brace is not an interpolation"
        );
    }

    #[test]
    fn combined_and_uppercase_prefixes_are_recognized() {
        let line = "a = RB'x'\nb = fr\"y\"\nc = BR'z'\n";
        for chunk in line.split_inclusive('\n') {
            let spans = tokens(chunk);
            assert!(spans.iter().any(|(_, t)| *t == Token::String), "{chunk:?}");
        }
    }

    #[test]
    fn broken_source_still_lexes_what_it_can() {
        let spans = tokens("def f(:\n    x = \n");
        assert!(spans.iter().any(|(_, t)| *t == Token::Keyword));
    }

    #[test]
    fn spans_never_cross_the_lines_length() {
        let line = "x = f\"{a + b}\" # note\n";
        for (range, _) in tokens(line) {
            assert!(range.end <= line.len());
        }
    }

    #[test]
    fn multibyte_text_never_produces_a_span_that_is_not_at_a_char_boundary() {
        let line = "# naïve café → 日本語\ns = \"naïve café → 日本語\"\n";
        for chunk in line.split_inclusive('\n') {
            for (range, _) in tokens(chunk) {
                let _ = &chunk[range]; // panics if it split a character
            }
        }
    }
}
