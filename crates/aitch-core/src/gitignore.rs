//! A hand-written `.gitignore` pattern matcher — Phase 4 Track A of
//! [`PLAN-ZERO-DEP.md`](../../../../PLAN-ZERO-DEP.md) §4, replacing the
//! `ignore` crate's `Gitignore`/`GitignoreBuilder` (the pattern-matching half
//! only; discovering *which* `.gitignore` files apply to a path, walking
//! directories, and pruning excluded subtrees are Track D's job in
//! `walk.rs`, not this module's — see `project.rs`'s `walker` doc comment for
//! the pruning behavior this module deliberately does not attempt to
//! reproduce on its own).
//!
//! [`Rules`] compiles one `.gitignore` file's worth of lines into an ordered
//! list of patterns and matches a single candidate path — already made
//! relative to that `.gitignore`'s own directory by the caller — against
//! them. Semantics follow `git help gitignore`:
//!
//! - `#` starts a comment; a blank line is ignored. Both are line-level: a
//!   line whose first character is literally `#` is a comment, so `\#foo` (a
//!   pattern that starts with a literal `#`) is not, because its first
//!   character is `\`. The same reasoning covers negation below, and means
//!   neither case needs special-casing — the generic backslash-escape
//!   handling in pattern bodies already produces the right literal character.
//! - `!` negates a pattern. Patterns are matched in file order and the last
//!   matching pattern wins, so a negation only un-ignores a path if it comes
//!   *after* the pattern that ignored it; a negation earlier in the file has
//!   no effect once a later pattern matches too.
//! - A pattern containing a `/` anywhere but its last character is anchored
//!   to the `.gitignore`'s own directory (matches only at that exact depth);
//!   a pattern with no interior `/` matches at any depth, as if `**/` were
//!   prepended.
//! - `*` matches any run of characters within one path segment; `?` matches
//!   exactly one character within one segment; neither crosses a `/`.
//! - `**` as a whole path segment matches zero or more entire segments:
//!   leading (`**/foo`), middle (`a/**/b`), or trailing (`a/**`, which
//!   matches everything *inside* `a` but, per git, not `a` itself — a
//!   trailing `**` requires at least one more segment to consume).
//! - A trailing `/` restricts a pattern to directories; it never matches a
//!   file of the same name.
//!
//! A line that reduces to nothing matchable (`/` alone, `//`, a bare `!`)
//! does not panic and does not contribute a pattern — it is silently
//! dropped, the same "a rule that will not compile is skipped rather than
//! fatal" philosophy `project.rs`'s `walker` doc comment already states for
//! `GitignoreBuilder::add_line`.
//!
//! Matching is always case-sensitive, on every platform — git never folds
//! case, and this module doesn't either, including on Windows.

use std::path::Path;

/// One compiled `.gitignore` file: an ordered list of patterns.
///
/// Order matters. [`Rules::is_ignored`] walks every pattern and keeps the
/// verdict of the *last* one that matches, exactly like git.
#[derive(Debug, Clone, Default)]
pub struct Rules {
    patterns: Vec<Pattern>,
}

impl Rules {
    /// Compile a `.gitignore` file's contents into a set of rules.
    ///
    /// One pattern per non-blank, non-comment line. A line that fails to
    /// produce a usable pattern (see the module docs) is skipped rather than
    /// rejected outright — malformed input never panics and never poisons
    /// the rest of the file's rules.
    pub fn parse(contents: &str) -> Rules {
        let patterns = contents.lines().filter_map(Pattern::parse_line).collect();
        Rules { patterns }
    }

    /// Same as [`Rules::parse`], for a caller that already has the file
    /// split into lines (e.g. read via `BufRead::lines`) rather than one
    /// `String`.
    pub fn from_lines<S: AsRef<str>>(lines: &[S]) -> Rules {
        let patterns = lines
            .iter()
            .filter_map(|line| Pattern::parse_line(line.as_ref()))
            .collect();
        Rules { patterns }
    }

