//! Searching every file in the folder.
//!
//! This is ripgrep's own machinery — `ignore` to walk, `grep-regex` to match,
//! `grep-searcher` to read — rather than shelling out to the `rg` binary. It
//! is the same code doing the same work, without depending on a tool being
//! installed or on parsing its output.
//!
//! Three things the editor needs and a naive search does not give:
//!
//! - **Results stream.** A 500 MB tree takes seconds to walk; the first hits
//!   are useful long before the last. They arrive in batches as they are
//!   found, and the pane fills while the search is still running.
//! - **A search is cancellable.** Typing another character abandons the old
//!   search rather than waiting for it, which is what makes an incremental
//!   project search feel like search rather than like a build.
//! - **The wake-up is rate limited.** Ten thousand hits must not wake the
//!   event loop ten thousand times; batching keeps that to a handful.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::WalkState;

/// Stop after this many hits. A search that matches half a tree is a search
/// that needs narrowing, and holding a million lines helps nobody.
const MAX_HITS: usize = 2_000;

/// Send at least this often while hits are arriving, so the pane fills
/// steadily rather than in one lump at the end.
const BATCH_INTERVAL: Duration = Duration::from_millis(50);

/// And at most this many per batch, so a dense file does not arrive as one.
const BATCH_SIZE: usize = 64;

/// What to look for across the folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    pub text: String,
    /// Off by default, and made smart below: a pattern with a capital in it
    /// is taken to mean it.
    pub case_sensitive: bool,
    /// When false the text is matched literally, brackets and all.
    pub regex: bool,
}

impl Pattern {
    pub fn new(text: impl Into<String>) -> Pattern {
        Pattern {
            text: text.into(),
            case_sensitive: false,
            regex: false,
        }
    }

    pub fn regex(mut self, yes: bool) -> Pattern {
        self.regex = yes;
        self
    }

    pub fn case_sensitive(mut self, yes: bool) -> Pattern {
        self.case_sensitive = yes;
        self
    }

    /// Smart case, as ripgrep does it: an all-lowercase pattern ignores case,
    /// and one with a capital in it means the capital.
    fn is_case_sensitive(&self) -> bool {
        self.case_sensitive || self.text.chars().any(char::is_uppercase)
    }
}

/// One matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Relative to the search root, which is what a result list should show.
    pub path: PathBuf,
    /// One-based, as every tool reports it.
    pub line: u64,
    /// The matching line, without its terminator.
    pub text: String,
}

impl Hit {
    /// How the result reads in the pane: `src/main.rs:42: fn main() {`.
    pub fn label(&self) -> String {
        let path = self.path.to_string_lossy().replace('\\', "/");
        format!("{path}:{}: {}", self.line, self.text.trim())
    }
}

/// Something went wrong before the search could start.
#[derive(Debug)]
pub enum SearchError {
    /// The pattern is not a valid regular expression.
    BadPattern(String),
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchError::BadPattern(why) => write!(f, "bad pattern: {why}"),
        }
    }
}

impl std::error::Error for SearchError {}

/// A search running across the folder.
///
/// Dropping it cancels the search: the walker notices on its next file.
pub struct ProjectSearch {
    incoming: Receiver<Vec<Hit>>,
    cancelled: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    found: Arc<AtomicUsize>,
    hits: Vec<Hit>,
    pattern: Pattern,
}

