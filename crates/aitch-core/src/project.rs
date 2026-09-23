//! The folder: a file tree, and an index of paths to search.
//!
//! nano has no equivalent, so this is invention — kept keyboard-first and
//! quiet, as PLAN.md §5 asks.
//!
//! Two things carry the performance budget for a tree the size of a kernel
//! checkout:
//!
//! - **Directories are read only when expanded.** Opening a folder reads one
//!   directory, not the whole tree. What the sidebar shows is a flat list of
//!   rows, rebuilt from the set of expanded folders, so the renderer can
//!   virtualize it by slicing.
//! - **The path index for quick open is built once, on first use.** Walking
//!   80k files is not free; matching against them afterwards is, which is the
//!   part that has to keep up with typing.
//!
//! `.gitignore` handling for `Tree` and `PathIndex` comes from the
//! hand-written [`crate::gitignore`] matcher and [`crate::walk`] walker
//! (Phase 4, `PLAN-ZERO-DEP.md` §4). [`crate::project_search`]'s
//! cancellable, streaming project-wide search (Phase 6) uses the same
//! [`IgnoreRules`], through [`crate::walk::walk_streaming`] — this crate has
//! had no `ignore` dependency at all since Phase 6 landed.

use std::path::{Path, PathBuf};

use crate::fuzzy;
use crate::gitignore;
use crate::walk;

/// One visible row of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub path: PathBuf,
    /// How deep below the root, for indentation. The root's children are 0.
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
}

impl Row {
    /// What the sidebar shows: indentation, a marker for folders, the name.
    pub fn label(&self) -> String {
        let indent = "  ".repeat(self.depth);
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.display().to_string());

        if self.is_dir {
            let marker = if self.expanded { 'v' } else { '>' };
            format!("{indent}{marker} {name}/")
        } else {
            format!("{indent}  {name}")
        }
    }
}

/// The sidebar's model: which folders are open, and the rows that follow.
#[derive(Debug)]
pub struct Tree {
    root: PathBuf,
    expanded: Vec<PathBuf>,
    rows: Vec<Row>,
    selected: usize,
    /// Extra ignore rules from the config. Held rather than passed in,
    /// because a folder is read again every time one is expanded.
    ignores: Vec<String>,
}

impl Tree {
    /// Open a folder. Only the root is read; nothing below it is touched.
    pub fn new(root: PathBuf, ignores: &[String]) -> Tree {
        let mut tree = Tree {
            root,
            expanded: Vec::new(),
            rows: Vec::new(),
            selected: 0,
            ignores: ignores.to_vec(),
        };
        tree.rebuild();
        tree
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn selected(&self) -> Option<&Row> {
        self.rows.get(self.selected)
    }

    pub fn select(&mut self, index: usize) -> bool {
        let clamped = index.min(self.rows.len().saturating_sub(1));
        let moved = clamped != self.selected;
        self.selected = clamped;
        moved
    }

    pub fn move_down(&mut self) -> bool {
        self.select(self.selected + 1)
    }

    pub fn move_up(&mut self) -> bool {
        self.select(self.selected.saturating_sub(1))
    }

    pub fn move_by(&mut self, rows: isize) -> bool {
        let target = if rows >= 0 {
            self.selected.saturating_add(rows as usize)
        } else {
            self.selected.saturating_sub(rows.unsigned_abs())
        };
        self.select(target)
    }

    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.iter().any(|open| open == path)
    }

    /// Act on the selected row: fold a directory open or shut, or report the
    /// file to open. The caller opens it — the tree does not read files.
    pub fn activate(&mut self) -> Option<PathBuf> {
        let row = self.rows.get(self.selected)?.clone();
        if !row.is_dir {
            return Some(row.path);
        }

        match self.expanded.iter().position(|open| *open == row.path) {
            Some(index) => {
                self.expanded.remove(index);
                // Anything below it was only open because this was.
                self.expanded.retain(|open| !open.starts_with(&row.path));
            }
            None => self.expanded.push(row.path.clone()),
        }

        let selected_path = row.path;
        self.rebuild();
        // Keep the same row under the cursor, wherever it moved to.
        if let Some(index) = self.rows.iter().position(|r| r.path == selected_path) {
            self.selected = index;
        }
        None
    }

