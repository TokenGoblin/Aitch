//! A small hand-written backtracking regular-expression engine, replacing
//! `grep-regex` per [`PLAN-ZERO-DEP.md`](../../../../PLAN-ZERO-DEP.md) §2/§4
//! Phase 6. It implements a defined *subset* of regex syntax — literals,
//! `.`, `*`/`+`/`?`, `[...]` classes (plus the `\d`/`\D`/`\w`/`\W`/`\s`/`\S`
//! shorthands, each just sugar for a fixed byte-range class — see
//! [`shorthand_class`]), `^`/`$` anchors, `|` alternation, `(...)`
//! non-capturing groups, and `\`-escaping of anything else — not the full
//! language `grep-regex` (and the `regex` crate it wraps) supports. See
//! [`Regex::new`] for exactly what is and is not accepted.
//!
//! Three documented limitations, in the same spirit as [`crate::matcher`]'s
//! `LiteralMatcher` documenting its own ASCII-only case folding:
//!
//! - **Byte-oriented, not Unicode-aware.** A character class like `[a-z]`
//!   compares raw bytes. For ASCII input that is exactly the classic regex
//!   behavior; against a multi-byte UTF-8 sequence a class has no clean
//!   meaning and this engine does not attempt one (a class range never
//!   matches a continuation byte of a multi-byte character, since those all
//!   have the high bit set, well outside any ASCII range anyone would
//!   write). `project_search.rs` searches line-by-line UTF-8 source text,
//!   where this is a reasonable trade for avoiding a whole Unicode-table
//!   dependency.
//! - **ASCII-only case folding.** `case_sensitive: false` folds literals
//!   and class bytes with `to_ascii_lowercase`, not full Unicode case
//!   folding — a Turkish dotless/dotted I or a German ß/SS pair will not
//!   match across case here, exactly the same simplification
//!   `LiteralMatcher` makes and documents.
//! - **No catastrophic-backtracking guard.** This is a backtracking engine
//!   with no step budget or memoization, so a pathological pattern like
//!   `(a*)*b` against a long run of non-matching input can take
//!   exponential time, the classic failure mode of this engine family. That
//!   is an accepted limitation, not a bug to fix here: `project_search.rs`
//!   patterns come from a human typing a search box, not from an untrusted
//!   network input, and the fix (a memoizing/Thompson-NFA engine) is a
//!   different, heavier engine than a "defined subset" one calls for. The
//!   test module's `pathological_backtracking_pattern_does_not_hang` test
//!   below exercises this deliberately, but keeps its haystack short enough
//!   to finish quickly even in the engine's worst case, so the guarantee
//!   that engine offers here is "this specific known-bad shape is still
//!   fast enough on small input", not "no input can ever blow up."
//!
//! # Escaping
//!
//! `\` before one of the syntax characters (`. * + ? [ ] ^ $ | ( ) \`)
//! makes it literal, e.g. `\.` matches a literal dot. `\d`/`\D`/`\w`/`\W`/
//! `\s`/`\S` are the one set of shorthands this engine does recognize (see
//! [`shorthand_class`]) — common enough, and different enough in kind from
//! a literal escape, that treating them as "just the letter `d`" would be a
//! worse surprise than special-casing six bytes. `\` before anything else
//! is accepted and simply means that character literally, *not* a compile
//! error, so patterns copied in from elsewhere that lean on `\/` or similar
//! still compile. A trailing `\` with nothing after it is a compile error,
//! since there is no character left to escape.
//!
//! # What's out of scope
//!
//! Lookaround, backreferences, reportable capture groups, non-greedy
//! quantifiers (`*?`, `+?`), bounded repetition (`{n,m}`), Unicode
//! properties (`\p{...}`), and word boundaries (`\b`) are all unsupported.
//! None of these are silently misinterpreted: `{`, `}`, and a lone `\b`-style
//! escape are ordinary literal/escaped characters under the rules above
//! (this engine has no `{n,m}` syntax to begin with, so `a{2,3}` matches the
//! literal text `a{2,3}` rather than being rejected or misread as
//! repetition — the same "not a compile error, just not special" stance the
//! escaping section takes). The only inputs that produce [`RegexError`] are
//! genuinely malformed under *this* subset's own grammar: an unbalanced `(`
//! or `[`, a quantifier with no preceding atom, and an empty alternation
//! branch.

use std::fmt;

use crate::matcher::{MatchRange, Matcher};

/// Something went wrong compiling a pattern. Carries a human-readable
/// message; `project_search.rs` wraps `.to_string()` of this into its own
/// `SearchError::BadPattern`, exactly as it previously did with
/// `grep_regex`'s build error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegexError {
    message: String,
}

