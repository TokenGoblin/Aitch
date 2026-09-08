# Project Plan — A Graphical nano

**Working name:** `nib` (placeholder — `pith`, `quill`, `slate` also free-ish)
**Target platforms:** Windows 10+, Linux (X11 + Wayland). macOS possible later, not a phase-1 constraint.
**Language:** Rust
**Built with:** Claude Code, phase by phase

---

## 1. What this is

A GUI text editor with nano's interaction model and VS Code's project model.

From nano:
- **Modeless.** You open it and you type. No modes, no hidden state, no "you are now in a mode where j moves down."
- **The footer is the UI.** A persistent two-row shortcut list at the bottom of the window, always visible, context-sensitive. This is the single most recognizable thing about nano and it must be preserved exactly.
- **Prompts happen at the bottom.** Filename entry, search terms, replace confirmations — all on a prompt line above the footer. No modal dialogs. No floating popups.
- **Nothing is discoverable only by memory.** If a command exists in the current context, it's either on the footer or one keypress from a footer entry.

From VS Code:
- **Open a folder, not just a file.** A file tree sidebar, multiple open buffers, project-wide search, fuzzy file open.

Explicitly *not* goals: extensions/plugins, LSP, debugger, integrated terminal, git UI, AI features, settings GUI. This is a fast, quiet, keyboard-first editor. Keep it that way.

**Design rule to apply when in doubt:** if a feature would require adding a third row to the footer or a floating window, it probably doesn't belong.

---

## 2. Stack

| Layer | Choice | Why |
|---|---|---|
| Window / input | `winit` | The standard. Wayland + X11 + Win32 in one API. |
| GPU surface | `wgpu` | Same reasoning as the browser project; one backend for D3D12 and Vulkan. |
| Text shaping + layout | `cosmic-text` | Does shaping (rustybuzz), rasterization (swash), font discovery (fontdb), and line layout. This is the piece you do *not* want to write yourself. |
| Text storage | `ropey` | Rope. O(log n) edits, cheap line indexing, handles a 1 GB file. |
| File watching | `notify` | Cross-platform, debounced. |
| Ignore rules | `ignore` | Ripgrep's crate. `.gitignore` handling for free. |
| Fuzzy matching | `nucleo` | Helix's matcher. Fast enough for 200k paths. |
| Project search | `grep-searcher` + `grep-regex` | Ripgrep internals, not shelling out to `rg`. |
| Syntax highlighting | `tree-sitter` | Incremental. Deferred to Phase 5 — do not pull it in early. |
| Config | `toml` + `serde` | A `nibrc.toml`, nanorc in spirit. |

### The one alternative worth naming

If the goal is "a genuinely good editor as fast as possible" rather than "another ground-up Rust project," the honest answer is **.NET 10 + Avalonia + AvaloniaEdit**. AvaloniaEdit is a mature text editor control — virtualization, folding, TextMate highlighting, column selection — already built, and it maps onto the WPF experience you already have. It would cut this plan roughly in half.

The Rust route is the recommendation here because it matches where the browser project is going and because the rendering/shaping work is transferable between the two. But that's a taste call, not a technical verdict. Decide before Phase 0 and don't revisit it at Phase 4.

---

## 3. Architecture

Hard rule: **the editor core must be a separate crate with zero GUI dependencies.** Everything Claude Code can test without a window goes in `nib-core`. This is what makes agentic development actually work — the agent can verify its own changes by running tests instead of asking you to look at a screenshot.