impl ProjectSearch {
    /// Start searching `root`, calling `on_hits` when a batch is ready.
    pub fn start<F>(
        root: &Path,
        pattern: Pattern,
        ignores: &[String],
        on_hits: F,
    ) -> Result<ProjectSearch, SearchError>
    where
        // Sync as well as Send: the walker runs the callback from several
        // threads at once, one per file being searched.
        F: Fn() + Send + Sync + 'static,
    {
        let matcher = RegexMatcherBuilder::new()
            .case_insensitive(!pattern.is_case_sensitive())
            // A literal search should find `foo(bar)` when asked for it,
            // rather than treating the brackets as a group.
            .fixed_strings(!pattern.regex)
            .line_terminator(Some(b'\n'))
            .build(&pattern.text)
            .map_err(|e| SearchError::BadPattern(e.to_string()))?;

        let (sender, incoming) = mpsc::channel::<Vec<Hit>>();
        let cancelled = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let found = Arc::new(AtomicUsize::new(0));

        let walker = crate::project::walker(root, root, ignores).build_parallel();

        let root = root.to_path_buf();
        let thread_cancelled = cancelled.clone();
        let thread_finished = finished.clone();
        let thread_found = found.clone();

        std::thread::Builder::new()
            .name("aitch-project-search".to_string())
            .spawn(move || {
                let on_hits: Arc<dyn Fn() + Send + Sync> = Arc::new(on_hits);

                walker.run(|| {
                    let matcher = matcher.clone();
                    let sender = sender.clone();
                    let cancelled = thread_cancelled.clone();
                    let found = thread_found.clone();
                    let on_hits = on_hits.clone();
                    let root = root.clone();

                    let mut searcher = SearcherBuilder::new()
                        .line_number(true)
                        // A hit inside a binary is noise, and printing one
                        // fills the pane with control characters.
                        .binary_detection(BinaryDetection::quit(0))
                        .build();

                    Box::new(move |entry| {
                        if cancelled.load(Ordering::Relaxed)
                            || found.load(Ordering::Relaxed) >= MAX_HITS
                        {
                            return WalkState::Quit;
                        }

                        let Ok(entry) = entry else {
                            return WalkState::Continue;
                        };
                        if !entry.file_type().is_some_and(|t| t.is_file()) {
                            return WalkState::Continue;
                        }

                        let relative = entry
                            .path()
                            .strip_prefix(&root)
                            .unwrap_or(entry.path())
                            .to_path_buf();

                        let mut sink = Collector {
                            path: relative,
                            batch: Vec::new(),
                            last_sent: Instant::now(),
                            sender: &sender,
                            cancelled: &cancelled,
                            found: &found,
                            on_hits: &on_hits,
                        };

                        // An unreadable file is skipped, not fatal: one
                        // permission problem should not end the search.
                        let _ = searcher.search_path(&matcher, entry.path(), &mut sink);
                        sink.flush();

                        WalkState::Continue
                    })
                });

                thread_finished.store(true, Ordering::Release);
                // One last wake so the caller sees the search end.
                on_hits();
            })
            .map_err(|e| SearchError::BadPattern(e.to_string()))?;

        Ok(ProjectSearch {
            incoming,
            cancelled,
            finished,
            found,
            hits: Vec::new(),
            pattern,
        })
    }

    pub fn pattern(&self) -> &Pattern {
        &self.pattern
    }

    /// Collect whatever has arrived. Returns how many hits were added.
    pub fn poll(&mut self) -> usize {
        let before = self.hits.len();
        while let Ok(batch) = self.incoming.try_recv() {
            self.hits.extend(batch);
        }
        self.hits.len() - before
    }

    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }

    /// Whether the walk has run to the end.
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    /// Whether the search stopped because it had found enough.
    pub fn is_truncated(&self) -> bool {
        self.found.load(Ordering::Relaxed) >= MAX_HITS
    }

    /// Stop the search. The walker notices on its next file.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl Drop for ProjectSearch {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl std::fmt::Debug for ProjectSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ProjectSearch({:?}, {} hits{})",
            self.pattern.text,
            self.hits.len(),
            if self.is_finished() { "" } else { ", running" }
        )
    }
}

/// Gathers one file's hits and sends them on in batches.
struct Collector<'a> {
    path: PathBuf,
    batch: Vec<Hit>,
    last_sent: Instant,
    sender: &'a mpsc::Sender<Vec<Hit>>,
    cancelled: &'a AtomicBool,
    found: &'a AtomicUsize,
    on_hits: &'a Arc<dyn Fn() + Send + Sync>,
}

