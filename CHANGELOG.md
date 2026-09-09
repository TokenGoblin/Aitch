# Changelog

All notable changes to this project are documented here.

## [Unreleased]

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
