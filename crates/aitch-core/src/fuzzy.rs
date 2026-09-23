//! Hand-written fzf-style fuzzy subsequence matcher.
//!
//! Replaces `nucleo`/`nucleo-matcher` behind `PathIndex::search` (see
//! `project.rs`), per PLAN-ZERO-DEP.md §4 Phase 4 Track C. Pure logic, no
//! filesystem access, and no dependency on `gitignore.rs`/`watcher.rs`/
//! `walk.rs` (Tracks A/B/D) or on `project.rs` itself — wiring this in to
//! replace `project.rs`'s current `nucleo` call is a later integration step,
//! once all four Phase 4 tracks are confirmed working together.
//!
//! ## Matching
//!
//! A candidate matches a query if every character of the query appears in
//! the candidate *in order* — a subsequence match, not necessarily
//! contiguous. Matching is case-insensitive by default, "smart case" like
//! `grep -i` and most fuzzy finders: the moment the query itself contains an
//! uppercase letter, matching becomes case-sensitive for the whole query.
//! `"mainrs"` matches `"src/main.rs"` (m-a-i-n-r-s in order, skipping `/`
//! and `.`); `"Main"` does not match `"src/main.rs"` (a literal capital `M`
//! is required once the query has one, and there isn't one there).
//!
//! ## Scoring
//!
//! Ranking is more than "did it match": [`score`] rewards the qualities a
//! person reading the result list actually wants to see land near the top —
//!
//! - **Consecutive runs beat scattered hits.** Each matched character earns
//!   a flat base score; a character matched immediately after the previous
//!   one (nothing skipped in between) earns an extra bonus, and a character
//!   matched after a gap pays a penalty proportional to how much was
//!   skipped to reach it. A query that lands as one unbroken run outscores
//!   the same letters spread thinly across a long string, and a
//!   one-character gap costs less than a five-character one.
//! - **Word/segment starts beat the middle of a word.** A character matched
//!   at the very start of the candidate, right after a `/`, `\`, `_`, `-`,
//!   `.` or space, or at a lowercase-to-uppercase transition (`camelCase`)
//!   earns a boundary bonus — the position a person would naturally aim a
//!   query at.
//! - **Shorter, tighter candidates beat longer ones with the same match.**
//!   After the character-by-character score above, a small penalty per
//!   character of the *whole* candidate is subtracted — a tie-breaker, not
//!   a dominant term, so `"src/lib.rs"` outranks a longer path that also
//!   happens to contain l-i-b in order somewhere within it.
//!
//! This is not an attempt at exact fzf-parity scoring — the constants and
//! the exact shape of the formula are original — but it aims for the same
//! qualitative behavior fzf, Sublime's "Goto Anything," and VS Code's quick
//! open all share, because that behavior is what makes fuzzy-match ranking
//! feel right rather than merely technically correct.
//!
//! The scoring search is a dynamic program over (query character, candidate
//! character) pairs: `O(query_len * candidate_len)` time and space per
//! candidate, the same complexity class as any scorer that has to consider
//! more than one possible alignment (the best alignment can depend on a
//! choice made several characters earlier). A cheap `O(candidate_len)`
//! subsequence pre-check — no scoring, no table — rejects the common case
//! first: most candidates in a large path index do not contain a given
//! query's letters in order at all, and [`score`] never builds the table for
//! those.

use std::cmp::Reverse;

/// Base score for each query character successfully matched.
const SCORE_MATCH: i64 = 16;

/// Extra reward for a matched character sitting immediately after the
/// previous matched character, with nothing skipped in between.
const BONUS_CONSECUTIVE: i64 = 16;

/// Extra reward for a matched character starting a new "word": the very
/// start of the candidate, right after a path/word separator, or a
/// lowercase-to-uppercase transition.
const BONUS_BOUNDARY: i64 = 12;

/// Cost per candidate character skipped between two matched characters,
/// charged once per skipped character — a two-character gap costs twice
/// what a one-character gap does.
const GAP_PENALTY: i64 = 2;

/// Cost per character of the candidate's own total length, applied once to
/// the whole match. Deliberately small next to `SCORE_MATCH` — a
/// tie-breaker between otherwise-similar matches, not something that should
/// let a long candidate's mere length swamp a genuinely better match
/// elsewhere.
const LENGTH_PENALTY: i64 = 1;

