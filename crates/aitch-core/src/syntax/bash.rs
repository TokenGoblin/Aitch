//! A hand-written Bash/shell lexer: `#` comments (a shebang is just a
//! comment lexically, no special-casing needed), single- and double-quoted
//! strings (the latter carrying `$var`/`${var}` references as nested
//! [`Token::Variable`] spans — see the module-level note below), variable
//! references and special parameters (`$1`, `$@`, `$#`, `$?`, `$$`, ...),
//! `$( ... )` command substitution and `` `...` `` backtick substitution
//! (delimiters only — the command inside is not itself lexed), keywords and
//! a handful of builtins, numbers, operators/punctuation, and a light
//! "first word of a command" heuristic for [`Token::Function`].
//!
//! No syntax tree, so nothing here tracks brackets or nesting beyond what a
//! [`LineState`] can carry across one line boundary — see `syntax.rs`'s
//! module docs for what that trades away.
//!
//! Design call: a `$var` (or `${var}`) inside a double-quoted string is
//! carved out of the surrounding [`Token::String`] span as its own
//! [`Token::Variable`] span, rather than the whole string staying one
//! opaque `String` run. A single-quoted string never does this — bash
//! itself never interpolates inside `'...'`, so `\` and `$` are completely
//! literal there.

use std::ops::Range;

use super::{Lexer, LineState, Token};

pub(crate) struct BashLexer;

// `LineState`'s layout for Bash: no bit-packing needed, just three plain
// states — nothing here nests (unlike Rust's block comments), so there is
// no depth to carry.
const NORMAL: LineState = LineState(0);
const IN_SINGLE_QUOTE: LineState = LineState(1);
const IN_DOUBLE_QUOTE: LineState = LineState(2);

const KEYWORDS: &[&str] = &[
    // Syntax keywords.
    "if", "then", "elif", "else", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "function", "select", "in", "time", "coproc",
    // Builtins reasonably coloured the same as keywords (see bash.rs's task
    // notes: a shell builtin and a syntax keyword are both "the language
    // talking to you", as opposed to a plain command name).
    "return", "local", "export", "readonly", "declare", "shift", "break", "continue", "exit", "set",
    "unset", "source", "typeset", "trap",
];

