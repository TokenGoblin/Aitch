# Changelog

All notable changes to this project are documented here.

## [Unreleased]

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