    /// Re-read every open directory. Called when the watcher reports a change.
    pub fn refresh(&mut self) {
        let selected_path = self.selected().map(|row| row.path.clone());
        self.rebuild();
        if let Some(path) = selected_path {
            if let Some(index) = self.rows.iter().position(|row| row.path == path) {
                self.selected = index;
            } else {
                self.selected = self.selected.min(self.rows.len().saturating_sub(1));
            }
        }
    }

    /// Rebuild the flat row list from the set of expanded folders.
    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        let root = self.root.clone();
        self.push_children(&root, 0, &mut rows);
        self.rows = rows;
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    fn push_children(&self, directory: &Path, depth: usize, rows: &mut Vec<Row>) {
        for (path, is_dir) in read_directory(directory, &self.root, &self.ignores) {
            let expanded = is_dir && self.is_expanded(&path);
            rows.push(Row {
                path: path.clone(),
                depth,
                is_dir,
                expanded,
            });
            if expanded {
                self.push_children(&path, depth + 1, rows);
            }
        }
    }
}

/// An `IgnoreRules`-shaped combination of the `.gitignore`/`.ignore` chain
/// (via [`crate::walk::Rules::extend`], one `gitignore::Rules` layer per
/// directory) and `aitchrc.toml`'s own extra patterns, hand-written to
/// replace `ignore::Gitignore`/`GitignoreBuilder` behind [`Tree`],
/// [`PathIndex`], and (Phase 6) `project_search.rs`'s streaming search — see
/// [`crate::gitignore`] and [`crate::walk`]. `pub(crate)` for that last one;
/// `Tree`/`PathIndex` are this module's own only other callers.
///
/// The extra patterns are **additive-only**: they can add an exclusion on
/// top of whatever `.gitignore`/`.ignore` already decided, but can never
/// re-include something those files excluded — the same shape the
/// `ignore`-crate-based walker this replaced gave its own `filter_entry`
/// closure, a second, independent check that could only narrow, never
/// widen, what its built-in `.gitignore` handling already allowed through.
/// Folding the extra patterns into the same last-match-wins cascade as the
/// `.gitignore` chain itself would let them incorrectly resurrect a
/// `.gitignore`-excluded path, so this keeps them a separate check instead.
///
/// **Does not read a global/machine-wide gitignore** (`core.excludesFile`):
/// neither did [`crate::walk`] for `Tree`/`PathIndex` since Phase 4, a
/// documented scope reduction there (see its own module docs) that Phase 6
/// extends to project-wide search too, for the same reason — it is a
/// real-filesystem, real-git-config dependency for a feature nothing in
/// this codebase's own tests exercises.
#[derive(Debug, Clone)]
pub(crate) struct IgnoreRules {
    /// `.gitignore`/`.ignore` layers accumulated root-to-current by
    /// [`walk::Rules::extend`]. Each entry is the directory that layer's
    /// rules were read from, so a path can be made relative to it before
    /// matching — `gitignore::Rules` expects a path relative to *its own*
    /// directory, not the project root.
    layers: Vec<(PathBuf, gitignore::Rules)>,
    root: PathBuf,
    /// `aitchrc.toml`'s own extra ignore patterns, always anchored at `root`
    /// regardless of how deep the walk currently is.
    extra: gitignore::Rules,
}

impl IgnoreRules {
    pub(crate) fn new(root: &Path, extra: &[String]) -> IgnoreRules {
        IgnoreRules {
            layers: Vec::new(),
            root: root.to_path_buf(),
            extra: gitignore::Rules::from_lines(extra),
        }
    }
}

