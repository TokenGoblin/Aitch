//! A hand-written YAML lexer: comments, mapping keys, block scalars (`|`
//! literal and `>` folded — the one real cross-line construct here, tracked
//! the same way `markdown.rs` tracks a fenced code block, by remembering one
//! column in [`LineState`]), anchors/aliases/tags, quoted and plain
//! scalars, list markers, and flow-collection punctuation.
//!
//! No syntax tree, so nesting inside flow collections (`[...]`/`{...}`) and
//! nested block mappings/sequences is not tracked precisely — see
//! `syntax.rs`'s module docs for what a line-state lexer trades away in
//! general. Design calls, since YAML's own vocabulary needed mapping onto
//! [`Token`]'s short shared list:
//! - A key (a bareword or quoted string, immediately followed by `:` and
//!   then a space, end of line, or nothing) → [`Token::Property`]. Only
//!   recognized at the very start of a line's content, after its
//!   indentation and any list-marker dashes — a flow mapping's `{a: 1}`
//!   does not get `a` coloured as a key, just the surrounding punctuation.
//! - `|`/`>` block scalar headers (with any chomping/explicit-indent
//!   indicators) → [`Token::Operator`]. Their content is coloured
//!   [`Token::String`] line by line for as long as the carried
//!   [`LineState`] says we're still inside one; a blank line inside is left
//!   unstyled (nothing to colour) but still keeps that state.
//! - Anchors (`&name`) and aliases (`*name`) → [`Token::Variable`]; tags
//!   (`!!str`, `!Custom`) → [`Token::Type`].
//! - Quoted scalars (`'...'`, `"..."`) → [`Token::String`]. Single-quote's
//!   only escape is `''`, for a literal `'`; double-quote uses `\`. Neither
//!   is tracked across a line boundary the way a block scalar (or Rust's
//!   strings) is — an unterminated one simply runs to the end of its own
//!   line.
//! - `true`/`false`/`yes`/`no`/`on`/`off`/`null`/`~` (case-insensitive) →
//!   [`Token::Constant`]; a bare integer or float → [`Token::Number`]. Any
//!   other plain scalar is left unstyled — YAML's plain scalars are
//!   genuinely just text.
//! - A `-` list marker (followed by a space or end of line) →
//!   [`Token::Punctuation`], same as a flow collection's
//!   `[`/`]`/`{`/`}`/`,`/`:`.
//! - `---`/`...` document markers, alone at column 0 → [`Token::Punctuation`].

use std::ops::Range;

use super::{Lexer, LineState, Token};

pub(crate) struct YamlLexer;

// `LineState`'s bit layout for YAML: bit 31 set means "inside a block
// scalar" (opened by a `|` or `>` header), and the low byte is the
// leading-space count of the line that header appeared on. A later line
// stays part of the scalar while its own leading-space count is greater
// than that (or it is blank); anything else ends it. This is a
// simplification of YAML's real rule (which measures from the key's
// column, not the raw line start) — see the block-scalar tests below for
// what it does and does not get right. Zero means "ordinary line", the
// same as `LineState::INITIAL`.
const BLOCK_SCALAR_FLAG: u32 = 1 << 31;
const INDENT_MASK: u32 = 0xFF;

fn block_scalar_state(indent: usize) -> LineState {
    LineState(BLOCK_SCALAR_FLAG | (indent.min(INDENT_MASK as usize) as u32))
}

const CONSTANTS: &[&str] = &["true", "false", "yes", "no", "on", "off", "null", "~"];

impl Lexer for YamlLexer {
    fn lex_line(&self, line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        if state.0 & BLOCK_SCALAR_FLAG != 0 {
            let parent_indent = (state.0 & INDENT_MASK) as usize;
            let indent = leading_spaces(line.as_bytes());
            let is_blank = line.trim_end_matches(['\n', '\r']).trim().is_empty();
            if is_blank {
                return (Vec::new(), state);
            }
            if indent > parent_indent {
                return (vec![(0..line.len(), Token::String)], state);
            }
            // Equal or lesser indentation: the scalar ended before this
            // line, which falls through to ordinary lexing below.
        }

        lex_ordinary_line(line)
    }
}

fn leading_spaces(bytes: &[u8]) -> usize {
    bytes.iter().take_while(|&&b| b == b' ').count()
}