```
nib/
├─ crates/
│  ├─ nib-core/        # no winit, no wgpu, no cosmic-text
│  │   ├─ buffer.rs        # rope, cursors, selections, line endings, encoding
│  │   ├─ edit.rs          # edit primitives; every mutation goes through here
│  │   ├─ history.rs       # undo/redo, edit coalescing
│  │   ├─ command.rs       # the Command enum — the entire editor API surface
│  │   ├─ keymap.rs        # (profile, context, chord) -> Command
│  │   ├─ search.rs        # incremental find, replace, regex
│  │   ├─ workspace.rs     # open folder, buffer set, active buffer
│  │   ├─ project.rs       # file tree model, ignore rules, watcher events
│  │   └─ fileio.rs        # load/save, encoding detect, atomic write
│  ├─ nib-ui/          # winit + wgpu + cosmic-text
│  │   ├─ app.rs           # event loop; translates input -> Command
│  │   ├─ render/          # text surface, footer, prompt line, sidebar
│  │   └─ theme.rs
│  └─ nib-harness/     # headless driver: feed keystrokes, assert buffer state
└─ nib/               # thin binary: arg parsing, wiring
```

**The Command layer is the contract.** The UI never mutates a buffer directly. It resolves input to a `Command`, hands it to core, and renders the result. This gives you, free: a scriptable test harness, remappable keys, a data-driven footer, and undo that can't be bypassed.

---

## 4. The keybinding decision

This needs settling in Phase 0 because everything else hangs off it.

Nano's bindings collide with every GUI convention:

| Key | nano | GUI expectation |
|---|---|---|
| `^X` | Exit | Cut |
| `^O` | Write Out (save) | Open |
| `^W` | Where Is (find) | Close window |
| `^K` | Cut line | — |
| `^U` | Uncut (paste) | — |
| `^C` | Show cursor position | Copy |
| `^S` | Save (newer nano) | Save ✓ |

**Decision: ship two keymap profiles in one declarative TOML file, `nano` as default.**

