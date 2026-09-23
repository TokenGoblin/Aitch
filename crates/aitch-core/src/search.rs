//! Finding text, the way nano finds it.
//!
//! Two behaviours matter more than speed here. Search **wraps**: running off
//! the end continues from the beginning and says so, because a search that
//! silently stops at the end of the file makes you scroll to the top and try
//! again. And it is **case-insensitive by default**, which is what nano does
//! and what someone hunting through a config file wants.
//!
//! This is a plain scan over the buffer's characters, via
//! [`crate::Buffer::snapshot_chars`]. Interactive search re-runs on every
//! keystroke, so it has to be cheap per call rather than clever — and it is
//! linear in the file, which for one search over even a large file is fine.
//! Project-wide search is a different, byte-oriented job — see
//! `project_search.rs`'s [`crate::matcher::Matcher`]/[`crate::regex::Regex`]
//! instead, per `PLAN-ZERO-DEP.md` Phase 6.

/// Which way a search runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    #[default]
    Forward,
    Backward,
}

impl Direction {
    pub fn reversed(self) -> Direction {
        match self {
            Direction::Forward => Direction::Backward,
            Direction::Backward => Direction::Forward,
        }
    }
}

/// Where a match was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    /// Character index of the first character of the match.
    pub start: usize,
    /// Character index just past the match.
    pub end: usize,
    /// The search ran off one end and continued from the other.
    pub wrapped: bool,
}

/// What to look for, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub term: String,
    /// nano searches case-insensitively unless told otherwise.
    pub case_sensitive: bool,
    pub direction: Direction,
}

impl Query {
    pub fn new(term: impl Into<String>) -> Query {
        Query {
            term: term.into(),
            case_sensitive: false,
            direction: Direction::Forward,
        }
    }

    pub fn case_sensitive(mut self, yes: bool) -> Query {
        self.case_sensitive = yes;
        self
    }

    pub fn direction(mut self, direction: Direction) -> Query {
        self.direction = direction;
        self
    }

    pub fn is_empty(&self) -> bool {
        self.term.is_empty()
    }
}

/// Find the next match at or after `from`, wrapping around the end.
///
/// `from` is where to start looking, not where the cursor is: to find the
/// *next* match rather than the one under the cursor, pass `cursor + 1`.
pub fn find(text: &[char], query: &Query, from: usize) -> Option<Match> {
    let term: Vec<char> = query.term.chars().collect();
    if term.is_empty() {
        return None;
    }
    let length = text.len();
    if term.len() > length {
        return None;
    }

    // The last index at which a match could still fit.
    let last_start = length - term.len();

    match query.direction {
        Direction::Forward => {
            let from = from.min(length);
            scan_forward(text, &term, from, last_start, query.case_sensitive)
                .map(|start| found(start, term.len(), false))
                .or_else(|| {
                    // Off the end: start again from the top.
                    scan_forward(text, &term, 0, from.min(last_start), query.case_sensitive)
                        .map(|start| found(start, term.len(), true))
                })
        }
        Direction::Backward => {
            let from = from.min(last_start);
            scan_backward(text, &term, from, query.case_sensitive)
                .map(|start| found(start, term.len(), false))
                .or_else(|| {
                    scan_backward(text, &term, last_start, query.case_sensitive)
                        .filter(|start| *start > from)
                        .map(|start| found(start, term.len(), true))
                })
        }
    }
}

/// Every match in the whole text, in order. Used by replace-all.
pub fn find_all(text: &[char], query: &Query) -> Vec<Match> {
    let term: Vec<char> = query.term.chars().collect();
    let mut matches = Vec::new();
    if term.is_empty() || term.len() > text.len() {
        return matches;
    }

    let last_start = text.len() - term.len();
    let mut at = 0usize;
    while at <= last_start {
        match scan_forward(text, &term, at, last_start, query.case_sensitive) {
            Some(start) => {
                matches.push(found(start, term.len(), false));
                // Non-overlapping, so resume past this match.
                at = start + term.len();
            }
            None => break,
        }
    }
    matches
}

fn found(start: usize, length: usize, wrapped: bool) -> Match {
    Match {
        start,
        end: start + length,
        wrapped,
    }
}

fn scan_forward(
    text: &[char],
    term: &[char],
    from: usize,
    last_start: usize,
    case_sensitive: bool,
) -> Option<usize> {
    let mut at = from;
    while at <= last_start {
        if matches_at(text, term, at, case_sensitive) {
            return Some(at);
        }
        at += 1;
    }
    None
}

fn scan_backward(text: &[char], term: &[char], from: usize, case_sensitive: bool) -> Option<usize> {
    let mut at = from;
    loop {
        if matches_at(text, term, at, case_sensitive) {
            return Some(at);
        }
        if at == 0 {
            return None;
        }
        at -= 1;
    }
}

fn matches_at(text: &[char], term: &[char], at: usize, case_sensitive: bool) -> bool {
    text[at..at + term.len()]
        .iter()
        .zip(term)
        .all(|(c, wanted)| same(*c, *wanted, case_sensitive))
}