/// Whether the byte at `i` is absent, or ends a token the way whitespace or
/// a line break does — used both for "is this `:` a key's colon" and "is
/// this `-` a list marker".
fn ends_token(bytes: &[u8], i: usize) -> bool {
    matches!(bytes.get(i), None | Some(b' ') | Some(b'\n') | Some(b'\r'))
}

fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Bytes that stop a plain scalar's run, each handled by its own branch in
/// [`lex_ordinary_line`]'s main loop.
fn is_indicator(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\t'
            | b'\n'
            | b'\r'
            | b'#'
            | b','
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'\''
            | b'"'
            | b':'
            | b'&'
            | b'*'
            | b'!'
            | b'|'
            | b'>'
    )
}

/// From `start` (an opening `'`), the byte offset just past the matching
/// close. `''` inside is an escaped literal `'`, not a close. `None` if
/// unterminated on this line.
fn scan_single_quoted(bytes: &[u8], start: usize) -> Option<usize> {
    let len = bytes.len();
    let mut i = start + 1;
    while i < len {
        if bytes[i] == b'\'' {
            if bytes.get(i + 1) == Some(&b'\'') {
                i += 2;
                continue;
            }
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

/// From `start` (an opening `"`), the byte offset just past the matching
/// close, honouring `\`-escapes. `None` if unterminated on this line.
fn scan_double_quoted(bytes: &[u8], start: usize) -> Option<usize> {
    let len = bytes.len();
    let mut i = start + 1;
    while i < len {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// A key at the very start of an entry: a bareword or quoted string
/// immediately followed by `:` and then a space, end of line, or nothing.
/// `None` if there is no such colon on this line — a value with no key, or
/// a colon that is not a key's (e.g. inside a URL, where it is followed by
/// something other than whitespace/end-of-line).
fn scan_key(bytes: &[u8], start: usize) -> Option<(Range<usize>, usize)> {
    let len = bytes.len();
    if start >= len {
        return None;
    }
    if bytes[start] == b'\'' || bytes[start] == b'"' {
        let quote = bytes[start];
        let end = if quote == b'\'' {
            scan_single_quoted(bytes, start)?
        } else {
            scan_double_quoted(bytes, start)?
        };
        if bytes.get(end) == Some(&b':') && ends_token(bytes, end + 1) {
            return Some((start..end, end + 1));
        }
        return None;
    }

    let mut i = start;
    while i < len {
        match bytes[i] {
            b':' if ends_token(bytes, i + 1) => {
                let mut key_end = i;
                while key_end > start && bytes[key_end - 1] == b' ' {
                    key_end -= 1;
                }
                if key_end == start {
                    return None;
                }
                return Some((start..key_end, i + 1));
            }
            b'\n' | b'\r' | b'#' => return None,
            _ => i += 1,
        }
    }
    None
}

fn classify_plain(word: &str) -> Option<Token> {
    if CONSTANTS.contains(&word.to_ascii_lowercase().as_str()) {
        return Some(Token::Constant);
    }
    None
}

/// From `start`, a run of non-indicator bytes (a plain scalar chunk),
/// advancing by whole characters so it never splits a multibyte one.
fn scan_plain_word(line: &str, bytes: &[u8], start: usize) -> usize {
    let len = bytes.len();
    let mut i = start;
    while i < len && !is_indicator(bytes[i]) {
        i += line[i..].chars().next().map_or(1, char::len_utf8);
    }
    i
}

/// A signed integer or float: optional `+`/`-`, then `0x`/`0o` (hex/octal)
/// or decimal digits with an optional `.` fraction and `e`/`E` exponent.
/// `None` if what follows isn't cleanly a number (e.g. `123abc`), checked
/// by requiring an indicator byte (or end of line) right after it — left
/// to plain-scalar scanning instead.
fn scan_number(bytes: &[u8], start: usize) -> Option<usize> {
    let len = bytes.len();
    let mut i = start;
    if matches!(bytes.get(i), Some(b'+') | Some(b'-')) {
        i += 1;
    }
    if bytes.get(i) == Some(&b'0') && matches!(bytes.get(i + 1), Some(b'x') | Some(b'X')) {
        let digits_start = i + 2;
        let mut j = digits_start;
        while j < len && bytes[j].is_ascii_hexdigit() {
            j += 1;
        }
        if j == digits_start || (j < len && !is_indicator(bytes[j])) {
            return None;
        }
        return Some(j);
    }
    if bytes.get(i) == Some(&b'0') && bytes.get(i + 1) == Some(&b'o') {
        let digits_start = i + 2;
        let mut j = digits_start;
        while j < len && (b'0'..=b'7').contains(&bytes[j]) {
            j += 1;
        }
        if j == digits_start || (j < len && !is_indicator(bytes[j])) {
            return None;
        }
        return Some(j);
    }

    let mut saw_digit = false;
    while i < len && bytes[i].is_ascii_digit() {
        i += 1;
        saw_digit = true;
    }
    if bytes.get(i) == Some(&b'.') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
        i += 1;
        while i < len && bytes[i].is_ascii_digit() {
            i += 1;
        }
        saw_digit = true;
    }
    if !saw_digit {
        return None;
    }
    if matches!(bytes.get(i), Some(b'e') | Some(b'E')) {
        let mut j = i + 1;
        if matches!(bytes.get(j), Some(b'+') | Some(b'-')) {
            j += 1;
        }
        if bytes.get(j).is_some_and(u8::is_ascii_digit) {
            j += 1;
            while j < len && bytes[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    if i < len && !is_indicator(bytes[i]) {
        return None;
    }
    Some(i)
}

/// Lex one line assuming it starts an ordinary (non-block-scalar) context —
/// the only case [`YamlLexer::lex_line`] doesn't handle directly itself.
fn lex_ordinary_line(line: &str) -> (Vec<(Range<usize>, Token)>, LineState) {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut spans = Vec::new();
    let indent = leading_spaces(bytes);
    let mut i = indent;

    if indent == 0 && (bytes.starts_with(b"---") || bytes.starts_with(b"...")) {
        let end = 3;
        if ends_token(bytes, end) || bytes.get(end) == Some(&b'#') {
            spans.push((0..end, Token::Punctuation));
            i = end;
        }
    }

    if i == indent {
        // No document marker matched (or this line was indented, which one
        // never is): list-marker dashes, then a key, may open this entry.
        loop {
            while i < len && bytes[i] == b' ' {
                i += 1;
            }
            if bytes.get(i) == Some(&b'-') && ends_token(bytes, i + 1) {
                spans.push((i..i + 1, Token::Punctuation));
                i += 1;
            } else {
                break;
            }
        }
        while i < len && bytes[i] == b' ' {
            i += 1;
        }
        if let Some((key_range, after)) = scan_key(bytes, i) {
            spans.push((key_range, Token::Property));
            i = after;
        }
    }

    while i < len {
        let byte = bytes[i];
        match byte {
            b' ' | b'\t' => i += 1,
            b'#' => {
                spans.push((i..len, Token::Comment));
                i = len;
            }
            b'&' | b'*' => {
                let start = i;
                i += 1;
                while i < len && is_word_char(bytes[i]) {
                    i += 1;
                }
                spans.push((start..i, Token::Variable));
            }
            b'!' => {
                let start = i;
                i += 1;
                if bytes.get(i) == Some(&b'!') {
                    i += 1;
                }
                while i < len && (is_word_char(bytes[i]) || bytes[i] == b':' || bytes[i] == b'/') {
                    i += 1;
                }
                spans.push((start..i, Token::Type));
            }
            b'\'' => {
                let start = i;
                match scan_single_quoted(bytes, i) {
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
            b'"' => {
                let start = i;
                match scan_double_quoted(bytes, i) {
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
            b'|' | b'>' => {
                let start = i;
                i += 1;
                while i < len && matches!(bytes[i], b'+' | b'-' | b'0'..=b'9') {
                    i += 1;
                }
                spans.push((start..i, Token::Operator));
                let mut j = i;
                while j < len && bytes[j] == b' ' {
                    j += 1;
                }
                if bytes.get(j) == Some(&b'#') {
                    spans.push((j..len, Token::Comment));
                }
                return (spans, block_scalar_state(indent));
            }
            b',' | b'[' | b']' | b'{' | b'}' | b':' => {
                spans.push((i..i + 1, Token::Punctuation));
                i += 1;
            }
            b'-' | b'+' | b'0'..=b'9' => match scan_number(bytes, i) {
                Some(end) => {
                    spans.push((i..end, Token::Number));
                    i = end;
                }
                None => {
                    let end = scan_plain_word(line, bytes, i);
                    if let Some(token) = classify_plain(&line[i..end]) {
                        spans.push((i..end, token));
                    }
                    i = end;
                }
            },
            _ => {
                let end = scan_plain_word(line, bytes, i);
                if end == i {
                    // A stray indicator byte no branch above claimed (only
                    // reachable for a bare `\n`/`\r`): skip it.
                    i += 1;
                } else {
                    if let Some(token) = classify_plain(&line[i..end]) {
                        spans.push((i..end, token));
                    }
                    i = end;
                }
            }
        }
    }

    (spans, LineState::INITIAL)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        YamlLexer.lex_line(line, state)
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
        let line = "# just a comment\n";
        let spans = tokens(line);
        assert_eq!(spans, vec![(0..line.len(), Token::Comment)]);
    }

    #[test]
    fn a_trailing_comment_after_a_value_is_still_a_comment() {
        let line = "name: value # trailing\n";
        let spans = tokens(line);
        let at = line.find('#').unwrap();
        assert!(spans.contains(&(at..line.len(), Token::Comment)));
    }

    #[test]
    fn a_key_with_a_scalar_value() {
        let line = "name: Aitch\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "name", line), Some(&Token::Property));
    }

    #[test]
    fn a_quoted_key_is_still_a_property() {
        let line = "'a key': 1\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "'a key'", line), Some(&Token::Property));
    }

    #[test]
    fn a_colon_that_is_not_a_key_is_left_alone() {
        // No space (or end of line) after the colon, so this is not a key —
        // notably a URL should not be misread as one.
        let line = "- http://example.com\n";
        let spans = tokens(line);
        assert!(!spans.iter().any(|(_, t)| *t == Token::Property));
    }

    #[test]
    fn a_literal_block_scalar_spans_lines_and_resumes() {
        let (spans0, state0) = lex("body: |\n", LineState::INITIAL);
        assert!(find(&spans0, "body", "body: |\n") == Some(&Token::Property));
        assert_ne!(state0, LineState::INITIAL, "still inside the block scalar");

        let (spans1, state1) = lex("  first line\n", state0);
        assert_eq!(spans1, vec![(0.."  first line\n".len(), Token::String)]);
        assert_eq!(state1, state0, "still inside, same remembered indent");

        let (spans2, state2) = lex("  second line\n", state1);
        assert_eq!(spans2, vec![(0.."  second line\n".len(), Token::String)]);
        assert_eq!(state2, state0);

        // Back at the original (lesser-or-equal) indentation: the scalar
        // has ended, and this is lexed as an ordinary key again.
        let (spans3, state3) = lex("next: value\n", state2);
        assert_eq!(state3, LineState::INITIAL);
        assert_eq!(
            find(&spans3, "next", "next: value\n"),
            Some(&Token::Property)
        );
        assert!(!spans3.iter().any(|(_, t)| *t == Token::String));
    }

    #[test]
    fn a_folded_block_scalar_behaves_the_same_as_literal() {
        let (_, state0) = lex("body: >\n", LineState::INITIAL);
        assert_ne!(state0, LineState::INITIAL);
        let (spans1, state1) = lex("  folded content\n", state0);
        assert_eq!(spans1, vec![(0.."  folded content\n".len(), Token::String)]);
        assert_eq!(state1, state0);

        let (spans2, state2) = lex("after: value\n", state1);
        assert_eq!(state2, LineState::INITIAL);
        assert_eq!(
            find(&spans2, "after", "after: value\n"),
            Some(&Token::Property)
        );
    }

    #[test]
    fn a_blank_line_inside_a_block_scalar_stays_inside_it() {
        let (_, state0) = lex("body: |\n", LineState::INITIAL);
        let (blank_spans, state1) = lex("\n", state0);
        assert!(blank_spans.is_empty());
        assert_eq!(state1, state0, "a blank line does not end the scalar");

        let (spans2, state2) = lex("  more\n", state1);
        assert_eq!(spans2, vec![(0.."  more\n".len(), Token::String)]);
        assert_eq!(state2, state0);
    }

    #[test]
    fn a_chomping_indicator_on_a_block_scalar_header_is_still_recognized() {
        let (spans0, state0) = lex("body: |-\n", LineState::INITIAL);
        assert!(spans0
            .iter()
            .any(|(r, t)| *t == Token::Operator && r.start == "body: ".len()));
        assert_ne!(state0, LineState::INITIAL);
    }

    #[test]
    fn an_anchor_and_an_alias() {
        let line1 = "base: &anchor value\n";
        let spans1 = tokens(line1);
        assert_eq!(find(&spans1, "&anchor", line1), Some(&Token::Variable));

        let line2 = "ref: *anchor\n";
        let spans2 = tokens(line2);
        assert_eq!(find(&spans2, "*anchor", line2), Some(&Token::Variable));
    }

    #[test]
    fn a_tag_is_a_type() {
        let line = "kind: !!str hello\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "!!str", line), Some(&Token::Type));
    }

    #[test]
    fn a_custom_tag_is_also_a_type() {
        let line = "point: !Point { x: 1, y: 2 }\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "!Point", line), Some(&Token::Type));
    }

    #[test]
    fn a_single_quoted_string_with_its_escape() {
        let line = "s: 'it''s here'\n";
        let spans = tokens(line);
        let start = line.find('\'').unwrap();
        let end = line.rfind('\'').unwrap() + 1;
        assert!(spans.contains(&(start..end, Token::String)));
    }

    #[test]
    fn a_double_quoted_string_with_an_escape() {
        let line = "s: \"a\\\"b\"\n";
        let spans = tokens(line);
        let start = line.find('"').unwrap();
        let end = line.rfind('"').unwrap() + 1;
        assert!(spans.contains(&(start..end, Token::String)));
    }

    #[test]
    fn booleans_and_null_are_constants() {
        for (line, word) in [
            ("a: true\n", "true"),
            ("b: false\n", "false"),
            ("c: yes\n", "yes"),
            ("d: no\n", "no"),
            ("e: on\n", "on"),
            ("f: off\n", "off"),
            ("g: null\n", "null"),
            ("h: ~\n", "~"),
            ("i: TRUE\n", "TRUE"),
        ] {
            let spans = tokens(line);
            assert_eq!(find(&spans, word, line), Some(&Token::Constant), "{line}");
        }
    }

    #[test]
    fn a_bare_number_is_a_number() {
        for (line, word) in [
            ("a: 42\n", "42"),
            ("b: -3.14\n", "-3.14"),
            ("c: 0x1F\n", "0x1F"),
            ("d: 1e10\n", "1e10"),
        ] {
            let spans = tokens(line);
            assert_eq!(find(&spans, word, line), Some(&Token::Number), "{line}");
        }
    }

    #[test]
    fn an_ordinary_plain_word_is_left_unstyled() {
        let line = "name: Aitch\n";
        let spans = tokens(line);
        assert!(!spans
            .iter()
            .any(|(r, _)| r.start == line.find("Aitch").unwrap()));
    }

    #[test]
    fn a_list_marker_is_punctuation() {
        let line = "- item one\n";
        let spans = tokens(line);
        assert!(spans.contains(&(0..1, Token::Punctuation)));
    }

    #[test]
    fn a_dash_not_followed_by_a_space_is_not_a_list_marker() {
        let line = "value: -3\n";
        let spans = tokens(line);
        assert!(!spans.contains(&("value: ".len().."value: ".len() + 1, Token::Punctuation)));
        assert_eq!(find(&spans, "-3", line), Some(&Token::Number));
    }

    #[test]
    fn document_markers_are_punctuation() {
        assert_eq!(tokens("---\n"), vec![(0..3, Token::Punctuation)]);
        assert_eq!(tokens("...\n"), vec![(0..3, Token::Punctuation)]);
    }

    #[test]
    fn flow_collection_punctuation() {
        let line = "vals: [1, 2, {a: 3}]\n";
        let spans = tokens(line);
        let open = line.find('[').unwrap();
        assert!(spans.contains(&(open..open + 1, Token::Punctuation)));
        let comma = line.find(',').unwrap();
        assert!(spans.contains(&(comma..comma + 1, Token::Punctuation)));
        let brace = line.find('{').unwrap();
        assert!(spans.contains(&(brace..brace + 1, Token::Punctuation)));
    }

    #[test]
    fn spans_never_cross_the_lines_length() {
        let line = "key: [1, 'two', &a three] # done\n";
        for (range, _) in tokens(line) {
            assert!(range.end <= line.len());
        }
    }

    #[test]
    fn multibyte_text_never_produces_a_span_that_is_not_at_a_char_boundary() {
        let text = "# naïve café → 日本語\nname: 日本語 value\n";
        let mut state = LineState::INITIAL;
        for chunk in text.split_inclusive('\n') {
            let (spans, next_state) = lex(chunk, state);
            for (range, _) in spans {
                let _ = &chunk[range]; // panics if it split a character
            }
            state = next_state;
        }
    }
}
