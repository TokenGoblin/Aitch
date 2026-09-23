//! A hand-written TOML lexer: `#` comments, the four string forms (basic
//! `"..."`, literal `'...'`, and their multi-line `"""..."""`/`'''...'''`
//! counterparts), bare/quoted/dotted keys, `[table]` and `[[array of
//! tables]]` headers, booleans, and numbers — including TOML's integer
//! radixes, underscores, special floats (`inf`/`nan`), and RFC 3339-ish
//! dates/datetimes/local-times, which this lexer colours as [`Token::Number`]
//! (TOML's own spec groups them with the other primitive value types, and
//! reusing the number scanner's digit-run machinery for them was the
//! pragmatic choice — see `scan_number` below).
//!
//! No syntax tree, so like `rust.rs` and `json.rs` this has no real nesting:
//! in particular, an inline table's own `key = value` pairs on the same line
//! as an outer `x = { ... }` are not told apart from the outer value — once
//! `=` has been seen once on a line, everything after it is "value context"
//! for the rest of that line. A real TOML document rarely nests inline
//! tables deeply enough for that to matter for highlighting.

use std::ops::Range;

use super::{Lexer, LineState, Token};

pub(crate) struct TomlLexer;

// `LineState`'s values for TOML: unlike Rust's bit-packed layout, TOML only
// ever needs to resume in one of a handful of distinct "kind of string, and
// which quote character" states, so a small enum-like set of plain values is
// enough.
const NORMAL: LineState = LineState(0);
// Not valid TOML — a `"..."`/`'...'` value may never contain a literal line
// break — but treating an unterminated single-line string as continuing onto
// the next line (rather than silently closing it at the line's end) is the
// same error-recovery call `rust.rs` and `json.rs` make for their own
// strings: broken source still highlights something reasonable instead of a
// confusing false close.
const IN_STRING_BASIC: LineState = LineState(1);
const IN_STRING_LITERAL: LineState = LineState(2);
const IN_ML_BASIC: LineState = LineState(3);
const IN_ML_LITERAL: LineState = LineState(4);

impl Lexer for TomlLexer {
    fn lex_line(&self, line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        let bytes = line.as_bytes();
        let len = bytes.len();
        let mut spans = Vec::new();
        let mut i = 0usize;

        match state {
            IN_ML_BASIC | IN_ML_LITERAL => {
                let literal = state == IN_ML_LITERAL;
                match scan_multiline_string_body(bytes, 0, literal) {
                    Some(end) => {
                        spans.push((0..end, Token::String));
                        i = end;
                    }
                    None => return (vec![(0..len, Token::String)], state),
                }
            }
            IN_STRING_BASIC | IN_STRING_LITERAL => {
                let literal = state == IN_STRING_LITERAL;
                match scan_single_string_body(bytes, 0, literal) {
                    Some(end) => {
                        spans.push((0..end, Token::String));
                        i = end;
                    }
                    None => return (vec![(0..len, Token::String)], state),
                }
            }
            _ => {}
        }

        // A table header (`[table]`/`[[array of tables]]`) can only be the
        // first thing on a line — an `i == 0` still here means no string
        // resumed above, so this line is free to start with one.
        if i == 0 {
            let trimmed = bytes
                .iter()
                .position(|b| !matches!(b, b' ' | b'\t'))
                .unwrap_or(len);
            if bytes.get(trimmed) == Some(&b'[') {
                let (header_spans, next_i) = scan_table_header(bytes, trimmed);
                spans.extend(header_spans);
                i = next_i;
            }
        }

        let mut after_equals = false;
        while i < len {
            let byte = bytes[i];
            match byte {
                b'#' => {
                    spans.push((i..len, Token::Comment));
                    i = len;
                }
                b' ' | b'\t' | b'\r' | b'\n' => i += 1,
                b'"' | b'\'' => {
                    let quote = byte;
                    let literal = quote == b'\'';
                    let start = i;
                    if bytes.get(i + 1) == Some(&quote) && bytes.get(i + 2) == Some(&quote) {
                        match scan_multiline_string_body(bytes, i + 3, literal) {
                            Some(end) => {
                                spans.push((start..end, Token::String));
                                i = end;
                            }
                            None => {
                                spans.push((start..len, Token::String));
                                return (spans, if literal { IN_ML_LITERAL } else { IN_ML_BASIC });
                            }
                        }
                    } else {
                        match scan_single_string_body(bytes, i + 1, literal) {
                            Some(end) => {
                                let token = if after_equals {
                                    Token::String
                                } else {
                                    Token::Property
                                };
                                spans.push((start..end, token));
                                i = end;
                            }
                            None => {
                                spans.push((start..len, Token::String));
                                return (
                                    spans,
                                    if literal {
                                        IN_STRING_LITERAL
                                    } else {
                                        IN_STRING_BASIC
                                    },
                                );
                            }
                        }
                    }
                }
                b'=' => {
                    spans.push((i..i + 1, Token::Operator));
                    after_equals = true;
                    i += 1;
                }
                b'.' | b',' | b'[' | b']' | b'{' | b'}' => {
                    spans.push((i..i + 1, Token::Punctuation));
                    i += 1;
                }
                b'0'..=b'9' if after_equals => {
                    let start = i;
                    i = scan_number(line, bytes, i);
                    spans.push((start..i, Token::Number));
                }
                b'+' | b'-'
                    if after_equals
                        && (bytes.get(i + 1).is_some_and(u8::is_ascii_digit)
                            || line[i + 1..].starts_with("inf")
                            || line[i + 1..].starts_with("nan")) =>
                {
                    let start = i;
                    i = scan_number(line, bytes, i);
                    spans.push((start..i, Token::Number));
                }
                _ if is_bare_key_byte(byte) => {
                    let start = i;
                    i += 1;
                    while i < len && is_bare_key_byte(bytes[i]) {
                        i += 1;
                    }
                    let word = &line[start..i];
                    if after_equals {
                        match word {
                            "true" | "false" => spans.push((start..i, Token::Constant)),
                            "inf" | "nan" => spans.push((start..i, Token::Number)),
                            _ => {} // an unrecognized bare word in value
                                    // position: malformed TOML, or a case
                                    // this lexer does not special-case.
                                    // Left unstyled rather than guessed at.
                        }
                    } else {
                        spans.push((start..i, Token::Property));
                    }
                }
                _ => i += 1,
            }
        }

        (spans, NORMAL)
    }
}