    /// Is `path` ignored by these rules?
    ///
    /// `path` is relative to this `.gitignore`'s own directory — the caller
    /// (Track D's walker) is responsible for that relative-path computation.
    /// `is_dir` tells a directory-only (trailing-`/`) pattern whether it's
    /// allowed to match at all.
    ///
    /// This evaluates every pattern against `path` in file order and returns
    /// whichever verdict the last matching one gives — it does not know
    /// about directory pruning (a negated pattern for a path underneath an
    /// already-ignored directory will still report "not ignored" here, since
    /// this function only ever sees the one path it's asked about). Whether
    /// that path is ever reached at all — the classic git gotcha where a
    /// negated file inside an ignored directory stays ignored because the
    /// directory itself is never walked into — is exactly the pruning
    /// behavior `project.rs`'s `walker` doc comment describes, and it lives
    /// in the walker, not here.
    ///
    /// Equivalent to `self.verdict(path, is_dir).unwrap_or(false)` — a
    /// single `.gitignore` file with nothing to say about a path is the same
    /// as one that doesn't ignore it. A caller combining *several* files'
    /// worth of rules (the parent-directory chain a real walk accumulates)
    /// needs [`Rules::verdict`] instead, to tell "this file said nothing" —
    /// defer to whatever a parent directory's rules already decided — apart
    /// from "this file explicitly said don't ignore," which is a real
    /// override a parent's broader ignore rule cannot see through this
    /// method alone.
    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        self.verdict(path, is_dir).unwrap_or(false)
    }

    /// Like [`Rules::is_ignored`], but distinguishes "no pattern here said
    /// anything about this path" (`None`) from "the last matching pattern
    /// explicitly ignored it" (`Some(true)`) or "...explicitly re-included
    /// it" (`Some(false)`).
    ///
    /// This distinction is exactly what a caller combining multiple
    /// `.gitignore` files needs and a bare `bool` cannot express: git treats
    /// the applicable files from every directory down to a path's own as one
    /// combined, ordered list of patterns (least specific — closest to the
    /// root — first) with last-match-wins across the *whole* list, not
    /// separately per file. If this file has an opinion (`Some`), that
    /// opinion is the one to carry forward as the running verdict *unless* a
    /// still-more-specific file later in the chain also has one; if this
    /// file says nothing (`None`), whatever a less-specific file already
    /// decided must stand, which a bare `bool` collapsing "no match" and
    /// "explicitly false" into the same value cannot represent.
    pub fn verdict(&self, path: &Path, is_dir: bool) -> Option<bool> {
        let owned_segments: Vec<String> = path_segments(path);
        let segments: Vec<&str> = owned_segments.iter().map(String::as_str).collect();

        let mut verdict = None;
        for pattern in &self.patterns {
            if pattern.dir_only && !is_dir {
                continue;
            }
            if pattern.matches(&segments) {
                verdict = Some(!pattern.negated);
            }
        }
        verdict
    }
}

