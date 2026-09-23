//! A byte-oriented match interface, shared by a literal string matcher and
//! (in `regex.rs`) a small regular-expression engine — replacing
//! `grep-matcher`/`grep-regex` per
//! [`PLAN-ZERO-DEP.md`](../../../../PLAN-ZERO-DEP.md) §2/§4 Phase 6.
//!
//! `project_search.rs` is the only caller today: it asks a [`Matcher`]
//! whether one line of a file matched, never caring which concrete kind it
//! is. That is the whole reason this trait exists rather than
//! `project_search.rs` branching on `Pattern::regex` itself — a query
//! resolves once to a `Box<dyn Matcher>`, and everything downstream of that
//! is the same code whether it is searching literally or by pattern.

use std::ops::Range;

/// A byte range within a haystack, e.g. the extent of a match.
pub type MatchRange = Range<usize>;

/// Something that can find matches in a byte slice.
pub trait Matcher: Send + Sync {
    /// The first match starting at or after byte offset `from`, if any.
    ///
    /// `from` is a byte offset, not a match count: callers that want every
    /// match in a haystack call this in a loop, each time passing the
    /// previous match's `end` (see [`Matcher::is_match`]'s note on why a
    /// single boolean check does not need to).
    fn find_at(&self, haystack: &[u8], from: usize) -> Option<MatchRange>;

    /// Whether there is a match anywhere in `haystack`. `project_search.rs`
    /// only ever needs this, not a match's exact position — a `Hit` records
    /// a whole matching line, not a column — but the trait exposes
    /// `find_at` directly too, since "is there a match" is just "is there a
    /// first match" with the position thrown away.
    fn is_match(&self, haystack: &[u8]) -> bool {
        self.find_at(haystack, 0).is_some()
    }
}

/// A plain substring, matched with Boyer-Moore-Horspool rather than a naive
/// scan: a project search runs over every file in a folder, where the
/// naive `O(haystack × needle)` worst case is a real cost on a large tree,
/// not an academic one.
pub struct LiteralMatcher {
    /// The needle, already lowercased when `case_sensitive` is false — see
    /// [`LiteralMatcher::matches_at`].
    needle: Vec<u8>,
    case_sensitive: bool,
    /// Boyer-Moore-Horspool's bad-character table: for each possible byte,
    /// how far the needle can safely slide when that byte is what the
    /// haystack had at the needle's last position and it is not the
    /// needle's own last byte. Built once, from `needle`, in
    /// [`LiteralMatcher::new`], not on every search.
    shift: [usize; 256],
}

impl LiteralMatcher {
    pub fn new(pattern: &str, case_sensitive: bool) -> LiteralMatcher {
        let needle: Vec<u8> = if case_sensitive {
            pattern.as_bytes().to_vec()
        } else {
            pattern.as_bytes().to_ascii_lowercase()
        };

        let mut shift = [needle.len().max(1); 256];
        // Every byte but the needle's own last one gets the distance from
        // its rightmost occurrence (excluding the last position) to the
        // needle's end; the last byte keeps the default (a full needle
        // length slide), which is Horspool's whole simplification over
        // full Boyer-Moore — one table, not two.
        if !needle.is_empty() {
            for (i, &byte) in needle[..needle.len() - 1].iter().enumerate() {
                shift[byte as usize] = needle.len() - 1 - i;
            }
        }

        LiteralMatcher {
            needle,
            case_sensitive,
            shift,
        }
    }

    /// Byte-for-byte comparison at `at`, folding case the same way the
    /// needle itself was folded when built.
    fn matches_at(&self, haystack: &[u8], at: usize) -> bool {
        if self.case_sensitive {
            haystack[at..at + self.needle.len()] == self.needle[..]
        } else {
            haystack[at..at + self.needle.len()]
                .iter()
                .zip(&self.needle)
                .all(|(h, n)| h.to_ascii_lowercase() == *n)
        }
    }
}