/// Compare two characters, folding case unless asked not to.
///
/// Folding is done on the first character of `to_lowercase`, which is exact
/// for every case pair a search term is likely to contain and avoids the
/// allocation that full Unicode folding would need on every comparison.
fn same(a: char, b: char, case_sensitive: bool) -> bool {
    if case_sensitive {
        return a == b;
    }
    if a == b {
        return true;
    }
    a.to_lowercase().next() == b.to_lowercase().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    #[test]
    fn a_match_is_found_where_it_is() {
        let text = chars("the quick brown fox");
        let found = find(&text, &Query::new("brown"), 0).unwrap();
        assert_eq!((found.start, found.end), (10, 15));
        assert!(!found.wrapped);
    }

    #[test]
    fn searching_is_case_insensitive_by_default() {
        let text = chars("The Quick Brown Fox");
        assert!(find(&text, &Query::new("quick"), 0).is_some());
        assert!(find(&text, &Query::new("QUICK"), 0).is_some());

        let exact = Query::new("quick").case_sensitive(true);
        assert!(find(&text, &exact, 0).is_none());
        assert!(find(&text, &Query::new("Quick").case_sensitive(true), 0).is_some());
    }

    #[test]
    fn a_forward_search_wraps_and_says_so() {
        let text = chars("needle in a haystack");
        // Start past the only match, so it can only be found by wrapping.
        let found = find(&text, &Query::new("needle"), 10).unwrap();
        assert_eq!(found.start, 0);
        assert!(found.wrapped, "the search went round the end");
    }

    #[test]
    fn a_backward_search_wraps_too() {
        let text = chars("alpha beta alpha gamma");
        let found = find(
            &text,
            &Query::new("gamma").direction(Direction::Backward),
            3,
        )
        .unwrap();
        assert_eq!(found.start, 17);
        assert!(found.wrapped);
    }

    #[test]
    fn searching_backward_finds_the_nearest_match_behind() {
        let text = chars("alpha beta alpha gamma");
        let backward = Query::new("alpha").direction(Direction::Backward);
        let found = find(&text, &backward, 16).unwrap();
        assert_eq!(found.start, 11, "the second alpha, not the first");
        assert!(!found.wrapped);
    }

    #[test]
    fn repeating_a_search_walks_through_every_match() {
        let text = chars("one two one two one");
        let query = Query::new("one");
        let mut at = 0;
        let mut starts = Vec::new();
        for _ in 0..3 {
            let found = find(&text, &query, at).unwrap();
            starts.push(found.start);
            at = found.start + 1;
        }
        assert_eq!(starts, [0, 8, 16]);
    }

    #[test]
    fn a_search_with_no_match_finds_nothing_rather_than_looping() {
        let text = chars("alpha beta gamma");
        assert!(find(&text, &Query::new("delta"), 0).is_none());
        assert!(find(&text, &Query::new("delta"), 10).is_none());
    }

    #[test]
    fn an_empty_term_matches_nothing() {
        let text = chars("anything at all");
        assert!(find(&text, &Query::new(""), 0).is_none());
        assert!(find_all(&text, &Query::new("")).is_empty());
    }

    #[test]
    fn a_term_longer_than_the_file_matches_nothing() {
        let text = chars("short");
        assert!(find(&text, &Query::new("much longer than that"), 0).is_none());
    }

    #[test]
    fn a_match_at_the_very_end_is_found() {
        let text = chars("first last");
        let found = find(&text, &Query::new("last"), 0).unwrap();
        assert_eq!((found.start, found.end), (6, 10));
    }

    #[test]
    fn searching_spans_line_breaks() {
        let text = chars("first line\nsecond line\n");
        let found = find(&text, &Query::new("line\nsecond"), 0).unwrap();
        assert_eq!(found.start, 6);
    }

    #[test]
    fn multibyte_text_is_matched_by_character_not_byte() {
        let text = chars("naïve café → 日本語");
        let found = find(&text, &Query::new("café"), 0).unwrap();
        assert_eq!((found.start, found.end), (6, 10));

        let japanese = find(&text, &Query::new("日本語"), 0).unwrap();
        assert_eq!(japanese.start, 13);
    }

    #[test]
    fn case_folding_reaches_past_ascii() {
        let text = chars("STRASSE und GRÜN");
        assert!(find(&text, &Query::new("grün"), 0).is_some());
    }

    #[test]
    fn find_all_returns_every_non_overlapping_match() {
        let text = chars("aaaa");
        let all = find_all(&text, &Query::new("aa"));
        assert_eq!(all.len(), 2, "non-overlapping, so two not three");
        assert_eq!(all[0].start, 0);
        assert_eq!(all[1].start, 2);
    }

    #[test]
    fn find_all_is_case_insensitive_to_match_find() {
        let text = chars("Cat cat CAT");
        assert_eq!(find_all(&text, &Query::new("cat")).len(), 3);
        assert_eq!(
            find_all(&text, &Query::new("cat").case_sensitive(true)).len(),
            1
        );
    }
}