/// Split a path into its component names, in order, using `Path`'s own
/// component parser rather than splitting on a literal separator character
/// — this makes matching agree on both `/`- and `\`-separated input, and
/// drops anything that isn't a plain named segment (a root, a prefix, `.`,
/// `..`).
fn path_segments(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

#[derive(Debug, Clone)]
struct Pattern {
    negated: bool,
    dir_only: bool,
    /// If true, this pattern was written with an interior `/` (anywhere but
    /// the last character) and only matches at the `.gitignore`'s own
    /// directory depth. If false, an implicit leading [`Seg::DoubleStar`] is
    /// prepended so it matches at any depth.
    segments: Vec<Seg>,
}

impl Pattern {
    /// Parse one raw line (still possibly a comment or blank) into a
    /// [`Pattern`], or `None` if it's a comment, blank, or reduces to
    /// nothing matchable.
    fn parse_line(line: &str) -> Option<Pattern> {
        // Trailing whitespace is always stripped. Real git only strips
        // *unescaped* trailing whitespace (`foo\ ` keeps its trailing
        // space) — that edge case is deliberately not supported here; it's
        // obscure enough that treating it as always-insignificant is a
        // reasonable, documented simplification.
        let line = line.trim_end();

        if line.is_empty() {
            return None;
        }
        // A literal '#' comment. A pattern meaning a literal leading '#' is
        // written `\#...`, whose first byte is '\\', not '#' — so no
        // special-casing is needed here to tell the two apart.
        if line.starts_with('#') {
            return None;
        }

        // Same reasoning for negation: `\!...` starts with '\\', not '!'.
        let (negated, rest) = match line.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        if rest.is_empty() {
            // A bare "!" has nothing to negate — skip rather than treat as
            // an empty always-matching pattern.
            return None;
        }

        let (dir_only, body) = match rest.strip_suffix('/') {
            Some(stripped) => (true, stripped),
            None => (false, rest),
        };
        if body.is_empty() {
            // "/" or "!/" alone: nothing left to match.
            return None;
        }

        // A '/' anywhere in `body` (leading, interior — trailing was already
        // stripped above) anchors the pattern to this .gitignore's own
        // directory rather than letting it match at any depth.
        let anchored = body.contains('/');
        let body = body.strip_prefix('/').unwrap_or(body);

        let mut segments = Vec::new();
        for raw in body.split('/') {
            if raw.is_empty() {
                // A doubled slash ("a//b") or similar — malformed, skip the
                // whole pattern rather than guess at intent.
                return None;
            }
            segments.push(if raw == "**" {
                Seg::DoubleStar
            } else {
                Seg::Literal(parse_glob_segment(raw))
            });
        }

        if !anchored {
            segments.insert(0, Seg::DoubleStar);
        }

        Some(Pattern {
            negated,
            dir_only,
            segments,
        })
    }

    fn matches(&self, path_segments: &[&str]) -> bool {
        segments_match(&self.segments, path_segments)
    }
}

/// One segment of a compiled pattern: either a literal `.gitignore` path
/// segment written as a whole `**`, or a segment matched with glob rules
/// (`*` / `?` / literal characters) via [`Elem`].
#[derive(Debug, Clone, PartialEq)]
enum Seg {
    DoubleStar,
    Literal(Vec<Elem>),
}

/// One token of a single path-segment glob.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Elem {
    Lit(char),
    /// `*` — zero or more characters, never crossing a `/`.
    Star,
    /// `?` — exactly one character.
    Any,
}

/// Turn one `/`-free raw segment (already known not to be a bare `**`) into
/// glob tokens, resolving `\x` escapes to a literal `x` as it goes. A
/// trailing lone backslash (nothing left to escape) is dropped rather than
/// treated as an error.
fn parse_glob_segment(raw: &str) -> Vec<Elem> {
    let mut out = Vec::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(escaped) = chars.next() {
                    out.push(Elem::Lit(escaped));
                }
            }
            '*' => out.push(Elem::Star),
            '?' => out.push(Elem::Any),
            other => out.push(Elem::Lit(other)),
        }
    }
    out
}

/// Match a compiled pattern's segments against a candidate path's segments,
/// in full — every path segment must be accounted for, since an anchored
/// pattern names an exact relative path and an unanchored one already has an
/// implicit leading [`Seg::DoubleStar`] to absorb whatever comes before it.
fn segments_match(pattern: &[Seg], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((Seg::DoubleStar, rest)) => {
            if rest.is_empty() {
                // A trailing "**" matches everything *inside* the preceding
                // directory, per git — it needs at least one more segment,
                // it does not also match the empty remainder (the directory
                // itself).
                !path.is_empty()
            } else {
                // "**" can absorb zero segments (try the rest immediately)
                // or one-and-recurse (try absorbing another segment).
                segments_match(rest, path)
                    || (!path.is_empty() && segments_match(pattern, &path[1..]))
            }
        }
        Some((Seg::Literal(elems), rest)) => match path.split_first() {
            Some((first, path_rest)) => {
                glob_match_segment(elems, first) && segments_match(rest, path_rest)
            }
            None => false,
        },
    }
}

