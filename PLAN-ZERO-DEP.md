# Zero-Dependency Rewrite Plan — Aitch

**Companion to [`PLAN.md`](PLAN.md), not a replacement for it.** Every phase
number, acceptance-gate, and architectural boundary below is the same one
PLAN.md defined. This document only replaces *what's behind* each boundary:
crates.io dependencies become hand-written code. [`CLAUDE.md`](CLAUDE.md)'s
rules still apply in full, plus the additions in §1.

## 0. Baseline, and what this actually commits to

**Status: complete.** Started at 386 locked packages; Phase 8's final gate
now shows exactly 4 — the workspace's own `aitch-core` / `aitch-ui` /
`aitch-harness` / `aitch`, and nothing else. The phase-by-phase log below
is kept as the record of how each dependency actually left, not just that
it did.

As of this writing (before the rewrite began): **386 locked packages**,
~18.8k lines of Rust, across `aitch-core` / `aitch-ui` / `aitch-harness` /
`aitch`.

**Goal: literal zero crates.io dependencies — runtime, build, and dev — with
no standing exceptions.** Every one is replaced with hand-written Rust behind
the *same* internal boundaries that exist today. §6 covers the only escape
hatch (a genuine correctness/safety hazard), and none is known to exist.

**Decisions locked in before drafting this (confirmed by the user):**
- Target is Windows only. Linux support (unpackaged today) is dropped from
  scope, not ported to a second hand-written backend.
- Full feature parity is the goal, phased exactly like PLAN.md — nothing is
  permanently descoped, but fidelity trade-offs are called out explicitly
  where they're unavoidable (§3).
- Strategy is **in-place subsystem replacement**, not a blank-slate rewrite.
  The `Command` layer, the `aitch-core`/`aitch-ui` split, and
  `aitch-harness`'s chord-driven tests all survive untouched. A ground-up
  rewrite of the whole codebase would throw away the test suite that makes
  this project's phase-by-phase workflow possible in the first place, for no
  benefit — the dependencies are the problem, not the architecture.

**Said plainly:** this replaces a GPU renderer, a font shaper/rasterizer, a
rope, a regex engine, a fuzzy matcher, an incremental-parsing framework across
13 grammars, a file watcher, and a TOML parser — each normally its own
open-source project. This is months of work phased exactly as conservatively
as PLAN.md, not a refactor sprint. Do not compress the phases.

## 1. Rules that extend CLAUDE.md

- **New non-negotiable:** CI gates on dependency count. A job fails the build
  if `Cargo.lock` lists any package outside `crates/aitch-core`,
  `crates/aitch-ui`, `crates/aitch-harness`, `crates/aitch`. Add it in Phase 0
  against the current count (386) and ratchet it down; never let it regress
  once a phase removes a crate.
- A crate is deleted from its `Cargo.toml` in the **same commit** that lands
  its replacement passing the same tests. No window where both exist.
- Build-dependencies and dev-dependencies count too — `winresource`
  (build-only, icon), `criterion` and `png` (dev-only, benches/screenshots)
  all get removed, no exceptions. They never reach users, so they're the
  lowest-risk items and land last, in Phase 8, once everything that ships is
  already at zero — but "never ships" is not a reason to keep one around.
- `windows-sys` / `libc` still count as dependencies. This plan writes
  `extern "system"` FFI blocks by hand instead — CLAUDE.md's "no new
  dependency without asking, justify against the stack table" applies to any
  exception exactly as it does today.

## 2. Replacement stack table

