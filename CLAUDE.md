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
