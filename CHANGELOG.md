# Changelog

All notable changes to this project are documented here.

## [Unreleased]

### Phase 0 — Skeleton

Added:

- Cargo workspace: `nib-core`, `nib-ui`, `nib-harness` and the `nib` binary.
- `nib-ui` opens a winit window with a wgpu surface, clears it, handles resize
  and DPI scale changes. Redraw is event-driven only — measured idle CPU is 0.
- `Command`, the editor's whole API surface, with kebab-case names for keymaps.
- `keymap.rs`: chord grammar, TOML profile parsing, context-scoped resolution,
  and footer generation from binding labels and priorities.
- `keymaps/nano.toml` and `keymaps/modern.toml`, both covering the same command
  set — a unit test fails if one profile drops a command the other binds.
- `nib-harness`: feed a chord sequence, assert on the commands and the footer.
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

Not done:

- Nothing is wired to behavior. Keys do not edit anything yet.
- CI does not open a window (no display, no GPU on the runners). The window is
  a manual check on both platforms.
