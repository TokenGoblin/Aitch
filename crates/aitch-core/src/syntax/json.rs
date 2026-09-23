//! A hand-written JSON (and JSONC — `Language::from_path` already treats
//! `.jsonc` as this language) lexer: strings, split into object keys
//! ([`Token::Property`]) and everything else ([`Token::String`]) by whether
//! a `:` follows; numbers; `true`/`false`/`null`; and `//`/`/* */` comments,
//! which strict JSON does not have but JSONC does.

use std::ops::Range;

use super::{Lexer, LineState, Token};

pub(crate) struct JsonLexer;

const NORMAL: LineState = LineState(0);
const IN_COMMENT: LineState = LineState(1);
const IN_STRING: LineState = LineState(2);

impl Lexer for JsonLexer {
    fn lex_line(&self, line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        let bytes = line.as_bytes();
        let len = bytes.len();
        let mut spans = Vec::new();
        let mut i = 0usize;

        if state == IN_COMMENT {
            match find_comment_end(bytes, 0) {
                Some(end) => {
                    spans.push((0..end, Token::Comment));
                    i = end;
                }
                None => return (vec![(0..len, Token::Comment)], IN_COMMENT),
            }
        } else if state == IN_STRING {
            match find_string_end(bytes, 0) {
                Some(end) => {
                    spans.push((0..end, string_token(bytes, end)));
                    i = end;
                }
                None => return (vec![(0..len, Token::String)], IN_STRING),
            }
        }

        while i < len {
            match bytes[i] {
                b'/' if bytes.get(i + 1) == Some(&b'/') => {
                    spans.push((i..len, Token::Comment));
                    i = len;
                }
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    let start = i;
                    match find_comment_end(bytes, i + 2) {
                        Some(end) => {
                            spans.push((start..end, Token::Comment));
                            i = end;
                        }
                        None => {
                            spans.push((start..len, Token::Comment));
                            return (spans, IN_COMMENT);
                        }
                    }
                }
                b'"' => {
                    let start = i;
                    match find_string_end(bytes, i + 1) {
                        Some(end) => {
                            spans.push((start..end, string_token(bytes, end)));
                            i = end;
                        }
                        None => {
                            spans.push((start..len, Token::String));
                            return (spans, IN_STRING);
                        }
                    }
                }
                b'-' | b'0'..=b'9' => {
                    let start = i;
                    i = scan_number(bytes, i);
                    spans.push((start..i, Token::Number));
                }
                b't' if line[i..].starts_with("true") => {
                    spans.push((i..i + 4, Token::Constant));
                    i += 4;
                }
                b'f' if line[i..].starts_with("false") => {
                    spans.push((i..i + 5, Token::Constant));
                    i += 5;
                }
                b'n' if line[i..].starts_with("null") => {
                    spans.push((i..i + 4, Token::Constant));
                    i += 4;
                }
                b'{' | b'}' | b'[' | b']' | b',' | b':' => {
                    spans.push((i..i + 1, Token::Punctuation));
                    i += 1;
                }
                _ => i += 1,
            }
        }

        (spans, NORMAL)
    }
}

