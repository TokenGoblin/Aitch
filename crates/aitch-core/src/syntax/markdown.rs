//! A hand-written Markdown lexer: ATX headings, fenced code blocks (the one
//! real cross-line construct here), inline code spans, emphasis/strong, and
//! link/image targets. A light touch on blockquote markers; list markers and
//! horizontal rules are left unstyled — see `syntax.rs`'s module docs for
//! what a line-state lexer (no tree, no nesting) trades away in general.
//!
//! Design calls, since Markdown's own vocabulary needed mapping onto
//! [`Token`]'s short shared list:
//! - Headings → [`Token::Heading`], the whole line.
//! - Inline code spans and fenced code *delimiter* lines → [`Token::String`].
//!   Content *inside* a fence is deliberately left with no spans at all
//!   (never coloured as prose) rather than lexed as the fenced language —
//!   `syntax.rs`'s module docs call this out as the accepted trade-off, and
//!   a lexer with no document-wide language table has nothing sensible to
//!   lex fence content as anyway.
//! - Emphasis and strong (`*x*`, `_x_`, `**x**`, `__x__`) → [`Token::Attribute`].
//! - A link/image's URL (inside `(...)`) → [`Token::Constant`]. The bracketed
//!   text/alt is left unstyled — one coloured span per link reads better
//!   than two competing ones.
//! - A blockquote's leading `>` → [`Token::Punctuation`]; nothing fancier.

use std::ops::Range;

use super::{Lexer, LineState, Token};

pub(crate) struct MarkdownLexer;

// `LineState`'s bit layout for Markdown: bit 31 set means "inside a fenced
// code block", bit 30 says which fence character opened it (set = `~~~`,
// clear = ```` ``` ````), and the low byte is the opening fence's length (how
// many characters a close needs to match or exceed). Zero means "ordinary
// prose", the same as `LineState::INITIAL`.
const FENCE_FLAG: u32 = 1 << 31;
const FENCE_TILDE: u32 = 1 << 30;
const FENCE_LEN_MASK: u32 = 0xFF;

fn fence_state(tilde: bool, len: usize) -> LineState {
    let mut value = FENCE_FLAG | (len.min(FENCE_LEN_MASK as usize) as u32);
    if tilde {
        value |= FENCE_TILDE;
    }
    LineState(value)
}

/// `(tilde, opening length)` for a state built by [`fence_state`].
fn decode_fence_state(state: LineState) -> (bool, usize) {
    let tilde = state.0 & FENCE_TILDE != 0;
    let len = (state.0 & FENCE_LEN_MASK) as usize;
    (tilde, len)
}

impl Lexer for MarkdownLexer {
    fn lex_line(&self, line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        if state.0 & FENCE_FLAG != 0 {
            let (open_tilde, open_len) = decode_fence_state(state);
            let open_char = if open_tilde { b'~' } else { b'`' };
            if let Some((ch, count, end)) = fence_delim(line) {
                if ch == open_char && count >= open_len && rest_is_blank(line, end) {
                    // The matching close: back to prose from the next line.
                    return (vec![(0..line.len(), Token::String)], LineState::INITIAL);
                }
            }
            // Still inside the fence: content is deliberately left unstyled.
            return (Vec::new(), state);
        }

        if let Some((ch, count, end)) = fence_delim(line) {
            let valid_open = if ch == b'`' {
                // A backtick fence's info string may not itself contain a
                // backtick (that would read as an inline code span instead).
                !line[end..].trim_end_matches(['\n', '\r']).contains('`')
            } else {
                true
            };
            if valid_open {
                let next_state = fence_state(ch == b'~', count);
                return (vec![(0..line.len(), Token::String)], next_state);
            }
        }

        (lex_prose_line(line), LineState::INITIAL)
    }
}

/// A run of `` ` `` or `~` (3 or more, after up to 3 leading spaces) that
/// could open or close a fence: `(character, run length, byte offset just
/// past the run)`.
fn fence_delim(line: &str) -> Option<(u8, usize, usize)> {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut spaces = 0;
    while i < len && bytes[i] == b' ' && spaces < 3 {
        i += 1;
        spaces += 1;
    }
    let ch = *bytes.get(i)?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let start = i;
    while i < len && bytes[i] == ch {
        i += 1;
    }
    let count = i - start;
    if count < 3 {
        return None;
    }
    Some((ch, count, i))
}

