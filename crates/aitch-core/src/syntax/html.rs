//! A hand-written HTML lexer: `<!-- -->` comments (the one real cross-line
//! construct besides `<script>`/`<style>` bodies), `<!DOCTYPE ...>`, tags,
//! attributes, and entities.
//!
//! No syntax tree, so nothing here tracks nesting beyond what a
//! [`LineState`] can carry across one line boundary — see `syntax.rs`'s
//! module docs for what that trades away in general, and this module's own
//! trade-off below.
//!
//! Design calls:
//! - A tag's delimiters (`<`, `>`, `</`, `/>`) → [`Token::Punctuation`]; the
//!   tag name itself → [`Token::Type`] (matching the old tree-sitter
//!   convention of colouring element names as types).
//! - An attribute name → [`Token::Property`]; a quoted value → [`Token::String`].
//!   A bare boolean attribute (`disabled`) is just its [`Token::Property`]
//!   span with nothing after it. `=` itself is left uncoloured — one
//!   Property/String pair per attribute reads better than three competing
//!   spans.
//! - `<!DOCTYPE html>` (case-insensitive `doctype`) → one [`Token::Keyword`]
//!   span for the whole declaration.
//! - `&amp;`, `&#39;`, `&#x27;` and friends → [`Token::Constant`].
//! - Plain text content between tags is left with no spans at all.
//! - **Attribute lists are assumed to close on the same line they open.**
//!   `<div\n  class="x">` is a real, if less common, way to write HTML, and
//!   supporting it properly would need its own carried [`LineState`]; this
//!   lexer does not track it. An attribute list left open at a line's end
//!   simply stops there (whatever was already scanned keeps its spans) and
//!   the next line resumes as ordinary top-level HTML — a graceful
//!   degradation in the same spirit as "broken source still lexes what it
//!   can", not a crash or a mis-colouring.
//! - `<script>...</script>` and `<style>...</style>`: once the opening tag's
//!   closing `>` is seen, everything up to the matching `</script>`/
//!   `</style>` is left with no spans at all — not lexed as HTML (a stray
//!   `<` or `"` in JS/CSS would otherwise look like a tag or a string) and
//!   not lexed as JS/CSS either, since a lexer with no document-wide
//!   language table has nothing sensible to lex it as. This is the same
//!   trade-off `markdown.rs` makes for fenced code block content, and reuses
//!   the same real cross-line `LineState` shape: an "in raw text" flag plus
//!   which of `script`/`style` is open, so the resuming line knows which
//!   closing tag to look for.

use std::ops::Range;

use super::{Lexer, LineState, Token};

pub(crate) struct HtmlLexer;

// `LineState`'s bit layout for HTML: bit 31 set means "inside a `<!-- -->`
// comment". Otherwise, bit 30 set means "inside a <script> or <style>
// element's raw text", with bit 29 saying which one (set = style, clear =
// script). Zero means ordinary top-level HTML, the same as
// `LineState::INITIAL`. The two cross-line constructs are mutually
// exclusive — a comment cannot open partway through a script body without
// first closing the tag — so one bit layout can carry either without
// ambiguity.
const COMMENT_FLAG: u32 = 1 << 31;
const RAW_FLAG: u32 = 1 << 30;
const RAW_STYLE_FLAG: u32 = 1 << 29;

fn raw_state(style: bool) -> LineState {
    let mut value = RAW_FLAG;
    if style {
        value |= RAW_STYLE_FLAG;
    }
    LineState(value)
}

impl Lexer for HtmlLexer {
    fn lex_line(&self, line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        let bytes = line.as_bytes();
        let len = bytes.len();
        let mut spans = Vec::new();
        let mut i = 0usize;

        if state.0 & COMMENT_FLAG != 0 {
            match find_bytes(bytes, 0, b"-->") {
                Some(start) => {
                    i = start + 3;
                    spans.push((0..i, Token::Comment));
                }
                None => return (vec![(0..len, Token::Comment)], state),
            }
        } else if state.0 & RAW_FLAG != 0 {
            let style = state.0 & RAW_STYLE_FLAG != 0;
            let needle: &[u8] = if style { b"</style" } else { b"</script" };
            match find_case_insensitive(bytes, 0, needle) {
                Some(start) => i = start,
                None => return (Vec::new(), state),
            }
        }

        scan_normal(line, bytes, len, i, spans)
    }
}

