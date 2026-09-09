# Changelog

All notable changes to this project are documented here.

## [Unreleased]

### Packaging

- **A portable Windows zip**, the second half of what PLAN.md Phase 8 asks of
  Windows. Same binary and same documents as the installer, with nothing to
  install: unpack it and run `aitch.exe`. It is built before WiX is invoked
  and needs nothing but PowerShell, so a machine without the WiX toolset can
  still produce something people can run. It is not portable in the sense of
  keeping its state beside itself — settings, sessions and recovery files live
  under `%APPDATA%` and `%LOCALAPPDATA%` exactly as the installed copy's do,
  and the release notes say so rather than implying otherwise.
- The release workflow unpacks that zip somewhere else entirely and runs the
  binary out of it, so what ships is known to start rather than merely known
  to exist.
- Both artifacts now land in `target/dist` rather than `target/wix`, which was
  a confusing home for a zip that WiX has nothing to do with.

### Fixed

- **Clicking landed in the wrong place whenever anything was to the left of
  the text.** The document is drawn past the file tree and the line-number
  gutter, but the hit test was handed the raw window x and knew about
  neither — so with `M-N` on, a click landed four columns right of where it
  was aimed, and with `M-T` open, twenty-eight. Clicking a file in the tree
  moved the text cursor instead of doing nothing. There was no hit-testing
  coverage at all; there is now, and `screen::text_origin_x` is the single
  place the document's left edge is decided, so drawing and hit-testing
  cannot drift apart again.

- **Every project-search hit but the first was unreachable.** Pressing Down to
  move through the results restarted the search underneath them: the hits were
  thrown away, the selection was clamped back to the top of a list that no
  longer had anything in it, and `Enter` then said "nothing to open". Typing
  into a project search is supposed to abandon the old search and start again
  — that is what makes it feel like search rather than like waiting for a
  build — but the arrows share the code path that notices the prompt changed,
  and moving the selection is not a change to the pattern.

  It survived Phase 6 because the one test that pressed Down only did so when
  the walker happened to return `src/lib.rs` before `src/main.rs`, which on
  this machine is about one run in ten. It showed up as a flaky test; it was
  a broken feature.

### Documentation

- **Screenshots in the README**, which PLAN.md Phase 8 calls the whole pitch:
  the editor showing Rust with the two-row nano footer under it, and the same
  file with `M-T`'s folder tree open and the footer switched to the tree's own
  keys. The second one makes the argument better than the paragraph next to it
  does — nothing was written twice for the footer to follow the focus.
- The images are **rendered by the editor, headlessly, one command each**,
  rather than captured by hand: `dump_frame` now writes a PNG when the output
  is named `.png`, and raw RGBA otherwise. A screenshot taken by hand goes
  stale the moment a colour or a footer entry changes and nobody notices.
  [`docs/screenshots.md`](docs/screenshots.md) has the commands and why those
  frames.
- `crates/aitch-harness/tests/documentation.rs` now checks that every image
  the README shows is actually in the repository, so a moved file is a failed
  test rather than a broken box on the front page.
- `png` is a dev-dependency of `aitch-ui` only. It is not new to the build:
  `arboard` already compiles the same version of it through `image`, so
  nothing extra is built and nothing extra ships.

## [0.1.0] — 2026-09-08

The first release. Everything below, from Phase 0 through Phase 7, plus a
Windows installer.

### Packaging

- **A Windows installer.** Per-user, into `%LOCALAPPDATA%\Programs\Aitch`,
  so there is no administrator prompt; puts `aitch` on the PATH so
  `EDITOR=aitch` works; ships the guide beside the binary. Built by
  `packaging/windows/build.ps1`, or by the release workflow on a tag.
- Released binaries are built with `--remap-path-prefix`, and the packaging
  script **refuses to package a binary that still carries the build
  machine's home directory**. Rust writes the absolute path of every source
  file into panic messages and debug info; before this the binary named the
  builder's home directory 639 times.

### Documentation

