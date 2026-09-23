//! A hand-written directory walker, replacing `ignore::WalkBuilder`/
//! `WalkState` per [`PLAN-ZERO-DEP.md`](../../../../PLAN-ZERO-DEP.md) §4
//! Phase 4 Track D.
//!
//! `ignore` turned out to be more than a `.gitignore` matcher: it is also the
//! thing that actually walks the filesystem (hidden-file skipping, the
//! `.gitignore`/`.ignore`/parent-chain resolution, a depth-1 single-directory
//! listing for the tree, and a parallel full walk for quick-open's path
//! index). This module reproduces exactly that, but knows nothing about
//! gitignore glob *syntax* — that lives in `gitignore.rs` (a sibling track,
//! landing separately). This module is generic over the [`Rules`] trait
//! instead, so the real matcher can be plugged in later with no change here.
//!
//! **Global gitignore (`core.excludesFile`) is not implemented.** `.gitignore`
//! and `.ignore` plus the parent-directory chain cover the overwhelming
//! majority of real use and are what is load-bearing for this project's own
//! repo; a machine-wide exclude file adds a real-filesystem, real-git-config
//! dependency for a feature nothing in this codebase's own tests exercises.
//! Call this a documented, honest scope reduction rather than an oversight —
//! PLAN-ZERO-DEP.md §4 explicitly allows it.
//!
//! Two entry points, matching what `project.rs`'s `Tree` and `PathIndex` need:
//! [`list_dir`] (depth-1, single directory) and [`walk_all`] (full recursive,
//! parallel). Sorting and Windows-path-separator normalization are the
//! caller's job, same as they are today — this module only yields entries.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

/// Whatever set of ignore rules is in effect at one point in the tree.
///
/// This is the seam the whole module is built around: `list_dir`/`walk_all`
/// know nothing about gitignore glob syntax, only that *something* can
/// answer "is this entry skipped?" and "what applies once I go one directory
/// deeper?". An integration step later plugs in the real gitignore matcher
/// as the concrete type; tests here use trivial stand-ins instead.
pub trait Rules {
    /// Should this entry, found while these rules are in effect, be skipped?
    ///
    /// `is_dir` matters beyond display: skipping a directory here prunes the
    /// whole subtree rather than filtering its contents after the fact —
    /// this method is never even asked about anything below a directory it
    /// said yes to, which is both the correctness point (a negated rule
    /// inside an excluded directory can't un-exclude anything nothing ever
    /// looked inside for) and the source of the speed.
    fn is_skipped(&self, path: &Path, is_dir: bool) -> bool;

    /// Layer on whatever new rules apply once the walk descends into `dir`.
    ///
    /// `lines` holds the raw lines of a `.gitignore` file directly inside
    /// `dir` followed by an `.ignore` file's (ripgrep's own addition, same
    /// syntax), in that order; empty if neither file exists. The returned
    /// rules apply to `dir`'s own children and, layered further, to
    /// everything below them — never to `dir` itself, which was already
    /// judged by whatever rules were in effect one level up.
    fn extend(&self, dir: &Path, lines: &[String]) -> Self;
}

/// A stateless skip predicate is a valid [`Rules`] on its own: `extend` is a
/// no-op, so it applies uniformly at every depth. Good enough for the
/// simplest stand-ins (a closure that always says "don't skip," one that
/// skips a hardcoded name) without needing the trait at all.
impl<F: Fn(&Path, bool) -> bool + Copy> Rules for F {
    fn is_skipped(&self, path: &Path, is_dir: bool) -> bool {
        self(path, is_dir)
    }

    fn extend(&self, _dir: &Path, _lines: &[String]) -> Self {
        *self
    }
}

/// What kind of thing a directory entry is, as far as this module cares.
/// A symlink (or anything else `fs::FileType` doesn't call a plain file or
/// directory) is `Other`: shown in a depth-1 listing as non-directory, same
/// as today's code, but never indexed and never walked into — this project
/// does not follow symlinks, matching `ignore`'s own default.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Dir,
    File,
    Other,
}

/// A dotfile or dot-directory is skipped uniformly, matching `.hidden(true)`:
/// no exemption for `.gitignore` itself or anything else git tracks by
/// convention. This runs whether or not any [`Rules`] would also skip it.
fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
}

