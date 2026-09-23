# Changelog

All notable changes to this project are documented here.

## [Unreleased]

## [0.1.0] — 2026-09-22

First release. A GUI text editor with nano's interaction model and VS Code's
project model, for Windows 10 and later.

### The editor

- **Modeless, and the footer is the UI.** Two rows of context-sensitive
  shortcuts, always visible, **generated from the active keymap rather than
  hardcoded** — so they stay true in `nano.toml`, in `modern.toml`, and in a
  keymap you write yourself. Prompts happen on a line above the footer. No
  modal dialogs, no floating windows.
- **Editing.** Arrow keys, Home/End, PgUp/PgDn, Ctrl+arrows by word,
  Ctrl+Home/End for the file; Shift with any of those selects, or `^6` sets a
  mark the way nano does. `^K` cuts, `^U` pastes, `^O` writes, `^X` exits,
  `M-U`/`M-E` undo and redo.
- **Search.** `^W` searches as you type and wraps; `^\` replaces with
  `y`/`n`/`a` confirmation; `^_` goes to a line; `^G` opens help.
- **Folders.** `M-T` opens the tree, `^T` finds a file by typing part of its
  name, `M-,`/`M-.`/`M-B` move between buffers. No tab bar, deliberately.
  `.gitignore` is respected throughout.
- **Project-wide search.** `M-^W` streams results as they are found — 5.5 ms
  to the first hit on an 868 MB tree — and `^\` from there replaces across the
  project, showing the plan before it writes anything.
- **Syntax highlighting** for thirteen languages (Rust, C, C++, Python,
  JavaScript, TypeScript, Bash, HTML, CSS, JSON, TOML, YAML, Markdown),
  hand-written per language and run on its own thread: a keystroke costs
  1.2 µs of the frame however large the file.
- **Files keep what they arrived with.** Open a UTF-16 file with CRLF
  endings, change one word, save, and only that word differs.
- **Sessions and recovery.** Starting with no file reopens what was open last
  time, at the cursor positions it had. Unsaved work survives a crash: it is
  offered back the next time that file is opened, and never deleted where it
  might be the only copy.
- **Configuration.** One optional TOML file — theme, keymap, tab width, line
  numbers, whitespace, ignore rules, font — applied the moment it is saved, no
  restart. A broken config never stops the editor opening; the problem goes on
  the status line and everything falls back to its default.
- **The title bar follows the theme**, via DWM, so the window is one surface
  rather than the editor with a strip of the shell on top.

### No dependencies

`cargo tree --workspace` shows the four workspace crates and nothing else.
Not "few dependencies" — none. The window, the GDI surface, the TrueType
parser and rasteriser, the clipboard, the rope, the regex engine, the TOML
parser, the directory walker, the `.gitignore` matcher, the fuzzy matcher and
every syntax highlighter are written here, behind the same architectural
boundaries a third-party crate sat behind.

`scripts/check-dependency-budget.ps1` fails CI if `Cargo.lock` ever grows past
the count in `dependency-budget.txt`, so this is enforced rather than
intended. See [`PLAN-ZERO-DEP.md`](PLAN-ZERO-DEP.md) for what each
hand-written replacement does and does not do relative to what it replaced.

### Architecture

- `aitch-core` — rope, edits, undo, search, syntax, keymap, footer, folder,
  config, session, recovery. **No GUI dependencies**, which is what lets
  nearly all of it be tested without a window.
- `aitch-ui` — Win32 window, GDI surface, TrueType rasteriser, clipboard.
  Never mutates a buffer: it resolves input to a `Command` and hands it over.
- `aitch-harness` — headless driver: feed chords, assert on state.
- `aitch` — the binary: argument parsing and wiring.

959 tests, including a documentation suite that fails the build if the guide
names a key that is not bound or claims a language that is not highlighted.

### Windows only

The window, the text rasteriser and the clipboard are written directly against
Win32 and GDI, so there is no Linux or macOS build — not packaging still to be
done, but a backend that does not exist. A second one could be written against
the same interfaces in `aitch-ui/src/platform/`; nothing above that layer would
have to change.

### Known limitations

Only the lines on screen are ever laid out, so a 50 MB log costs the same per
frame as a 50 line one — but a single line longer than a few hundred thousand
characters will still be slow to move through. See the README for the current
list.