/// Whether everything from `from` to the end of `line` (its own line break
/// aside) is blank — what a fence's closing line requires after its marker.
fn rest_is_blank(line: &str, from: usize) -> bool {
    line[from..]
        .trim_end_matches(['\n', '\r'])
        .chars()
        .all(|c| c == ' ' || c == '\t')
}

fn lex_prose_line(line: &str) -> Vec<(Range<usize>, Token)> {
    if is_atx_heading(line) {
        return vec![(0..line.len(), Token::Heading)];
    }

    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut spans = Vec::new();
    let mut i = 0;

    // A light-touch blockquote marker: colour the `>` itself and carry on
    // lexing the rest of the line as usual.
    {
        let mut j = 0;
        let mut spaces = 0;
        while j < len && bytes[j] == b' ' && spaces < 3 {
            j += 1;
            spaces += 1;
        }
        if bytes.get(j) == Some(&b'>') {
            spans.push((j..j + 1, Token::Punctuation));
            i = j + 1;
        }
    }

    while i < len {
        match bytes[i] {
            b'`' => {
                let start = i;
                let mut j = i;
                while j < len && bytes[j] == b'`' {
                    j += 1;
                }
                let delim_len = j - i;
                match find_code_close(bytes, j, delim_len) {
                    Some(close_start) => {
                        let end = close_start + delim_len;
                        spans.push((start..end, Token::String));
                        i = end;
                    }
                    None => i = j,
                }
            }
            b'*' | b'_' => {
                if let Some(end) = scan_emphasis(bytes, i) {
                    spans.push((i..end, Token::Attribute));
                    i = end;
                } else {
                    i += 1;
                }
            }
            b'!' if bytes.get(i + 1) == Some(&b'[') => match scan_link(line, i + 1) {
                Some((url_range, full_end)) => {
                    spans.push((url_range, Token::Constant));
                    i = full_end;
                }
                None => i += 1,
            },
            b'[' => match scan_link(line, i) {
                Some((url_range, full_end)) => {
                    spans.push((url_range, Token::Constant));
                    i = full_end;
                }
                None => i += 1,
            },
            _ => {
                // `i` is always a char boundary here: every branch above
                // only ever advances it past whole ASCII delimiter runs or
                // matched (also boundary-safe, see `scan_link`/
                // `scan_emphasis`) spans, never into the middle of a
                // multibyte character.
                let ch_len = line[i..].chars().next().map_or(1, char::len_utf8);
                i += ch_len;
            }
        }
    }

    spans
}

/// Up to 3 leading spaces, then 1-6 `#`, then a space/tab, a line break, or
/// nothing (end of file). Anything else — no hashes, more than 6, or a
/// `#` run glued to text — is not a heading.
fn is_atx_heading(line: &str) -> bool {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut spaces = 0;
    while i < len && bytes[i] == b' ' && spaces < 3 {
        i += 1;
        spaces += 1;
    }
    let hash_start = i;
    while i < len && bytes[i] == b'#' && i - hash_start < 6 {
        i += 1;
    }
    if i == hash_start {
        return false;
    }
    matches!(
        bytes.get(i),
        None | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')
    )
}

/// From `i` (just past an opening code-span delimiter run), the byte offset
/// where a closing run of exactly `delim_len` backticks starts. `None` if
/// there is none on this line — an unmatched backtick run is not a code
/// span, per CommonMark, so it is left unstyled rather than run to the end
/// of the line the way an unterminated string would be.
fn find_code_close(bytes: &[u8], mut i: usize, delim_len: usize) -> Option<usize> {
    let len = bytes.len();
    while i < len {
        if bytes[i] == b'`' {
            let start = i;
            while i < len && bytes[i] == b'`' {
                i += 1;
            }
            if i - start == delim_len {
                return Some(start);
            }
        } else {
            i += 1;
        }
    }
    None
}

/// From `i` (an opening `*` or `_`), the byte offset just past a matching
/// closing run of the same length (1 or 2 — longer runs are treated as a
/// bold delimiter, the same as `**`). `None` if unmatched, or if it would
/// only wrap empty content (`**`).
fn scan_emphasis(bytes: &[u8], i: usize) -> Option<usize> {
    let len = bytes.len();
    let marker = bytes[i];
    let mut j = i;
    while j < len && bytes[j] == marker {
        j += 1;
    }
    let delim_len = (j - i).min(2);
    let content_start = i + delim_len;

    let mut k = content_start;
    while k + delim_len <= len {
        if bytes[k..k + delim_len] == bytes[i..i + delim_len] && k > content_start {
            return Some(k + delim_len);
        }
        k += 1;
    }
    None
}