impl Matcher for LiteralMatcher {
    fn find_at(&self, haystack: &[u8], from: usize) -> Option<MatchRange> {
        if self.needle.is_empty() || from > haystack.len() {
            return None;
        }
        let needle_len = self.needle.len();
        if needle_len > haystack.len() {
            return None;
        }

        // The window's last byte is what the shift table is keyed on, so
        // this walks by the window's *end*, not its start.
        let mut end = from + needle_len - 1;
        while end < haystack.len() {
            let start = end + 1 - needle_len;
            if self.matches_at(haystack, start) {
                return Some(start..start + needle_len);
            }
            let last = if self.case_sensitive {
                haystack[end]
            } else {
                haystack[end].to_ascii_lowercase()
            };
            end += self.shift[last as usize];
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, case_sensitive: bool, haystack: &str) -> Vec<MatchRange> {
        let matcher = LiteralMatcher::new(pattern, case_sensitive);
        let bytes = haystack.as_bytes();
        let mut out = Vec::new();
        let mut at = 0;
        while let Some(range) = matcher.find_at(bytes, at) {
            at = range.end.max(range.start + 1);
            out.push(range);
        }
        out
    }

    #[test]
    fn a_plain_substring_is_found() {
        assert_eq!(matches("brown", true, "the quick brown fox"), vec![10..15]);
    }

    #[test]
    fn no_match_is_no_match() {
        assert!(matches("purple", true, "the quick brown fox").is_empty());
    }

    #[test]
    fn case_insensitive_by_request() {
        assert_eq!(matches("BROWN", false, "the quick brown fox"), vec![10..15]);
        assert!(matches("BROWN", true, "the quick brown fox").is_empty());
    }

    #[test]
    fn overlapping_occurrences_are_all_found_when_asked_one_at_a_time() {
        // find_at itself is happy to return an overlapping match; it is the
        // caller's choice (as the `matches` test helper makes here) whether
        // to skip past `end` or just past `start`.
        let matcher = LiteralMatcher::new("aa", true);
        let hay = b"aaaa";
        assert_eq!(matcher.find_at(hay, 0), Some(0..2));
        assert_eq!(matcher.find_at(hay, 1), Some(1..3));
        assert_eq!(matcher.find_at(hay, 2), Some(2..4));
        assert_eq!(matcher.find_at(hay, 3), None);
    }

    #[test]
    fn an_empty_pattern_matches_nothing() {
        assert!(matches("", true, "anything").is_empty());
    }

    #[test]
    fn a_pattern_longer_than_the_haystack_matches_nothing() {
        assert!(matches("much longer than this", true, "short").is_empty());
    }

    #[test]
    fn a_match_at_the_very_start_and_end_are_both_found() {
        assert_eq!(matches("first", true, "first last"), vec![0..5]);
        assert_eq!(matches("last", true, "first last"), vec![6..10]);
    }

    #[test]
    fn the_whole_haystack_matching_the_pattern_is_found() {
        assert_eq!(matches("exact", true, "exact"), vec![0..5]);
    }

    #[test]
    fn a_repeated_pattern_finds_every_non_overlapping_occurrence() {
        assert_eq!(matches("ab", true, "ababab"), vec![0..2, 2..4, 4..6]);
    }

    #[test]
    fn bytes_that_never_appear_in_the_needle_still_work_as_a_haystack() {
        // Exercises the bad-character table's default shift (needle.len())
        // for a haystack byte that never occurs in the needle at all.
        assert_eq!(matches("xyz", true, "aaaaaaaaaaxyz"), vec![10..13]);
    }

    #[test]
    fn a_needle_of_one_repeated_byte_does_not_infinite_loop() {
        // The needle's own last byte always keeps the table's default
        // shift; a needle where every byte is that same byte is the
        // sharpest test that this does not regress to a zero-length slide.
        assert_eq!(matches("aaa", true, "aaaaaaa"), vec![0..3, 3..6]);
    }

    #[test]
    fn case_insensitive_matching_is_ascii_only() {
        // Documented limitation vs. grep-regex, which folds full Unicode:
        // a Turkish dotless/dotted I or a German ß/SS pair does not match
        // across case here. Folding is byte-wise ASCII, same simplification
        // `rust.rs`'s number scanner and other Phase 5 lexers already made
        // for similar reasons.
        assert!(matches("STRASSE", false, "straße").is_empty());
    }
}