| Today | What it does | Replacement | Lives in |
|---|---|---|---|
| `winit` 0.30 | window + event loop | Hand-written Win32: `RegisterClassExW` / `CreateWindowExW` / `GetMessage` loop via `extern "system"` bindings to `user32.dll` | `aitch-ui/src/platform/win32/window.rs` |
| `wgpu` 26 + `pollster` | GPU device/surface, async init | Software rendering: a DIB section (`CreateDIBSection`) rasterized into on the CPU, presented with `StretchDIBits`/`BitBlt` — see §3 | `aitch-ui/src/platform/win32/surface.rs` |
| `cosmic-text` 0.17 | font discovery, shaping, rasterization | Hand-written sfnt/TrueType parser (`cmap`, `glyf`, `hmtx`, `loca`) + scanline rasterizer + glyph cache; one bundled monospace font via `include_bytes!` — see §3 | `aitch-ui/src/text/{font,shape,raster}.rs` |
| `ropey` 1.6 | rope buffer | Hand-written piece table + incremental line-offset index — see §3 | `aitch-core/src/buffer.rs` — **not actually the sole boundary**: `search.rs`, `syntax.rs`, `highlighter.rs`, and `editor.rs` all take `ropey::Rope` directly too (found landing Phase 1's piece table; CLAUDE.md's claim otherwise was wrong). `ropey` stays declared until those are rewritten in Phases 5/6 — `Buffer::text()` materializes an owned `Rope` for them meanwhile |
| `arboard` 3 | clipboard | Raw Win32 clipboard: `OpenClipboard` / `EmptyClipboard` / `SetClipboardData` / `GetClipboardData` with `CF_UNICODETEXT` | `aitch-ui/src/platform/win32/clipboard.rs` |
| `notify` 8 | file watching | Raw `ReadDirectoryChangesW` + a small debounce timer | `aitch-core/src/watcher.rs` (already the sole boundary) |
| `ignore` 0.4 | `.gitignore` matching | Hand-written gitignore-glob matcher: `*`, `**`, `?`, `!negation`, anchored `/`, directory-only trailing `/` | `aitch-core/src/project.rs` |
| `nucleo` 0.5 | fuzzy matcher | Hand-written fzf-style subsequence scorer: consecutive-run bonus, word-boundary bonus, gap penalty | `aitch-core/src/project.rs` (quick-open) |
| `grep-searcher` + `grep-regex` + `grep-matcher` | project search + regex | Hand-written literal search (Boyer-Moore-Horspool) + a small backtracking regex engine for a defined subset (literals, `.`, `*`, `+`, `?`, `[...]`, `^`/`$`, alternation, groups — no lookaround, no backrefs) | `aitch-core/src/search.rs`, `project_search.rs` |
| `tree-sitter` 0.26 + 13 grammar crates | incremental parse + highlight | Hand-written per-language lexer: a state machine that saves/restores its state at line boundaries, so an edit only re-lexes from the changed line onward — not a parse tree. See §3 for fidelity trade-offs | `aitch-core/src/syntax.rs` + one module per language |
| `toml` 0.8 + `serde` (config) | config file parsing | Hand-written parser for the config schema's actual subset (strings, bools, integers, string arrays — no dotted/inline tables) + manual struct construction, no derive | `aitch-core/src/config.rs` |
| `serde` (session/recovery) | (de)serialization | A small hand-written line-oriented format — doesn't need to be human-edited, so it doesn't need to be TOML | `aitch-core/src/session.rs` |
| `winresource` (build-only) | icon embedding | Hand-written `.rc` file + `std::process::Command` invoking the SDK's `rc.exe`/`llvm-rc` directly from `build.rs` | `crates/aitch/build.rs` |
| `criterion` (dev-only) | benchmarking | `std::time::Instant`-based micro-bench harness | `aitch-core/benches` |
| `png` (dev-only) | screenshot-test comparison | Minimal hand-written stored (uncompressed) PNG encoder (~80 lines) | `aitch-ui/examples`, tests |

## 3. Architecture decisions needing signoff before coding

Same discipline as PLAN.md §9 ("flag anything underspecified and wait").