impl Lexer for BashLexer {
    fn lex_line(&self, line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        let bytes = line.as_bytes();
        let len = bytes.len();
        let mut spans = Vec::new();
        let mut i = 0usize;
        let mut expect_command = true;

        if state == IN_SINGLE_QUOTE {
            match scan_single_quoted_body(bytes, 0) {
                Some(end) => {
                    spans.push((0..end, Token::String));
                    i = end;
                    expect_command = false;
                }
                None => return (vec![(0..len, Token::String)], IN_SINGLE_QUOTE),
            }
        } else if state == IN_DOUBLE_QUOTE {
            match scan_double_quoted_body(line, 0, 0, &mut spans) {
                Some(end) => {
                    i = end;
                    expect_command = false;
                }
                None => return (spans, IN_DOUBLE_QUOTE),
            }
        }

        while i < len {
            let byte = bytes[i];
            match byte {
                b'#' => {
                    spans.push((i..len, Token::Comment));
                    i = len;
                }
                b'\'' => {
                    let start = i;
                    match scan_single_quoted_body(bytes, i + 1) {
                        Some(end) => {
                            spans.push((start..end, Token::String));
                            i = end;
                        }
                        None => {
                            spans.push((start..len, Token::String));
                            return (spans, IN_SINGLE_QUOTE);
                        }
                    }
                    expect_command = false;
                }
                b'"' => {
                    match scan_double_quoted_body(line, i + 1, i, &mut spans) {
                        Some(end) => i = end,
                        None => return (spans, IN_DOUBLE_QUOTE),
                    }
                    expect_command = false;
                }
                b'$' if bytes.get(i + 1) == Some(&b'(') => {
                    let start = i;
                    let mut j = i + 2;
                    let mut depth = 1i32;
                    while j < len && depth > 0 {
                        match bytes[j] {
                            b'(' => {
                                depth += 1;
                                j += 1;
                            }
                            b')' => {
                                depth -= 1;
                                j += 1;
                            }
                            _ => j += 1,
                        }
                    }
                    spans.push((start..start + 2, Token::Punctuation));
                    if depth == 0 {
                        spans.push((j - 1..j, Token::Punctuation));
                    }
                    i = j;
                    expect_command = false;
                }
                b'$' => {
                    let start = i;
                    let end = scan_variable(bytes, i);
                    if end > start {
                        spans.push((start..end, Token::Variable));
                        i = end;
                    } else {
                        i += 1;
                    }
                    expect_command = false;
                }
                b'`' => {
                    let start = i;
                    let mut j = i + 1;
                    let mut found = None;
                    while j < len {
                        if bytes[j] == b'\\' && j + 1 < len {
                            j += 2;
                            continue;
                        }
                        if bytes[j] == b'`' {
                            found = Some(j);
                            break;
                        }
                        j += 1;
                    }
                    spans.push((start..start + 1, Token::Punctuation));
                    match found {
                        Some(end) => {
                            spans.push((end..end + 1, Token::Punctuation));
                            i = end + 1;
                        }
                        None => i = len,
                    }
                    expect_command = false;
                }
                b'&' if bytes.get(i + 1) == Some(&b'&') => {
                    spans.push((i..i + 2, Token::Operator));
                    i += 2;
                    expect_command = true;
                }
                b'|' if bytes.get(i + 1) == Some(&b'|') => {
                    spans.push((i..i + 2, Token::Operator));
                    i += 2;
                    expect_command = true;
                }
                b'>' if bytes.get(i + 1) == Some(&b'>') => {
                    spans.push((i..i + 2, Token::Operator));
                    i += 2;
                    expect_command = false;
                }
                b'<' if bytes.get(i + 1) == Some(&b'<') => {
                    spans.push((i..i + 2, Token::Operator));
                    i += 2;
                    expect_command = false;
                }
                b'|' | b'&' | b';' => {
                    spans.push((i..i + 1, Token::Operator));
                    i += 1;
                    expect_command = true;
                }
                b'>' | b'<' | b'=' => {
                    spans.push((i..i + 1, Token::Operator));
                    i += 1;
                    expect_command = false;
                }
                b'(' | b'{' => {
                    spans.push((i..i + 1, Token::Punctuation));
                    i += 1;
                    expect_command = true;
                }
                b')' | b'}' | b'[' | b']' => {
                    spans.push((i..i + 1, Token::Punctuation));
                    i += 1;
                    expect_command = false;
                }
                b'0'..=b'9' => {
                    let start = i;
                    while i < len && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                    spans.push((start..i, Token::Number));
                    expect_command = false;
                }
                _ if is_word_byte(byte) => {
                    let start = i;
                    while i < len && is_word_byte(bytes[i]) {
                        i += 1;
                    }
                    let word = &line[start..i];
                    if bytes.get(i) == Some(&b'=') && is_valid_identifier(word) {
                        // `NAME=value`: an assignment target, not a command
                        // name — styled as a variable being written to.
                        spans.push((start..i, Token::Variable));
                        expect_command = false;
                    } else if let Some(token) = classify_word(word) {
                        spans.push((start..i, token));
                        expect_command = token == Token::Keyword;
                    } else if expect_command {
                        // The first bareword on a line (or right after `|`,
                        // `;`, `&`, `&&`, `||`, `(`, `{`) is a plausible
                        // command name.
                        spans.push((start..i, Token::Function));
                        expect_command = false;
                    } else {
                        expect_command = false;
                    }
                }
                _ => i += 1,
            }
        }

        (spans, NORMAL)
    }
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/')
}

