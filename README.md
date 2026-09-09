# Aitch

A clean and simple text editor with file browsing — nano's interaction model,
VS Code's project model.

![The editor: Rust source with syntax highlighting, the filename on the status
line, and nano's two-row shortcut footer beneath it](docs/images/editor.png)

Modeless: you open it and you type. The footer is the UI — two rows of
context-sensitive shortcuts, always visible, generated from the active keymap
rather than hardcoded. Prompts happen on a line above the footer. No modal
dialogs, no floating windows.

`M-T` opens the folder. The footer follows the focus — these are the tree's
keys, and nothing had to be written twice for that to happen:

![The same file with the folder tree open on the left, and a footer showing the
tree's own shortcuts](docs/images/tree.png)

Both images are rendered by the editor itself, headlessly, by one command each:
[`docs/screenshots.md`](docs/screenshots.md).

**Status: 0.1.0.** It looks and behaves like nano, opens folders, highlights
thirteen languages, searches a whole tree, reads a config file that applies as
you save it, and keeps your work when it does not shut down cleanly. There is
a Windows installer. See [`PLAN.md`](PLAN.md) for what is built and what is
not, and [known limitations](#known-limitations) below.

**New to it? [`docs/guide.md`](docs/guide.md) is the guide** — everything you
need in the order you need it.

## Install

**Windows.** Download the `.msi` from the
[latest release](https://github.com/TokenGoblin/Aitch/releases/latest) and run
it. It installs per-user into `%LOCALAPPDATA%\Programs\Aitch`, so there is no
administrator prompt, and puts `aitch` on your PATH.

There is a `.zip` beside it with the same binary and nothing to install —
unpack it and run `aitch.exe`. It is not on your PATH and has no Start menu
entry; settings, sessions and recovery files still live under `%APPDATA%` and
`%LOCALAPPDATA%` exactly as the installed copy's do.

**Anywhere else,** and to build it yourself — needs a stable Rust toolchain,
1.85 or newer:

```
cargo install --path crates/aitch
```

Linux and macOS packages are not built yet — no AppImage, no `.deb`, no
Flatpak. The editor itself builds, tests and runs on Linux, and CI does all
three on every push; only the packaging is missing.

## Build and run

```
cargo test --workspace
cargo run -p aitch -- some-file.txt
cargo run -p aitch -- some/folder
```

To build the Windows installer, with the [WiX toolset](https://wixtoolset.org)
installed as a dotnet tool:

```
dotnet tool install --global wix --version 5.0.2
./packaging/windows/build.ps1
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

Starting with no file reopens what was open last time, at the cursor positions
it had. Unsaved work survives a crash: it is offered back the next time that
file is opened, and never deleted where it might be the only copy.

## Configuring

One optional TOML file — theme, keymap, tab width, line numbers, whitespace,
extra ignore rules, font — that applies the moment you save it, no restart and
no reload command. A broken config never stops the editor opening; the problem
goes on the status line and everything falls back to its default.

```toml
theme = "dark"
keymap = "nano"
tab_width = 4
expand_tabs = false
ignore = ["target", "node_modules"]
```

`aitch --no-config` ignores it, `--config PATH` reads somewhere else, and
`--no-session` starts empty. `aitch +42:8 notes.txt` opens at a position, and
`git log | aitch` opens a pipe. The full set is in
[`docs/config.md`](docs/config.md).

## Layout

```
crates/
  aitch-core/     rope, edits, undo, search, syntax, keymap, footer, folder,
                  config, session, recovery — no GUI dependencies
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

## Documentation

| | |
|---|---|
| [`docs/guide.md`](docs/guide.md) | How to use it, start to finish |
| [`docs/config.md`](docs/config.md) | Every setting, and where the file goes |
| [`docs/keymap.md`](docs/keymap.md) | Writing your own keymap |
| [`docs/screenshots.md`](docs/screenshots.md) | Regenerating the images above |
| [`CHANGELOG.md`](CHANGELOG.md) | What changed, and what it measured |
| [`PLAN.md`](PLAN.md) | The design, and the budgets it is held to |

## Testing

```
cargo test --workspace                  # unit, golden-file, harness, render
cargo bench -p aitch-core               # the PLAN.md §6 budgets
cargo run -p aitch-ui --example dump_frame -- file.rs frame.png
```

`dump_frame` renders a frame headlessly — no window, no display — so the one
remaining manual step, looking at the text, is a picture you can open. It
writes a PNG or the raw RGBA the render tests compare against, chosen on the
file extension.

## Known limitations

Honest about what it does not do yet:

- **Cold start is about 400 ms**, against a 150 ms budget. Setting up the GPU
  surface is nearly all of it.
- **It holds about 215 MB** with a file open, almost all of it fixed cost from
  GPU initialisation rather than anything to do with the file. A 50 MB log
  adds around 75 MB on top of that.
- **No soft wrap.** Long lines scroll sideways.
- **Highlighting inside Markdown code fences** is not done — the fence is
  highlighted, its contents are not.
- **One window.** Several buffers, no second window.
- **No double-click to select a word** and no triple-click for a line. Click
  to place the cursor and drag to select both work, as do the wheel and a
  touchpad's kinetic scrolling; the file tree is keyboard-only.
- **Packaged for Windows only.** See [`PLAN.md`](PLAN.md) Phase 8.

## Not goals

Extensions, LSP, a debugger, an integrated terminal, a git UI, a minimap, a tab
bar, a settings GUI. If a feature would need a third footer row or a floating
window, it does not belong here.

## License

MIT — see [`LICENSE`](LICENSE).