/// Path/word separators that mark the start of a new segment.
fn is_separator(c: char) -> bool {
    matches!(c, '/' | '\\' | '_' | '-' | '.' | ' ')
}

/// Whether position `i` in `chars` (original case, never case-folded —
/// folding would destroy the camelCase signal) starts a new word: the very
/// start of the string, right after a separator, or a lowercase-to-uppercase
/// transition.
fn is_boundary(chars: &[char], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let prev = chars[i - 1];
    is_separator(prev) || (prev.is_lowercase() && chars[i].is_uppercase())
}

fn boundary_bonus(candidate: &[char], pos: usize) -> i64 {
    if is_boundary(candidate, pos) {
        BONUS_BOUNDARY
    } else {
        0
    }
}

/// "Smart case," the same convention `grep -i` and most fuzzy finders use:
/// case-insensitive unless the query itself contains an uppercase letter, in
/// which case matching becomes case-sensitive for the whole query.
fn is_case_sensitive(query: &str) -> bool {
    query.chars().any(char::is_uppercase)
}

/// Case-fold one character, or leave it alone under case-sensitive matching.
/// `to_lowercase` returns an iterator because a handful of Unicode
/// characters lower-case to more than one `char`; taking just the first is
/// the same simplification most editors make; it doesn't turn any real
/// ASCII path character (the overwhelming case) into anything but itself.
fn fold(c: char, case_sensitive: bool) -> char {
    if case_sensitive {
        c
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// The larger of two optional scores, treating `None` as "not a candidate"
/// rather than as negative infinity — so `opt_max(None, None)` is `None`,
/// not a crash or a made-up number.
fn opt_max(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

/// A cheap `O(candidate_len)` check for whether `query`'s characters appear
/// in `candidate` in order at all, with no scoring and no table. Most
/// candidates fail this for a typical query, so [`score`] runs it before
/// paying for the full dynamic program.
fn is_subsequence(query: &[char], candidate: &[char], case_sensitive: bool) -> bool {
    let mut wanted = query.iter().map(|c| fold(*c, case_sensitive));
    let Some(mut want) = wanted.next() else {
        return true; // an empty query is a subsequence of everything
    };
    for &c in candidate {
        if fold(c, case_sensitive) == want {
            match wanted.next() {
                Some(next) => want = next,
                None => return true,
            }
        }
    }
    false
}

/// Score how well `candidate` matches `query`, or `None` if `query`'s
/// characters are not a subsequence of `candidate` at all. Higher is
/// better; see the module docs for the scoring rationale.
///
/// An empty query matches every candidate — vacuously, there is nothing for
/// it to fail to find — scored on length alone so a shorter candidate still
/// edges out a longer one. [`crate::project::PathIndex::search`] does not
/// actually call this for an empty query today (an empty query is its own
/// special case: show the first `limit` paths, unscored, so quick open opens
/// with something on screen rather than a blank list) — but nothing here
/// panics or does anything surprising if it ever is.
pub fn score(candidate: &str, query: &str) -> Option<i64> {
    let case_sensitive = is_case_sensitive(query);
    let query: Vec<char> = query.chars().collect();
    let candidate: Vec<char> = candidate.chars().collect();

    if query.is_empty() {
        return Some(-LENGTH_PENALTY * candidate.len() as i64);
    }
    if query.len() > candidate.len() {
        return None;
    }
    if !is_subsequence(&query, &candidate, case_sensitive) {
        return None;
    }

    let n = query.len();
    let m = candidate.len();

    // `table[i][j]`: best score of matching `query[..i]` to `candidate[..j]`
    // with `query[i - 1]` matched exactly at `candidate[j - 1]`. `None`
    // means no valid alignment does that.
    let mut table: Vec<Vec<Option<i64>>> = vec![vec![None; m + 1]; n + 1];
    // `best[i][j]`: best score of matching `query[..i]` somewhere within
    // `candidate[..j]` (not necessarily using `candidate[j - 1]`) — the
    // running max of `table[i][1..=j]`.
    let mut best: Vec<Vec<Option<i64>>> = vec![vec![None; m + 1]; n + 1];

    for i in 1..=n {
        let want = fold(query[i - 1], case_sensitive);

        // Running max, as `j` sweeps left to right, of
        // `best[i - 1][p + 1] + GAP_PENALTY * p` over every gap start `p`
        // seen so far. This is the affine-gap trick that keeps the whole
        // table `O(n * m)` instead of `O(n * m^2)`: a distance-proportional
        // gap penalty naively means checking every possible previous match
        // position for every new one, but the penalty is linear in `p`, so
        // the best choice so far can be carried forward as one running
        // value instead of rescanned.
        let mut running_gap_max: Option<i64> = None;

        for j in 1..=m {
            // `p = j - 2` is the newest gap-start candidate this column
            // admits: a match at `candidate[p]` (i.e. `best[i - 1][p + 1]`)
            // followed by a gap ending just before `candidate[j - 1]`.
            if j >= 2 {
                let p = j - 2;
                if let Some(v) = best[i - 1][p + 1] {
                    running_gap_max = opt_max(running_gap_max, Some(v + GAP_PENALTY * p as i64));
                }
            }

            let matched = fold(candidate[j - 1], case_sensitive) == want;
            table[i][j] = if !matched {
                None
            } else if i == 1 {
                Some(SCORE_MATCH + boundary_bonus(&candidate, j - 1))
            } else {
                let gap_ext = running_gap_max.map(|v| v - GAP_PENALTY * (j as i64 - 2));
                let consecutive_ext = table[i - 1][j - 1].map(|v| v + BONUS_CONSECUTIVE);
                opt_max(gap_ext, consecutive_ext)
                    .map(|ext| SCORE_MATCH + boundary_bonus(&candidate, j - 1) + ext)
            };

            best[i][j] = opt_max(best[i][j - 1], table[i][j]);
        }
    }

    // `is_subsequence` already confirmed a match exists, so `best[n][m]`
    // being empty here would mean the two disagree — treated as no match
    // rather than panicking or inventing a score, since being wrong quietly
    // is worse than being wrong visibly.
    let raw = best[n][m]?;
    Some(raw - LENGTH_PENALTY * m as i64)
}

/// Search `candidates` for the best matches to `query`, best first, at most
/// `limit` of them. A candidate that does not match at all (per [`score`])
/// is left out rather than padding the result with something irrelevant.
///
/// Ties are broken by leaving equally-scored candidates in the order they
/// arrived — [`slice::sort_by_key`] is a stable sort — which is what makes
/// this testable with a fixed input order, and what keeps quick-open's list
/// from jittering between keystrokes when nothing about the ranking actually
/// changed.
pub fn best_matches<'a>(
    candidates: impl Iterator<Item = &'a str>,
    query: &str,
    limit: usize,
) -> Vec<&'a str> {
    let mut scored: Vec<(&'a str, i64)> = candidates
        .filter_map(|candidate| score(candidate, query).map(|s| (candidate, s)))
        .collect();
    scored.sort_by_key(|(_, s)| Reverse(*s));
    scored.truncate(limit);
    scored.into_iter().map(|(candidate, _)| candidate).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- matching -----------------------------------------------------------

    #[test]
    fn letters_in_order_match_even_when_not_contiguous() {
        assert!(score("src/main.rs", "mainrs").is_some());
        assert!(score("src/deep/nested.rs", "nested").is_some());
    }

    #[test]
    fn the_same_letters_out_of_order_do_not_match() {
        assert!(score("src/main.rs", "rsmain").is_none());
    }

    #[test]
    fn a_query_longer_than_the_candidate_cannot_match() {
        assert!(score("a.rs", "muchlongerquery").is_none());
    }

    #[test]
    fn a_query_matching_nothing_returns_none() {
        assert!(score("src/main.rs", "zzz").is_none());
    }

    // -- case sensitivity -----------------------------------------------------

    #[test]
    fn matching_is_case_insensitive_by_default() {
        // Neither query has an uppercase letter, so both match regardless of
        // the candidate's own case.
        assert!(score("src/Main.rs", "main").is_some());
        assert!(score("src/MAIN.rs", "main").is_some());
    }

    #[test]
    fn an_uppercase_letter_in_the_query_switches_to_case_sensitive_matching() {
        // The query has a capital, so it must be matched literally — and
        // there is no capital "M" in this candidate.
        assert!(score("src/main.rs", "Main").is_none());
        // Same query, a candidate that really does have the capital.
        assert!(score("src/Main.rs", "Main").is_some());
    }

    // -- scoring: boundaries, length, and the two behaviors project.rs's own
    //    tests already pin ------------------------------------------------

    #[test]
    fn a_short_tight_match_outranks_a_longer_scattered_one() {
        // Both contain l-i-b in order. "src/lib.rs" has it as one
        // consecutive, boundary-started run right after "/"; the other one
        // only has it as three separate word-starts, spread across a much
        // longer string.
        let tight = score("src/lib.rs", "lib").unwrap();
        let scattered = score("long/immutable/big.rs", "lib").unwrap();
        assert!(
            tight > scattered,
            "tight {tight} should outrank scattered {scattered}"
        );
    }

    #[test]
    fn quick_open_ranks_the_closer_match_first() {
        // The exact claim project.rs's own test makes, reproduced directly
        // against this module rather than through PathIndex.
        let candidates = ["src/lib.rs", "long/immutable/big.rs", "src/main.rs"];
        let results = best_matches(candidates.into_iter(), "lib", 10);
        assert_eq!(results.first(), Some(&"src/lib.rs"));
    }

    #[test]
    fn a_consecutive_run_outranks_the_same_letters_scattered() {
        let contiguous = score("abcxyz", "abc").unwrap();
        let scattered = score("axbxcx", "abc").unwrap();
        assert!(
            contiguous > scattered,
            "contiguous {contiguous} should outrank scattered {scattered}"
        );
    }

    #[test]
    fn a_boundary_start_outranks_a_mid_word_start() {
        // Both match "b" as the first letter of the query at the earliest
        // possible position; one sits right after a separator, the other
        // mid-word.
        let boundary = score("foo/bar", "bar").unwrap();
        let mid_word = score("foobar", "bar").unwrap();
        assert!(
            boundary > mid_word,
            "boundary {boundary} should outrank mid-word {mid_word}"
        );
    }

    #[test]
    fn a_shorter_candidate_wins_a_tie_on_everything_else() {
        let short = score("lib.rs", "lib").unwrap();
        let long = score("lib.rs.backup.old", "lib").unwrap();
        assert!(
            short > long,
            "short {short} should outrank long {long} for the same leading match"
        );
    }

    // -- best_matches ---------------------------------------------------------

    #[test]
    fn best_matches_leaves_out_non_matches_entirely() {
        let candidates = ["src/lib.rs", "README.md", "src/main.rs"];
        let results = best_matches(candidates.into_iter(), "lib", 10);
        assert_eq!(results, ["src/lib.rs"]);
    }

    #[test]
    fn best_matches_is_empty_when_nothing_matches() {
        let candidates = ["a.rs", "b.rs", "c.rs"];
        assert!(best_matches(candidates.into_iter(), "zzznotathing", 10).is_empty());
    }

    #[test]
    fn best_matches_respects_the_limit() {
        let candidates = ["lib1.rs", "lib2.rs", "lib3.rs", "lib4.rs"];
        let results = best_matches(candidates.into_iter(), "lib", 2);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn an_empty_query_does_not_panic_and_matches_everything() {
        let candidates = ["a.rs", "b.rs", "c.rs"];
        let results = best_matches(candidates.into_iter(), "", 2);
        assert_eq!(results.len(), 2, "an empty query still returns results");
        assert!(score("anything", "").is_some());
    }

    // -- scale ------------------------------------------------------------

    /// Not a hard performance benchmark — just a sanity check that scoring a
    /// few thousand candidates is not accidentally quadratic-in-the-wrong-
    /// thing slow. `PathIndex` is expected to hold up to ~80k paths.
    #[test]
    fn a_few_thousand_candidates_sort_correctly_and_quickly() {
        let paths: Vec<String> = (0..5_000)
            .map(|i| format!("src/module{i}/deeply/nested/file{i}.rs"))
            .collect();
        // One genuinely best match: short, tight, boundary-started.
        let mut with_target = paths.clone();
        with_target.push("lib.rs".to_string());

        let started = std::time::Instant::now();
        let results = best_matches(with_target.iter().map(String::as_str), "librs", 10);
        let elapsed = started.elapsed();

        assert_eq!(results.first(), Some(&"lib.rs"));
        assert!(results.len() <= 10);
        assert!(
            elapsed.as_secs() < 5,
            "scoring 5000 candidates took {elapsed:?}, which suggests something is quadratic"
        );

        // Sorted best first: every score is >= the one after it.
        let scores: Vec<i64> = results
            .iter()
            .map(|candidate| score(candidate, "librs").unwrap())
            .collect();
        assert!(scores.windows(2).all(|w| w[0] >= w[1]), "{scores:?}");
    }
}