- `nano` profile: faithful. `^X` exits, `^O` writes, `^W` finds, `^K`/`^U` cut and uncut, `^_` goto line, `^\` replace, `M-U`/`M-E` undo/redo.
- `modern` profile: `^S` save, `^F` find, `^C`/`^X`/`^V` clipboard, `^Z`/`^Y` undo/redo, `^Q` quit.
- Both profiles are just data. Users can define their own.

The footer **renders from the active keymap**, so it's correct automatically in both profiles and for custom binds. Do not hardcode footer strings. This is the design detail that makes the whole thing hold together.

On first run, show a one-time footer prompt: `^X Exit is nano-style — press M-M for modern keys`. Then never mention it again.

---

## 5. Phases

Each phase is a working, runnable, committed program. No phase is complete until its acceptance criteria pass. Do not start a phase before the previous one's criteria are met.

### Phase 0 — Skeleton
- Cargo workspace with the crate layout above.
- `winit` window + `wgpu` surface, clears to a background color, resizes, handles DPI scaling.
- GitHub Actions: build + test + clippy on `windows-latest` and `ubuntu-latest`.
- `CLAUDE.md` written (see §8).
- Keymap profile decision locked in as a TOML file, parsed, unit-tested. No behavior wired yet.

**Acceptance:** `cargo test` green; the binary opens a window on both OSes; CI passes.

### Phase 1 — Text on screen
- `cosmic-text` integration: monospace font discovery, shaping, glyph atlas, draw a static buffer.
- Rope-backed `Buffer`, viewport-based rendering (only visible lines are laid out).
- Cursor rendering, click-to-position, arrow keys, Home/End/PgUp/PgDn.
- Scrolling: keyboard, mouse wheel, touchpad kinetic.

**Acceptance:** open a 50 MB log file, scroll to the end, no stutter, memory under ~2× file size.

### Phase 2 — Actually editing
- Insert, delete, backspace, newline. All mutations through `edit.rs`.
- Selection: shift+arrows, mouse drag, double-click word, triple-click line.
- Undo/redo with sensible coalescing (a run of typed characters is one undo step; a deletion run is another).
- Clipboard via `arboard`.
- File load/save: **encoding detection** (UTF-8, UTF-16 LE/BE, BOM handling, Latin-1 fallback), **line-ending detection and preservation** (a CRLF file stays CRLF), atomic save via temp file + rename.
- Dirty flag, and an exit prompt on unsaved changes.

**Acceptance:** load → make one edit → save produces a file byte-identical to the original except the edit. Golden-file tested across CRLF/LF/BOM/UTF-16 fixtures. This test suite is the safety net for everything after.

### Phase 3 — The nano personality
This is the phase that makes it *this* editor rather than a generic one. Don't rush it.

- **Footer**: two rows, rendered from the active keymap for the current context, reflows on narrow windows (drop lowest-priority entries first, like nano does).
- **Status line**: filename, modified marker, cursor position, transient messages that expire.
- **Prompt line**: single-line input above the footer, with its own footer row of shortcuts while active. Used for save-as, goto line, search, replace. History with up/down.
- **Contexts**: `editor`, `prompt`, `tree`, `search`. Keymap and footer are context-scoped.
- Commands: `^G` help (a scrollable text pane, not a dialog), `^W` incremental search with match highlight and wrap-around, `^\` search-and-replace with per-match y/n/a confirmation, `^_` goto line:column, `^K`/`^U` cut buffer with the nano accumulate-on-consecutive-cuts behavior, `^C` cursor position, `^R` insert file at cursor.
- `nib-harness`: drive the editor headlessly by feeding chord strings, assert on buffer + status + footer state.

**Acceptance:** someone with nano muscle memory sits down and edits a config file without reading anything. Harness tests cover every footer command.

### Phase 4 — Folder mode
Nano has no equivalent, so this is invention. Keep it keyboard-first and keep it quiet.

- `nib .` or `nib ~/src/project` opens a workspace.
- Sidebar tree: lazy directory reads, virtualized (only visible rows exist), `ignore` crate for `.gitignore` + a config ignore list. Toggle with `M-T`. Focusable with its own footer.
- `notify` watcher with debounce; external changes update the tree and mark buffers stale (prompt to reload, never silently overwrite).
- Multiple buffers with a `M-,` / `M-.` cycle and `M-B` buffer list on the prompt line. **Decide now: no tab bar.** A tab strip is a second chrome element competing with the footer. A buffer list on the prompt line is more nano.
- Quick open: `^T`, fuzzy path match via `nucleo`, results in a list above the prompt line.

**Acceptance:** open the Linux kernel source tree. Sidebar appears in under 500 ms, scrolls at 60 fps, quick-open filters 80k paths with no perceptible lag.

### Phase 5 — Syntax highlighting
- `tree-sitter` with incremental reparse on edit, parsing off the UI thread.
- Ship grammars for: Rust, C/C++, Python, JS/TS, JSON, TOML, YAML, Markdown, shell, HTML/CSS.
- Highlight query → theme token → color. Two themes to start, one light one dark, plus `nibrc` overrides.
- Bracket match, current-line highlight, optional line numbers, whitespace rendering, soft wrap toggle.

**Acceptance:** typing in a 10k-line Rust file stays under a 16 ms frame budget with highlighting on.

### Phase 6 — Project-wide search
- `M-W` opens project search on the prompt line; results in a scrollable pane; enter jumps to the hit.
- `grep-searcher` on a worker pool, streaming results as they arrive, cancellable on keystroke.
- Project-wide replace with a preview and an all-or-nothing apply.

**Acceptance:** search a 500 MB tree, first results visible in under 200 ms, UI never blocks.

### Phase 7 — Config and polish
- `nibrc.toml`: theme, keymap profile, custom binds, tab width, expand-tabs, wrap, ignore globs, font family/size. Live reload on save.
- `--help`, `+LINE` argument, stdin piping, `$EDITOR` compatibility (blocks until buffer closes — needed for git commit messages).
- Session restore: reopen last workspace and buffers.
- Crash-safe recovery files.

### Phase 8 — Ship it
- Windows: portable zip + a signed MSI if certs exist.
- Linux: AppImage as primary, plus `.deb` and a Flatpak manifest.
- Release CI on tags, with a static-linked Linux build to avoid glibc pain.
- README with a screenshot of the footer. That screenshot is the whole pitch.

---

## 6. Performance budgets

Treat these as tests, not aspirations. Add a `benches/` directory in Phase 1 and fail CI on regression.

- Cold start to first paint: **< 150 ms**
- Keystroke to painted glyph: **< 16 ms** at p99
- Open 100 MB file: **< 2 s**, memory < 2.5× file size
- Sidebar with 100k files: scroll at 60 fps
- Idle CPU: **0%** (event-driven redraw only — never a spinning render loop)

That last one matters more than it sounds. An editor that burns a core while sitting still is disqualifying on a laptop.

---

## 7. Testing strategy

Three layers, in priority order:

1. **Core unit tests** — buffer ops, undo coalescing, encoding round-trips, keymap resolution, ignore matching. Fast, no GPU, run on every save.
2. **Harness tests** — `nib-harness` feeds chord sequences into the full command pipeline and asserts on buffer content, cursor position, status text, and footer contents. This is where nano-fidelity gets locked down. Every Phase 3 command needs one.
3. **Golden-file IO tests** — the round-trip fixtures from Phase 2. Never delete these.

Manual/visual checks are the only thing left for rendering, and they should be the only thing left.

---

## 8. `CLAUDE.md` for the repo

Put this at the root before writing any code:

```markdown
# nib — agent instructions