/// The raw lines of `dir`'s own `.gitignore` then `.ignore`, empty if
/// neither exists or either can't be read. Reading these is independent of
/// whether `dir` itself would be *shown* in a listing (it wouldn't be —
/// both names are hidden by [`is_hidden`]), same split `ignore` itself makes.
fn ignore_lines(dir: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    for name in [".gitignore", ".ignore"] {
        if let Ok(contents) = fs::read_to_string(dir.join(name)) {
            lines.extend(contents.lines().map(str::to_owned));
        }
    }
    lines
}

/// `dir`'s immediate children, already past the hidden filter and `rules`.
/// An unreadable directory yields nothing rather than an error — a
/// permission problem three folders down should not take the whole walk
/// with it, matching `project.rs`'s existing `read_directory` doc comment.
fn children<R: Rules>(dir: &Path, rules: &R) -> Vec<(PathBuf, Kind)> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return out,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if is_hidden(&path) {
            continue;
        }
        let kind = match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => Kind::Dir,
            Ok(file_type) if file_type.is_file() => Kind::File,
            _ => Kind::Other,
        };
        let is_dir = kind == Kind::Dir;
        if rules.is_skipped(&path, is_dir) {
            continue;
        }
        out.push((path, kind));
    }
    out
}

/// The rules in effect for `dir`'s own children: `initial` extended once per
/// directory from `root` down to `dir` inclusive, reading each one's own
/// `.gitignore`/`.ignore` along the way.
///
/// This is what lets a single directory be listed on its own — as `Tree`
/// does every time a folder expands — and still see the same rules a full
/// walk from `root` would have accumulated by the time it got there,
/// matching `ignore::WalkBuilder`'s `.parents(true)`.
fn chain_rules<R: Rules>(root: &Path, dir: &Path, initial: &R) -> R {
    let mut chain = vec![root.to_path_buf()];
    if dir != root {
        match dir.strip_prefix(root) {
            Ok(relative) => {
                let mut current = root.to_path_buf();
                for component in relative.components() {
                    current.push(component);
                    chain.push(current.clone());
                }
            }
            // dir isn't under root at all; there's no ancestor chain to
            // climb, so treat dir as its own root rather than guessing.
            Err(_) => chain = vec![dir.to_path_buf()],
        }
    }

    let mut rules = initial.extend(&chain[0], &ignore_lines(&chain[0]));
    for directory in &chain[1..] {
        rules = rules.extend(directory, &ignore_lines(directory));
    }
    rules
}

/// One directory's entries, depth-1, respecting `rules` and everything above
/// `dir` up to `root`. This is what `Tree` needs every time a folder expands.
///
/// `root` anchors the parent-chain climb; it is usually the project root,
/// and deliberately not the same as `dir` once a subfolder is being read.
pub fn list_dir<R: Rules>(dir: &Path, root: &Path, rules: &R) -> Vec<(PathBuf, bool)> {
    let rules = chain_rules(root, dir, rules);
    children(dir, &rules)
        .into_iter()
        .map(|(path, kind)| (path, kind == Kind::Dir))
        .collect()
}

/// Depth-first from `dir`, whose own rules (already extended for its
/// ancestors up to and including itself) are `rules`. Appends every file
/// found to `out`; directories that aren't skipped are recursed into,
/// skipped ones are pruned outright — never opened, so nothing below them
/// is ever asked about.
fn walk_recursive<R: Rules>(dir: &Path, parent_rules: &R, out: &mut Vec<PathBuf>) {
    let rules = parent_rules.extend(dir, &ignore_lines(dir));
    for (path, kind) in children(dir, &rules) {
        match kind {
            Kind::Dir => walk_recursive(&path, &rules, out),
            Kind::File => out.push(path),
            Kind::Other => {}
        }
    }
}

/// Split `items` round-robin into up to `buckets` non-empty groups, so work
/// spreads evenly regardless of how it compares to the thread count.
fn split_evenly<T>(items: Vec<T>, buckets: usize) -> Vec<Vec<T>> {
    let buckets = buckets.max(1);
    let mut groups: Vec<Vec<T>> = (0..buckets).map(|_| Vec::new()).collect();
    for (index, item) in items.into_iter().enumerate() {
        groups[index % buckets].push(item);
    }
    groups.retain(|group| !group.is_empty());
    groups
}

