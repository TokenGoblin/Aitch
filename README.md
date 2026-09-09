# Aitch

A clean and simple text editor with file browsing — nano's interaction model,
VS Code's project model.

Modeless: you open it and you type. The footer is the UI — two rows of
context-sensitive shortcuts, always visible, generated from the active keymap
rather than hardcoded. Prompts happen on a line above the footer. No modal
dialogs, no floating windows.

**Status: Phase 1.** Text renders and you can move around it. Editing is Phase
2, the footer Phase 3, folder mode Phase 4. See [`PLAN.md`](PLAN.md).

## Build and run

Needs a stable Rust toolchain.

```
cargo test --workspace
cargo run -p aitch -- some-file.txt
```

Arrow keys, Home/End, PgUp/PgDn, Ctrl+arrows for words, Ctrl+Home/End for the
file. Click to place the cursor, scroll with the wheel or a touchpad.

## Layout

```
crates/
  aitch-core/     rope, cursor, commands, keymap  — zero GUI dependencies
  aitch-ui/       winit + wgpu + cosmic-text
  aitch-harness/  headless driver: feed chords, assert on state
  aitch/          the binary: argument parsing and wiring
keymaps/          nano.toml (default) and modern.toml
```

`aitch-core` has no GUI dependencies, which is what lets nearly all of this be
tested without a window. The UI never mutates a buffer: it resolves input to a
`Command` and hands it over.

Only the lines on screen are ever laid out, so a 50 MB log costs the same per
frame as a 50 line one.

## Keymaps

Two profiles ship, both just data:

- **`nano`** (default) — `^X` exits, `^O` writes, `^W` finds, `^K`/`^U` cut and
  uncut, `M-U`/`M-E` undo and redo.
- **`modern`** — `^S` saves, `^F` finds, `^C`/`^X`/`^V` are the clipboard,
  `^Z`/`^Y` undo and redo, `^Q` quits.

`M-M` switches between them. Writing your own is the same format:
[`docs/keymap.md`](docs/keymap.md).

## Testing

```
cargo test --workspace                  # unit, harness and offscreen render tests
cargo bench -p aitch-core               # the PLAN.md §6 budgets
cargo run -p aitch-ui --example dump_frame -- file.rs frame.raw
```

`dump_frame` renders a frame headlessly and writes raw RGBA, so the one
remaining manual step — looking at the text — needs no display.

## Not goals

Extensions, LSP, a debugger, an integrated terminal, a git UI, a minimap, a tab
bar, a settings GUI. If a feature would need a third footer row or a floating
window, it does not belong here.

## License

MIT — see [`LICENSE`](LICENSE).
