# Aitch

A clean and simple text editor with file browsing — nano's interaction model,
VS Code's project model.

Modeless: you open it and you type. The footer is the UI — two rows of
context-sensitive shortcuts, always visible, generated from the active keymap
rather than hardcoded. Prompts happen on a line above the footer. No modal
dialogs, no floating windows.

**Status: Phase 6.** It looks and behaves like nano, opens folders, highlights
thirteen languages, and searches a whole tree. Configuration is Phase 7,
packaging Phase 8. See [`PLAN.md`](PLAN.md).

## Build and run

Needs a stable Rust toolchain.

```
cargo test --workspace
cargo run -p aitch -- some-file.txt
cargo run -p aitch -- some/folder
```

Type. Arrow keys, Home/End, PgUp/PgDn, Ctrl+arrows for words, Ctrl+Home/End
for the file. Shift with any of those selects, or `^6` sets a mark the way
nano does. `^K` cuts a line and `^U` puts it back, `^O` writes, `^X` exits,
`M-U` and `M-E` undo and redo.

`^W` searches as you type and wraps around; `^\` replaces with `y`/`n`/`a`
confirmation; `^_` goes to a line; `^G` opens help. Everything a nano user
already knows is on the footer, and the footer is generated from the keymap —
so it stays true in both profiles and in one you write yourself.

Open a folder and `M-T` shows the tree, `^T` finds a file by typing part of
its name, and `M-,` / `M-.` / `M-B` move between open buffers. There is no tab
bar, deliberately — the buffer list lives on the prompt line with everything
else. `.gitignore` is respected throughout.

Syntax highlighting comes from tree-sitter and runs on its own thread, so a
keystroke costs 1.2 µs of the frame however large the file. `M-N` shows line
numbers and `M-P` shows tabs and trailing spaces.

`M-^W` searches every file in the folder, streaming results as they are found
— 5.5 ms to the first hit on an 868 MB tree — and `^\` from there replaces
across the whole project, showing you the plan before it writes anything.

Files keep the encoding and line endings they arrived with. Open a UTF-16 file
with CRLF endings, change one word, save, and only that word differs.

## Layout

```
crates/
  aitch-core/     rope, edits, undo, search, syntax, keymap, footer, folder,
                  session — no GUI dependencies
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
cargo test --workspace                  # unit, golden-file, harness, render
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