/// Every file under `root`, walked in parallel. This is what `PathIndex`
/// needs: the cost is dominated by stat calls and ignore-file lookups, both
/// of which parallelize well across independent subtrees.
///
/// Strategy: read `root` itself on the calling thread, then divide its
/// immediate subdirectories round-robin across a fixed pool sized by
/// [`std::thread::available_parallelism`], each walking its share
/// sequentially and returning its own `Vec<PathBuf>`. Using
/// [`std::thread::scope`] means every worker's result is joined explicitly
/// before this function returns — unlike a callback-driven walker with no
/// "flush what's left" hook, there is no way for a worker's last partial
/// batch to be silently dropped, because nothing is reported until the
/// thread's return value is collected whole.
pub fn walk_all<R: Rules + Sync>(root: &Path, rules: &R) -> Vec<PathBuf> {
    let root_rules = chain_rules(root, root, rules);

    let mut files = Vec::new();
    let mut subdirs = Vec::new();
    for (path, kind) in children(root, &root_rules) {
        match kind {
            Kind::File => files.push(path),
            Kind::Dir => subdirs.push(path),
            Kind::Other => {}
        }
    }

    if subdirs.is_empty() {
        return files;
    }

    let thread_count = thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        .min(subdirs.len());
    let chunks = split_evenly(subdirs, thread_count);

    let per_thread: Vec<Vec<PathBuf>> = thread::scope(|scope| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                let root_rules = &root_rules;
                scope.spawn(move || {
                    let mut found = Vec::new();
                    for dir in chunk {
                        walk_recursive(&dir, root_rules, &mut found);
                    }
                    found
                })
            })
            .collect();

        // A panicking worker (there shouldn't be one) loses only its own
        // share rather than the whole walk, same philosophy as an
        // unreadable directory yielding nothing instead of an error.
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or_default())
            .collect()
    });

    for mut batch in per_thread {
        files.append(&mut batch);
    }
    files
}

/// Depth-first from `dir` (whose own rules are already extended for it),
/// calling `on_file` for every file found and checking `cancelled` between
/// each one — [`walk_streaming`]'s per-thread half.
fn walk_recursive_streaming<R: Rules>(
    dir: &Path,
    parent_rules: &R,
    cancelled: &AtomicBool,
    on_file: &(dyn Fn(&Path) + Sync),
) {
    if cancelled.load(Ordering::Relaxed) {
        return;
    }
    let rules = parent_rules.extend(dir, &ignore_lines(dir));
    for (path, kind) in children(dir, &rules) {
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        match kind {
            Kind::Dir => walk_recursive_streaming(&path, &rules, cancelled, on_file),
            Kind::File => on_file(&path),
            Kind::Other => {}
        }
    }
}