- **Software rendering, not GPU.** Direct3D11 without a crate means
  hand-written COM vtable calls *and* an HLSL compiler dependency
  (`d3dcompiler_47.dll`, loadable via `LoadLibrary`/`GetProcAddress` with no
  crate — technically zero-dep, but a lot of code for no visible win in a
  text editor). GDI software blit is simpler and genuinely zero-dep, and
  because only visible lines are ever laid out (PLAN.md's own rule), CPU
  rasterization of a terminal-sized glyph grid at 60fps is realistic — but
  **this must be measured, not assumed.** Re-run every PLAN.md §6 budget
  (cold start < 150ms, keystroke-to-paint < 16ms) starting in Phase 1 and
  record actual numbers; they may need revising for a software path.
- **Piece table, not a hand-rolled balanced-tree rope.** A from-scratch rope
  with `ropey`'s UTF-8-boundary-safety guarantees is a real correctness
  hazard to hand-roll. A piece table (original buffer + add buffer + a
  sequence of piece descriptors, plus a separately maintained line-start
  index) gets comparable practical performance for an editor's access pattern
  with far less code. This changes `buffer.rs`'s internals only —
  `command.rs`, `edit.rs`, `history.rs` don't see the difference.
- **One bundled font, no system font discovery.** Dropping `cosmic-text`'s
  `fontdb` means no "use whatever monospace font is installed." Ship one
  open-license monospace font's bytes in the binary (license it in
  `docs/third-party.md`, same tracking discipline as today's crate licenses).
  The `font` key in `aitchrc.toml` becomes a no-op or is removed — a
  user-facing behavior change from today's README, call it out in the
  CHANGELOG when it lands.
- **Syntax highlighting drops from a real parse tree to line-state lexing.**
  Acceptable given the "full parity, phased" answer, but a hand-written
  parser generator to match tree-sitter's incremental-parse-tree fidelity is
  out of proportion to this project. Things tree-sitter gives for free
  (bracket matching across nested constructs, highlighting inside Markdown
  fences — already a known limitation today) need their own small pass or
  stay unsupported. Document this in the README's known-limitations section,
  don't let it regress silently.
- **The regex engine covers a defined subset**, not `grep-regex`-equivalence.
  `docs/config.md` and the search/replace docs need an explicit
  supported-syntax list.

## 4. Phases

Same acceptance-gate discipline as PLAN.md: each phase replaces one or more
stack-table rows, deletes the crate from `Cargo.toml` in the same commit that
lands its replacement, and keeps `cargo test --workspace` green throughout.
`aitch-harness` tests should need zero changes where they assert on
`Command`-level behavior rather than implementation — if one breaks, that's a
signal the replacement leaked through the boundary it shouldn't have.

**Parallelism is within a phase, never across phases.** CLAUDE.md's "one
phase at a time" rule doesn't change — the next phase still doesn't start
until the current one's acceptance gate passes. What changes is that a
phase's own tasks are broken into tracks a sub-agent can take independently.
Each phase below lists its **Parallel tracks**. To run them concurrently
without collision:

1. **Agree the interface first.** Where two tracks meet (e.g. the font
   parser's glyph-outline type, which the rasterizer track then consumes),
   land that shared type/trait/enum in one small sequential commit *before*
   fanning out — everyone codes against a contract, not against each other's
   in-progress work.
2. **One track, one set of files.** No two concurrent tracks touch the same
   file. Where the stack table names the same file for two replacements
   (Phase 4's gitignore matcher and fuzzy matcher both land in `project.rs`
   today), give each its own new module first (e.g. `gitignore.rs`,
   `fuzzy.rs`) so `project.rs` only gets a thin, sequential wiring change
   afterward.
3. **Each track ships its own tests**, per CLAUDE.md's vertical-slice rule —
   a track is done when its unit tests pass standalone, before integration.
4. **One integration step closes the phase**: a single commit (or agent)
   wires the finished tracks together, deletes the old crate(s) from
   `Cargo.toml`, and re-runs the phase's acceptance gate in full.
5. Use a separate git worktree or branch per concurrent agent so their
   in-progress edits can't collide on disk; merge each into the phase's
   integration branch only once its own tests are green.

### Phase 0 — Zero-dep skeleton & governance

**Status: done.** `winit`, `wgpu`, `pollster`, and `cosmic-text` are gone;
`aitch-ui` builds on the hand-written Win32 window and GDI surface below.
`Cargo.lock` is down from 386 packages to 164. The real `aitch` binary opens,
resizes, and closes cleanly on this backend today — confirmed live, not just
by its own unit tests — but draws only a clear color: no text and no
keyboard/mouse input yet. The old wgpu-era `render` module, `input.rs`, the
`dump_frame`/`make_icon` examples, and their tests were deleted rather than
kept around unbuildable; Phase 1 rebuilds rendering and its tests from
scratch against [`Surface`], and Phase 3 rebuilds input against [`Window`].

- Stand up the CI dependency-count gate against the current baseline (386).
- Replace `winit` with the raw Win32 window/message loop; replace
  `wgpu`+`pollster` with a GDI DIB-section clear-to-color surface (no real
  text yet — same scope as PLAN.md Phase 0).
- **Parallel tracks** (contract first: agree the minimal `Window` surface —
  create/resize/close callbacks — the other two tracks need):
  - **A — CI gate.** New workflow job/script asserting `Cargo.lock` has no
    non-workspace packages. Fully independent of A/B below; different files
    (`.github/workflows/`).
  - **B — Win32 window + message loop.** `RegisterClassExW` /
    `CreateWindowExW` / `GetMessage` loop in
    `aitch-ui/src/platform/win32/window.rs`.
  - **C — GDI DIB-section surface.** `CreateDIBSection` +
    `StretchDIBits`/`BitBlt` clear-to-color in
    `aitch-ui/src/platform/win32/surface.rs`, built against a stub HWND
    until B lands.
  - **Integration:** wire B+C together, delete `winit`/`wgpu`/`pollster` from
    `Cargo.toml`, confirm the acceptance gate.
- **Acceptance:** window opens, resizes, clears to color, is DPI-aware; zero
  non-workspace runtime deps in `aitch-ui`/`aitch`; CI green on
  `windows-latest` only (Ubuntu job removed).

### Phase 1 — Text on screen

**Status: done.** The hand-written sfnt parser, rasterizer, glyph cache, and
piece table all landed and are wired into `render_frame`; the real `aitch`
binary draws real text through this pipeline today — confirmed live via
`GetPixel` sampling against the actual running window, not just unit tests.
`ropey` did not leave (see below); nothing else in the stack table changed.

- `cosmic-text` is already gone as of Phase 0's integration (removed
  alongside `wgpu`, since nothing in the old render pipeline could survive
  either one leaving). This phase is now purely building the hand-written
  sfnt parser + rasterizer + glyph cache from nothing, not "replacing
  cosmic-text" in place — there's no old text-rendering code left to swap
  pieces out of; `aitch-ui`'s render module was deleted and starts fresh here.
- Hand-written sfnt parser + rasterizer + glyph cache; bundle one monospace
  font. Replace `ropey` with the piece table.
- **Parallel tracks** (contract first: agree the glyph-outline type A hands
  to B):
  - **A — sfnt/TrueType parser.** `cmap`/`glyf`/`hmtx`/`loca` parsing in
    `aitch-ui/src/text/font.rs`, unit-tested against the bundled font's bytes
    directly — no dependency on B.
  - **B — Rasterizer + glyph cache.** Scanline fill + cache in
    `aitch-ui/src/text/{shape,raster}.rs`, developed against a handful of
    hand-built fixture outlines until A lands, then swapped over.
  - **C — Piece table.** `aitch-core/src/buffer.rs` — a different crate
    entirely from A/B, so fully independent; only touches `edit.rs`'s call
    sites at the end, not its logic.
  - **Integration:** wire A+B into the render path, confirm C's API matches
    what `edit.rs`/`history.rs` expect. **`ropey` does not leave in this
    phase** — `search.rs`/`syntax.rs`/`highlighter.rs`/`editor.rs` still call
    it directly (see the stack-table correction above), so removing it is
    coupled to Phases 5/6's rewrite of those modules, not a Phase 1
    integration step. `cosmic-text` already left in Phase 0's integration.
- **Acceptance:** PLAN.md Phase 1's criteria (50MB file, no stutter), but
  re-measured against software rendering — record real numbers.

### Phase 2 — Actually editing — done
- Raw Win32 clipboard. `fileio.rs`'s encoding/line-ending detection was
  *assumed* to already have no dependency — **wrong, the same way the
  `ropey`/`buffer.rs` claim was**: `line_ending.rs`'s `LineEnding::dominant`
  took a `&ropey::Rope` for no reason beyond calling `.chars()` on it. Found
  by Track B's audit and fixed (now takes `&str`) — `fileio.rs` is genuinely
  dependency-free as of this phase.
- **Parallel tracks** (no shared interface needed — these three don't touch
  each other):
  - **A — Win32 clipboard.** `OpenClipboard`/`SetClipboardData`/
    `GetClipboardData` in `aitch-ui/src/platform/win32/clipboard.rs`,
    replacing `arboard` (and, transitively, 17 other crates that only existed
    for macOS/Linux clipboard backends this Windows-only project never
    built).
  - **B — `fileio.rs` audit.** Confirm encoding/line-ending detection has no
    hidden dependency; extend the golden-file fixture set. Found the
    `line_ending.rs` dependency above; flagged rather than fixed inline, per
    CLAUDE.md's "stop and ask before touching fileio.rs" rule — fixed
    separately once confirmed.
  - **C — Piece-table call-site adaptation.** Any remaining `edit.rs`/
    `history.rs` spots still assuming ropey's API get updated to the Phase 1
    piece table.
  - **Integration:** none needed beyond each track's own tests — this phase
    has no shared file to reconcile.
- **Acceptance:** identical to PLAN.md Phase 2 — byte-identical round trip
  across the golden CRLF/LF/BOM/UTF-16 fixtures. This suite doesn't get to
  regress; it's the safety net for everything after it.

### Phase 3 — The nano personality

**Status: done.** Real keyboard and mouse input work end to end in the real
`aitch` binary — typing, resizing, click-drag selection, and wheel scrolling
were all confirmed live against the running window via synthetic
`PostMessageW` input (not `SendMessageW`, which does not reliably wake a
thread blocked in `GetMessageW` from another thread — a real, if narrow,
Windows message-passing lesson learned doing this verification), not just
unit tests. One gap found and closed during integration, outside any single
track's stated scope: neither a cursor caret nor a selection highlight had
ever been drawn — `render_frame` gained both. Still missing, correctly out
of scope per below: a folder/config watcher and a recovery-write timer
(Phase 4/7-shaped work, needing `SetTimer`/`WM_TIMER` or a cross-thread
`PostMessageW` wake that doesn't exist yet).

- No new subsystem: input now arrives as raw `WM_KEYDOWN`/`WM_CHAR`/mouse
  messages instead of winit's abstracted events, remapped to chords at the
  `aitch-ui` boundary only. `keymap.rs`, footer, prompt untouched.
- **Revised: this does have real parallel tracks, contract first.** The
  original call here ("no parallel tracks, one integration surface") was
  made before Phase 1's font/render pipeline existed to reuse for chrome
  rendering, and before `Window` had any keyboard/mouse events to build
  against — at the time, "input handling" really did look like one
  inseparable lump of event-loop code. It wasn't a wrong read of what
  existed then, just a judgment worth revisiting once more of the pipeline
  landed. The actual seam was always "raw event in, pure translated output
  out" — the same shape Phase 1's rasterizer/bearing/compositing tracks
  proved out — it just wasn't visible until `Window`'s event contract
  existed to translate *from*. That contract (`Event::KeyDown`/`Char`/
  `MouseMove`/`MouseButton`/`MouseWheel`, no modifier state threaded through
  — `Ctrl`/`Alt`/`Shift` read live via `GetKeyState`) was landed as its own
  step before fanning out, the same discipline as Phase 1's `outline.rs`.
  - **A — Keyboard → chord.** A new `aitch-ui/src/input.rs` (recreating what
    Phase 0 deleted, rebuilt against `Event::KeyDown`/`Char` instead of
    winit's `KeyEvent`): virtual-key-to-`NamedKey` mapping, the
    `Ctrl`+letter C0-control-code unmapping the deleted file already solved
    once, and — the one genuinely new rule — discarding a `Char` that
    duplicates a same-keypress `KeyDown` (`Enter`/`Tab`/`Backspace`/
    `Escape`/`Space`; see `window.rs`'s module docs). Pure function, no
    window needed to test it: feed synthetic events, assert the `Chord`.
  - **B — Mouse → position/selection.** A new module (e.g.
    `aitch-ui/src/hit.rs`) recovering what Phase 0 stripped from `app.rs`
    (`position_at_pointer`, click-to-place, drag-to-select, wheel-to-scroll)
    against `Metrics` from Phase 1's `app.rs` instead of the old
    `render::Surface::hit`. Pure function taking pixel coordinates plus
    whatever buffer/viewport state it needs, returning a `Position` —
    testable with fabricated inputs, no real window or click required.
  - **C — Footer, status, and prompt rendering.** `aitch-core`'s
    `footer.rs`/`prompt.rs` already generate the right text and need no
    changes (untouched since before this rewrite began); what's missing is
    drawing it. A function alongside Phase 1's `render_frame` (same file or
    a new one) that lays out and draws the two footer rows, status line, and
    prompt line through the existing pipeline (`shape::layout_line` →
    `raster`/`GlyphCache` → `Surface::draw_coverage`) — the same one
    `render_frame` already uses for buffer text. Testable the same way
    Phase 1's render test was: an in-memory `Surface`, assert ink lands
    where the chrome should be.
  - **Integration:** wire A (dispatch a resolved `Command` to `Editor`,
    trigger a redraw) and B (mouse-driven cursor/selection changes) into
    `app.rs`'s event loop, and call C from `render_frame`. This is where the
    actual event-handling code lives and where the three tracks' outputs
    meet — same pattern as every other phase's integration step, done once
    the parallel pieces are proven standalone.
- **Acceptance:** identical to PLAN.md Phase 3.

### Phase 4 — Folder mode — done for `aitch-core`'s dependencies; UI still pending
- Hand-written gitignore matcher, `ReadDirectoryChangesW`-based watcher,
  hand-written fuzzy matcher.
- **Correction: `ignore` is bigger than a pattern matcher.** `project.rs`
  doesn't just call a gitignore matcher — `ignore::WalkBuilder`/`WalkState`
  also does the recursive and parallel directory walking itself (hidden-file
  skipping, the `.gitignore`/`.ignore`/global/parent-chain resolution, a
  depth-1 single-directory listing for the tree, and a parallel full walk for
  quick-open's path index). Same class of surprise as the `ropey`/`buffer.rs`
  and `ropey`/`fileio.rs` corrections earlier — found now, corrected now,
  added as its own track (D) rather than quietly folded into Track A.
- **Also out of scope for these four tracks, flagged for a separate
  decision:** the *aitch-ui* side of folder mode — a sidebar rendering the
  tree, `M-T` to toggle it, a quick-open UI — was fully deleted along with
  the wgpu-based renderer in Phase 0 and has not been rebuilt. These four
  tracks replace `aitch-core`'s dependencies on `ignore`/`nucleo`/`notify`
  behind the *existing* `Tree`/`PathIndex`/`Watcher` APIs, which is what
  "zero dependencies" needs — they do not by themselves make folder mode
  usable again in the real editor. That's real, comparable-in-size UI work
  (a whole new chrome element, like Phase 3's footer track was), not a small
  integration step, and needs its own explicit go-ahead before starting.
- **Parallel tracks** (the stack table puts the gitignore matcher and the
  fuzzy matcher in `project.rs` — split each into its own new module first
  specifically so these tracks don't collide on one file):
  - **A — Gitignore matcher.** New `aitch-core/src/gitignore.rs`:
    `*`, `**`, `?`, `!negation`, anchored `/`, directory-only trailing `/`.
    Pure function, own test file, zero dependency on B/C/D.
  - **B — File watcher.** `ReadDirectoryChangesW` + debounce in
    `aitch-core/src/watcher.rs` — already the sole boundary, untouched by
    A/C/D. Preserve `Watcher::new`'s exact existing signature and behavior;
    its own test suite (new-file, debounced-burst, drop-stops-it) is the
    regression net.
  - **C — Fuzzy matcher.** New `aitch-core/src/fuzzy.rs`: fzf-style
    subsequence scorer. Pure function, own test file, zero dependency on
    A/B/D.
  - **D — Directory walker.** New `aitch-core/src/walk.rs`: a hand-written
    recursive walker (single-directory depth-1 listing for the tree) and a
    parallel full-tree walker (for `PathIndex::build`), replacing
    `ignore::WalkBuilder`/`WalkState`. Built against its own placeholder
    ignore-predicate (a closure or small trait) until Track A's real matcher
    exists, same pattern as Phase 1's rasterizer track building against
    fixture outlines before the font parser existed. Global gitignore
    (`core.excludesFile`) may be dropped as a documented, honest scope
    reduction if it adds disproportionate complexity — `.gitignore`/`.ignore`
    plus the parent-directory chain covers the overwhelming majority of real
    use and is what's load-bearing for this project's own repo.
  - **Integration:** wire A into D (the real gitignore predicate replacing
    D's placeholder) and both into `project.rs` (tree building and
    quick-open) alongside C — the only place these tracks meet.
- **Integration landed.** `Tree`/`PathIndex` now run entirely on
  `gitignore.rs`/`walk.rs`/`fuzzy.rs`; every pre-existing test in
  `project.rs` passed unmodified against the swap, plus a real-directory
  end-to-end check (a `.gitignore` excluding a vendor folder and a `.env`
  file, a nested subdirectory with its own `.gitignore` re-including a name
  the root never excluded) confirmed correct.
- **`nucleo`/`nucleo-matcher`/`notify` removed. `ignore` is not** — found
  during integration that `project_search.rs` (Phase 6) still needs
  `ignore::WalkBuilder::build_parallel().run(..)`'s callback-per-file
  streaming walk for its cancellable search, which `walk::walk_all` (a
  blocking walk, one `Vec` at the end) was never asked to provide. `ignore`'s
  removal now naturally couples with Phase 6's own rewrite of that file
  (already touching `grep-searcher`/`grep-regex`) rather than being a loose
  end here. `project.rs`'s own `walker()` function stays, used only by
  `project_search.rs` now.
- **A real correctness gap found and fixed**: `gitignore::Rules::is_ignored`
  returned a bare `bool`, collapsing "no pattern here says anything" and "a
  pattern explicitly re-included this" into the same `false` — losing
  exactly the information needed to combine multiple `.gitignore` files'
  verdicts across the parent-directory chain the way git actually does
  (last match wins across the *whole* combined list, not per file). Added
  `Rules::verdict(path, is_dir) -> Option<bool>` alongside the existing
  `is_ignored` (now defined as `verdict(..).unwrap_or(false)`), and an
  `IgnoreRules` type in `project.rs` that layers per-directory `verdict`
  calls correctly. Proven both in isolation (a `gitignore.rs` unit test) and
  end to end (the real-directory check above).
- Dependency count: 146 → 118.
- **Acceptance:** PLAN.md Phase 4's criteria (500ms sidebar, 60fps scroll,
  80k-path fuzzy filter) — **not measured**: there is no folder-mode UI to
  measure them against yet (see the still-open UI note above). The
  `aitch-core`-level logic is correct and tested; the performance budgets
  apply once a sidebar/quick-open UI exists to drive it.

### Phase 5 — Syntax highlighting

**Status: done — all thirteen languages have a real lexer.** The contract
landed first and alone (`Token`, `LineState`, the `Lexer` trait, and
`Highlighter`'s incremental re-lex engine, proven against a fixture lexer
before any real language existed — the same discipline as Phase 1's
font/rasterizer split), with Rust and JSON as its two reference
implementations. TOML, Markdown and Bash then landed as three genuinely
parallel agents working against that contract in the same checkout (Wave 1),
each touching only its own new file — no integration conflicts. Wave 2 (C,
C++, CSS, HTML, JavaScript, Python, TypeScript, YAML) followed the same
pattern at full width — eight parallel agents, one file each, one
integration commit wiring all eight into `Language::lexer` — confirming the
plan's own prediction that this phase has the least collision risk of any of
them, at both scales it was tried. `tree-sitter` and all thirteen grammar
crates are gone. 599 `aitch-core` tests pass (250 of them the thirteen
lexers' own). **Not yet done** — same gap Phase 4 left for folder mode:
`aitch-ui` does not draw the colour yet; `Highlights`/`Span`/`token_at` are
consumed today only by `aitch-core`'s own bracket matching and tests. The
16ms-frame acceptance budget is therefore not measured here, for the same
reason Phase 4's UI-side budgets weren't: nothing exists yet to draw it
against.

- Per-language hand-written lexers. Start with the languages this repo's own
  code uses (Rust, JSON, TOML, Markdown, shell) to prove the line-state
  re-lex architecture before expanding to the remaining eight.
- **This is the phase with the most natural parallelism in the whole plan**:
  once the shared contract exists, each language is a self-contained file
  with no cross-language dependency.
  - **Contract (sequential, one track, lands first):** the `Token` type and
    a `Lexer` trait with explicit save/restore state-at-line-boundary
    methods, in `aitch-core/src/syntax.rs`. Every language track codes
    against this and nothing else.
  - **Wave 1 (5 parallel tracks, one per language):** Rust, JSON, TOML,
    Markdown, shell — each its own new file under `aitch-core/src/syntax/`,
    each with its own fixture files and test file. This wave proves the
    line-state re-lex architecture before the rest scale out.
  - **Wave 2 (up to 8 parallel tracks, one per language):** C, C++, Python,
    JavaScript, TypeScript, YAML, CSS, HTML — same pattern, started only
    after Wave 1's tracks pass their tests (the architecture needs proving
    once, not eight times over, before committing eight agents to it).
  - **Integration:** register each finished lexer against its file
    extension — a one-line addition per language, low collision risk even
    landing several in the same window.
- **Acceptance:** same 16ms frame budget on a 10k-line file, plus a written
  note on where fidelity is lower than tree-sitter (§3).

### Phase 6 — Project-wide search

**Status: done.** `grep-matcher`, `grep-regex`, `grep-searcher`, `ignore`,
and `ropey` are all gone — this crate has zero declared dependencies beyond
`serde`/`toml` (Phase 7's). The `Matcher` trait (a byte-range hit over a
byte slice, per this section's own contract) landed with `LiteralMatcher`
(Boyer-Moore-Horspool) as its own reference implementation, Track A and the
contract in one step; a single agent then built `regex.rs`'s backtracking
engine as Track B against that contract, self-verified in a scratchpad
crate since it had no way to compile against the real one yet (a process
gap worth naming: the agent should have been handed a stub `mod regex;`
wired into `lib.rs`, the same pattern Phase 5's Wave 1/2 language tracks
used, so `cargo test` actually ran rather than being simulated elsewhere —
it worked out, but by luck of a resourceful agent, not by design here).
Integration (Track C) replaced `ignore::WalkBuilder::build_parallel()` with
a new [`crate::walk::walk_streaming`] (the same `IgnoreRules`
`Tree`/`PathIndex` already used, now `pub(crate)` for this second caller —
`project.rs`'s old `ignore`-based `walker()` function is deleted outright)
and `grep-searcher`'s `Searcher`/`Sink` with a hand-written line scan over
`fileio::load`'s already-decoded text — reusing Phase 2's encoding
detection rather than reimplementing a weaker version turned out to matter
for real: a first pass read raw bytes and flagged any UTF-16 file as
"binary" (its ASCII content is `XX 00`-interleaved, which is almost all
NUL bytes), silently dropping real hits, caught by the pre-existing
`a_replace_keeps_the_encoding_and_line_endings_it_found` test. One
deliberate scope addition beyond the original contract: `regex.rs`'s
`\d`/`\D`/`\w`/`\W`/`\s`/`\S` shorthand classes, added during integration
because a *pre-existing* `project_search.rs` test already relied on `\d`
meaning "digit" (a reasonable ripgrep-parity expectation the original task
prompt to the regex-engine agent never actually asked for) — not adding
them would have meant weakening that test instead of the engine.
Project-wide search also loses the machine-wide gitignore
(`core.excludesFile`) that `ignore` gave for free, the same documented,
accepted trade `Tree`/`PathIndex` already made in Phase 4 — extended here
rather than repeated as a separate decision.

`ropey` leaving was folded into this phase too, per the stack table's own
note that it stayed declared only until `search.rs` (in-buffer search) was
rewritten. `Buffer` gained `snapshot`/`snapshot_chars`/`char_at`/
`char_to_byte` (all thin, read-only wrappers the piece table already had
the logic for) and lost `text()` entirely; `search.rs`'s `find`/`find_all`
now take `&[char]` instead of `&ropey::Rope`, and `editor.rs`'s bracket
matching — the other caller the original stack-table note named — was
rewritten against the same new `Buffer` methods. `aitch-core` now has zero
`ropey` references anywhere, code or `Cargo.toml`.

- Hand-written literal search + subset regex engine; worker-thread streaming
  via `std::thread`/`mpsc` (already dependency-free).
- **Parallel tracks** (contract first: agree the `Matcher` interface —
  something producing byte-range hits over a byte slice — both A and B
  implement it, so C can consume either without caring which):
  - **A — Literal search.** Boyer-Moore-Horspool. **Correction while
    landing:** put in a new `aitch-core/src/matcher.rs` alongside the
    `Matcher` trait itself, not `search.rs` as first written here —
    `search.rs` already existed as the interactive in-buffer search
    (unrelated to project-wide search beyond both meaning "find text"),
    and folding a new byte-oriented multi-file matcher into that file would
    have blurred a boundary that was already clean. Landed by the same
    agent as the contract, in one step, both proven together.
  - **B — Regex engine.** Backtracking engine for the defined subset
    (literals, `.`, `*`, `+`, `?`, `[...]`, `^`/`$`, alternation, groups) —
    large enough to be its own file, e.g. `aitch-core/src/regex.rs`.
  - **Integration (track C, starts once A+B's interface is stable, doesn't
    need to wait for full completion):** worker-thread streaming and
    cancellation in `project_search.rs`, wired to whichever matcher a query
    resolves to.
- **Acceptance:** PLAN.md Phase 6's criteria, plus a documented list of
  regex features not supported relative to `grep-regex` today.

### Phase 7 — Config and polish

**Status: done.** `serde` and `toml` are both gone — `aitch-core` now
declares zero runtime dependencies at all. `toml.rs`'s `Value` enum and
hand-written parser landed first (Track A), immediately consumed by
`config.rs`'s rewritten `Config::from_value`/`FontConfig::from_value`
(Track B, done together rather than staged, since both were mine to write
and the contract needed no separate proving step). Track C
(session/recovery, fully independent) went to a parallel agent and landed
clean: 23 tests, all passing, no design drift from the format specified up
front. **A real correction found only after B was drafted:** `keymap.rs`
was never in this section's scope, but turned out to be a third `serde`+
`toml` consumer (`Keymap::from_toml` parsing `keymaps/nano.toml`/
`modern.toml`, ~98 `[[binding]]` entries each) — the same shape of gap as
`ignore`-is-bigger-than-a-pattern-matcher back in Phase 4, found by
grepping for remaining `toml::`/`serde::` usage before touching
`Cargo.toml`, not by the plan anticipating it. Fixed in the same phase
rather than deferred: `Context` lost its `Deserialize` derive for a
hand-written `parse`, and `RawKeymap`/`RawBinding` now build from `Value`
instead of deriving `Deserialize` — which is also what pushed the parser to
support `[[array of tables]]`, not just `[section]`, since that's the whole
shape a keymap file is. Both real keymap files parse and pass every
existing test unchanged. 672 `aitch-core` tests passing (up from 652),
clippy and fmt clean.

- Hand-written TOML-subset parser and config struct construction;
  hand-written session/recovery serialization.
- **Parallel tracks** (contract first: agree the parser's output `Value`
  enum — table/string/bool/integer/array — before B starts):
  - **A — TOML-subset parser.** Produces the `Value` tree; own module, own
    fixture-file tests, no knowledge of `Config`'s shape.
  - **B — Config struct construction.** Reads a `Value` tree into `Config`
    with validation/defaults, in `aitch-core/src/config.rs` — built against
    a hand-built `Value` fixture until A lands, then swapped over.
  - **C — Session/recovery serialization.** Fully independent: different
    file (`session.rs`), different format (not TOML), no shared type with
    A/B.
  - **Integration:** wire A's output into B; C needs none.
- **Acceptance:** identical to PLAN.md Phase 7.

### Phase 8 — Ship it

**Status: done. Final gate passed** — `cargo tree --workspace` shows
exactly the four workspace crates and nothing else, dependencies, dev-
dependencies, and build-dependencies alike; `Cargo.lock` holds four
`[[package]]` entries total. Track C turned out to already be done before
it started: `png` was declared in `aitch-ui/Cargo.toml` but never actually
imported anywhere (its own comment already said so — "nothing currently
uses it," left over from before the GDI surface existed for a screenshot
tool to target) — removed outright, no encoder needed. Tracks A and B ran
as genuinely parallel agents. A (`winresource` → a hand-written `.rc`
compiled via `windres`/`rc.exe`, branching on `CARGO_CFG_TARGET_ENV`) was
verified for real on this dev machine's GNU/MinGW toolchain — the built
`aitch.exe`'s icon resource was extracted back out with `[System.Drawing.
Icon]::ExtractAssociatedIcon` to confirm it's genuinely embedded, not just
"the build didn't fail" — but the MSVC/`rc.exe` branch (what CI and
releases actually use, since GitHub's `windows-latest` runner defaults to
MSVC) could only be written from documented `rc.exe` usage, not run-tested
locally; flagged honestly rather than claimed as verified. B (`criterion`
→ a hand-written `Instant`-based harness: minimum iteration count *and*
minimum duration, capped by a maximum iteration count, so a cheap closure
still gets a stable sample and an expensive one doesn't get forced through
extra iterations it doesn't need) surfaced a real, pre-existing
performance bug while re-running the benchmarks it replaced:
`visible_window`'s read cost at the middle/end of a large file measured
dramatically higher than at the start, contradicting that benchmark's own
doc comment claim of staying flat regardless of position. **Found and fixed
in a follow-up**: the piece table's every char↔byte conversion
(`char_to_byte`, `byte_to_char`, `char`, `slice_to_string`, and
`split_at_char` — which runs on every edit, not just every read) scanned a
piece's text from byte 0 regardless of how far in the target was, since a
piece has no internal chunking the way a rope's leaves do. Fixed with a
`PieceTable::checkpoint_before` that resumes the scan from the nearest
known line boundary (already tracked by `line_starts`, now given a byte-
offset twin, `line_bytes`, maintained the same incremental way) instead of
the start of the document — bounding every lookup by the target line's own
length rather than its position in the file. `visible_window`'s at_start/
at_middle/at_end went from 64µs/859ms/1.72s to a flat 8.3/8.6/8.6µs;
`set_cursor_random_line` dropped from 25ms to 0.6µs. All existing tests
(including the fuzz-style `a_long_sequence_of_edits_matches_a_plain_string_
reference`, extended to also check the new `line_bytes` index) passed
unchanged, plus one new named regression test.

- Drop `winresource`: `build.rs` shells out to `rc.exe` directly against a
  hand-written `.rc`. Re-verify the MSI/zip pipeline (WiX is a build tool,
  not a Rust dependency — out of scope).
- **Parallel tracks** (all three are independent — different files, no
  shared type, nothing ships that depends on another):
  - **A — `winresource` replacement.** Hand-written `.rc` +
    `std::process::Command` call to `rc.exe`/`llvm-rc` in
    `crates/aitch/build.rs`.
  - **B — `criterion` replacement.** `std::time::Instant`-based micro-bench
    harness in `aitch-core/benches`.
  - **C — `png` replacement.** Minimal stored-PNG encoder for
    `aitch-ui/examples`/tests.
  - **Integration:** none required between A/B/C; the only sequential step
    left is the final gate below, which depends on every prior phase's
    crates actually being gone, not just this phase's three.
- **Final gate:** a release build's dependency graph contains exactly the
  four workspace crates and nothing else.

## 5. Testing strategy carries over unchanged

Every hand-rolled subsystem above — piece table, TOML parser, regex engine,
fuzzy matcher, gitignore matcher, per-language lexers — is pure logic with no
OS dependency, exactly as unit-testable as `ropey`/`toml`/`nucleo`/tree-sitter
are today. That's what makes "zero dependencies" tractable instead of a
research project: nothing about the testing pyramid in PLAN.md §7 changes.
The golden-file IO tests are the tightest regression net in the whole
project — extend them, never weaken them.

## 6. What this plan does not do

- Does not touch PLAN.md's non-goals list (extensions, LSP, debugger,
  terminal, git UI, minimap, tab bar, settings GUI).
- Does not restore Linux/macOS. If that changes later, every Win32-specific
  module in §2 needs a full second backend (X11/Wayland wire protocol or
  Cocoa) — that's a separate plan, not an addition to this one.
- Does not chase literal-zero over correctness. Nothing identified above
  needs an exception, but if one turns up, CLAUDE.md's existing "no new
  dependency without asking" rule is the process for it — same bar as today.