- [`docs/guide.md`](docs/guide.md), a guide for people who want to use the
  editor rather than read about its design: the screen, the keys, projects,
  searching, recovery and configuration.
- `crates/aitch-harness/tests/documentation.rs` checks the guide against the
  keymap — every chord it names parses and is bound to something — and
  against the list of languages that are actually highlighted. It caught the
  guide claiming `^A` selects all (it goes to the start of the line), `^V`
  pastes (it is a page down), `M-W` finds backwards (it finds forwards), and
  Go, Java and Ruby highlighting, none of which exists.
- A README section listing what the editor does *not* do yet, rather than
  leaving it to be discovered.

### Fixed

- **Typing a comment sometimes left it uncoloured.** The syntax worker drops
  superseded requests rather than queuing them, which is right, but it was
  dropping their *edits* too. Tree-sitter replays those edits against the tree
  it already has before reparsing, so a gap in the sequence left it adjusting
  a document that never existed and reusing subtrees at meaningless offsets.
  Typing fast enough to coalesce two requests — which is to say, typing —
  reproduced it about a third of the time. Coalescing now keeps every edit,
  in order.
- **The guard against a corrupting edit now exists in release builds.** All
  buffer mutation goes through `edit.rs`, which checks that an edit removes
  the text it claims to; that check was a `debug_assert`, so the binary
  people actually run would have applied a mismatched edit and corrupted the
  buffer without a word. It is a real assertion now, comparing chars rather
  than building a `String` so it does not allocate — the keystroke benchmark
  is unchanged at 1.35 µs. Found by running the suite in release profile,
  which the release workflow now does before it packages anything.

### Known limitations

- Cold start is about 400 ms against a 150 ms budget, nearly all of it GPU
  surface setup, and the process holds about 215 MB of mostly fixed
  GPU-initialisation memory.
- No soft wrap; no highlighting inside Markdown code fences; no double-click
  word selection; one window.

### Phase 7 — Configuration, sessions and recovery

Added:

- `aitchrc.toml`: theme, keymap, `tab_width`, `expand_tabs`, `line_numbers`,
  `whitespace`, extra `ignore` rules and `[font]`. Documented in
  [`docs/config.md`](docs/config.md).
- **A broken config never stops the editor opening.** A parse error, an
  unknown key or a keymap that does not exist is reported on the status line
  and the setting falls back to its default. The keys keep working: better
  the keymap you had than no keymap at all.
- The config file is watched, so saving it applies immediately — everything
  except `[font]`, which is settled when the text atlas is built and says so.
  A change to some other file in the same folder is not announced as a
  settings reload.
- `ignore` rules reach the folder tree, quick open *and* project search, all
  three of which now share one walker. Same syntax as `.gitignore`, negation
  included, and the same rule about excluded folders being pruned rather
  than walked.
- **Sessions.** Starting with no arguments reopens what was open last time at
  the cursor positions it had. A file since deleted is skipped without
  comment; naming a file, a folder or `--no-session` opens that instead.
- **Crash recovery.** Unsaved work is written a couple of seconds after
  typing stops and offered back when that file is next opened, as one
  undoable edit left unsaved so it can be compared before being kept.
  Recovery files are deleted when you quit deliberately — surviving *not*
  doing that is their whole purpose — and never deleted in the background,
  where they might be the only copy of that text.
- Command line: `--config PATH`, `--no-config`, `--no-session`,
  `+LINE[:COLUMN]`, `-h`, `-V`, and text on stdin as an unnamed buffer, so
  `git log | aitch` works and `EDITOR=aitch` behaves.

Fixed:

- Tab is now intercepted before the buffer sees it, so `expand_tabs` actually
  expands. The `Command::InsertTab` arm it used to reach was unreachable —
  the buffer answered first — which would have made the setting quietly do
  nothing.
- **Esc at the recovery prompt no longer deletes the recovery file.** The
  answer arm caught Cancel along with No, so "let me think about it" threw
  the work away — the one thing this feature exists to prevent.