/// Every file under `root`, streamed to `on_file` as it is found rather than
/// collected into a `Vec` — what `project_search.rs` needs
/// (`PLAN-ZERO-DEP.md` Phase 6), replacing
/// `ignore::WalkBuilder::build_parallel().run(..)`'s callback-per-file walk.
///
/// Parallel the same way [`walk_all`] is (root's own subdirectories divided
/// round-robin across a thread pool, each walked sequentially), but streamed
/// instead of joined: `on_file` is called directly from whichever worker
/// thread found that file, potentially concurrently with another worker
/// calling it for a different file, which is why it must be [`Sync`] — a
/// caller wanting to collect results still can, through whatever
/// thread-safe channel or shared state `on_file` itself closes over (see
/// `project_search.rs`'s own `Collector`), same as `ignore`'s callback did.
///
/// This function blocks until the walk finishes or `cancelled` is set to
/// `true` (checked between files and between directories, never mid-file) —
/// callers that want it to run in the background call it from their own
/// spawned thread, the same way `project_search.rs` already spawns
/// `"aitch-project-search"` and blocked inside it on `ignore`'s own
/// `walker.run(..)`.
pub fn walk_streaming<R, F>(root: &Path, rules: &R, cancelled: &AtomicBool, on_file: F)
where
    R: Rules + Sync,
    F: Fn(&Path) + Sync,
{
    let root_rules = chain_rules(root, root, rules);

    let mut files_here = Vec::new();
    let mut subdirs = Vec::new();
    for (path, kind) in children(root, &root_rules) {
        match kind {
            Kind::File => files_here.push(path),
            Kind::Dir => subdirs.push(path),
            Kind::Other => {}
        }
    }

    for path in &files_here {
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        on_file(path);
    }
    if subdirs.is_empty() || cancelled.load(Ordering::Relaxed) {
        return;
    }

    let thread_count = thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        .min(subdirs.len());
    let chunks = split_evenly(subdirs, thread_count);

    thread::scope(|scope| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                let root_rules = &root_rules;
                let on_file = &on_file;
                scope.spawn(move || {
                    for dir in chunk {
                        if cancelled.load(Ordering::Relaxed) {
                            return;
                        }
                        walk_recursive_streaming(&dir, root_rules, cancelled, on_file);
                    }
                })
            })
            .collect();
        for handle in handles {
            // A panicking worker loses only its own share rather than the
            // whole walk, same philosophy as `walk_all`'s.
            let _ = handle.join();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// A scratch folder that cleans itself up. Same pattern `project.rs`
    /// uses, written fresh here since that file isn't touched by this track.
    struct Scratch(PathBuf);

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("aitch-walk-{name}-{}-{unique}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn file(&self, relative: &str, contents: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            path
        }

        fn dir(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(&path).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The simplest possible `Rules`: never skip anything, at any depth.
    fn never_skip(_path: &Path, _is_dir: bool) -> bool {
        false
    }

    /// A stand-in that skips one exact file/directory name, uniformly at
    /// every depth, with no parent-chain awareness at all.
    fn skip_named(name: &'static str) -> impl Rules + Copy {
        move |path: &Path, _is_dir: bool| path.file_name().and_then(|n| n.to_str()) == Some(name)
    }

    /// Records every path it was ever asked about, and whether it says skip,
    /// so a test can prove a pruned directory's contents were never even
    /// looked at (not merely filtered afterward). Shares its log/counter
    /// across `extend` calls via `Arc`, since each directory level gets its
    /// own owned `Rules` value but they should all count toward one total.
    #[derive(Clone)]
    struct CountingSkip {
        banned_name: &'static str,
        asked: Arc<Mutex<Vec<PathBuf>>>,
    }

    impl CountingSkip {
        fn new(banned_name: &'static str) -> CountingSkip {
            CountingSkip {
                banned_name,
                asked: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn was_ever_asked_about(&self, path: &Path) -> bool {
            self.asked.lock().unwrap().iter().any(|seen| seen == path)
        }
    }

    impl Rules for CountingSkip {
        fn is_skipped(&self, path: &Path, _is_dir: bool) -> bool {
            self.asked.lock().unwrap().push(path.to_path_buf());
            path.file_name().and_then(|n| n.to_str()) == Some(self.banned_name)
        }

        fn extend(&self, _dir: &Path, _lines: &[String]) -> Self {
            self.clone()
        }
    }

    /// A trivial, non-glob "ignore" stand-in: each non-blank line in an
    /// ignore file names an exact file/directory name to skip anywhere at or
    /// below the directory that declared it. Not real gitignore syntax —
    /// proving the parent-chain accumulation/scoping is walk.rs's job
    /// independent of any particular pattern language, which is exactly
    /// what this exercises.
    #[derive(Clone, Default)]
    struct NameRules {
        banned: Vec<String>,
    }

    impl Rules for NameRules {
        fn is_skipped(&self, path: &Path, _is_dir: bool) -> bool {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| self.banned.iter().any(|b| b == name))
        }

        fn extend(&self, _dir: &Path, lines: &[String]) -> Self {
            let mut banned = self.banned.clone();
            banned.extend(
                lines
                    .iter()
                    .map(|l| l.trim().to_owned())
                    .filter(|l| !l.is_empty()),
            );
            NameRules { banned }
        }
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        let mut names: Vec<String> = paths
            .iter()
            .filter_map(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .collect();
        names.sort();
        names
    }

    // -- depth-1 listing -----------------------------------------------

    #[test]
    fn depth_one_listing_returns_only_immediate_children() {
        let scratch = Scratch::new("depth1");
        scratch.file("a.txt", "a");
        scratch.file("b.txt", "b");
        scratch.dir("sub");
        scratch.file("sub/deep.txt", "deep");

        let entries = list_dir(&scratch.0, &scratch.0, &never_skip);
        let mut got: Vec<(String, bool)> = entries
            .into_iter()
            .map(|(p, is_dir)| (p.file_name().unwrap().to_string_lossy().to_string(), is_dir))
            .collect();
        got.sort();

        assert_eq!(
            got,
            [
                ("a.txt".to_string(), false),
                ("b.txt".to_string(), false),
                ("sub".to_string(), true),
            ],
            "no grandchildren, and the right file/dir flags"
        );
    }

    #[test]
    fn an_unreadable_directory_yields_nothing_rather_than_an_error() {
        let scratch = Scratch::new("missing");
        let missing = scratch.0.join("does-not-exist");
        assert_eq!(list_dir(&missing, &scratch.0, &never_skip), Vec::new());
    }

    // -- full recursive walk --------------------------------------------

    #[test]
    fn full_walk_finds_every_file_in_a_multi_level_tree() {
        let scratch = Scratch::new("full");
        scratch.file("README.md", "readme");
        scratch.file("src/main.rs", "fn main() {}");
        scratch.file("src/lib.rs", "pub fn go() {}");
        scratch.file("src/deep/nested.rs", "nested");
        scratch.file("src/deep/deeper/leaf.rs", "leaf");

        let found = walk_all(&scratch.0, &never_skip);
        assert_eq!(
            names(&found),
            ["README.md", "leaf.rs", "lib.rs", "main.rs", "nested.rs"]
        );
    }

    // -- hidden files ------------------------------------------------------

    #[test]
    fn hidden_files_and_directories_are_skipped_by_default() {
        let scratch = Scratch::new("hidden");
        scratch.file("visible.txt", "v");
        scratch.file(".dotfile", "hidden");
        scratch.dir(".dotdir");
        scratch.file(".dotdir/inside.txt", "also hidden");

        let listing = list_dir(&scratch.0, &scratch.0, &never_skip);
        assert_eq!(
            names(&listing.into_iter().map(|(p, _)| p).collect::<Vec<_>>()),
            ["visible.txt"]
        );

        let walked = walk_all(&scratch.0, &never_skip);
        assert_eq!(names(&walked), ["visible.txt"]);
    }

    // -- the skip-predicate hook ---------------------------------------

    #[test]
    fn a_stand_in_predicate_hides_the_name_it_targets() {
        let scratch = Scratch::new("skip-named");
        scratch.file("keep.txt", "keep");
        scratch.file("drop.txt", "drop");

        let found = walk_all(&scratch.0, &skip_named("drop.txt"));
        assert_eq!(names(&found), ["keep.txt"]);
    }

    #[test]
    fn a_skipped_directory_and_everything_below_it_never_appears() {
        let scratch = Scratch::new("skip-dir");
        scratch.file("keep/a.txt", "a");
        scratch.file("blocked/b.txt", "b");
        scratch.file("blocked/deeper/c.txt", "c");

        let found = walk_all(&scratch.0, &skip_named("blocked"));
        assert_eq!(names(&found), ["a.txt"]);
    }

    // -- directory pruning, proven by a call counter ------------------------

    #[test]
    fn a_pruned_directory_is_never_even_opened() {
        let scratch = Scratch::new("prune");
        scratch.file("keep/a.txt", "a");
        let inside = scratch.file("blocked/inside.txt", "would otherwise be included");

        let rules = CountingSkip::new("blocked");
        let found = walk_all(&scratch.0, &rules);

        assert_eq!(names(&found), ["a.txt"], "blocked/ must be pruned");
        assert!(
            !rules.was_ever_asked_about(&inside),
            "the walker must never even ask about a path inside a pruned directory \
             -- it should have been pruned before the directory was opened, not \
             filtered out after the fact"
        );
    }

    // -- the .gitignore/.ignore parent chain --------------------------------

    #[test]
    fn a_root_level_rule_reaches_several_levels_down() {
        let scratch = Scratch::new("chain-root");
        scratch.file(".gitignore", "vendor\n");
        scratch.file("a/b/vendor/c/d.txt", "should never appear");
        scratch.file("a/b/keep.txt", "kept");

        let found = walk_all(&scratch.0, &NameRules::default());
        assert_eq!(names(&found), ["keep.txt"]);
    }

    #[test]
    fn a_subdirectorys_own_rule_does_not_leak_to_its_siblings_or_parent() {
        let scratch = Scratch::new("chain-scope");
        scratch.file(
            "src/local.txt",
            "kept: sibling of the folder that ignores it",
        );
        scratch.file("src/nested/.gitignore", "local.txt\n");
        scratch.file("src/nested/local.txt", "excluded: nested's own rule");
        scratch.file("src/nested/keep.txt", "kept");

        let found = walk_all(&scratch.0, &NameRules::default());
        let mut got = names(&found);
        got.sort();
        assert_eq!(
            got,
            ["keep.txt", "local.txt"],
            "only nested/local.txt is gone"
        );
    }

    #[test]
    fn listing_a_deep_directory_directly_still_sees_its_ancestors_rules() {
        let scratch = Scratch::new("chain-list");
        scratch.file(".gitignore", "vendor\n");
        let vendor = scratch.dir("a/b/vendor");
        scratch.file("a/b/vendor/c.txt", "would be here if not pruned");
        scratch.file("a/b/keep.txt", "kept");

        // Simulates `Tree` expanding a/b directly, without having walked
        // down from the root first -- the same shape as `read_directory`
        // being called fresh on whatever folder was just expanded.
        let listing = list_dir(&scratch.0.join("a/b"), &scratch.0, &NameRules::default());
        let names: Vec<String> = listing
            .into_iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            names,
            ["keep.txt"],
            "vendor/ pruned even though listing started at a/b"
        );

        // The rules accumulated on the way down still ban "vendor" by name
        // -- pruning is `a/b`'s listing choosing not to recurse into it, not
        // some special-case around `vendor` itself, which a direct listing
        // of it (a distinct operation, same as `Tree` re-reading any single
        // folder) is free to do and see its own contents.
        let rules_at_b = chain_rules(&scratch.0, &scratch.0.join("a/b"), &NameRules::default());
        assert!(rules_at_b.is_skipped(&vendor, true));
    }

    // -- symlinks (best-effort; see module test notes) ----------------------

    #[test]
    fn a_broken_symlink_does_not_crash_the_walk() {
        let scratch = Scratch::new("symlink");
        scratch.file("real.txt", "real");
        let link = scratch.0.join("broken-link.txt");
        let target = scratch.0.join("does-not-exist.txt");

        // Creating a symlink on Windows needs a privilege (Developer Mode or
        // admin) this test environment may not have. If it fails, that's a
        // property of the sandbox, not of the walker -- skip rather than
        // fail, but still prove the walk doesn't crash either way.
        if std::os::windows::fs::symlink_file(&target, &link).is_err() {
            let found = walk_all(&scratch.0, &never_skip);
            assert_eq!(names(&found), ["real.txt"]);
            return;
        }

        let found = walk_all(&scratch.0, &never_skip);
        assert_eq!(
            names(&found),
            ["real.txt"],
            "a symlink (broken or not) is neither indexed nor walked into"
        );

        let listing = list_dir(&scratch.0, &scratch.0, &never_skip);
        assert!(
            listing.iter().any(|(p, is_dir)| p == &link && !is_dir),
            "a depth-1 listing still shows the symlink entry, as a non-directory"
        );
    }

    // -- parallel walk matches the sequential/shallow walk -------------

    #[test]
    fn the_parallel_walk_matches_a_manual_sequential_walk() {
        let scratch = Scratch::new("parallel-parity");
        for top in 0..8 {
            for leaf in 0..6 {
                scratch.file(&format!("dir{top}/leaf{leaf}.txt"), "x");
            }
            scratch.file(&format!("dir{top}/sub/deep.txt"), "y");
        }
        scratch.file("root-file.txt", "z");

        fn sequential(dir: &Path, rules: &impl Rules, out: &mut Vec<PathBuf>) {
            for (path, is_dir) in list_dir(dir, dir, rules) {
                if is_dir {
                    sequential(&path, rules, out);
                } else {
                    out.push(path);
                }
            }
        }

        // `list_dir`-based sequential walk re-derives rules from `dir` as
        // its own root at each step, which is fine here since `never_skip`
        // doesn't care about anchoring -- this only exercises the walking
        // shape, not rule-chain semantics (those have their own tests above).
        let mut expected = Vec::new();
        sequential(&scratch.0, &never_skip, &mut expected);

        let actual = walk_all(&scratch.0, &never_skip);

        let mut expected_names = names(&expected);
        let mut actual_names = names(&actual);
        expected_names.sort();
        actual_names.sort();
        assert_eq!(actual_names, expected_names);
        assert_eq!(
            expected.len(),
            8 * 6 + 8 + 1,
            "sanity: the fixture itself has this many files"
        );
        assert_eq!(
            actual.len(),
            expected.len(),
            "no file dropped from a worker's last batch"
        );
    }

    // -- walk_streaming -----------------------------------------------------

    fn streamed(root: &Path, rules: &(impl Rules + Sync)) -> Vec<PathBuf> {
        let found: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
        let cancelled = AtomicBool::new(false);
        walk_streaming(root, rules, &cancelled, |path| {
            found.lock().unwrap().push(path.to_path_buf());
        });
        found.into_inner().unwrap()
    }

    #[test]
    fn streaming_finds_every_file_a_batch_walk_does() {
        let scratch = Scratch::new("stream-parity");
        scratch.file("a.txt", "");
        scratch.file("sub/b.txt", "");
        scratch.file("sub/deeper/c.txt", "");

        let mut expected = names(&walk_all(&scratch.0, &never_skip));
        let mut actual = names(&streamed(&scratch.0, &never_skip));
        expected.sort();
        actual.sort();
        assert_eq!(actual, expected);
    }

    #[test]
    fn streaming_still_honours_skip_rules() {
        let scratch = Scratch::new("stream-skip");
        scratch.file("keep.txt", "");
        scratch.file("skip.txt", "");

        let found = streamed(&scratch.0, &skip_named("skip.txt"));
        assert_eq!(names(&found), vec!["keep.txt".to_string()]);
    }

    #[test]
    fn a_pruned_directory_is_never_opened_by_the_streaming_walk_either() {
        let scratch = Scratch::new("stream-prune");
        let inside = scratch.file("banned/inside.txt", "");
        scratch.file("kept.txt", "");

        let rules = CountingSkip::new("banned");
        let found = streamed(&scratch.0, &rules);

        assert_eq!(names(&found), vec!["kept.txt".to_string()]);
        assert!(
            !rules.was_ever_asked_about(&inside),
            "a file inside a pruned directory was still looked at"
        );
    }

    #[test]
    fn cancelling_stops_the_streaming_walk_promptly() {
        let scratch = Scratch::new("stream-cancel");
        for i in 0..500 {
            scratch.file(&format!("dir{}/file{i}.txt", i % 20), "");
        }

        let cancelled = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(AtomicUsize::new(0));
        let cancel_after = cancelled.clone();
        let seen_count = seen.clone();

        let root = scratch.0.clone();
        let handle = thread::spawn(move || {
            walk_streaming(&root, &never_skip, &cancel_after, |_path| {
                seen_count.fetch_add(1, Ordering::Relaxed);
            });
        });

        // Cancel almost immediately; the walk must still terminate rather
        // than running to completion regardless.
        cancelled.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        assert!(
            seen.load(Ordering::Relaxed) < 500,
            "the cancelled walk still visited every file"
        );
    }

    #[test]
    fn on_file_can_be_called_concurrently_from_several_workers() {
        // Exercises walk_streaming's own Sync bound on `on_file`: a shared
        // counter incremented from whichever thread found each file must
        // still land on the right total, with no lost updates.
        let scratch = Scratch::new("stream-concurrent");
        for i in 0..200 {
            scratch.file(&format!("dir{}/file{i}.txt", i % 16), "");
        }

        let cancelled = AtomicBool::new(false);
        let count = AtomicUsize::new(0);
        walk_streaming(&scratch.0, &never_skip, &cancelled, |_path| {
            count.fetch_add(1, Ordering::Relaxed);
        });

        assert_eq!(count.load(Ordering::Relaxed), 200);
    }
}