impl walk::Rules for IgnoreRules {
    fn is_skipped(&self, path: &Path, is_dir: bool) -> bool {
        // Root-to-current order: a deeper (more specific) directory's own
        // `.gitignore` gets the final say over the same path, exactly like
        // git concatenates applicable files from least to most specific and
        // takes the last match — see `gitignore::Rules::is_ignored`'s own
        // last-match-wins docs for the single-file version of this rule.
        let mut verdict = None;
        for (dir, rules) in &self.layers {
            if let Ok(relative) = path.strip_prefix(dir) {
                if let Some(v) = rules.verdict(relative, is_dir) {
                    verdict = Some(v);
                }
            }
        }
        if verdict.unwrap_or(false) {
            return true;
        }

        // Additive-only: see this struct's doc comment for why this is a
        // separate check rather than another layer in the cascade above.
        match path.strip_prefix(&self.root) {
            Ok(relative) => self.extra.is_ignored(relative, is_dir),
            Err(_) => false,
        }
    }

    fn extend(&self, dir: &Path, lines: &[String]) -> Self {
        let mut layers = self.layers.clone();
        if !lines.is_empty() {
            layers.push((dir.to_path_buf(), gitignore::Rules::from_lines(lines)));
        }
        IgnoreRules {
            layers,
            root: self.root.clone(),
            extra: self.extra.clone(),
        }
    }
}

/// One directory's entries, respecting `.gitignore`, folders first.
///
/// An unreadable directory yields nothing rather than an error: a permission
/// problem three folders down should not take the sidebar with it — see
/// [`crate::walk::list_dir`], which this is now a thin wrapper over.
fn read_directory(directory: &Path, root: &Path, ignores: &[String]) -> Vec<(PathBuf, bool)> {
    let rules = IgnoreRules::new(root, ignores);
    let mut entries = walk::list_dir(directory, root, &rules);

    entries.sort_by(|(a, a_dir), (b, b_dir)| {
        b_dir
            .cmp(a_dir)
            .then_with(|| natural_name(a).cmp(&natural_name(b)))
    });
    entries
}

fn natural_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Every file under the root, for quick open.
///
/// Built once and kept: walking is the slow half, matching is the fast half,
/// and only the fast half happens on a keystroke.
#[derive(Debug, Default)]
pub struct PathIndex {
    root: PathBuf,
    /// Paths relative to the root, which is what a user searches by and what
    /// the results should show.
    relative: Vec<String>,
}

impl PathIndex {
    /// Walk a folder and index every file in it.
    ///
    /// Uses [`crate::walk::walk_all`]'s parallel walk: the cost is dominated
    /// by stat calls and `.gitignore` lookups, both of which parallelize
    /// well. A single-threaded walk of 35k files measured at 3.6 seconds,
    /// which is a visible stall the first time quick open is pressed.
    ///
    /// `walk_all` already joins every worker thread's own owned result
    /// before returning, so there is no partial-last-batch risk here the way
    /// there was with the old callback-driven walker (see that module's own
    /// docs) — nothing left to stream through a channel.
    pub fn build(root: &Path, ignores: &[String]) -> PathIndex {
        let rules = IgnoreRules::new(root, ignores);
        let files = walk::walk_all(root, &rules);

        let mut relative: Vec<String> = files
            .iter()
            .filter_map(|path| path.strip_prefix(root).ok())
            // Windows separators, so a query reads the same way on either
            // platform.
            .map(|suffix| suffix.to_string_lossy().replace('\\', "/"))
            .collect();
        relative.sort();

        PathIndex {
            root: root.to_path_buf(),
            relative,
        }
    }