fn is_valid_identifier(word: &str) -> bool {
    let mut chars = word.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn classify_word(word: &str) -> Option<Token> {
    if KEYWORDS.contains(&word) {
        return Some(Token::Keyword);
    }
    if word == "true" || word == "false" {
        return Some(Token::Constant);
    }
    None
}

/// From just after an opening `'` (or the start of a line already inside
/// one), the byte past the closing `'`. `None` if it is still open — a
/// single-quoted string is completely literal, so there is nothing to skip
/// over except the quote itself.
fn scan_single_quoted_body(bytes: &[u8], start: usize) -> Option<usize> {
    let len = bytes.len();
    let mut i = start;
    while i < len {
        if bytes[i] == b'\'' {
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

/// Scans a double-quoted string's body, pushing [`Token::String`] spans for
/// its literal text (including its delimiting quotes, the same convention
/// `rust.rs`/`json.rs` use) and [`Token::Variable`] spans for any
/// `$name`/`${name}` reference found inside directly into `spans`. Returns
/// the byte offset just past the closing `"`, or `None` if the string is
/// still open at the end of the line (with spans already pushed through
/// line end).
///
/// `content_start` is where scanning for `\`/`$`/`"` begins — just past the
/// opening quote, or 0 when resuming a string that was already open at the
/// start of this line. `span_start` is where the first emitted span should
/// begin — the opening quote's own position, or 0 when resuming (there is
/// no quote character on this line to include).
fn scan_double_quoted_body(
    line: &str,
    content_start: usize,
    span_start: usize,
    spans: &mut Vec<(Range<usize>, Token)>,
) -> Option<usize> {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = content_start;
    let mut text_start = span_start;
    while i < len {
        match bytes[i] {
            b'\\' if i + 1 < len => i += 2,
            b'\\' => i += 1,
            b'"' => {
                spans.push((text_start..i + 1, Token::String));
                return Some(i + 1);
            }
            b'$' => {
                if text_start < i {
                    spans.push((text_start..i, Token::String));
                }
                let end = scan_variable(bytes, i);
                if end > i {
                    spans.push((i..end, Token::Variable));
                    i = end;
                } else {
                    i += 1;
                }
                text_start = i;
            }
            _ => i += 1,
        }
    }
    if text_start < len {
        spans.push((text_start..len, Token::String));
    }
    None
}

/// From `bytes[i] == b'$'`, the end of a variable reference: `${...}`, a
/// single-character special parameter (`$@`, `$#`, `$?`, `$$`, `$!`, `$-`,
/// `$*`), a single-digit positional parameter (`$1`), or a `$name`
/// identifier. Returns `i` itself (no advance) if `$` is not followed by
/// anything that makes it a reference — a lone `$`, or the start of a `$(`
/// command substitution, which the caller handles separately.
fn scan_variable(bytes: &[u8], i: usize) -> usize {
    let len = bytes.len();
    let mut j = i + 1;
    if j >= len {
        return i;
    }
    match bytes[j] {
        b'{' => {
            j += 1;
            while j < len && bytes[j] != b'}' {
                j += 1;
            }
            if j < len {
                j += 1;
            }
            j
        }
        b'@' | b'#' | b'?' | b'$' | b'!' | b'-' | b'*' | b'0'..=b'9' => j + 1,
        b if b.is_ascii_alphabetic() || b == b'_' => {
            j += 1;
            while j < len && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            j
        }
        _ => i,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(line: &str, state: LineState) -> (Vec<(Range<usize>, Token)>, LineState) {
        BashLexer.lex_line(line, state)
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
        let line = "echo hi # trailing note\n";
        let spans = tokens(line);
        let at = line.find('#').unwrap();
        assert!(spans.contains(&(at..line.len(), Token::Comment)));
    }

    #[test]
    fn a_shebang_is_just_a_comment() {
        let line = "#!/bin/bash\n";
        let spans = tokens(line);
        assert_eq!(spans, vec![(0..line.len(), Token::Comment)]);
    }

    #[test]
    fn a_single_quoted_string_is_completely_literal() {
        // Neither the backslash nor the `$var` mean anything special inside
        // `'...'` — the whole thing is one opaque String span, unlike a
        // double-quoted string (see the next test).
        let line = "echo 'a\\nb $var'\n";
        let spans = tokens(line);
        let start = line.find('\'').unwrap();
        let end = line.rfind('\'').unwrap() + 1;
        assert!(spans.contains(&(start..end, Token::String)));
        assert!(
            !spans.iter().any(|(_, t)| *t == Token::Variable),
            "{spans:?}: nothing inside a single-quoted string interpolates"
        );
    }

    #[test]
    fn a_double_quoted_string_carves_out_a_variable_reference() {
        // Design call: `$var` inside `"..."` is pulled out of the
        // surrounding String span as its own Variable span, rather than the
        // whole string staying one opaque run.
        let line = "echo \"hello $name!\"\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "$name", line), Some(&Token::Variable));
        let quote = line.find('"').unwrap();
        let dollar = line.find('$').unwrap();
        assert!(spans.contains(&(quote..dollar, Token::String)));
        let after = dollar + "$name".len();
        let close = line.rfind('"').unwrap() + 1;
        assert!(spans.contains(&(after..close, Token::String)));
    }

    #[test]
    fn a_double_quoted_string_can_hold_braced_and_special_variables() {
        let line = "echo \"${name} $1 $@ $#\"\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "${name}", line), Some(&Token::Variable));
        assert_eq!(find(&spans, "$1", line), Some(&Token::Variable));
        assert_eq!(find(&spans, "$@", line), Some(&Token::Variable));
        assert_eq!(find(&spans, "$#", line), Some(&Token::Variable));
    }

    #[test]
    fn a_bare_variable_reference_outside_a_string() {
        let line = "echo $VAR ${VAR}\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "$VAR", line), Some(&Token::Variable));
        assert_eq!(find(&spans, "${VAR}", line), Some(&Token::Variable));
    }

    #[test]
    fn an_unterminated_double_quoted_string_spans_lines_and_resumes() {
        let (spans1, state) = lex("echo \"still going\n", LineState::INITIAL);
        assert!(spans1.iter().any(|(_, t)| *t == Token::String));
        assert_ne!(state, LineState::INITIAL);

        let (spans2, state2) = lex("closes here\";\n", state);
        let close = "closes here\"".len();
        assert!(spans2.contains(&(0..close, Token::String)));
        assert_eq!(state2, LineState::INITIAL);
    }

    #[test]
    fn an_unterminated_double_quoted_string_still_carves_out_a_variable() {
        let (spans1, state) = lex("echo \"hi $name\n", LineState::INITIAL);
        assert_eq!(
            find(&spans1, "$name", "echo \"hi $name\n"),
            Some(&Token::Variable)
        );
        assert_ne!(state, LineState::INITIAL);

        let (spans2, state2) = lex("more text\"\n", state);
        assert!(spans2.iter().any(|(_, t)| *t == Token::String));
        assert_eq!(state2, LineState::INITIAL);
    }

    #[test]
    fn an_unterminated_single_quoted_string_spans_lines_and_resumes() {
        let (spans1, state) = lex("echo 'still going\n", LineState::INITIAL);
        assert!(spans1.iter().any(|(_, t)| *t == Token::String));
        assert_ne!(state, LineState::INITIAL);

        let (spans2, state2) = lex("closes here';\n", state);
        let close = "closes here'".len();
        assert!(spans2.contains(&(0..close, Token::String)));
        assert_eq!(state2, LineState::INITIAL);
    }

    #[test]
    fn keywords_come_out_as_keywords() {
        let line = "if true; then echo hi; fi\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "if", line), Some(&Token::Keyword));
        assert_eq!(find(&spans, "then", line), Some(&Token::Keyword));
        assert_eq!(find(&spans, "fi", line), Some(&Token::Keyword));
        assert_eq!(find(&spans, "true", line), Some(&Token::Constant));
    }

    #[test]
    fn a_builtin_is_a_keyword_too() {
        let line = "local x=1\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "local", line), Some(&Token::Keyword));
    }

    #[test]
    fn the_first_word_of_a_command_is_a_function() {
        let line = "grep -n foo file.txt\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "grep", line), Some(&Token::Function));
        // Its argument is not.
        assert_ne!(find(&spans, "foo", line), Some(&Token::Function));
    }

    #[test]
    fn a_command_after_a_pipe_or_semicolon_is_also_a_function() {
        let line = "cat foo | grep bar; echo done\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "cat", line), Some(&Token::Function));
        assert_eq!(find(&spans, "grep", line), Some(&Token::Function));
        assert_eq!(find(&spans, "echo", line), Some(&Token::Function));
    }

    #[test]
    fn an_assignment_target_is_a_variable_not_a_function() {
        let line = "FOO=bar\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "FOO", line), Some(&Token::Variable));
    }

    #[test]
    fn command_substitution_delimiters_are_punctuation() {
        let line = "x=$(echo hi)\n";
        let spans = tokens(line);
        let open = line.find("$(").unwrap();
        assert!(spans.contains(&(open..open + 2, Token::Punctuation)));
        let close = line.rfind(')').unwrap();
        assert!(spans.contains(&(close..close + 1, Token::Punctuation)));
    }

    #[test]
    fn backtick_substitution_delimiters_are_punctuation() {
        let line = "x=`echo hi`\n";
        let spans = tokens(line);
        let open = line.find('`').unwrap();
        let close = line.rfind('`').unwrap();
        assert!(spans.contains(&(open..open + 1, Token::Punctuation)));
        assert!(spans.contains(&(close..close + 1, Token::Punctuation)));
    }

    #[test]
    fn numbers_and_operators() {
        let line = "sleep 10 && echo ok || echo fail\n";
        let spans = tokens(line);
        assert_eq!(find(&spans, "10", line), Some(&Token::Number));
        assert_eq!(find(&spans, "&&", line), Some(&Token::Operator));
        assert_eq!(find(&spans, "||", line), Some(&Token::Operator));
    }

    #[test]
    fn broken_source_still_lexes_what_it_can() {
        let spans = tokens("if [ -z \"$x\" \n  echo\n");
        assert!(spans.iter().any(|(_, t)| *t == Token::Keyword));
    }

    #[test]
    fn multibyte_text_never_produces_a_span_that_is_not_at_a_char_boundary() {
        let line = "# naïve café → 日本語\necho \"héllo 日本語\"\n";
        for chunk in line.split_inclusive('\n') {
            for (range, _) in tokens(chunk) {
                let _ = &chunk[range]; // panics if it split a character
            }
        }
    }

    #[test]
    fn spans_never_cross_the_lines_length() {
        let line = "echo \"a $b c\" 'd' # note\n";
        for (range, _) in tokens(line) {
            assert!(range.end <= line.len());
        }
    }
}