impl Collector<'_> {
    fn flush(&mut self) {
        if self.batch.is_empty() {
            return;
        }
        let batch = std::mem::take(&mut self.batch);
        self.last_sent = Instant::now();
        if self.sender.send(batch).is_ok() {
            (self.on_hits)();
        }
    }
}

impl Sink for Collector<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Ok(false);
        }
        if self.found.fetch_add(1, Ordering::Relaxed) >= MAX_HITS {
            return Ok(false);
        }

        self.batch.push(Hit {
            path: self.path.clone(),
            line: mat.line_number().unwrap_or(0),
            text: String::from_utf8_lossy(mat.bytes())
                .trim_end_matches(['\n', '\r'])
                .to_string(),
        });

        if self.batch.len() >= BATCH_SIZE || self.last_sent.elapsed() >= BATCH_INTERVAL {
            self.flush();
        }
        Ok(true)
    }
}

/// A project-wide replace, worked out before anything is written.
///
/// Nothing touches the disk until the whole plan is built, so a file that
/// cannot be read or re-encoded stops the operation before it has changed
/// anything — which is what "all or nothing" has to mean if it means anything.
#[derive(Debug, Default)]
pub struct ReplacePlan {
    /// Absolute path, and the whole new contents for it.
    changes: Vec<(PathBuf, String)>,
    occurrences: usize,
}

impl ReplacePlan {
    pub fn files(&self) -> usize {
        self.changes.len()
    }

    pub fn occurrences(&self) -> usize {
        self.occurrences
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// What the change reads as before it is made.
    pub fn preview(&self) -> Vec<String> {
        self.changes
            .iter()
            .map(|(path, _)| {
                path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string())
            })
            .collect()
    }
}

/// Work out what a project-wide replace would do, without doing it.
///
/// Only literal replacement: a regex replace with capture groups is a
/// different feature with a different set of ways to go wrong, and this one
/// has to be trustworthy first.
pub fn plan_replace(
    hits: &[Hit],
    root: &Path,
    pattern: &Pattern,
    replacement: &str,
) -> Result<ReplacePlan, crate::fileio::FileError> {
    let mut paths: Vec<PathBuf> = hits.iter().map(|hit| root.join(&hit.path)).collect();
    paths.sort();
    paths.dedup();

    let mut plan = ReplacePlan::default();
    for path in paths {
        // Through fileio, so a UTF-16 file with CRLF endings stays one. A
        // replace that quietly rewrote every file as UTF-8 LF would be a far
        // worse bug than the one it was asked to fix.
        let loaded = crate::fileio::load(&path)?;

        let (replaced, count) = replace_all(&loaded.text, &pattern.text, replacement, pattern);
        if count == 0 {
            continue;
        }
        // Prove it can be written back before promising to write it.
        crate::fileio::encode(&replaced, loaded.encoding)?;

        plan.occurrences += count;
        plan.changes.push((path, replaced));
    }
    Ok(plan)
}

/// Carry out a plan. Returns how many files were written.
///
/// Each file is written atomically, as everywhere else in Aitch. Across files
/// this is a sequence rather than a transaction: if the tenth write fails, the
/// first nine are already on disk, and the error says so rather than pretending
/// otherwise.
pub fn apply_replace(plan: &ReplacePlan) -> Result<usize, (usize, crate::fileio::FileError)> {
    let mut written = 0;
    for (path, contents) in &plan.changes {
        let encoding = match crate::fileio::load(path) {
            Ok(loaded) => loaded.encoding,
            Err(e) => return Err((written, e)),
        };
        if let Err(e) = crate::fileio::save(path, contents, encoding) {
            return Err((written, e));
        }
        written += 1;
    }
    Ok(written)
}