fn is_bare_key_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// From just after an opening single/double quote (or the start of a line
/// already inside one), the byte past the matching closing quote. `None` if
/// it is still open — see `IN_STRING_BASIC`/`IN_STRING_LITERAL`'s doc
/// comment for what that means for TOML.
fn scan_single_string_body(bytes: &[u8], mut i: usize, literal: bool) -> Option<usize> {
    let len = bytes.len();
    let quote = if literal { b'\'' } else { b'"' };
    while i < len {
        match bytes[i] {
            b'\\' if !literal => i += 2,
            b if b == quote => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// From just after an opening `"""`/`'''` (or the start of a line already
/// inside one), the byte past the matching closing triple-quote. `None` if
/// it is still open at the end of the line.
fn scan_multiline_string_body(bytes: &[u8], mut i: usize, literal: bool) -> Option<usize> {
    let len = bytes.len();
    let quote = if literal { b'\'' } else { b'"' };
    while i < len {
        match bytes[i] {
            b'\\' if !literal => i += 2,
            b if b == quote => {
                if bytes.get(i + 1) == Some(&quote) && bytes.get(i + 2) == Some(&quote) {
                    return Some(i + 3);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// From an opening `[` (table header) or `[[` (array-of-tables header) at
/// `start`, the header's spans and the byte just past its closing
/// bracket(s) — or past whatever was there, if the line does not actually
/// close it (malformed TOML; this just stops rather than scanning forever).
///
/// The double brackets of `[[...]]` are each scanned as one two-byte
/// [`Token::Punctuation`] span rather than two one-byte ones — unlike the
/// single-character `[`/`]` punctuation used for array values elsewhere in a
/// line — since they always appear as a pair here.
fn scan_table_header(bytes: &[u8], start: usize) -> (Vec<(Range<usize>, Token)>, usize) {
    let len = bytes.len();
    let mut spans = Vec::new();
    let mut i = start + 1;
    if bytes.get(i) == Some(&b'[') {
        i += 1;
    }
    spans.push((start..i, Token::Punctuation));

    loop {
        while i < len && matches!(bytes[i], b' ' | b'\t' | b'\r') {
            i += 1;
        }
        match bytes.get(i) {
            None | Some(b']') => break,
            Some(b'.') => {
                spans.push((i..i + 1, Token::Punctuation));
                i += 1;
            }
            Some(&quote @ (b'"' | b'\'')) => {
                let seg_start = i;
                i += 1;
                while i < len && bytes[i] != quote {
                    if quote == b'"' && bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                if i < len {
                    i += 1; // the closing quote
                }
                spans.push((seg_start..i, Token::Namespace));
            }
            Some(&b) if is_bare_key_byte(b) => {
                let seg_start = i;
                while i < len && is_bare_key_byte(bytes[i]) {
                    i += 1;
                }
                spans.push((seg_start..i, Token::Namespace));
            }
            Some(_) => i += 1, // a stray byte in a malformed header
        }
    }

    if bytes.get(i) == Some(&b']') {
        let close_start = i;
        i += 1;
        if bytes.get(i) == Some(&b']') {
            i += 1;
        }
        spans.push((close_start..i, Token::Punctuation));
    }

    (spans, i)
}

/// A number, from its first byte (a digit, or a leading `+`/`-` already
/// confirmed by the caller to lead into one). Also the entry point for
/// TOML's date/time forms, which share a number's leading digit run —
/// `1979-05-27...` is told apart from a plain integer by a `-` right after
/// exactly four digits, and a bare `07:32:00` by a `:` right after exactly
/// two.
fn scan_number(line: &str, bytes: &[u8], start: usize) -> usize {
    let len = bytes.len();
    let mut i = start;
    if matches!(bytes.get(i), Some(b'+') | Some(b'-')) {
        i += 1;
    }
    if line[i..].starts_with("inf") {
        return i + 3;
    }
    if line[i..].starts_with("nan") {
        return i + 3;
    }
    if bytes.get(i) == Some(&b'0') {
        match bytes.get(i + 1) {
            Some(b'x') => return scan_radix_digits(bytes, i + 2, |b| b.is_ascii_hexdigit()),
            Some(b'o') => return scan_radix_digits(bytes, i + 2, |b| (b'0'..=b'7').contains(&b)),
            Some(b'b') => return scan_radix_digits(bytes, i + 2, |b| b == b'0' || b == b'1'),
            _ => {}
        }
    }

    let digit_start = i;
    while i < len && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
        i += 1;
    }
    let digit_count = i - digit_start;

    if digit_count == 4
        && bytes.get(i) == Some(&b'-')
        && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)
    {
        return scan_date_time(bytes, digit_start);
    }
    if digit_count == 2
        && bytes.get(i) == Some(&b':')
        && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)
    {
        return scan_time(bytes, digit_start);
    }

    if bytes.get(i) == Some(&b'.') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
        i += 1;
        while i < len && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
            i += 1;
        }
    }
    if matches!(bytes.get(i), Some(b'e') | Some(b'E')) {
        let mut j = i + 1;
        if matches!(bytes.get(j), Some(b'+') | Some(b'-')) {
            j += 1;
        }
        if bytes.get(j).is_some_and(u8::is_ascii_digit) {
            i = j;
            while i < len && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
                i += 1;
            }
        }
    }
    i
}

fn scan_radix_digits(bytes: &[u8], mut i: usize, is_digit: impl Fn(u8) -> bool) -> usize {
    let len = bytes.len();
    while i < len && (is_digit(bytes[i]) || bytes[i] == b'_') {
        i += 1;
    }
    i
}

/// A full date, from its first digit, with an optional attached time
/// (`1979-05-27` or `1979-05-27T07:32:00Z`).
fn scan_date_time(bytes: &[u8], start: usize) -> usize {
    let mut i = consume_digits(bytes, start, 4);
    if bytes.get(i) != Some(&b'-') {
        return i;
    }
    i = consume_digits(bytes, i + 1, 2);
    if bytes.get(i) != Some(&b'-') {
        return i;
    }
    i = consume_digits(bytes, i + 1, 2);

    if matches!(bytes.get(i), Some(b'T') | Some(b't') | Some(b' '))
        && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)
    {
        i = scan_time(bytes, i + 1);
    }
    i
}

/// A time, from its first digit (`07:32:00`, optionally with fractional
/// seconds and a `Z`/`±HH:MM` offset).
fn scan_time(bytes: &[u8], start: usize) -> usize {
    let len = bytes.len();
    let mut i = consume_digits(bytes, start, 2);
    if bytes.get(i) != Some(&b':') {
        return i;
    }
    i = consume_digits(bytes, i + 1, 2);
    if bytes.get(i) != Some(&b':') {
        return i;
    }
    i = consume_digits(bytes, i + 1, 2);

    if bytes.get(i) == Some(&b'.') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
        i += 1;
        while i < len && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }

    match bytes.get(i) {
        Some(b'Z') | Some(b'z') => i + 1,
        Some(b'+') | Some(b'-') => {
            let offset_start = i;
            let after_hours = consume_digits(bytes, i + 1, 2);
            if bytes.get(after_hours) == Some(&b':') {
                consume_digits(bytes, after_hours + 1, 2)
            } else {
                offset_start // not a well-formed offset; leave it unconsumed
            }
        }
        _ => i,
    }
}

/// Up to `max` ASCII digits from `start`.
fn consume_digits(bytes: &[u8], start: usize, max: usize) -> usize {
    let len = bytes.len();
    let mut i = start;
    let mut count = 0;
    while i < len && count < max && bytes[i].is_ascii_digit() {
        i += 1;
        count += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        TomlLexer.lex_line(line, state)
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
        let line = "key = 1 # trailing note\n";
        let spans = tokens(line);
        let at = line.find('#').unwrap();
        assert!(spans.contains(&(at..line.len(), Token::Comment)));
    }

    #[test]
    fn a_whole_line_comment_is_still_a_comment() {
        let line = "# just a comment\n";
        let spans = tokens(line);
        assert!(spans.contains(&(0..line.len(), Token::Comment)));
    }

    #[test]
    fn a_basic_string_value_colours_as_a_string() {
        let line = "greeting = \"hello\"\n";
        let spans = tokens(line);
        let start = line.find('"').unwrap();
        let end = line.rfind('"').unwrap() + 1;
        assert!(spans.contains(&(start..end, Token::String)));
    }

    #[test]
    fn an_escaped_quote_does_not_end_a_basic_string() {
        let line = "s = \"a\\\"b\"\n";
        let spans = tokens(line);
        let start = line.find('"').unwrap();
        let end = line.rfind('"').unwrap() + 1;
        assert!(spans.contains(&(start..end, Token::String)));
    }

    #[test]
    fn a_literal_string_ignores_backslashes() {
        let line = "path = 'C:\\Users\\x'\n";
        let spans = tokens(line);
        let start = line.find('\'').unwrap();
        let end = line.rfind('\'').unwrap() + 1;
        assert!(spans.contains(&(start..end, Token::String)));
    }

    #[test]
    fn a_multiline_basic_string_spans_lines_and_resumes() {
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
    fn a_multiline_literal_string_spans_lines_and_resumes() {
        let (spans1, state) = lex("s = '''start\n", LineState::INITIAL);
        assert!(spans1.iter().any(|(_, t)| *t == Token::String));
        assert_ne!(state, LineState::INITIAL);

        let (spans2, state2) = lex("c:\\still\\going\n", state);
        assert_eq!(
            spans2,
            vec![(0.."c:\\still\\going\n".len(), Token::String)],
            "no escapes in a literal string"
        );

        let (spans3, state3) = lex("end'''\n", state2);
        assert!(spans3.contains(&(0.."end'''".len(), Token::String)));
        assert_eq!(state3, LineState::INITIAL);
    }

    #[test]
    fn bare_keys_are_properties() {
        let line = "my-key_1 = 1\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "my-key_1", line), Some(&Token::Property));
    }

    #[test]
    fn dotted_keys_have_punctuation_dots_and_property_segments() {
        let line = "a.b.c = 1\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "a", line), Some(&Token::Property));
        assert_eq!(find(&spans, "c", line), Some(&Token::Property));
        let dots: Vec<usize> = line.match_indices('.').map(|(i, _)| i).collect();
        for dot in dots {
            assert!(spans.contains(&(dot..dot + 1, Token::Punctuation)));
        }
    }

    #[test]
    fn a_quoted_key_is_a_property() {
        let line = "\"a key\" = 1\n";
        let spans = tokens(line);
        let end = line.find("\" =").unwrap() + 1;
        assert!(spans.contains(&(0..end, Token::Property)));
    }

    #[test]
    fn equals_is_an_operator() {
        let line = "x = 1\n";
        let spans = tokens(line);
        let at = line.find('=').unwrap();
        assert!(spans.contains(&(at..at + 1, Token::Operator)));
    }

    #[test]
    fn a_table_header_is_namespace_and_punctuation() {
        let line = "[a.b]\n";
        let spans = tokens(line);
        assert!(spans.contains(&(0..1, Token::Punctuation)));
        assert_eq!(find(&spans, "a", line), Some(&Token::Namespace));
        assert_eq!(find(&spans, "b", line), Some(&Token::Namespace));
        let dot = line.find('.').unwrap();
        assert!(spans.contains(&(dot..dot + 1, Token::Punctuation)));
        let close = line.find(']').unwrap();
        assert!(spans.contains(&(close..close + 1, Token::Punctuation)));
    }

    #[test]
    fn an_array_of_tables_header_uses_double_brackets() {
        let line = "[[fruits]]\n";
        let spans = tokens(line);
        assert!(spans.contains(&(0..2, Token::Punctuation)));
        assert_eq!(find(&spans, "fruits", line), Some(&Token::Namespace));
        let close = line.find("]]").unwrap();
        assert!(spans.contains(&(close..close + 2, Token::Punctuation)));
    }

    #[test]
    fn a_quoted_table_header_segment_is_a_namespace() {
        let line = "[\"weird key\"]\n";
        let spans = tokens(line);
        let end = line.rfind('"').unwrap() + 1;
        let start = line.find('"').unwrap();
        assert!(spans.contains(&(start..end, Token::Namespace)));
    }

    #[test]
    fn booleans_are_constants() {
        let line = "a = true\nb = false\n";
        for chunk in line.split_inclusive('\n') {
            let spans = tokens(chunk);
            if chunk.contains("true") {
                assert_eq!(find(&spans, "true", chunk), Some(&Token::Constant));
            } else {
                assert_eq!(find(&spans, "false", chunk), Some(&Token::Constant));
            }
        }
    }

    #[test]
    fn integers_include_underscores_and_radix_prefixes() {
        let line = "a = 1_000\nb = 0xFF\nc = 0o17\nd = 0b1010\ne = -5\n";
        for chunk in line.split_inclusive('\n') {
            let spans = tokens(chunk);
            let value = chunk.split_once('=').unwrap().1.trim_end();
            assert_eq!(
                find(&spans, value.trim(), chunk),
                Some(&Token::Number),
                "{chunk:?}"
            );
        }
    }

    #[test]
    fn floats_include_exponents() {
        let line = "a = 3.14\nb = 6.02e23\nc = +1.5e-3\n";
        for chunk in line.split_inclusive('\n') {
            let spans = tokens(chunk);
            let value = chunk.split_once('=').unwrap().1.trim_end();
            assert_eq!(
                find(&spans, value.trim(), chunk),
                Some(&Token::Number),
                "{chunk:?}"
            );
        }
    }

    #[test]
    fn special_floats_are_numbers() {
        let line = "a = inf\nb = -inf\nc = nan\n";
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
    fn a_datetime_is_a_number() {
        let line = "created = 1979-05-27T07:32:00Z\n";
        let spans = tokens(line);
        assert_eq!(
            find(&spans, "1979-05-27T07:32:00Z", line),
            Some(&Token::Number)
        );
    }

    #[test]
    fn a_local_date_and_local_time_are_numbers() {
        let line = "d = 1979-05-27\nt = 07:32:00\n";
        for chunk in line.split_inclusive('\n') {
            let spans = tokens(chunk);
            let value = chunk.split_once('=').unwrap().1.trim_end();
            assert_eq!(
                find(&spans, value.trim(), chunk),
                Some(&Token::Number),
                "{chunk:?}"
            );
        }
    }

    #[test]
    fn a_datetime_with_a_fractional_second_and_offset_is_one_span() {
        let line = "t = 1979-05-27T00:32:00.999-07:00\n";
        let spans = tokens(line);
        assert_eq!(
            find(&spans, "1979-05-27T00:32:00.999-07:00", line),
            Some(&Token::Number)
        );
    }

    #[test]
    fn array_and_inline_table_punctuation() {
        let line = "a = [1, 2]\nb = {x = 1}\n";
        for chunk in line.split_inclusive('\n') {
            let spans = tokens(chunk);
            for ch in ['[', ']', '{', '}', ','] {
                if let Some(at) = chunk.find(ch) {
                    assert!(
                        spans.contains(&(at..at + 1, Token::Punctuation)),
                        "{ch:?} in {chunk:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn broken_source_still_lexes_what_it_can() {
        let spans = tokens("key = \n");
        assert!(spans.iter().any(|(_, t)| *t == Token::Property));
        assert!(spans.iter().any(|(_, t)| *t == Token::Operator));
    }

    #[test]
    fn spans_never_cross_the_lines_length() {
        let line = "[a]\nkey = \"value\" # note\n1979-05-27T07:32:00Z\n";
        for chunk in line.split_inclusive('\n') {
            for (range, _) in tokens(chunk) {
                assert!(range.end <= chunk.len());
            }
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
