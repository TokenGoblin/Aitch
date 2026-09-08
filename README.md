# nib

A GUI text editor with nano's interaction model and VS Code's project model.

Modeless — you open it and you type. The footer is the UI: two rows of
context-sensitive shortcuts, always visible, generated from the active keymap
rather than hardcoded. Prompts happen on a line above the footer. No modal
dialogs, no floating windows.

**Status: Phase 0.** The window opens and clears. The keymap data model is
built and tested; nothing is wired to behavior yet. See [`PLAN.md`](PLAN.md)
for the phases.

## Layout

```
crates/
  nib-core/     rope, edits, history, commands, keymap, search, workspace, IO
  nib-ui/       winit + wgpu + (from Phase 1) cosmic-text
  nib-harness/  headless driver: feed chords, assert on state
  nib/          the binary: argument parsing and wiring
keymaps/        nano.toml (default) and modern.toml
```

`nib-core` has zero GUI dependencies, which is what lets almost all of this be
tested without a window. The UI never mutates a buffer: it resolves input to a
`Command` and hands it to core.

## Keymaps

Two profiles ship, both just data:

- **`nano`** (default) — `^X` exits, `^O` writes, `^W` finds, `^K`/`^U` cut and
  uncut, `M-U`/`M-E` undo and redo.
- **`modern`** — `^S` saves, `^F` finds, `^C`/`^X`/`^V` are the clipboard,
  `^Z`/`^Y` undo and redo, `^Q` quits.

`M-M` switches between them. Writing your own is the same format:
[`docs/keymap.md`](docs/keymap.md).

## Build

Needs a stable Rust toolchain.

```
cargo test --workspace
cargo run
```

## Not goals

Extensions, LSP, a debugger, an integrated terminal, a git UI, a minimap, a tab
bar, a settings GUI. If a feature would need a third footer row or a floating
window, it does not belong here.

## License

MIT — see [`LICENSE`](LICENSE).