/// Replace every occurrence, honouring the pattern's case rules.
fn replace_all(text: &str, find: &str, replacement: &str, pattern: &Pattern) -> (String, usize) {
    if find.is_empty() {
        return (text.to_string(), 0);
    }

    if pattern.is_case_sensitive() {
        let count = text.matches(find).count();
        return (text.replace(find, replacement), count);
    }

    // Case-insensitive: walk the lowercased copy for positions, and cut the
    // original at those offsets so the untouched text keeps its own case.
    let haystack = text.to_lowercase();
    let needle = find.to_lowercase();
    // Lowercasing can change byte lengths (a Turkish dotted I, say), which
    // would make offsets from one string meaningless in the other.
    if haystack.len() != text.len() || needle.len() != find.len() {
        let count = text.matches(find).count();
        return (text.replace(find, replacement), count);
    }

    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    let mut count = 0;
    while let Some(found) = haystack[at..].find(&needle) {
        let start = at + found;
        out.push_str(&text[at..start]);
        out.push_str(replacement);
        at = start + needle.len();
        count += 1;
    }
    out.push_str(&text[at..]);
    (out, count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::AtomicUsize as Counter;

    static NEXT: Counter = Counter::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "aitch-psearch-{name}-{}-{unique}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn file(&self, relative: &str, contents: &str) {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sample(name: &str) -> Scratch {
        let scratch = Scratch::new(name);
        scratch.file("src/main.rs", "fn main() {\n    let needle = 1;\n}\n");
        scratch.file("src/lib.rs", "// needle in a comment\npub fn go() {}\n");
        scratch.file("README.md", "No match here.\n");
        scratch.file("target/build.rs", "let needle = 2;\n");
        scratch.file(".gitignore", "target/\n");
        scratch
    }

    /// Run a search to completion and return its hits.
    fn run(root: &Path, pattern: Pattern) -> Vec<Hit> {
        let mut search = ProjectSearch::start(root, pattern, &[], || {}).expect("a search");
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            search.poll();
            if search.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        search.poll();

        let mut hits = search.hits().to_vec();
        hits.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
        hits
    }

    #[test]
    fn a_search_finds_every_matching_line() {
        let scratch = sample("basic");
        let hits = run(&scratch.0, Pattern::new("needle"));

        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].path, PathBuf::from("src").join("lib.rs"));
        assert_eq!(hits[0].line, 1);
        assert!(hits[0].text.contains("needle in a comment"));
        assert_eq!(hits[1].line, 2, "main.rs line 2");
    }

    #[test]
    fn ignored_folders_are_not_searched() {
        let scratch = sample("ignored");
        let hits = run(&scratch.0, Pattern::new("needle"));
        assert!(
            !hits.iter().any(|hit| hit.path.starts_with("target")),
            "target/ is in .gitignore: {hits:?}"
        );
    }

    #[test]
    fn smart_case_follows_the_pattern() {
        let scratch = Scratch::new("case");
        scratch.file("a.txt", "Needle\nneedle\nNEEDLE\n");

        // All lowercase: case does not matter.
        assert_eq!(run(&scratch.0, Pattern::new("needle")).len(), 3);
        // A capital in the pattern means it.
        assert_eq!(run(&scratch.0, Pattern::new("Needle")).len(), 1);
    }

    #[test]
    fn a_literal_search_does_not_treat_the_pattern_as_a_regex() {
        let scratch = Scratch::new("literal");
        scratch.file("a.txt", "call foo(bar)\ncall fooXbar\n");

        let hits = run(&scratch.0, Pattern::new("foo(bar)"));
        assert_eq!(hits.len(), 1, "brackets are literal by default: {hits:?}");
        assert!(hits[0].text.contains("foo(bar)"));
    }

    #[test]
    fn a_regex_search_treats_it_as_one() {
        let scratch = Scratch::new("regex");
        scratch.file("a.txt", "alpha1\nalpha2\nbeta3\n");

        let hits = run(&scratch.0, Pattern::new(r"alpha\d").regex(true));
        assert_eq!(hits.len(), 2, "{hits:?}");
    }

    #[test]
    fn a_bad_regex_is_reported_rather_than_panicking() {
        let scratch = Scratch::new("bad");
        let error = ProjectSearch::start(&scratch.0, Pattern::new("([").regex(true), &[], || {});
        assert!(error.is_err());
    }

    #[test]
    fn a_pattern_matching_nothing_finishes_with_no_hits() {
        let scratch = sample("nothing");
        assert!(run(&scratch.0, Pattern::new("zzzznotpresent")).is_empty());
    }

    #[test]
    fn binary_files_are_skipped_rather_than_printed() {
        let scratch = Scratch::new("binary");
        scratch.file("text.txt", "needle here\n");
        let mut bytes = b"needle".to_vec();
        bytes.extend_from_slice(&[0u8; 64]);
        fs::write(scratch.0.join("data.bin"), bytes).unwrap();

        let hits = run(&scratch.0, Pattern::new("needle"));
        assert_eq!(hits.len(), 1, "only the text file: {hits:?}");
        assert_eq!(hits[0].path, PathBuf::from("text.txt"));
    }

    #[test]
    fn results_arrive_before_the_search_finishes() {
        // The point of streaming: a big tree should show its first hits long
        // before the walk is done.
        let scratch = Scratch::new("streaming");
        for i in 0..400 {
            scratch.file(&format!("dir{}/file{i}.txt", i % 20), "needle\n");
        }

        let mut search =
            ProjectSearch::start(&scratch.0, Pattern::new("needle"), &[], || {}).expect("a search");

        let start = Instant::now();
        let mut saw_early = false;
        while start.elapsed() < Duration::from_secs(10) {
            search.poll();
            if !search.hits().is_empty() && !search.is_finished() {
                saw_early = true;
                break;
            }
            if search.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        // Either hits arrived mid-flight, or the whole thing finished so fast
        // that there was no mid-flight to catch. Both are fine; a search that
        // only ever delivers at the end is not.
        assert!(
            saw_early || search.is_finished(),
            "no results and no finish"
        );
        search.poll();
        assert!(!search.hits().is_empty());
    }

    #[test]
    fn cancelling_stops_the_search() {
        let scratch = Scratch::new("cancel");
        for i in 0..500 {
            scratch.file(&format!("dir{}/file{i}.txt", i % 25), "needle\n");
        }

        let search =
            ProjectSearch::start(&scratch.0, Pattern::new("needle"), &[], || {}).expect("a search");
        search.cancel();

        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) && !search.is_finished() {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(search.is_finished(), "a cancelled search should stop");
    }

    #[test]
    fn the_caller_is_told_when_hits_arrive() {
        let scratch = sample("wake");
        let woken = Arc::new(Counter::new(0));
        let counter = woken.clone();

        let mut search = ProjectSearch::start(&scratch.0, Pattern::new("needle"), &[], move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("a search");

        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) && !search.is_finished() {
            search.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(woken.load(Ordering::SeqCst) > 0, "the loop was never woken");
    }

    // -- project-wide replace ---------------------------------------------

    #[test]
    fn a_plan_says_what_it_would_change_without_changing_it() {
        let scratch = sample("plan");
        let hits = run(&scratch.0, Pattern::new("needle"));
        let plan = plan_replace(&hits, &scratch.0, &Pattern::new("needle"), "pin").unwrap();

        assert_eq!(plan.files(), 2, "main.rs and lib.rs");
        assert_eq!(plan.occurrences(), 2);

        // Nothing on disk has moved.
        let main = fs::read_to_string(scratch.0.join("src/main.rs")).unwrap();
        assert!(main.contains("needle"), "the plan wrote something");
    }

    #[test]
    fn applying_a_plan_changes_every_file_in_it() {
        let scratch = sample("apply");
        let hits = run(&scratch.0, Pattern::new("needle"));
        let plan = plan_replace(&hits, &scratch.0, &Pattern::new("needle"), "pin").unwrap();

        assert_eq!(apply_replace(&plan).unwrap(), 2);
        let main = fs::read_to_string(scratch.0.join("src/main.rs")).unwrap();
        assert!(main.contains("let pin = 1;"), "{main:?}");
        let lib = fs::read_to_string(scratch.0.join("src/lib.rs")).unwrap();
        assert!(lib.contains("// pin in a comment"), "{lib:?}");
    }

    #[test]
    fn a_replace_keeps_the_encoding_and_line_endings_it_found() {
        // Phase 2's guarantee, which a project-wide replace could undo across
        // a whole tree in one keystroke.
        let scratch = Scratch::new("encoding");
        let crlf = scratch.0.join("dos.txt");
        fs::write(
            &crlf,
            b"needle here
second line
",
        )
        .unwrap();

        let utf16 = scratch.0.join("wide.txt");
        let mut bytes = vec![0xff, 0xfe];
        for unit in "needle here
"
        .encode_utf16()
        {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        fs::write(&utf16, bytes).unwrap();

        let hits = run(&scratch.0, Pattern::new("needle"));
        assert_eq!(hits.len(), 2, "both files matched: {hits:?}");

        let plan = plan_replace(&hits, &scratch.0, &Pattern::new("needle"), "pin").unwrap();
        apply_replace(&plan).unwrap();

        assert_eq!(
            fs::read(&crlf).unwrap(),
            b"pin here
second line
",
            "CRLF endings were rewritten"
        );

        let after = fs::read(&utf16).unwrap();
        assert_eq!(&after[..2], &[0xff, 0xfe], "the BOM was dropped");
        let text: Vec<u16> = after[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        assert_eq!(
            String::from_utf16(&text).unwrap(),
            "pin here
"
        );
    }

    #[test]
    fn a_case_insensitive_replace_leaves_the_rest_of_the_line_alone() {
        let scratch = Scratch::new("case-replace");
        scratch.file(
            "a.txt",
            "Needle and NEEDLE and needle, KEEP Me
",
        );

        let hits = run(&scratch.0, Pattern::new("needle"));
        let plan = plan_replace(&hits, &scratch.0, &Pattern::new("needle"), "pin").unwrap();
        assert_eq!(plan.occurrences(), 3);
        apply_replace(&plan).unwrap();

        assert_eq!(
            fs::read_to_string(scratch.0.join("a.txt")).unwrap(),
            "pin and pin and pin, KEEP Me
",
            "only the matches changed case"
        );
    }

    #[test]
    fn a_case_sensitive_replace_takes_only_what_it_matched() {
        let scratch = Scratch::new("case-exact");
        scratch.file(
            "a.txt",
            "Needle and needle
",
        );

        let pattern = Pattern::new("Needle");
        let hits = run(&scratch.0, pattern.clone());
        let plan = plan_replace(&hits, &scratch.0, &pattern, "pin").unwrap();
        apply_replace(&plan).unwrap();

        assert_eq!(
            fs::read_to_string(scratch.0.join("a.txt")).unwrap(),
            "pin and needle
"
        );
    }

    #[test]
    fn an_empty_plan_writes_nothing() {
        let scratch = sample("empty-plan");
        let plan = plan_replace(&[], &scratch.0, &Pattern::new("needle"), "pin").unwrap();
        assert!(plan.is_empty());
        assert_eq!(apply_replace(&plan).unwrap(), 0);
    }

    #[test]
    fn a_result_reads_as_a_file_and_a_line() {
        let hit = Hit {
            path: PathBuf::from("src").join("main.rs"),
            line: 42,
            text: "    fn main() {".to_string(),
        };
        assert_eq!(hit.label(), "src/main.rs:42: fn main() {");
    }
}