    /// An index over paths already in hand, for tests and benchmarks that
    /// should not depend on a folder of the right size existing on disk.
    pub fn from_paths(root: PathBuf, mut relative: Vec<String>) -> PathIndex {
        relative.sort();
        PathIndex { root, relative }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn len(&self) -> usize {
        self.relative.len()
    }

    pub fn is_empty(&self) -> bool {
        self.relative.is_empty()
    }

    pub fn paths(&self) -> &[String] {
        &self.relative
    }

    /// The best matches for a query, most relevant first.
    ///
    /// An empty query is not "everything scored equally" — it is the first
    /// `limit` paths, so opening quick open shows something immediately.
    pub fn search(&self, query: &str, limit: usize) -> Vec<String> {
        if query.trim().is_empty() {
            return self.relative.iter().take(limit).cloned().collect();
        }

        fuzzy::best_matches(self.relative.iter().map(String::as_str), query, limit)
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    /// Turn a result back into a path that can be opened.
    pub fn resolve(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A scratch folder that cleans itself up.
    struct Scratch(PathBuf);

    /// Tests in a binary run in parallel, and several of them ask for the same
    /// sample tree. Without a counter they would share one directory and wipe
    /// it out from under each other.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "aitch-project-{name}-{}-{unique}",
                std::process::id()
            ));
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

    fn sample() -> Scratch {
        let scratch = Scratch::new("tree");
        scratch.file("README.md", "readme");
        scratch.file("Cargo.toml", "manifest");
        scratch.file("src/main.rs", "fn main() {}");
        scratch.file("src/lib.rs", "pub fn go() {}");
        scratch.file("src/deep/nested.rs", "nested");
        scratch.dir("target");
        scratch.file("target/junk.o", "build output");
        scratch.file(".gitignore", "target/\n");
        scratch
    }

    fn labels(tree: &Tree) -> Vec<String> {
        tree.rows().iter().map(Row::label).collect()
    }

    #[test]
    fn opening_a_folder_shows_only_its_top_level() {
        let scratch = sample();
        let tree = Tree::new(scratch.0.clone(), &[]);

        assert_eq!(
            labels(&tree),
            ["> src/", "  Cargo.toml", "  README.md"],
            "folders first, then files, and nothing below the top level"
        );
    }

    #[test]
    fn gitignored_folders_stay_out_of_the_tree() {
        let scratch = sample();
        let tree = Tree::new(scratch.0.clone(), &[]);
        assert!(
            !labels(&tree).iter().any(|row| row.contains("target")),
            "target/ is in .gitignore and must not be listed"
        );
    }

    #[test]
    fn expanding_a_folder_reveals_its_children_and_folds_them_away_again() {
        let scratch = sample();
        let mut tree = Tree::new(scratch.0.clone(), &[]);

        assert_eq!(tree.activate(), None, "a folder opens rather than opening");
        assert_eq!(
            labels(&tree),
            [
                "v src/",
                "  > deep/",
                "    lib.rs",
                "    main.rs",
                "  Cargo.toml",
                "  README.md",
            ]
        );

        tree.activate();
        assert_eq!(labels(&tree), ["> src/", "  Cargo.toml", "  README.md"]);
    }

    #[test]
    fn collapsing_a_folder_forgets_what_was_open_inside_it() {
        let scratch = sample();
        let mut tree = Tree::new(scratch.0.clone(), &[]);

        tree.activate(); // open src/
        tree.move_down();
        tree.activate(); // open src/deep/
        assert!(labels(&tree).iter().any(|row| row.contains("nested.rs")));

        tree.move_up();
        tree.activate(); // close src/
        tree.activate(); // open src/ again
        assert!(
            !labels(&tree).iter().any(|row| row.contains("nested.rs")),
            "deep/ should come back shut, not remember it was open"
        );
    }

    #[test]
    fn activating_a_file_reports_it_rather_than_opening_it() {
        let scratch = sample();
        let mut tree = Tree::new(scratch.0.clone(), &[]);
        tree.activate(); // src/
        tree.move_down();
        tree.move_down(); // src/lib.rs

        let opened = tree.activate().expect("a file should be reported");
        assert!(opened.ends_with("lib.rs"), "{opened:?}");
    }

    #[test]
    fn the_selection_stays_on_the_same_row_when_a_folder_opens() {
        let scratch = sample();
        let mut tree = Tree::new(scratch.0.clone(), &[]);
        let before = tree.selected().unwrap().path.clone();
        tree.activate();
        assert_eq!(tree.selected().unwrap().path, before);
    }

    #[test]
    fn the_selection_stops_at_both_ends() {
        let scratch = sample();
        let mut tree = Tree::new(scratch.0.clone(), &[]);

        assert!(!tree.move_up(), "already at the top");
        for _ in 0..20 {
            tree.move_down();
        }
        assert_eq!(tree.selected_index(), tree.len() - 1);
        assert!(!tree.move_down(), "already at the end");
    }

    #[test]
    fn an_empty_folder_has_no_rows_and_does_not_panic() {
        let scratch = Scratch::new("empty");
        let mut tree = Tree::new(scratch.0.clone(), &[]);
        assert!(tree.is_empty());
        assert!(tree.selected().is_none());
        assert_eq!(tree.activate(), None);
        assert!(!tree.move_down());
    }

    #[test]
    fn a_refresh_picks_up_a_new_file_and_keeps_the_selection() {
        let scratch = sample();
        let mut tree = Tree::new(scratch.0.clone(), &[]);
        let before = tree.selected().unwrap().path.clone();

        scratch.file("AAA-new.txt", "added outside the editor");
        tree.refresh();

        assert!(labels(&tree).iter().any(|row| row.contains("AAA-new.txt")));
        assert_eq!(tree.selected().unwrap().path, before, "selection followed");
    }

    // -- quick open --------------------------------------------------------

    #[test]
    fn the_index_holds_every_file_and_no_folders() {
        let scratch = sample();
        let index = PathIndex::build(&scratch.0, &[]);

        let mut paths = index.paths().to_vec();
        paths.sort();
        assert_eq!(
            paths,
            [
                "Cargo.toml",
                "README.md",
                "src/deep/nested.rs",
                "src/lib.rs",
                "src/main.rs"
            ],
            "files only, relative to the root; target/ ignored and dotfiles hidden"
        );
    }

    #[test]
    fn quick_open_finds_a_path_by_fuzzy_pieces() {
        let scratch = sample();
        let index = PathIndex::build(&scratch.0, &[]);

        let results = index.search("mainrs", 10);
        assert_eq!(results.first().map(String::as_str), Some("src/main.rs"));

        let nested = index.search("nested", 10);
        assert_eq!(
            nested.first().map(String::as_str),
            Some("src/deep/nested.rs")
        );
    }

    #[test]
    fn quick_open_ranks_the_closer_match_first() {
        let scratch = sample();
        let index = PathIndex::build(&scratch.0, &[]);

        let results = index.search("lib", 10);
        assert_eq!(
            results.first().map(String::as_str),
            Some("src/lib.rs"),
            "got {results:?}"
        );
    }

    #[test]
    fn an_empty_query_shows_something_rather_than_nothing() {
        let scratch = sample();
        let index = PathIndex::build(&scratch.0, &[]);

        let results = index.search("", 3);
        assert_eq!(
            results.len(),
            3,
            "quick open opens with a list, not a blank"
        );
    }

    #[test]
    fn a_query_matching_nothing_returns_nothing() {
        let scratch = sample();
        let index = PathIndex::build(&scratch.0, &[]);
        assert!(index.search("zzzznotathing", 10).is_empty());
    }

    #[test]
    fn results_resolve_back_to_openable_paths() {
        let scratch = sample();
        let index = PathIndex::build(&scratch.0, &[]);

        let result = &index.search("mainrs", 1)[0];
        let path = index.resolve(result);
        assert!(path.exists(), "{path:?} should be a real file");
    }
}