- **Unsaved work in a buffer with no name is offered back.** It was written
  to a file keyed on the buffer's position, which nothing could ever match
  against a later run, and which the next run's own scratch buffer then
  deleted. Recovery files for unnamed buffers are now keyed per run, and an
  empty unnamed buffer is offered one.
- Quitting deliberately after answering "no" to save-before-quit now drops
  the recovery file, instead of offering the edits back tomorrow.
- `[font] size = 0` aborted before the window opened: a zero line height is
  an assertion failure inside the text shaper. Sizes are clamped, and the
  shaper's metrics have a floor as well.
- `tab_width` now sets how wide a tab is *drawn*, not only what the Tab key
  inserts, so a file that already contains tabs lines up as configured.
- Ignore rules containing a slash are anchored at the project root. The tree
  anchored them at whichever folder was being expanded, so `src/generated`
  became `src/src/generated` there and matched nothing, while quick open and
  project search excluded it.
- `echo hi | aitch notes.txt` opens `notes.txt`. The pipe was read first and
  silently replaced the file, `+LINE` and all — and a pipe left open by a
  script held the editor closed.
- Restoring a session no longer lands on the wrong buffer when one of the
  files has been deleted since; `--config` reports a path that is not there;
  a bad keymap name in a live reload reports the problem instead of
  "settings reloaded"; `+LINE` scrolls to the line rather than leaving the
  cursor off-screen; the recovery prompt shows a date instead of epoch
  seconds; and `stable_hash` uses the actual FNV-1a prime, which had an
  extra digit in it.

The recovery timer is the one thing that could have cost idle CPU, so it is
armed only while something is unsaved and stood down the moment it is saved.
Idle remains 0.000 s.

### Phase 6 — Project-wide search

Added:

- `M-^W` (`Ctrl+Shift+F` in the modern profile) searches every file in the
  folder, using ripgrep's own machinery — `ignore` to walk, `grep-regex` to
  match, `grep-searcher` to read — rather than shelling out to `rg`.
- Results stream into the pane above the prompt line as they are found, with
  `file:line: text` per hit. Enter opens the file at that line.
- Typing another character abandons the running search and starts again, which
  is what makes it feel like search rather than like waiting for a build.
- Smart case, as ripgrep has it: an all-lowercase pattern ignores case, one
  with a capital in it means the capital.
- Literal by default; a pattern is only a regular expression when asked. So
  searching for `foo(bar)` finds `foo(bar)`.