/// From just after an opening `"` (or the start of a line already inside
/// one), the byte past a closing `"`. `None` if it is still open.
fn find_string_end(bytes: &[u8], mut i: usize) -> Option<usize> {
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

/// A key if a `:` (skipping spaces and tabs) follows the string that just
/// closed at `end`; a plain value string otherwise.
fn string_token(bytes: &[u8], end: usize) -> Token {
    let len = bytes.len();
    let mut j = end;
    while j < len && matches!(bytes[j], b' ' | b'\t') {
        j += 1;
    }
    if bytes.get(j) == Some(&b':') {
        Token::Property
    } else {
        Token::String
    }
}

fn find_comment_end(bytes: &[u8], mut i: usize) -> Option<usize> {
    let len = bytes.len();
    while i + 1 < len {
        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

fn scan_number(bytes: &[u8], mut i: usize) -> usize {
    let len = bytes.len();
    if bytes[i] == b'-' {
        i += 1;
    }
    while i < len && matches!(bytes[i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-') {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        JsonLexer.lex_line(line, state)
    }

    fn tokens(line: &str) -> Vec<(Range<usize>, Token)> {
        lex(line, LineState::INITIAL).0
    }

    #[test]
    fn a_key_and_its_string_value_are_told_apart() {
        let line = "{\"name\": \"aitch\"}\n";
        let spans = tokens(line);
        let key = line.find("\"name\"").unwrap();
        assert!(spans.contains(&(key..key + 6, Token::Property)));
        let value = line.find("\"aitch\"").unwrap();
        assert!(spans.contains(&(value..value + 7, Token::String)));
    }

    #[test]
    fn a_key_with_no_space_before_the_colon_is_still_a_property() {
        let line = "{\"x\":1}\n";
        let spans = tokens(line);
        let key = line.find("\"x\"").unwrap();
        assert!(spans.contains(&(key..key + 3, Token::Property)));
    }

    #[test]
    fn numbers_include_signs_decimals_and_exponents() {
        let line = "[-1, 2.5, 3e10]\n";
        let spans = tokens(line);
        assert!(spans.contains(&(1..3, Token::Number)));
        assert!(spans.contains(&(5..8, Token::Number)));
        assert!(spans.contains(&(10..14, Token::Number)));
    }

    #[test]
    fn true_false_and_null_are_constants() {
        let line = "[true, false, null]\n";
        let spans = tokens(line);
        assert!(spans.iter().filter(|(_, t)| *t == Token::Constant).count() == 3);
    }

    #[test]
    fn brackets_and_commas_are_punctuation() {
        let spans = tokens("{}\n");
        assert!(spans.contains(&(0..1, Token::Punctuation)));
        assert!(spans.contains(&(1..2, Token::Punctuation)));
    }

    #[test]
    fn a_string_can_span_lines_and_resumes() {
        let (spans1, state) = lex("{\"note\": \"long text\n", LineState::INITIAL);
        assert!(spans1.iter().any(|(_, t)| *t == Token::String));
        assert_ne!(state, LineState::INITIAL);

        let (spans2, state2) = lex("goes on\"}\n", state);
        assert!(spans2.contains(&(0.."goes on\"".len(), Token::String)));
        assert_eq!(state2, LineState::INITIAL);
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string() {
        let line = "\"a\\\"b\"\n";
        let spans = tokens(line);
        assert!(spans.contains(&(0..6, Token::String)));
    }

    #[test]
    fn jsonc_line_and_block_comments_are_supported() {
        let line = "// leading\n{\"a\": 1} /* trailing */\n";
        for chunk in line.split_inclusive('\n') {
            let spans = tokens(chunk);
            if chunk.starts_with("//") {
                assert!(spans.contains(&(0..chunk.len(), Token::Comment)));
            } else {
                assert!(spans.iter().any(|(_, t)| *t == Token::Comment));
            }
        }
    }

    #[test]
    fn a_block_comment_spans_lines_and_resumes() {
        let (spans1, state) = lex("/* start\n", LineState::INITIAL);
        assert!(spans1.iter().any(|(_, t)| *t == Token::Comment));
        assert_ne!(state, LineState::INITIAL);

        let (spans2, state2) = lex("end */ 1\n", state);
        assert!(spans2.contains(&(0.."end */".len(), Token::Comment)));
        assert_eq!(state2, LineState::INITIAL);
        assert!(spans2.iter().any(|(_, t)| *t == Token::Number));
    }

    #[test]
    fn spans_never_cross_the_lines_length() {
        let line = "{\"a\": [1, 2, 3]}\n";
        for (range, _) in tokens(line) {
            assert!(range.end <= line.len());
        }
    }
}