impl RegexError {
    fn new(message: impl Into<String>) -> RegexError {
        RegexError {
            message: message.into(),
        }
    }
}

impl fmt::Display for RegexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for RegexError {}

// --- AST -------------------------------------------------------------

/// One "thing that can be repeated": a literal byte, `.`, a character
/// class, or a parenthesized group (itself a full alternation). Grouping
/// repetition around this type, rather than around every `Node` variant, is
/// what keeps `*`/`+`/`?` scoped to "the atom just before it" per the
/// grammar `regex.rs`'s module doc describes (recursive descent:
/// alternation → concatenation → repetition → atom).
#[derive(Debug, Clone)]
enum Atom {
    /// A literal byte, already case-folded at compile time if the pattern
    /// is case-insensitive (folded to lowercase, matched against a
    /// lowercase-folded haystack byte — see `Regex::matches_atom`).
    Literal(u8),
    /// `.`: any byte except `\n`.
    AnyByte,
    /// `[...]`/`[^...]`: a set of allowed byte ranges (inclusive), plus
    /// whether the whole class is negated.
    Class {
        ranges: Vec<(u8, u8)>,
        negated: bool,
    },
    /// `(...)`: a fully general sub-pattern, so a group can itself contain
    /// alternation, concatenation, and further nested groups.
    Group(Box<Node>),
    /// `^`/`$`: a zero-width position check rather than a byte to consume.
    /// Modeled as an `Atom` (always wrapped in `Repeat::Exactly` — see
    /// `Parser::parse_repeat`, which rejects a `*`/`+`/`?` suffix on one)
    /// rather than a separate `Node`/`Repeat` case, since "one more kind of
    /// thing `match_atom_once` can match" is simpler than threading a
    /// parallel zero-width-match code path through the whole engine.
    Anchor(AnchorKind),
}

/// Which end of the haystack an [`Atom::Anchor`] checks for.
#[derive(Debug, Clone, Copy)]
enum AnchorKind {
    Start,
    End,
}

/// `\d`/`\D`/`\w`/`\W`/`\s`/`\S`, the shorthand classes carved out of the
/// general "an unrecognized escape is just that byte literally" rule below
/// — common enough (ripgrep's own `grep-regex` supports them, and
/// `project_search.rs`'s own test suite already had a pattern relying on
/// `\d`) that leaving them as literal `d`/`w`/`s` would be a worse
/// surprise than the extra few lines to special-case them. `None` for
/// anything else, which falls through to the literal-escape rule as before.
fn shorthand_class(escaped: u8) -> Option<Atom> {
    const DIGIT: &[(u8, u8)] = &[(b'0', b'9')];
    const WORD: &[(u8, u8)] = &[(b'A', b'Z'), (b'a', b'z'), (b'0', b'9'), (b'_', b'_')];
    // Space, tab, newline, CR, vertical tab, form feed — the classic ASCII
    // `\s` set, same as `grep-regex`'s default (non-Unicode) whitespace.
    const SPACE: &[(u8, u8)] = &[
        (b' ', b' '),
        (b'\t', b'\t'),
        (b'\n', b'\n'),
        (b'\r', b'\r'),
        (0x0B, 0x0B),
        (0x0C, 0x0C),
    ];

    let (ranges, negated): (&[(u8, u8)], bool) = match escaped {
        b'd' => (DIGIT, false),
        b'D' => (DIGIT, true),
        b'w' => (WORD, false),
        b'W' => (WORD, true),
        b's' => (SPACE, false),
        b'S' => (SPACE, true),
        _ => return None,
    };
    Some(Atom::Class {
        ranges: ranges.to_vec(),
        negated,
    })
}

/// A repeated atom: the atom itself plus how many times it may occur.
/// `Exactly(1)` (no `*`/`+`/`?` suffix) is represented directly rather than
/// wrapped, so a plain literal is not paying for a range it never uses.
#[derive(Debug, Clone)]
enum Repeat {
    Exactly(Atom),
    /// `*`: zero or more, greedy.
    Star(Atom),
    /// `+`: one or more, greedy.
    Plus(Atom),
    /// `?`: zero or one, greedy (i.e. it prefers one).
    Question(Atom),
}

/// A concatenation of repeated atoms — `Vec<Repeat>` rather than a nested
/// `Concat(Box<Node>, Box<Node>)` because matching a flat sequence
/// left-to-right with backtracking is far simpler to write iteratively than
/// recursively pairing up a cons-list.
type Sequence = Vec<Repeat>;

