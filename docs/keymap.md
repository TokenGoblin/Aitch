# Keymap files

A keymap is a TOML file mapping `(context, chord)` to a command. The editor
ships two: `keymaps/nano.toml` (the default) and `keymaps/modern.toml`. A user
keymap is the same format.

Nothing about the footer is written in code. The footer renders from whichever
keymap is active, so it is correct in both profiles and in a custom one.

## Chord grammar

A chord is zero or more modifiers plus one key. Two spellings are accepted and
mean the same thing:

| nano shorthand | long form      | means                 |
|----------------|----------------|-----------------------|
| `^X`           | `Ctrl+X`       | Ctrl and `x`          |
| `M-U`          | `Alt+U`        | Alt and `u`           |
| `M-^X`         | `Ctrl+Alt+X`   | Ctrl, Alt and `x`     |
| `S-Left`       | `Shift+Left`   | Shift and Left        |
| —              | `F5`           | F5                    |

Modifier names are case-insensitive: `Ctrl` / `Control` / `C`, `Alt` / `Meta` /
`M`, `Shift` / `S`. `M-` is Alt on both Windows and Linux.

Named keys: `Enter` (`Return`), `Tab`, `Backspace` (`Bksp`), `Delete` (`Del`),
`Escape` (`Esc`), `Space`, `Left`, `Right`, `Up`, `Down`, `Home`, `End`,
`PageUp` (`PgUp`), `PageDown` (`PgDn`), `Insert` (`Ins`), `F1`–`F12`.

### Keys are logical, not physical

The key half of a chord is the character the layout produces, ignoring Ctrl.
Binding on physical scancodes would move `^\` and `^_` to a different place on
every non-US layout; binding on the logical character keeps a keymap file
meaning what it says.

Three rules follow:

- **ASCII letters fold to lowercase.** `^X` and `^x` are one chord.
- **Shift is significant for letters and named keys.** `Ctrl+Shift+F` is not
  `^F`, and `Shift+Left` is not `Left`. A GUI can tell these apart where a
  terminal cannot, and the modern profile needs it for `Ctrl+Shift+F`.
- **Shift is dropped for every other character.** `_` is Shift+minus on a US
  layout and something else elsewhere, so the character is the whole identity:
  `^_` matches however the layout produced the `_`.

A chord a layout cannot reach is a keymap problem, not a code problem — a
binding takes a list of chords, so a profile can offer an alternate spelling.

## File format

```toml
profile = "nano"
description = "GNU nano bindings, faithfully."

[[binding]]
context = "editor"          # editor | prompt | tree | search | help
chords = ["^_", "M-G"]      # or chord = "^_" for a single one
command = "goto-line"       # must name a Command; unknown names are an error
label = "Go To"             # optional: appears on the footer
priority = 191              # optional: higher survives a footer reflow
```

- A binding needs at least one chord.
- A chord may be bound once per context. The same chord in two contexts is fine.
- Unknown fields and unknown commands are errors, not warnings — a typo in a
  keymap should not silently unbind a key.
- `switch-profile` is the one command taking an `arg`: the profile to switch to.

### Footer

`label` is what puts a binding on the footer; a binding without one is still
reachable, just not advertised. `priority` orders the footer and decides what
gets dropped first when the window is too narrow. Ties keep file order.

The shipped profiles number their priorities column-wise, so the two rows read
the way nano's do:

```
^G Help    ^O Write Out  ^W Where Is  ^K Cut    ^C Location  M-U Undo
^X Exit    ^R Read File  ^\ Replace   ^U Paste  ^_ Go To     M-E Redo
```

## Where the code is

- `crates/nib-core/src/keymap.rs` — chord parsing, file parsing, resolution,
  footer generation.
- `crates/nib-core/src/command.rs` — the command names a keymap may use.
- `crates/nib-harness/src/lib.rs` — feed a chord sequence, assert what it did.