/// Scan ordinary top-level HTML from `i` to the end of the line, having
/// already handled (or found nothing of) any cross-line construct carried in
/// from the previous line. `spans` may already hold entries from that
/// resumption (a comment's closing span).
fn scan_normal(
    line: &str,
    bytes: &[u8],
    len: usize,
    mut i: usize,
    mut spans: Vec<(Range<usize>, Token)>,
) -> (Vec<(Range<usize>, Token)>, LineState) {
    while i < len {
        match bytes[i] {
            b'<' if bytes[i..].starts_with(b"<!--") => {
                let start = i;
                match find_bytes(bytes, i + 4, b"-->") {
                    Some(close_start) => {
                        let end = close_start + 3;
                        spans.push((start..end, Token::Comment));
                        i = end;
                    }
                    None => {
                        spans.push((start..len, Token::Comment));
                        return (spans, LineState(COMMENT_FLAG));
                    }
                }
            }
            b'<' if bytes.get(i + 1) == Some(&b'!') => {
                if let Some(end) = scan_doctype(bytes, i) {
                    spans.push((i..end, Token::Keyword));
                    i = end;
                } else {
                    // Not a comment (handled above) and not a doctype: some
                    // other `<!...>` construct we don't special-case. Leave
                    // the `<` itself uncoloured and let the rest of the line
                    // fall through to ordinary scanning.
                    i += 1;
                }
            }
            b'<' if bytes.get(i + 1) == Some(&b'/') => {
                i = scan_closing_tag(bytes, i, &mut spans);
            }
            b'<' if bytes.get(i + 1).is_some_and(u8::is_ascii_alphabetic) => {
                let (end, raw_kind) = scan_opening_tag(line, bytes, i, &mut spans);
                i = end;
                if let Some(style) = raw_kind {
                    let needle: &[u8] = if style { b"</style" } else { b"</script" };
                    match find_case_insensitive(bytes, i, needle) {
                        Some(close_start) => i = close_start,
                        None => return (spans, raw_state(style)),
                    }
                }
            }
            b'<' => {
                // A lone `<` matching none of the above (whitespace or a
                // symbol right after it): not a construct we recognise,
                // leave it uncoloured.
                i += 1;
            }
            b'&' => match scan_entity(bytes, i) {
                Some(end) => {
                    spans.push((i..end, Token::Constant));
                    i = end;
                }
                None => i += 1,
            },
            _ => {
                // `i` is always a char boundary here: every branch above
                // only ever advances it past whole ASCII delimiters/names or
                // matched, boundary-safe spans, never into the middle of a
                // multibyte character (see the module doc's reasoning,
                // mirrored from `markdown.rs`).
                let ch_len = line[i..].chars().next().map_or(1, char::len_utf8);
                i += ch_len;
            }
        }
    }
    (spans, LineState::INITIAL)
}

/// From `i` (a `<` immediately followed by `/`), a closing tag: `</`, the
/// tag name, and `>` if it is present on this line. Always makes progress
/// and returns the byte offset just past what it consumed.
fn scan_closing_tag(bytes: &[u8], i: usize, spans: &mut Vec<(Range<usize>, Token)>) -> usize {
    let len = bytes.len();
    spans.push((i..i + 2, Token::Punctuation));
    let mut j = i + 2;
    let name_start = j;
    while j < len && is_tag_name_char(bytes[j]) {
        j += 1;
    }
    if j > name_start {
        spans.push((name_start..j, Token::Type));
    }
    while j < len && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    if j < len && bytes[j] == b'>' {
        spans.push((j..j + 1, Token::Punctuation));
        j += 1;
    }
    j
}