/// Classic wildcard match of one glob-token segment against one literal path
/// segment: `*` (zero or more chars), `?` (exactly one char), and literal
/// characters. Iterative with backtracking on the most recent `*`, so a
/// segment with several stars doesn't recurse per character.
fn glob_match_segment(pattern: &[Elem], text: &str) -> bool {
    let text: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None; // (pattern index after '*', text index tried)

    while ti < text.len() {
        if pi < pattern.len() {
            match pattern[pi] {
                Elem::Lit(c) if c == text[ti] => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                Elem::Any => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                Elem::Star => {
                    star = Some((pi + 1, ti));
                    pi += 1;
                    continue;
                }
                _ => {}
            }
        }
        if let Some((star_pi, star_ti)) = star {
            let next_ti = star_ti + 1;
            star = Some((star_pi, next_ti));
            pi = star_pi;
            ti = next_ti;
        } else {
            return false;
        }
    }
    while matches!(pattern.get(pi), Some(Elem::Star)) {
        pi += 1;
    }
    pi == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ignored(rules: &Rules, path: &str, is_dir: bool) -> bool {
        rules.is_ignored(Path::new(path), is_dir)
    }

    #[test]
    fn plain_name_matches_at_any_depth() {
        let rules = Rules::parse("foo.txt");
        assert!(ignored(&rules, "foo.txt", false));
        assert!(ignored(&rules, "sub/foo.txt", false));
        assert!(ignored(&rules, "a/b/foo.txt", false));
        assert!(!ignored(&rules, "foo.txt.bak", false));
    }

    #[test]
    fn star_matches_within_one_segment() {
        let rules = Rules::parse("*.log");
        assert!(ignored(&rules, "debug.log", false));
        assert!(ignored(&rules, "logs/debug.log", false));
        // '*' must not cross the '/'.
        assert!(!ignored(&rules, "logdir/file", false));
        assert!(!ignored(&rules, "notlog", false));
    }

    #[test]
    fn question_mark_matches_exactly_one_char() {
        let rules = Rules::parse("fil?.txt");
        assert!(ignored(&rules, "file.txt", false));
        assert!(!ignored(&rules, "fil.txt", false));
        assert!(!ignored(&rules, "filee.txt", false));
    }

    #[test]
    fn doublestar_leading() {
        let rules = Rules::parse("**/foo");
        assert!(ignored(&rules, "foo", false));
        assert!(ignored(&rules, "a/foo", false));
        assert!(ignored(&rules, "a/b/foo", false));
        assert!(!ignored(&rules, "foobar", false));
    }

    #[test]
    fn doublestar_middle_matches_zero_or_more_dirs() {
        let rules = Rules::parse("a/**/b");
        assert!(ignored(&rules, "a/b", false));
        assert!(ignored(&rules, "a/x/b", false));
        assert!(ignored(&rules, "a/x/y/b", false));
        // Anchored: must start with "a" at the top.
        assert!(!ignored(&rules, "x/a/b", false));
        assert!(!ignored(&rules, "a/c", false));
    }

    #[test]
    fn doublestar_trailing_matches_contents_not_the_dir_itself() {
        let rules = Rules::parse("a/**");
        assert!(ignored(&rules, "a/x", false));
        assert!(ignored(&rules, "a/x/y", false));
        // git: "a/**" matches everything *inside* a, not "a" itself.
        assert!(!ignored(&rules, "a", true));
    }

    #[test]
    fn trailing_slash_is_directory_only() {
        let rules = Rules::parse("build/");
        assert!(ignored(&rules, "build", true));
        assert!(!ignored(&rules, "build", false));
        // Unanchored (no interior slash): matches at any depth too.
        assert!(ignored(&rules, "sub/build", true));
    }

    #[test]
    fn anchored_leading_slash_matches_only_at_top() {
        let rules = Rules::parse("/build");
        assert!(ignored(&rules, "build", false));
        assert!(!ignored(&rules, "sub/build", false));
    }

    #[test]
    fn interior_slash_without_leading_slash_still_anchors() {
        let rules = Rules::parse("src/generated");
        assert!(ignored(&rules, "src/generated", false));
        assert!(!ignored(&rules, "generated", false));
        assert!(!ignored(&rules, "a/src/generated", false));
    }

    #[test]
    fn unanchored_pattern_matches_multiple_depths() {
        let rules = Rules::parse("target");
        assert!(ignored(&rules, "target", true));
        assert!(ignored(&rules, "a/target", true));
        assert!(ignored(&rules, "a/b/target", true));
    }

    #[test]
    fn negation_simple_reincludes() {
        let rules = Rules::parse("*.log\n!keep.log");
        assert!(ignored(&rules, "debug.log", false));
        assert!(!ignored(&rules, "keep.log", false));
    }

    #[test]
    fn negation_order_matters_last_match_wins() {
        // Negation BEFORE the ignoring pattern: the later ignore pattern is
        // the last match, so it wins and the path stays ignored.
        let rules = Rules::parse("!important.log\n*.log");
        assert!(ignored(&rules, "important.log", false));

        // Negation AFTER: it's the last match, so it wins and un-ignores.
        let rules = Rules::parse("*.log\n!important.log");
        assert!(!ignored(&rules, "important.log", false));
    }

    #[test]
    fn negated_file_inside_ignored_directory_is_correct_in_isolation() {
        // The matcher only ever sees the one path it's asked about, so
        // asked directly about "dir/file.txt" it correctly finds the later,
        // more specific negation and reports "not ignored".
        let rules = Rules::parse("dir/\n!dir/file.txt");
        assert!(ignored(&rules, "dir", true));
        assert!(!ignored(&rules, "dir/file.txt", false));
        // In a real walk this file is never resurrected, because the
        // walker prunes "dir" and never descends into it to find it — that
        // pruning behavior lives in walk.rs (Track D), not this matcher.
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let rules = Rules::parse("# a comment\n\nfoo\n   \n!bar\n");
        assert!(ignored(&rules, "foo", false));
        assert!(!ignored(&rules, "bar", false));
        assert!(!ignored(&rules, "# a comment", false));
    }

    #[test]
    fn escaped_leading_hash_and_bang_are_literal() {
        let rules = Rules::parse("\\#hash\n\\!bang");
        assert!(ignored(&rules, "#hash", false));
        assert!(ignored(&rules, "!bang", false));
    }

    #[test]
    fn matching_is_case_sensitive() {
        let rules = Rules::parse("Foo");
        assert!(ignored(&rules, "Foo", false));
        assert!(!ignored(&rules, "foo", false));
        assert!(!ignored(&rules, "FOO", false));
    }

    #[test]
    fn malformed_lines_are_skipped_without_panicking() {
        // "/", "//", a bare "!", "foo//bar" (a doubled interior slash), and
        // a lone trailing "\\" (nothing left to escape) all reduce to
        // nothing matchable and contribute no pattern at all.
        let rules = Rules::parse("/\n//\n!\nfoo//bar\n\\\n");
        assert!(!ignored(&rules, "anything", false));
        assert!(!ignored(&rules, "", false));
        assert!(!ignored(&rules, "a/b/c", true));
    }

    #[test]
    fn repeated_wildcards_collapse_but_still_match() {
        // Not malformed — "***" is a well-defined (if unusual) glob segment
        // that behaves the same as a single "*". Documented mainly to prove
        // it doesn't panic and matches consistently, not because it's a
        // pattern anyone should write.
        let rules = Rules::parse("***");
        assert!(ignored(&rules, "whatever", false));
        assert!(ignored(&rules, "a/b/whatever", false));
    }

    #[test]
    fn empty_file_ignores_nothing() {
        let rules = Rules::parse("");
        assert!(!ignored(&rules, "anything", false));
        assert!(!ignored(&rules, "a/b/c", true));
    }

    #[test]
    fn verdict_distinguishes_no_opinion_from_an_explicit_one() {
        let rules = Rules::parse("*.log\n!keep.log");
        assert_eq!(
            rules.verdict(Path::new("unrelated.txt"), false),
            None,
            "nothing here says anything about this path"
        );
        assert_eq!(
            rules.verdict(Path::new("debug.log"), false),
            Some(true),
            "explicitly ignored"
        );
        assert_eq!(
            rules.verdict(Path::new("keep.log"), false),
            Some(false),
            "explicitly re-included, not merely \"not ignored\""
        );
        // is_ignored collapses the last two the same way a single bool
        // always has; the middle case is the one it can't be built from.
        assert!(!rules.is_ignored(Path::new("unrelated.txt"), false));
        assert!(rules.is_ignored(Path::new("debug.log"), false));
        assert!(!rules.is_ignored(Path::new("keep.log"), false));
    }

    #[test]
    fn from_lines_matches_parse() {
        let lines = vec!["foo".to_string(), "!foo/bar".to_string()];
        let rules = Rules::from_lines(&lines);
        assert!(ignored(&rules, "foo", true));
        assert!(!ignored(&rules, "foo/bar", false));
    }
}