## Non-negotiables
- `nib-core` has ZERO gui dependencies. No winit, wgpu, or cosmic-text in its Cargo.toml. Ever.
- All buffer mutation goes through `edit.rs`. If you're calling ropey directly outside that file, stop.
- The UI does not know what keys do. It resolves input to a Command and sends it.
- Footer text is generated from the keymap, never hardcoded.
- No new dependency without asking. Justify it against the stack table in the plan.

## Workflow
- One phase at a time. Do not start the next phase's work "while you're in there."
- Vertical slices: each commit builds, runs, and passes tests.
- Write the test before the fix for any bug.
- `cargo clippy -- -D warnings` and `cargo fmt` before every commit.
- Update CHANGELOG.md as you go.

## Stop and ask before
- Adding a floating window, dialog, or third footer row
- Changing the Command enum's shape
- Anything touching fileio.rs encoding or line-ending logic
- Adding a feature from the explicit non-goals list

## Non-goals (do not implement)
Extensions, LSP, debugger, terminal, git UI, minimap, tab bar, settings GUI.
```

---

## 9. Kickoff prompt

Paste this into Claude Code once the repo exists:

> Read `PLAN.md` and `CLAUDE.md` in full before writing anything.
>
> We're doing Phase 0 only. Set up the Cargo workspace exactly as specified in the architecture section, get a winit window with a wgpu surface clearing to a solid color, and wire up GitHub Actions to build, test, and clippy on Windows and Ubuntu.
>
> Also write `keymaps/nano.toml` and `keymaps/modern.toml` per the keybinding section, plus the parser and its unit tests in `nib-core/src/keymap.rs`. Nothing is wired to behavior yet — I just want the data model and tests.
>
> Before you start: list what you're going to create, flag anything in the plan you think is wrong or underspecified, and wait for me to confirm.

That last sentence matters. The plan will have gaps, and the cheapest time to find them is before there's code.

---

## 10. Known open questions

Worth deciding early, not urgent:

- **Mouse support depth.** Nano has minimal mouse. A GUI editor with no mouse selection feels broken. Recommendation: full mouse for selection and tree, but every command reachable by keyboard.
- **Multiple cursors.** Enormously useful; totally alien to nano. Recommendation: skip through Phase 8, revisit as an opt-in config flag.
- **Wayland fractional scaling.** `winit` handles it, `cosmic-text` glyph caching needs to be scale-aware. Get this right in Phase 1 or it's painful to retrofit.
- **The name.** Anything that reads as a nano fork will attract the wrong expectations. Pick something that stands alone.