/// From `i` (a `<` immediately followed by an ASCII letter), an opening tag:
/// `<`, the tag name, its attributes, and `>` or `/>` if the tag closes on
/// this line (see the module doc's note on multi-line attribute lists for
/// what happens if it does not).
///
/// Returns the byte offset just past what it consumed, and — only when the
/// tag closed with a plain `>` and its name was `script` or `style` —
/// `Some(is_style)` so the caller knows to switch into raw-text scanning.
fn scan_opening_tag(
    line: &str,
    bytes: &[u8],
    i: usize,
    spans: &mut Vec<(Range<usize>, Token)>,
) -> (usize, Option<bool>) {
    let len = bytes.len();
    spans.push((i..i + 1, Token::Punctuation));
    let mut j = i + 1;
    let name_start = j;
    while j < len && is_tag_name_char(bytes[j]) {
        j += 1;
    }
    let tag_name = &line[name_start..j];
    if j > name_start {
        spans.push((name_start..j, Token::Type));
    }
    let lower_name = tag_name.to_ascii_lowercase();
    let is_raw_tag = lower_name == "script" || lower_name == "style";

    loop {
        while j < len && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= len {
            // Unclosed on this line: the module doc's documented limit.
            // Whatever was already scanned keeps its spans; no state is
            // carried, so the next line resumes as ordinary top-level HTML.
            return (j, None);
        }
        match bytes[j] {
            b'>' => {
                spans.push((j..j + 1, Token::Punctuation));
                j += 1;
                let raw_kind = is_raw_tag.then_some(lower_name == "style");
                return (j, raw_kind);
            }
            b'/' if bytes.get(j + 1) == Some(&b'>') => {
                spans.push((j..j + 2, Token::Punctuation));
                j += 2;
                // Self-closing: never raw, whatever the tag name.
                return (j, None);
            }
            b if is_attr_name_char(b) => {
                let attr_start = j;
                while j < len && is_attr_name_char(bytes[j]) {
                    j += 1;
                }
                spans.push((attr_start..j, Token::Property));

                while j < len && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < len && bytes[j] == b'=' {
                    j += 1;
                    while j < len && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j < len && (bytes[j] == b'"' || bytes[j] == b'\'') {
                        let quote = bytes[j];
                        let value_start = j;
                        j += 1;
                        while j < len && bytes[j] != quote {
                            let ch_len = line[j..].chars().next().map_or(1, char::len_utf8);
                            j += ch_len;
                        }
                        if j < len && bytes[j] == quote {
                            j += 1;
                            spans.push((value_start..j, Token::String));
                        } else {
                            // Unterminated on this line: colour to the end
                            // and stop, same "same-line" assumption as the
                            // tag itself.
                            spans.push((value_start..len, Token::String));
                            return (len, None);
                        }
                    } else {
                        // An unquoted value: run to whitespace or `>`. Safe
                        // byte-by-byte, like `is_attr_name_char` below — a
                        // multibyte continuation byte is never ASCII
                        // whitespace or `>`, so this never stops mid-char.
                        let value_start = j;
                        while j < len && !bytes[j].is_ascii_whitespace() && bytes[j] != b'>' {
                            j += 1;
                        }
                        if j > value_start {
                            spans.push((value_start..j, Token::String));
                        }
                    }
                }
                // No `=`: a bare boolean attribute, already spanned above.
            }
            _ => {
                // A stray byte inside the tag we don't recognise: skip it.
                // Safe for the same reason as the unquoted-value scan above
                // — this only ever stops mid-multibyte-character to resume
                // skipping, never to open a span.
                j += 1;
            }
        }
    }
}

/// `<!` followed by case-insensitive `doctype`, with `>` somewhere later on
/// this line (doctype declarations are vanishingly unlikely to wrap, so
/// unlike comments and raw text this is not tracked across lines — if `>`
/// is missing, the whole rest of the line is still coloured, but nothing
/// carries over).
fn scan_doctype(bytes: &[u8], i: usize) -> Option<usize> {
    let len = bytes.len();
    let name_start = i + 2;
    let name_end = name_start + 7;
    if name_end > len || !bytes[name_start..name_end].eq_ignore_ascii_case(b"doctype") {
        return None;
    }
    let mut j = name_end;
    while j < len && bytes[j] != b'>' {
        j += 1;
    }
    Some(if j < len { j + 1 } else { len })
}