/// A link or image target: from `bracket_start` (the index of a `[`), the
/// URL's byte range inside `(...)` and the offset just past the closing
/// `)`. `None` if this is not `[...](...)` — a reference-style link
/// (`[text][ref]`) or a bare `[` falls through here and is left unstyled.
///
/// Does not handle a nested `[`/`(` inside the text or URL — a pragmatic
/// simplification, same spirit as `rust.rs`'s number scanner not
/// special-casing a signed exponent.
fn scan_link(line: &str, bracket_start: usize) -> Option<(Range<usize>, usize)> {
    let bytes = line.as_bytes();
    let len = bytes.len();
    if bytes.get(bracket_start) != Some(&b'[') {
        return None;
    }
    let mut j = bracket_start + 1;
    while j < len && bytes[j] != b']' && bytes[j] != b'\n' {
        j += 1;
    }
    if bytes.get(j) != Some(&b']') {
        return None;
    }
    let close_bracket = j;
    if bytes.get(close_bracket + 1) != Some(&b'(') {
        return None;
    }
    let paren_start = close_bracket + 1;
    let mut k = paren_start + 1;
    while k < len && bytes[k] != b')' && bytes[k] != b'\n' {
        k += 1;
    }
    if bytes.get(k) != Some(&b')') {
        return None;
    }
    let url_range = paren_start + 1..k;
    Some((url_range, k + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        MarkdownLexer.lex_line(line, state)
    }

    fn tokens(line: &str) -> Vec<(Range<usize>, Token)> {
        lex(line, LineState::INITIAL).0
    }

    #[test]
    fn atx_headings_are_recognized_at_each_level() {
        for level in 1..=6 {
            let line = format!("{} Heading\n", "#".repeat(level));
            let spans = tokens(&line);
            assert_eq!(
                spans,
                vec![(0..line.len(), Token::Heading)],
                "level {level}"
            );
        }
    }

    #[test]
    fn a_heading_with_no_trailing_text_is_still_a_heading() {
        let line = "###\n";
        assert_eq!(tokens(line), vec![(0..line.len(), Token::Heading)]);
    }

    #[test]
    fn seven_hashes_is_not_a_heading() {
        let line = "####### not a heading\n";
        assert!(!tokens(line).iter().any(|(_, t)| *t == Token::Heading));
    }

    #[test]
    fn a_hash_glued_to_text_is_not_a_heading() {
        let line = "#nope\n";
        assert!(!tokens(line).iter().any(|(_, t)| *t == Token::Heading));
    }

    #[test]
    fn inline_code_is_a_string() {
        let line = "Use `code` here\n";
        let spans = tokens(line);
        let at = line.find('`').unwrap();
        let end = line.rfind('`').unwrap() + 1;
        assert!(spans.contains(&(at..end, Token::String)));
    }

    #[test]
    fn an_unterminated_backtick_is_left_unstyled() {
        let line = "no closing `tick here\n";
        assert!(!tokens(line).iter().any(|(_, t)| *t == Token::String));
    }

    #[test]
    fn emphasis_and_strong_are_attributes() {
        let line = "*a* _b_ **c** __d__\n";
        let spans = tokens(line);
        let attrs: Vec<Range<usize>> = spans
            .iter()
            .filter(|(_, t)| *t == Token::Attribute)
            .map(|(r, _)| r.clone())
            .collect();
        assert_eq!(
            attrs,
            vec![
                line.find("*a*").unwrap()..line.find("*a*").unwrap() + 3,
                line.find("_b_").unwrap()..line.find("_b_").unwrap() + 3,
                line.find("**c**").unwrap()..line.find("**c**").unwrap() + 5,
                line.find("__d__").unwrap()..line.find("__d__").unwrap() + 5,
            ]
        );
    }

    #[test]
    fn a_link_url_is_a_constant() {
        let line = "See [the docs](https://example.com/x) for more\n";
        let spans = tokens(line);
        let url = "https://example.com/x";
        let at = line.find(url).unwrap();
        assert!(spans.contains(&(at..at + url.len(), Token::Constant)));
        assert!(
            !spans.iter().any(|(_, t)| *t == Token::Heading),
            "a link should not be confused for anything else"
        );
    }

    #[test]
    fn an_image_url_is_also_a_constant() {
        let line = "![alt text](./img.png)\n";
        let spans = tokens(line);
        let url = "./img.png";
        let at = line.find(url).unwrap();
        assert!(spans.contains(&(at..at + url.len(), Token::Constant)));
    }

    #[test]
    fn a_reference_style_link_has_no_url_span() {
        let line = "[text][ref]\n";
        assert!(!tokens(line).iter().any(|(_, t)| *t == Token::Constant));
    }

    #[test]
    fn a_blockquote_marker_is_punctuation() {
        let line = "> quoted text\n";
        let spans = tokens(line);
        assert!(spans.contains(&(0..1, Token::Punctuation)));
    }

    #[test]
    fn a_fenced_code_block_spans_lines_and_resumes() {
        let (open_spans, state) = lex("```\n", LineState::INITIAL);
        assert_eq!(open_spans, vec![(0.."```\n".len(), Token::String)]);
        assert_ne!(state, LineState::INITIAL, "still inside the fence");

        // Content that would otherwise look like a heading or emphasis must
        // not be lexed as either while the fence is open.
        let (spans1, state1) = lex("# not a heading\n", state);
        assert!(spans1.is_empty(), "{spans1:?}");
        assert_eq!(state1, state);

        let (spans2, state2) = lex("*not emphasis either*\n", state1);
        assert!(spans2.is_empty(), "{spans2:?}");
        assert_eq!(state2, state);

        let (close_spans, state3) = lex("```\n", state2);
        assert_eq!(close_spans, vec![(0.."```\n".len(), Token::String)]);
        assert_eq!(state3, LineState::INITIAL);

        // Prose after the fence closes is lexed normally again.
        let (after_spans, after_state) = lex("# Heading\n", state3);
        assert_eq!(after_spans, vec![(0.."# Heading\n".len(), Token::Heading)]);
        assert_eq!(after_state, LineState::INITIAL);
    }

    #[test]
    fn a_tilde_fence_behaves_the_same_as_a_backtick_one() {
        let (_, state) = lex("~~~\n", LineState::INITIAL);
        assert_ne!(state, LineState::INITIAL);
        let (content_spans, state1) = lex("plain content\n", state);
        assert!(content_spans.is_empty());
        let (_, state2) = lex("~~~\n", state1);
        assert_eq!(state2, LineState::INITIAL);
    }

    #[test]
    fn a_backtick_fence_does_not_close_on_a_tilde_line() {
        let (_, state) = lex("```\n", LineState::INITIAL);
        let (spans, still_open) = lex("~~~\n", state);
        assert!(spans.is_empty(), "not a close, so still unstyled content");
        assert_eq!(still_open, state);
    }

    #[test]
    fn a_shorter_closing_fence_does_not_close() {
        let (_, state) = lex("````\n", LineState::INITIAL); // 4 backticks
        let (spans, still_open) = lex("```\n", state); // only 3: too short
        assert!(spans.is_empty());
        assert_eq!(still_open, state);

        let (close_spans, closed) = lex("````\n", still_open);
        assert_eq!(close_spans, vec![(0.."````\n".len(), Token::String)]);
        assert_eq!(closed, LineState::INITIAL);
    }

    #[test]
    fn a_backtick_fence_info_string_with_a_backtick_does_not_open_a_fence() {
        let line = "```has`a`backtick\n";
        let (spans, state) = lex(line, LineState::INITIAL);
        assert_eq!(state, LineState::INITIAL, "not treated as an opening fence");
        // The stray backticks are still eligible for ordinary inline-code
        // scanning instead.
        let _ = spans;
    }

    #[test]
    fn spans_never_cross_the_lines_length() {
        let line = "# Heading with `code` and *em* and [a](b)\n";
        for (range, _) in tokens(line) {
            assert!(range.end <= line.len());
        }
    }

    #[test]
    fn multibyte_text_never_produces_a_span_that_is_not_at_a_char_boundary() {
        let text = "# naïve café → 日本語\n**bold café** and `code 日本語`\n";
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
