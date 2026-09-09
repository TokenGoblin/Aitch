# Making the screenshots

The images in the README are rendered by the editor itself, headlessly, by the
`dump_frame` example. There is no window, no display and no screen capture
involved: `dump_frame` draws one frame through exactly the same
`render::screen` code the real window uses and writes it straight to a PNG.

That matters for more than tidiness. A screenshot taken by hand goes stale the
moment a colour or a footer entry changes, and nobody notices until someone
reads the README and finds it describing an editor that no longer exists. These
are one command each, so they can be regenerated whenever the answer changes.

## The commands

Run from the repository root. Both are 896×384 — the width has to be a multiple
of 64 to satisfy the GPU copy alignment.

```
cargo run -p aitch-ui --release --example dump_frame -- \
    crates/aitch-core/src/edit.rs docs/images/editor.png "^V ^V ^V ^V"

cargo run -p aitch-ui --release --example dump_frame -- \
    crates/aitch-core/src/edit.rs docs/images/tree.png "M-T"
```

The third argument is a sequence of chords fed to the editor before the frame
is drawn, the same notation the keymaps use. The four `^V` page down into
`edit.rs` far enough to be looking at code rather than the module comment; the
`M-T` opens the folder tree and moves the focus into it, which is why the
second image's footer is the tree's rather than the editor's.

Anything unbound is typed as text, so a typo in a chord silently edits the
buffer instead of failing. Check the picture.

## Choosing what to show

The footer is the pitch, so it has to be legible and it has to be the real one
— generated from the keymap like every other footer, not drawn for the
occasion. Beyond that:

- **Code, not comments.** The top of most files is a module comment, which
  renders as one flat colour and sells nothing. Page down until there are
  keywords, types and function names on screen.
- **The current line highlight should land somewhere sensible**, since it is
  the one piece of state a still image can show.
- `--release`, because a debug build takes about eight seconds to lay the frame
  out and it is easy to assume it has hung.

## Other output

Give the output a `.raw` extension instead and you get the raw dump the render
tests compare against: `u32` width, `u32` height, then `width × height` RGBA
pixels, tightly packed. `.png` is chosen on the extension and nothing else.