/// An HTML entity from `i` (a `&`): `&name;`, `&#123;`, or `&#x1F;`. `None`
/// if it does not terminate in `;` on this line.
fn scan_entity(bytes: &[u8], i: usize) -> Option<usize> {
    let len = bytes.len();
    let mut j = i + 1;
    if j >= len {
        return None;
    }
    if bytes[j] == b'#' {
        j += 1;
        let hex = matches!(bytes.get(j), Some(b'x') | Some(b'X'));
        if hex {
            j += 1;
        }
        let digits_start = j;
        if hex {
            while j < len && bytes[j].is_ascii_hexdigit() {
                j += 1;
            }
        } else {
            while j < len && bytes[j].is_ascii_digit() {
                j += 1;
            }
        }
        if j == digits_start {
            return None;
        }
    } else if bytes[j].is_ascii_alphabetic() {
        while j < len && bytes[j].is_ascii_alphanumeric() {
            j += 1;
        }
    } else {
        return None;
    }
    (bytes.get(j) == Some(&b';')).then_some(j + 1)
}

fn is_tag_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-'
}

fn is_attr_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.')
}

/// The byte offset of the first occurrence of `needle` at or after `from`.
fn find_bytes(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// Like [`find_bytes`], but ASCII case-insensitive — used to find
/// `</script`/`</style` regardless of how the document spells them.
fn find_case_insensitive(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    let n = needle.len();
    if n == 0 || from > haystack.len() || haystack.len() - from < n {
        return None;
    }
    (from..=haystack.len() - n).find(|&p| haystack[p..p + n].eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        HtmlLexer.lex_line(line, state)
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
    fn a_comment_on_one_line_is_recognized() {
        let line = "<!-- a note -->\n";
        let spans = tokens(line);
        assert_eq!(spans, vec![(0.."<!-- a note -->".len(), Token::Comment)]);
    }

    #[test]
    fn a_comment_spans_lines_and_resumes() {
        let (spans1, state) = lex("<!-- start\n", LineState::INITIAL);
        assert_eq!(spans1, vec![(0.."<!-- start\n".len(), Token::Comment)]);
        assert_ne!(state, LineState::INITIAL, "still inside the comment");

        let (spans2, state2) = lex("still commented, even a <tag>\n", state);
        assert_eq!(
            spans2,
            vec![(0.."still commented, even a <tag>\n".len(), Token::Comment)],
            "a stray < inside the comment must not be lexed as a tag"
        );
        assert_eq!(state2, state);

        let (spans3, state3) = lex("end --> <p>after</p>\n", state2);
        let close = "end -->".len();
        assert!(spans3.contains(&(0..close, Token::Comment)));
        assert_eq!(state3, LineState::INITIAL);
        assert_eq!(
            find(&spans3, "p", "end --> <p>after</p>\n"),
            Some(&Token::Type)
        );
    }

    #[test]
    fn an_opening_tag_with_an_attribute_is_lexed() {
        let line = "<div class=\"box\">\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "<", line), Some(&Token::Punctuation));
        assert_eq!(find(&spans, "div", line), Some(&Token::Type));
        assert_eq!(find(&spans, "class", line), Some(&Token::Property));
        assert_eq!(find(&spans, "\"box\"", line), Some(&Token::String));
        assert_eq!(find(&spans, ">", line), Some(&Token::Punctuation));
    }

    #[test]
    fn a_boolean_attribute_has_no_value_span() {
        let line = "<input disabled>\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "disabled", line), Some(&Token::Property));
    }

    #[test]
    fn a_self_closing_tag_is_lexed() {
        let line = "<br/>\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "br", line), Some(&Token::Type));
        assert_eq!(find(&spans, "/>", line), Some(&Token::Punctuation));

        let line2 = "<img src=\"x.png\" />\n";
        let spans2 = tokens(line2);
        assert_eq!(find(&spans2, "img", line2), Some(&Token::Type));
        assert_eq!(find(&spans2, "/>", line2), Some(&Token::Punctuation));
    }

    #[test]
    fn a_closing_tag_is_lexed() {
        let line = "</div>\n";
        let spans = tokens(line);
        assert_eq!(spans[0], (0..2, Token::Punctuation));
        assert_eq!(find(&spans, "div", line), Some(&Token::Type));
        let gt = line.find('>').unwrap();
        assert_eq!(find(&spans, ">", line), Some(&Token::Punctuation));
        let _ = gt;
    }

    #[test]
    fn a_doctype_is_a_keyword() {
        let line = "<!DOCTYPE html>\n";
        let spans = tokens(line);
        assert_eq!(spans, vec![(0.."<!DOCTYPE html>".len(), Token::Keyword)]);

        let lower = "<!doctype html>\n";
        assert_eq!(
            tokens(lower),
            vec![(0.."<!doctype html>".len(), Token::Keyword)],
            "case-insensitive"
        );
    }

    #[test]
    fn entities_are_constants() {
        let line = "R&amp;D &#39; &#x27;\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "&amp;", line), Some(&Token::Constant));
        assert_eq!(find(&spans, "&#39;", line), Some(&Token::Constant));
        assert_eq!(find(&spans, "&#x27;", line), Some(&Token::Constant));
    }

    #[test]
    fn a_script_block_is_not_lexed_as_html_and_resumes_after() {
        let (open_spans, state) = lex("<script>\n", LineState::INITIAL);
        assert_eq!(
            find(&open_spans, "script", "<script>\n"),
            Some(&Token::Type)
        );
        assert_ne!(state, LineState::INITIAL, "still inside the script body");

        // Content that would otherwise look like a tag or a string must not
        // be lexed as either while the script body is open.
        let (spans1, state1) = lex("var s = \"<div class='x'>\" + 1;\n", state);
        assert!(spans1.is_empty(), "{spans1:?}");
        assert_eq!(state1, state);

        let (close_spans, state2) = lex("</script>\n", state1);
        assert_eq!(
            find(&close_spans, "script", "</script>\n"),
            Some(&Token::Type)
        );
        assert_eq!(state2, LineState::INITIAL);

        // Ordinary HTML after </script> is lexed normally again.
        let (after_spans, after_state) = lex("<p>hi</p>\n", state2);
        assert_eq!(find(&after_spans, "p", "<p>hi</p>\n"), Some(&Token::Type));
        assert_eq!(after_state, LineState::INITIAL);
    }

    #[test]
    fn a_style_block_is_not_lexed_as_html_and_resumes_after() {
        let (open_spans, state) = lex("<style>\n", LineState::INITIAL);
        assert_eq!(find(&open_spans, "style", "<style>\n"), Some(&Token::Type));
        assert_ne!(state, LineState::INITIAL);

        let (spans1, state1) = lex(".x[data-a=\"<y>\"] { color: red; }\n", state);
        assert!(spans1.is_empty(), "{spans1:?}");
        assert_eq!(state1, state);

        let (close_spans, state2) = lex("</style>\n", state1);
        assert_eq!(
            find(&close_spans, "style", "</style>\n"),
            Some(&Token::Type)
        );
        assert_eq!(state2, LineState::INITIAL);

        let (after_spans, after_state) = lex("<div></div>\n", state2);
        assert_eq!(
            find(&after_spans, "div", "<div></div>\n"),
            Some(&Token::Type)
        );
        assert_eq!(after_state, LineState::INITIAL);
    }

    #[test]
    fn a_script_tag_and_its_content_can_share_a_line() {
        let line = "<script>var x = 1;</script>\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "script", line), Some(&Token::Type));
        // Only two "script" occurrences (open/close tag names) should have
        // spans; the JS body in between must have none.
        assert!(!spans.iter().any(|(r, _)| line[r.clone()].contains("var")));
    }

    #[test]
    fn broken_source_still_lexes_what_it_can() {
        let spans = tokens("<div class=\"unterminated\n");
        assert_eq!(
            find(&spans, "div", "<div class=\"unterminated\n"),
            Some(&Token::Type)
        );
        assert_eq!(
            find(&spans, "class", "<div class=\"unterminated\n"),
            Some(&Token::Property)
        );
    }

    #[test]
    fn spans_never_cross_the_lines_length() {
        let line = "<div class=\"x\">text &amp; <!-- c --></div>\n";
        for (range, _) in tokens(line) {
            assert!(range.end <= line.len());
        }
    }

    #[test]
    fn multibyte_text_never_produces_a_span_that_is_not_at_a_char_boundary() {
        let text = "<!-- naïve café → 日本語 -->\n<div title=\"café 日本語\">日本語</div>\n";
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