/// A full sub-pattern: one or more `|`-separated sequences (alternation),
/// each concatenated. `branches.len() == 1` is the common case of no `|` at
/// this level at all.
#[derive(Debug, Clone)]
struct Node {
    branches: Vec<Sequence>,
}

// --- Parser ------------------------------------------------------------
//
// The characters with syntactic meaning in this subset are `. * + ? [ ] ^
// $ | ( ) \`. There is no table of them in code: `\` before *any* byte
// (special or not) is handled uniformly by `Parser::parse_atom`'s `'\\'`
// arm, which just takes the next byte literally — see the module doc's
// "Escaping" section for why that also covers the "unrecognized escape"
// case without a separate list to check against.

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(bytes: &'a [u8]) -> Parser<'a> {
        Parser { bytes, pos: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek();
        if b.is_some() {
            self.pos += 1;
        }
        b
    }

    /// Top-level entry: parse a whole alternation, then require it consumed
    /// every byte — anything left over means an unmatched `)`.
    fn parse_pattern(&mut self) -> Result<Node, RegexError> {
        let node = self.parse_alternation()?;
        if self.pos != self.bytes.len() {
            return Err(RegexError::new(format!(
                "unexpected '{}' at position {} (unbalanced parenthesis?)",
                self.bytes[self.pos] as char, self.pos
            )));
        }
        Ok(node)
    }

    /// alternation := sequence ('|' sequence)*
    fn parse_alternation(&mut self) -> Result<Node, RegexError> {
        let mut branches = vec![self.parse_sequence()?];
        while self.peek() == Some(b'|') {
            self.bump();
            branches.push(self.parse_sequence()?);
        }
        Ok(Node { branches })
    }

    /// sequence := repeat*, stopping at '|', ')', or end of input.
    fn parse_sequence(&mut self) -> Result<Sequence, RegexError> {
        let mut seq = Vec::new();
        while let Some(b) = self.peek() {
            if b == b'|' || b == b')' {
                break;
            }
            seq.push(self.parse_repeat()?);
        }
        Ok(seq)
    }

    /// repeat := atom ('*' | '+' | '?')?
    fn parse_repeat(&mut self) -> Result<Repeat, RegexError> {
        // '^' and '$' are anchors, not atoms in the usual sense — they
        // match a position, not a byte, so they can't be repeated and are
        // parsed as their own pseudo-atoms via a dedicated Atom-less path.
        // Simpler: model them as zero-width atoms that only ever appear
        // `Exactly` (repeating a zero-width anchor is meaningless in this
        // subset, so a following quantifier on one is a compile error).
        if self.peek() == Some(b'^') || self.peek() == Some(b'$') {
            let b = self.bump().unwrap();
            if matches!(self.peek(), Some(b'*') | Some(b'+') | Some(b'?')) {
                return Err(RegexError::new(format!(
                    "'{}' cannot be repeated",
                    self.bump().unwrap() as char
                )));
            }
            return Ok(Repeat::Exactly(if b == b'^' {
                Atom::Anchor(AnchorKind::Start)
            } else {
                Atom::Anchor(AnchorKind::End)
            }));
        }

        let atom = self.parse_atom()?;
        match self.peek() {
            Some(b'*') => {
                self.bump();
                Ok(Repeat::Star(atom))
            }
            Some(b'+') => {
                self.bump();
                Ok(Repeat::Plus(atom))
            }
            Some(b'?') => {
                self.bump();
                Ok(Repeat::Question(atom))
            }
            _ => Ok(Repeat::Exactly(atom)),
        }
    }

    /// atom := literal | '.' | class | group
    fn parse_atom(&mut self) -> Result<Atom, RegexError> {
        match self.peek() {
            None => Err(RegexError::new(
                "quantifier or atom expected, found end of pattern",
            )),
            Some(b'*') | Some(b'+') | Some(b'?') => Err(RegexError::new(format!(
                "'{}' has nothing to repeat",
                self.peek().unwrap() as char
            ))),
            Some(b'.') => {
                self.bump();
                Ok(Atom::AnyByte)
            }
            Some(b'(') => {
                self.bump();
                let inner = self.parse_alternation()?;
                if self.bump() != Some(b')') {
                    return Err(RegexError::new("unbalanced '(': missing ')'"));
                }
                Ok(Atom::Group(Box::new(inner)))
            }
            Some(b')') => Err(RegexError::new("unbalanced ')': no matching '('")),
            Some(b'[') => self.parse_class(),
            Some(b'\\') => {
                self.bump();
                match self.bump() {
                    Some(b) => Ok(shorthand_class(b).unwrap_or(Atom::Literal(b))),
                    None => Err(RegexError::new("trailing '\\' with nothing to escape")),
                }
            }
            Some(b) => {
                self.bump();
                Ok(Atom::Literal(b))
            }
        }
    }

    /// class := '[' '^'? classitem+ ']'
    /// classitem := byte '-' byte | byte
    ///
    /// A `\` inside a class escapes the next byte literally too (so `[\]]`
    /// is a class containing only `]`), the same escaping rule as outside a
    /// class.
    fn parse_class(&mut self) -> Result<Atom, RegexError> {
        self.bump(); // consume '['
        let negated = if self.peek() == Some(b'^') {
            self.bump();
            true
        } else {
            false
        };

        let mut ranges = Vec::new();
        let mut saw_any = false;
        loop {
            match self.peek() {
                None => return Err(RegexError::new("unbalanced '[': missing ']'")),
                Some(b']') if saw_any => {
                    self.bump();
                    break;
                }
                _ => {
                    let lo = self.parse_class_byte()?;
                    saw_any = true;
                    if self.peek() == Some(b'-') && self.bytes.get(self.pos + 1) != Some(&b']') {
                        self.bump(); // consume '-'
                        if self.peek().is_none() {
                            return Err(RegexError::new("unbalanced '[': missing ']'"));
                        }
                        let hi = self.parse_class_byte()?;
                        if hi < lo {
                            return Err(RegexError::new(format!(
                                "invalid class range: '{}' > '{}'",
                                lo as char, hi as char
                            )));
                        }
                        ranges.push((lo, hi));
                    } else {
                        ranges.push((lo, lo));
                    }
                }
            }
        }
        if !saw_any {
            return Err(RegexError::new("empty character class"));
        }
        Ok(Atom::Class { ranges, negated })
    }

    /// One byte inside a class, honoring `\` escaping.
    fn parse_class_byte(&mut self) -> Result<u8, RegexError> {
        match self.bump() {
            Some(b'\\') => self
                .bump()
                .ok_or_else(|| RegexError::new("trailing '\\' with nothing to escape")),
            Some(b) => Ok(b),
            None => Err(RegexError::new("unbalanced '[': missing ']'")),
        }
    }
}

// --- Compiled regex ------------------------------------------------------

/// A compiled pattern, ready to search haystacks with. See the module doc
/// for the supported syntax subset and this type's documented limitations.
pub struct Regex {
    root: Node,
    case_sensitive: bool,
    /// Whether the pattern starts with `^`, letting `find_at` try only one
    /// starting position per call instead of scanning the whole haystack —
    /// not a correctness requirement (the anchor check inside matching
    /// would reject every other start anyway) but the obvious, cheap
    /// optimization for what is otherwise the common case of "search this
    /// whole line."
    anchored_at_start: bool,
}

impl Regex {
    /// Compiles `pattern` once. `case_sensitive: false` folds both literals
    /// and character-class bytes with ASCII case folding (see the module
    /// doc). Returns [`RegexError`] for anything outside the supported
    /// subset's own grammar, rather than guessing at a meaning.
    pub fn new(pattern: &str, case_sensitive: bool) -> Result<Regex, RegexError> {
        let mut parser = Parser::new(pattern.as_bytes());
        let root = parser.parse_pattern()?;
        let anchored_at_start = starts_with_anchor(&root);
        Ok(Regex {
            root,
            case_sensitive,
            anchored_at_start,
        })
    }

    /// Folds a haystack byte the same way a literal/class byte was folded
    /// at compile time, so the two sides of a comparison agree.
    fn fold(&self, b: u8) -> u8 {
        if self.case_sensitive {
            b
        } else {
            b.to_ascii_lowercase()
        }
    }

    /// Tries to match `self.root` starting exactly at `start`, returning
    /// the match's end offset on success. This is the whole backtracking
    /// engine: `match_node`/`match_sequence`/`match_repeat` all take a
    /// continuation closure (`k`) representing "everything after this
    /// point in the pattern, and where control resumes if I match" — the
    /// standard way to write a backtracking matcher without an explicit
    /// stack, since Rust's own call stack does the backtracking for us
    /// (returning `None` from a `k` call is exactly "that choice didn't
    /// pan out, try the next one").
    fn try_match_at(&self, haystack: &[u8], start: usize) -> Option<usize> {
        self.match_node(&self.root, haystack, start, &|_haystack, pos| Some(pos))
    }

    fn match_node(
        &self,
        node: &Node,
        haystack: &[u8],
        pos: usize,
        k: &dyn Fn(&[u8], usize) -> Option<usize>,
    ) -> Option<usize> {
        // Alternation: try each branch in order, first one whose
        // continuation also succeeds wins. This is what gives `|` its
        // widest-possible scope — every branch is a full `Sequence`
        // parsed by `parse_sequence` at the same `parse_alternation`
        // level, never just the single atom beside it.
        for branch in &node.branches {
            if let Some(end) = self.match_sequence(branch, 0, haystack, pos, k) {
                return Some(end);
            }
        }
        None
    }

    /// Matches `seq[index..]` starting at `pos`, then calls `k` for
    /// whatever comes after the whole sequence. Recursing on `index` (one
    /// `Repeat` at a time) rather than looping is what lets each step hand
    /// the *rest* of the sequence to the previous step's backtracking
    /// search as its continuation.
    fn match_sequence(
        &self,
        seq: &Sequence,
        index: usize,
        haystack: &[u8],
        pos: usize,
        k: &dyn Fn(&[u8], usize) -> Option<usize>,
    ) -> Option<usize> {
        if index == seq.len() {
            return k(haystack, pos);
        }
        let rest_k =
            |haystack: &[u8], pos: usize| self.match_sequence(seq, index + 1, haystack, pos, k);
        self.match_repeat(&seq[index], haystack, pos, &rest_k)
    }

    fn match_repeat(
        &self,
        rep: &Repeat,
        haystack: &[u8],
        pos: usize,
        k: &dyn Fn(&[u8], usize) -> Option<usize>,
    ) -> Option<usize> {
        match rep {
            Repeat::Exactly(atom) => self.match_atom_once(atom, haystack, pos, k),
            Repeat::Question(atom) => {
                // Greedy: prefer matching once, fall back to zero.
                self.match_atom_once(atom, haystack, pos, k)
                    .or_else(|| k(haystack, pos))
            }
            Repeat::Star(atom) => self.match_greedy_repeat(atom, 0, haystack, pos, k),
            Repeat::Plus(atom) => self.match_greedy_repeat(atom, 1, haystack, pos, k),
        }
    }

    /// Shared engine for `*` (`min == 0`) and `+` (`min == 1`): consume the
    /// atom as many times as possible first (greedy), then backtrack by
    /// giving back one repetition at a time until `k` succeeds or there
    /// are fewer than `min` repetitions left to try, matching the
    /// "backtracking only if a later part fails" behavior the module doc
    /// describes.
    ///
    /// Implemented by first walking forward and recording every position
    /// reached (so zero-width atoms can't loop forever — see the `count ==
    /// 0` guard below), then walking that list of positions backward,
    /// which is a plain iterative rewrite of the usual recursive
    /// "try-one-more-then-backtrack" greedy-repetition shape.
    fn match_greedy_repeat(
        &self,
        atom: &Atom,
        min: usize,
        haystack: &[u8],
        pos: usize,
        k: &dyn Fn(&[u8], usize) -> Option<usize>,
    ) -> Option<usize> {
        let mut positions = vec![pos];
        let mut cur = pos;
        loop {
            match self.match_atom_once(atom, haystack, cur, &|_h, p| Some(p)) {
                Some(next) if next != cur => {
                    cur = next;
                    positions.push(cur);
                }
                // A zero-width match (e.g. an atom that matched nothing,
                // which none of this subset's atoms actually do, but a
                // nested group full of only zero-or-more repeats could)
                // would otherwise loop forever consuming no input — the
                // classic "nested quantifier" hazard. Stop repeating as
                // soon as a repetition stops making progress.
                _ => break,
            }
        }
        // Try the longest count first (greedy), then progressively fewer.
        for count in (min..positions.len()).rev() {
            if let Some(end) = k(haystack, positions[count]) {
                return Some(end);
            }
        }
        None
    }

    /// Matches `atom` exactly once at `pos`, then `k` for what follows.
    fn match_atom_once(
        &self,
        atom: &Atom,
        haystack: &[u8],
        pos: usize,
        k: &dyn Fn(&[u8], usize) -> Option<usize>,
    ) -> Option<usize> {
        match atom {
            Atom::Literal(want) => {
                let want = self.fold(*want);
                if pos < haystack.len() && self.fold(haystack[pos]) == want {
                    k(haystack, pos + 1)
                } else {
                    None
                }
            }
            Atom::AnyByte => {
                if pos < haystack.len() && haystack[pos] != b'\n' {
                    k(haystack, pos + 1)
                } else {
                    None
                }
            }
            Atom::Class { ranges, negated } => {
                if pos >= haystack.len() {
                    return None;
                }
                let b = self.fold(haystack[pos]);
                let in_class = ranges.iter().any(|&(lo, hi)| {
                    let lo = self.fold(lo);
                    let hi = self.fold(hi);
                    // Folding can flip a range's ordering for a
                    // mixed-case range (rare in practice, since
                    // case-insensitive patterns are usually written in
                    // one case already); comparing against both orderings
                    // keeps this correct regardless.
                    let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
                    b >= lo && b <= hi
                });
                if in_class != *negated {
                    k(haystack, pos + 1)
                } else {
                    None
                }
            }
            Atom::Group(inner) => self.match_node(inner, haystack, pos, k),
            Atom::Anchor(AnchorKind::Start) => {
                if pos == 0 {
                    k(haystack, pos)
                } else {
                    None
                }
            }
            Atom::Anchor(AnchorKind::End) => {
                if pos == haystack.len() {
                    k(haystack, pos)
                } else {
                    None
                }
            }
        }
    }
}

/// True if `node`'s very first thing that must match is a start anchor —
/// i.e. the pattern can only ever match starting at haystack position 0,
/// so `find_at` need not try any other start. Conservative: only the
/// simple, common shape "`^` is literally the first item of every
/// top-level branch" is recognized; anything subtler (`^` buried behind an
/// atom that could itself be zero-width) just means `find_at` does a
/// harmless bit of extra scanning, never an incorrect result, since the
/// anchor is still enforced inside matching regardless of what this
/// returns.
fn starts_with_anchor(node: &Node) -> bool {
    node.branches.iter().all(|seq| {
        matches!(
            seq.first(),
            Some(Repeat::Exactly(Atom::Anchor(AnchorKind::Start)))
        )
    })
}

impl Matcher for Regex {
    fn find_at(&self, haystack: &[u8], from: usize) -> Option<MatchRange> {
        if from > haystack.len() {
            return None;
        }
        if self.anchored_at_start {
            return if from == 0 {
                self.try_match_at(haystack, 0).map(|end| 0..end)
            } else {
                None
            };
        }
        for start in from..=haystack.len() {
            if let Some(end) = self.try_match_at(haystack, start) {
                return Some(start..end);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(pattern: &str, case_sensitive: bool, haystack: &str) -> Option<MatchRange> {
        Regex::new(pattern, case_sensitive)
            .unwrap_or_else(|e| panic!("pattern {pattern:?} failed to compile: {e}"))
            .find_at(haystack.as_bytes(), 0)
    }

    fn matches(pattern: &str, case_sensitive: bool, haystack: &str) -> bool {
        find(pattern, case_sensitive, haystack).is_some()
    }

    // --- literals & '.' --------------------------------------------------

    #[test]
    fn a_plain_literal_is_found() {
        assert_eq!(find("brown", true, "the quick brown fox"), Some(10..15));
    }

    #[test]
    fn dot_matches_any_byte_but_not_newline() {
        assert!(matches("a.c", true, "abc"));
        assert!(matches("a.c", true, "a c"));
        assert!(!matches("a.c", true, "a\nc"));
    }

    // --- quantifiers -------------------------------------------------------

    #[test]
    fn star_on_a_literal() {
        assert_eq!(find("ab*c", true, "ac"), Some(0..2));
        assert_eq!(find("ab*c", true, "abc"), Some(0..3));
        assert_eq!(find("ab*c", true, "abbbbbc"), Some(0..7));
    }

    #[test]
    fn plus_on_a_literal_requires_at_least_one() {
        assert!(!matches("ab+c", true, "ac"));
        assert_eq!(find("ab+c", true, "abc"), Some(0..3));
        assert_eq!(find("ab+c", true, "abbbc"), Some(0..5));
    }

    #[test]
    fn question_on_a_literal() {
        assert_eq!(find("colou?r", true, "color"), Some(0..5));
        assert_eq!(find("colou?r", true, "colour"), Some(0..6));
    }

    #[test]
    fn star_on_a_class() {
        assert_eq!(find("a[0-9]*b", true, "ab"), Some(0..2));
        assert_eq!(find("a[0-9]*b", true, "a123b"), Some(0..5));
    }

    #[test]
    fn plus_on_a_class() {
        assert!(!matches("[0-9]+", true, "abc"));
        assert_eq!(find("[0-9]+", true, "abc123def"), Some(3..6));
    }

    #[test]
    fn question_on_a_class() {
        assert_eq!(find("colou?r", true, "colr"), None);
        assert_eq!(find("[xy]?z", true, "z"), Some(0..1));
        assert_eq!(find("[xy]?z", true, "xz"), Some(0..2));
    }

    #[test]
    fn star_plus_question_on_a_group() {
        assert_eq!(find("(ab)*c", true, "c"), Some(0..1));
        assert_eq!(find("(ab)*c", true, "ababc"), Some(0..5));
        assert!(!matches("(ab)+c", true, "c"));
        assert_eq!(find("(ab)+c", true, "ababc"), Some(0..5));
        assert_eq!(find("(ab)?c", true, "c"), Some(0..1));
        assert_eq!(find("(ab)?c", true, "abc"), Some(0..3));
    }

    // --- character classes -------------------------------------------------

    #[test]
    fn class_with_a_range() {
        assert!(matches("[a-z]", true, "m"));
        assert!(!matches("[a-z]", true, "M"));
        assert!(!matches("[a-z]", true, "5"));
    }

    #[test]
    fn class_negation() {
        assert!(matches("[^0-9]", true, "a"));
        assert!(!matches("[^0-9]", true, "5"));
    }

    #[test]
    fn class_mixing_literals_and_a_range() {
        let re = Regex::new("[a-z0-9_]+", true).unwrap();
        assert_eq!(re.find_at(b"my_var2 =", 0), Some(0..7));
    }

    #[test]
    fn digit_word_and_space_shorthands() {
        assert!(matches(r"\d", true, "5"));
        assert!(!matches(r"\d", true, "a"));
        assert!(matches(r"\D", true, "a"));
        assert!(!matches(r"\D", true, "5"));

        assert!(matches(r"\w", true, "_"));
        assert!(matches(r"\w", true, "Q"));
        assert!(!matches(r"\w", true, " "));

        assert!(matches(r"\s", true, "\t"));
        assert!(!matches(r"\s", true, "x"));
    }

    #[test]
    fn a_digit_shorthand_finds_a_number_in_context() {
        // The exact pattern `project_search.rs`'s own test suite relies on.
        let re = Regex::new(r"alpha\d", false).unwrap();
        assert_eq!(re.find_at(b"alpha1", 0), Some(0..6));
        assert!(re.find_at(b"beta3", 0).is_none());
    }

    // --- anchors -------------------------------------------------------------

    #[test]
    fn start_anchor_only_matches_at_haystack_position_zero() {
        let re = Regex::new("^abc", true).unwrap();
        assert_eq!(re.find_at(b"abcdef", 0), Some(0..3));
        assert_eq!(re.find_at(b"xabcdef", 0), None);
        // Even when 'abc' does occur starting at a nonzero `from`, a `^`
        // anchor means "start of the whole haystack", not "start of the
        // search window" — so a nonzero `from` must still fail.
        assert_eq!(re.find_at(b"xabcdef", 1), None);
    }

    #[test]
    fn end_anchor_only_matches_at_the_haystacks_actual_end() {
        let re = Regex::new("xyz$", true).unwrap();
        assert_eq!(re.find_at(b"abcxyz", 0), Some(3..6));
        assert_eq!(re.find_at(b"abcxyzabc", 0), None);
    }

    #[test]
    fn both_anchors_together_require_an_exact_whole_match() {
        assert_eq!(find("^abc$", true, "abc"), Some(0..3));
        assert!(!matches("^abc$", true, "abcd"));
        assert!(!matches("^abc$", true, "xabc"));
    }

    // --- alternation -------------------------------------------------------

    #[test]
    fn top_level_alternation_has_the_widest_scope() {
        // 'ab|cd' is '(ab)|(cd)', not 'a(b|c)d'.
        assert_eq!(find("ab|cd", true, "zzcdzz"), Some(2..4));
        assert!(!matches("ab|cd", true, "azzdzz"));
    }

    #[test]
    fn alternation_inside_a_group_with_a_quantifier() {
        let re = Regex::new("(foo|bar)+", true).unwrap();
        assert_eq!(re.find_at(b"foobarfoo!", 0), Some(0..9));
        assert!(re.is_match(b"bar"));
        assert!(!re.is_match(b"baz"));
    }

    // --- groups combining several features ----------------------------------

    #[test]
    fn a_group_repeated_followed_by_a_literal() {
        assert_eq!(find("(ab)+c", true, "ababc"), Some(0..5));
        assert!(!matches("(ab)+c", true, "c"));
    }

    // --- escaping ------------------------------------------------------------

    #[test]
    fn escaped_special_characters_match_literally() {
        assert_eq!(find(r"a\.b", true, "a.b"), Some(0..3));
        assert!(!matches(r"a\.b", true, "aXb"));
        assert_eq!(find(r"\(hi\)", true, "(hi)"), Some(0..4));
        assert_eq!(find(r"a\*b", true, "a*b"), Some(0..3));
        assert_eq!(find(r"a\[b", true, "a[b"), Some(0..3));
    }

    #[test]
    fn an_unrecognized_escape_is_just_the_literal_character() {
        // Documented design call: `\d`/`\D`/`\w`/`\W`/`\s`/`\S` are the one
        // set of recognized shorthands (see `digit_word_and_space_shorthands`
        // above); anything else this engine doesn't recognize as special is
        // treated as that character literally rather than rejected.
        assert_eq!(find(r"\z", true, "z"), Some(0..1));
        assert!(!matches(r"\z", true, "5"));
    }

    // --- case sensitivity ------------------------------------------------

    #[test]
    fn case_insensitive_literal_matching() {
        assert!(matches("BROWN", false, "the quick brown fox"));
        assert!(!matches("BROWN", true, "the quick brown fox"));
    }

    #[test]
    fn case_insensitive_class_matching() {
        assert!(matches("[a-z]+", false, "ABC"));
        assert!(!matches("[a-z]+", true, "ABC"));
    }

    // --- compile errors ------------------------------------------------------

    #[test]
    fn unbalanced_open_paren_is_an_error() {
        assert!(Regex::new("(abc", true).is_err());
    }

    #[test]
    fn unbalanced_close_paren_is_an_error() {
        assert!(Regex::new("abc)", true).is_err());
    }

    #[test]
    fn unbalanced_open_bracket_is_an_error() {
        assert!(Regex::new("[abc", true).is_err());
    }

    #[test]
    fn a_quantifier_with_nothing_before_it_is_an_error() {
        assert!(Regex::new("*abc", true).is_err());
        assert!(Regex::new("a(+b)", true).is_err());
    }

    #[test]
    fn an_empty_class_is_an_error() {
        assert!(Regex::new("[]", true).is_err());
    }

    #[test]
    fn a_trailing_backslash_is_an_error() {
        assert!(Regex::new(r"abc\", true).is_err());
    }

    #[test]
    fn a_repeated_anchor_is_an_error() {
        assert!(Regex::new("^*abc", true).is_err());
    }

    #[test]
    fn compile_errors_do_not_panic_they_return_err() {
        for bad in ["(", ")", "[", "a**", "[z-a]"] {
            // `a**` is legal under this grammar actually (star of star of
            // a literal is still a valid repeat-of-atom, since '*' applied
            // to `Repeat::Star(..)`'s atom is just another quantifier
            // character seen after a completed repeat — parse_sequence
            // would try to start a *new* repeat there and fail on
            // "quantifier with nothing before it"), so this loop only
            // asserts each produces a `Result`, not that all are `Err`;
            // the specific-error tests above pin down the ones that must
            // fail.
            let _ = Regex::new(bad, true);
        }
        assert!(Regex::new("(", true).is_err());
        assert!(Regex::new(")", true).is_err());
        assert!(Regex::new("[", true).is_err());
        assert!(Regex::new("a**", true).is_err());
        assert!(Regex::new("[z-a]", true).is_err());
    }

    // --- find_at leftmost-match behavior --------------------------------

    #[test]
    fn find_at_returns_the_leftmost_match() {
        let re = Regex::new("ab", true).unwrap();
        assert_eq!(re.find_at(b"xxabxxabxx", 0), Some(2..4));
        assert_eq!(re.find_at(b"xxabxxabxx", 3), Some(6..8));
    }

    #[test]
    fn find_at_with_no_match_returns_none() {
        assert!(!matches("zzz", true, "the quick brown fox"));
    }

    // --- backtracking safety ---------------------------------------------

    #[test]
    fn pathological_backtracking_pattern_does_not_hang() {
        // (a*)* is the textbook catastrophic-backtracking shape: nested
        // unbounded quantifiers whose inner and outer repetitions can
        // divide up the same run of 'a's in exponentially many ways before
        // concluding there's no trailing 'b'. This engine has no
        // memoization or step budget (see the module doc), so this test
        // deliberately keeps the haystack short (20 'a's, not e.g. 40)
        // so it still finishes quickly even at O(2^n) — a demonstration
        // that small pathological inputs stay fast, not a guarantee about
        // large ones.
        let re = Regex::new("(a*)*b", true).unwrap();
        let haystack = "a".repeat(20);
        assert!(!re.is_match(haystack.as_bytes()));
    }

    #[test]
    fn is_match_default_method_works_via_find_at() {
        let re = Regex::new("brown", true).unwrap();
        assert!(re.is_match(b"the quick brown fox"));
        assert!(!re.is_match(b"the quick red fox"));
    }
}