- `^\` from the results starts a **project-wide replace**: it asks what to put
  in place, works out the whole plan, and shows how many occurrences in how
  many files before writing anything.

Measured, on an 868 MB tree of 37,677 files:

| | measured | PLAN.md Phase 6 budget |
|---|---|---|
| **First results visible** | **5.5 ms** | under 200 ms ✓ |
| Whole tree searched, no matches | 748 ms | |
| Rare pattern, 43 hits | 66 ms to first, 432 ms done | |
| UI thread blocked | never — searching is on its own threads | ✓ |

Decisions:

- **Nothing is written until the whole plan is built.** A file that cannot be
  read or re-encoded stops a project replace before it has changed anything,
  which is what "all or nothing" has to mean if it means anything. Across
  files the write phase is a sequence rather than a transaction: each file is
  written atomically, and a failure part-way says how many were done rather
  than pretending otherwise.
- **A replace goes through `fileio`.** A UTF-16 file with CRLF endings stays
  one. A project-wide replace that quietly rewrote a tree as UTF-8 LF would be
  a far worse bug than whatever it was asked to fix, and there is a test for
  exactly that.
- Hits are capped at 2,000 and the status line says when the cap was reached.
  A search matching half a tree needs narrowing, not more memory.
- Wake-ups are batched. Ten thousand hits must not wake the event loop ten
  thousand times.
- Patterns shorter than three characters are not searched for: they match most
  of a tree and say nothing.
- A project search lives in the `search` context, where the arrows already
  walk results and `^\` is already Replace.

Not done:

- Replace is literal only. A regex replace with capture groups is a different
  feature with its own ways to go wrong, and this one has to be trustworthy
  first.
- Soft wrap, double-click and triple-click selection, and the Phase 1 cold
  start and idle-memory budgets, all still carried forward.

### Phase 5 — Syntax highlighting

Added:

- tree-sitter parsing with incremental reparse, **off the UI thread**. The
  editor hands the worker a rope snapshot — free, because a rope clone shares
  its structure — and carries on drawing. Colour arrives when it arrives.
- Grammars for all thirteen languages PLAN.md lists: Rust, C, C++, Python,
  JavaScript, TypeScript, JSON, TOML, YAML, Markdown, shell, HTML and CSS.
- Two complete themes, one dark and one light, with a test that every syntax
  colour clears 3:1 contrast against its own background.
- Bracket matching that skips brackets inside strings and comments, which is
  what having a syntax tree is worth.
- The current line, line numbers (`M-N`) and whitespace (`M-P`). Whitespace
  marks tabs and *trailing* spaces only; marking every space makes prose
  unreadable, and the ones that matter are the invisible ones.

Measured, on a 10,000-line Rust file:

| | measured | PLAN.md Phase 5 budget |
|---|---|---|
| **Keystroke cost on the UI thread** | **1.2 µs** | under 16 ms ✓ |
| Reparse after a keystroke (worker) | 1.16 ms | keeps up with typing |
| Query one screen of spans (worker) | 172 µs | |
| First parse of the file (worker) | 70 ms | once, at open |

The acceptance criterion is the first row. It is 1.2 µs rather than
milliseconds because the frame never waits for a parse: the design decision
carries the budget, not the speed of the parser.

Decisions:

- **Stale parses are dropped, not queued.** Typing faster than the parser runs
  would otherwise build a backlog of answers about documents three keystrokes
  old. The worker takes the newest request and discards the rest.
- **The token set is deliberately small.** Grammars disagree about detail —
  `@variable.parameter.builtin` in one, `@parameter` in another — so captures
  are matched on their leading component. An unfamiliar refinement lands on the
  general token rather than falling through to unstyled text.
- Some grammars ship only what their language *adds* to another. C++'s query is
  939 bytes of C++-isms that expect C's 1432 bytes underneath; alone it
  highlights ordinary C++ not at all. Those are concatenated, which is what
  `inherits:` means elsewhere in the tree-sitter world. TypeScript inherits
  JavaScript the same way.
- `tree-sitter-toml` is stuck at 0.20, before grammars exported a version-
  independent `LanguageFn`, so it cannot link against a modern tree-sitter.
  `tree-sitter-toml-ng` is used instead.

Not done:

- **Soft wrap**, deferred by agreement to its own phase. One buffer line is
  still exactly one screen row, and the viewport, cursor movement, hit testing
  and scrolling all rely on that; changing it is a redesign rather than a
  toggle, and half-doing it would put the cursor in the wrong places.
- Markdown highlights block structure only. Emphasis and links inside a
  paragraph need the inline grammar as an injection.
- Double-click word and triple-click line selection, still owed from Phase 2.

### Phase 4 — Folder mode

nano has no equivalent, so this phase is invention. Kept keyboard-first and
quiet, and with no tab bar: PLAN.md §5 settles that, and the buffer list goes
on the prompt line where every other list already is.

Added:

- `aitch some/folder` opens a workspace. `M-T` shows the sidebar and gives it
  the keys; `M-T` again from inside puts it away, while `Escape` just hands
  the keys back. Folders fold open and shut with Enter; files open.
- `^T` quick open: fuzzy path matching over the whole folder, with the results
  listed above the prompt line. The arrows move through that list rather than
  through past answers, because a list on screen is what arrows are obviously
  for.
- Several buffers at once: `M-,` and `M-.` cycle, `M-B` lists them on the
  prompt line with a marker on the ones that need saving.
- `.gitignore` handling from ripgrep's `ignore` crate, so `target/` and its
  like never appear in the tree or in quick open.
- A `notify` watcher on the folder, debounced, so the sidebar keeps up with a
  build or a branch switch without being asked.
- **Never silently overwrite.** A document remembers when it last read or wrote
  its file. Saving over a file that something else changed asks first, and a
  file that has been deleted counts as changed — writing it back would recreate
  something someone removed on purpose.

Measured:

| | measured | PLAN.md §5 budget |
|---|---|---|
| Open a folder (`Tree::new`) | 1.7 ms | sidebar under 500 ms ✓ |
| Quick-open filter, 80k paths | 2.4–7.9 ms | no perceptible lag ✓ |
| Index 35k files | 128 ms | one-off, on first `^T` |
| Idle CPU with a watcher running | 0.000 s over 5 s | 0% ✓ |

The tree reads one directory per expansion rather than walking the whole
folder, which is why its cost does not depend on the size of the checkout.
The index walk started at 3.6 seconds single-threaded; ripgrep's parallel
walker took it to 128 ms for the same 34,725 files.

Not done:

- Reload-on-change is a report, not a prompt: the status line says the file
  moved, and `^O` asks before overwriting. Offering to re-read it needs a
  question the prompt line can ask, which is a small addition rather than a
  missing guarantee — nothing is lost either way.
- Double-click word and triple-click line selection, still owed from Phase 2.
- Search is literal, not regex; that arrives with Phase 6.

### Phase 3 — The nano personality

This is the phase that makes it this editor rather than a generic one.

Added:

- **The footer.** Two rows, generated from the active keymap for the current
  context, laid out column-major so `^G Help` sits above `^X Exit` the way
  nano has it. A narrow window drops whole columns of the lowest-priority
  entries, and the column width is measured over the cells actually shown —
  sizing to an entry that gets dropped would waste width and drop more.
- **The status line**: filename, modified marker, cursor position on `^C`,
  and transient messages that expire on the next keystroke rather than on a
  timer, which is both what nano does and what a headless test can assert on.
- **The prompt line**: one line above the footer for save-as, goto, search,
  replace and the yes/no questions, each with its own footer row of keys and
  its own history walked with the arrows. A prompt replaces the status line
  rather than stacking on it — three rows of chrome is the limit.
- **`^W` incremental search**: the match is found and highlighted as the term
  is typed, wraps around the end and says so, and cancelling puts the cursor
  back where it started. `M-W` repeats without a prompt.
- **`^\` replace** with per-match `y`/`n`/`a` confirmation.
- **`^_` goto line**, taking `line` or `line:column`, counting from one.
- **`^G` help**: a scrollable pane generated from the keymap, so it cannot
  drift from what the keys actually do. Not a dialog — the footer stays put
  underneath and says how to leave.
- **`^R` insert file** at the cursor, and `^O` save-as when there is no name.
- `editor.rs`, the session that owns document, viewport, keymap, context,
  prompt and status. The UI shrank to the window: pixels, pointer, clipboard
  and fractional scrolling.
- `render/screen.rs` lays out a frame without needing a window, so the
  offscreen tests and `dump_frame` draw through the same code the window does.

Changed:

- At a search prompt the arrows now walk past searches rather than stepping
  between matches, as nano's do; `M-W` and `M-Q` step between matches.
- `^X` on a modified buffer asks "Save modified buffer?" on the prompt line.
  Phase 2 had to put that warning in the window title for want of anywhere to
  ask; the title is now just a title again.

Acceptance:

- Every labelled footer command has a harness test that presses the chord a
  nano user would press and asserts on what would be on screen. A guard test
  compares the footer against the list those tests cover, so a new entry fails
  until someone writes one.
- Nine offscreen render tests, including that the footer reaches the
  framebuffer and that a prompt puts no ink below the three chrome rows.

Not done:

- Search is literal, not regex. `search.rs` is named for regex in PLAN.md §3;
  the regex engine arrives with ripgrep's machinery in Phase 6.
- Double-click word and triple-click line selection, still.

### Phase 2 — Actually editing

Added:

- `edit.rs`: one primitive for every mutation — replace a run of text at a
  character index with another. Insert, delete and replace are the same
  operation with one side empty, so the inverse of an edit is the edit with
  its sides swapped. The only file that calls a mutating ropey method.
- `history.rs`: undo and redo with coalescing. A run of typed characters is one
  step; a run of backspaces is another; a newline ends a run from both sides.
  Dirtiness is undo depth against the depth at the last save, so undoing back
  to a saved state reads as clean — and a save point stranded in the redo
  stack stops counting.
- `fileio.rs`: encoding detection (BOM, UTF-16 LE/BE, UTF-8, Latin-1 fallback),
  byte-exact round-trip, and atomic save via a temporary file and a rename.
- `line_ending.rs`: which ending a file mostly uses. Existing breaks are stored
  verbatim and never rewritten; only a newly typed newline consults this.
- `document.rs`: a buffer plus the path and encoding a save needs.
- Selection: nano's mark (`^6` / `M-A` then move) and shift+arrows, drawn as a
  translucent band behind the text. Mouse press-and-drag selects.
- Cut and uncut with nano's accumulate-on-consecutive-cuts behavior, and the
  system clipboard via `arboard`.
- `aitch-harness` now runs commands against a real buffer, so a chord sequence
  can be asserted on end to end.
- 15 golden-file fixtures and a round-trip suite over all of them.

Decisions:

- **Encoding detection is hand-rolled, with no new dependency.** The supported
  set is the one PLAN.md names, and it refuses to guess beyond it: a wrong
  charset guess would be written back as fact on the next save. `chardetng`
  would add legacy codepages nobody asked for and that failure mode with them.
- **A mixed-ending file stays mixed.** Line breaks are stored as read. The
  alternative — normalize in memory, write the dominant ending back — rewrites
  lines the user never touched and fails the byte-identical criterion.
- **`Command` grew two variants**, both agreed before writing them:
  `InsertText(String)` so typed text reaches the buffer as a command rather
  than by the UI reaching in, and `Select(Box<Command>)` so one wrapper covers
  shift+every-movement instead of a dozen `select-*` twins.
- The cursor can no longer land between the CR and the LF of a CRLF. Ropey
  counts the pair as one break, so a cursor inside it has a column past the end
  of its own line — a Phase 1 bug that only CRLF files would have shown.
- `document.rs` is not in PLAN.md §3's module list. Phase 2 needs somewhere to
  hold the path and encoding a save depends on; Phase 4's `workspace.rs` will
  own a set of these.

Acceptance:

- Load, one edit, save is byte-identical except the edit, across 15 fixtures:
  LF, CRLF, CR and mixed endings; UTF-8 with and without BOM; UTF-16 LE and BE
  with and without BOM; UTF-16 with a surrogate pair; UTF-16 with CRLF inside
  it; Latin-1; a file with no trailing newline; an empty file.
- Saving an untouched file rewrites zero bytes.
- A Latin-1 file that gains a character Latin-1 cannot hold refuses to save
  and leaves the original intact, rather than writing a `?` over the data.

Testing:

- The offscreen render tests had been skipping on Linux since Phase 1. A test
  that skips for want of a GPU still reports "ok", and libtest hides the
  message saying why; the only visible sign was that all five finished in
  0.01s on Ubuntu against 3.53s on Windows. CI now installs lavapipe and sets
  `AITCH_REQUIRE_GPU`, which turns a missing adapter into a failure, so the
  render path cannot quietly lose a platform again. Both runners now enumerate
  a device and run the tests for real.

Not done:

- The exit prompt has nowhere to live: the prompt line is Phase 3. Quitting a
  modified buffer arms a confirmation and puts the warning in the window
  title, which is also carrying the modified marker and transient messages
  until Phase 3 builds a status line.
- Saving a buffer with no filename needs a prompt to ask for one, so it
  reports "this buffer has no filename yet" instead.
- Double-click word and triple-click line selection.
- Tab still inserts a literal tab; tab width and expand-tabs are Phase 7.

### Phase 1 — Text on screen

Added:

- `Buffer`: a rope, a cursor with a goal column, and movement over both —
  by character, word, line, screen and file. Nothing mutates the text yet.
- `Viewport`: the visible window in whole lines, and the least-movement scroll
  that keeps the cursor in it.
- cosmic-text integration: monospace discovery, shaping, and a glyph atlas
  packed on first sight of each glyph. Color emoji and mask glyphs share one
  `Rgba8UnormSrgb` atlas, one sampler and one pipeline.
- A single quad pipeline for everything drawn: glyphs sample the atlas, solid
  fills sample its opaque texel. Instances are packed by hand into a byte
  buffer, so no casting crate is needed.
- Input: winit key events to chords, resolved through the active keymap. C0
  control characters are unmapped, so `^\` and `^_` arrive as the keymap
  spells them. `M-M` switches profiles for real.
- Scrolling by wheel and touchpad in real pixels, with a sub-line offset, so a
  fling is smooth rather than a line-at-a-time ratchet. Click to place the
  cursor, hit-tested through the shaped layout.
- DPI changes re-derive the metrics and drop the glyph atlas; cache keys carry
  the physical font size, so nothing stale is ever sampled.
- An offscreen render test: draw to a texture, read the pixels back, and assert
  that glyphs and the cursor actually arrived.
- `examples/dump_frame`: render a frame headlessly to raw RGBA, for looking at.
- `benches/buffer.rs`: load, visible-window reads, and movement.

Renamed:

- The editor is now **Aitch** (binary `aitch`), and the crates with it.

Measured, on a 50 MB / 685k-line log:

| | measured | PLAN.md §6 budget |
|---|---|---|
| Load 50 MB into the rope | 35 ms | — |
| Memory for the buffer | 1.5× file size | < 2× ✓ |
| Visible-window read, start vs end of file | 14 µs vs 19 µs | flat ✓ |
| Jump to end of file | 7 ns | — |
| Idle CPU | 0.016 s over 5 s | 0% ✓ |
| Cold start to window | 412 ms | < 150 ms ✗ |
| Process memory, no file open | 215 MB | — ✗ |

The two misses are both fixed GPU-initialization cost, not per-file cost:
`FontSystem` accounts for under 7 MB and the rope scales as budgeted.
Restricting wgpu to primary backends was tried and changed nothing (412 ms vs
421 ms), so it was reverted rather than kept for a cost it did not pay.

Not done:

- Editing, selection, undo and the clipboard — all Phase 2.
- Grapheme-cluster cursor movement: columns count `char`s for now, so the
  cursor steps through the halves of a family emoji.
- The bench budgets are measured but not enforced; a CI gate needs a stored
  baseline.

### Phase 0 — Skeleton

Added:

- Cargo workspace: `aitch-core`, `aitch-ui`, `aitch-harness` and the `aitch`
  binary.
- `aitch-ui` opens a winit window with a wgpu surface, clears it, handles resize
  and DPI scale changes. Redraw is event-driven only — measured idle CPU is 0.
- `Command`, the editor's whole API surface, with kebab-case names for keymaps.
- `keymap.rs`: chord grammar, TOML profile parsing, context-scoped resolution,
  and footer generation from binding labels and priorities.
- `keymaps/nano.toml` and `keymaps/modern.toml`, both covering the same command
  set — a unit test fails if one profile drops a command the other binds.
- `aitch-harness`: feed a chord sequence, assert on the commands and the footer.
- GitHub Actions: fmt, clippy, test and release build on Windows and Ubuntu.
- `docs/keymap.md`, `CLAUDE.md`, MIT license.

Decisions:

- Chord keys are **logical, not physical**. Shift is significant for letters and
  named keys, and dropped for other characters. This is what makes `^_` work on
  any layout and `Ctrl+Shift+F` a chord distinct from `^F`. See `docs/keymap.md`.
- Footer metadata (`label`, `priority`) and `context` are part of the keymap
  schema from the start, so Phase 3's footer reflow needs no parser change.
- The nano profile keeps `M-W` for repeat-search, as nano has it, and puts
  project search on `M-^W` instead of the `M-W` the plan suggested.
- `pollster` is a dependency outside the plan's stack table: wgpu's adapter and
  device requests are async and the event loop is not.
- Licensed MIT.
