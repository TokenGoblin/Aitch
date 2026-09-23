# Using Aitch

A guide for people who want to get work done in it. No configuration is
needed to follow along — everything here works out of the box.

If you have used nano, you already know most of this and can skim to
[Folders and projects](#folders-and-projects), which is the part nano has no
equivalent for.

- [The idea in one minute](#the-idea-in-one-minute)
- [Reading the screen](#reading-the-screen)
- [Typing and moving around](#typing-and-moving-around)
- [Selecting, cutting and pasting](#selecting-cutting-and-pasting)
- [Undo](#undo)
- [Opening and saving](#opening-and-saving)
- [Finding things](#finding-things)
- [Folders and projects](#folders-and-projects)
- [Searching a whole project](#searching-a-whole-project)
- [Several files at once](#several-files-at-once)
- [Highlighting and the view](#highlighting-and-the-view)
- [When something goes wrong](#when-something-goes-wrong)
- [Making it yours](#making-it-yours)
- [Every key, in one place](#every-key-in-one-place)

## The idea in one minute

**Open it and type.** There are no modes to be in or escape from. The keys
that are not letters do things, and the two rows across the bottom of the
window say what those keys are. That footer is the manual: it changes with
whatever you are doing, so the keys it offers are always the keys that
currently work.

Anything that needs an answer — a filename, a search term, yes or no — is
asked on one line just above the footer. Nothing ever opens in a floating
window on top of your text.

Two conventions, from nano, used throughout this guide:

| Written | Means |
|---|---|
| `^O` | Hold **Ctrl** and press **O** |
| `M-U` | Hold **Alt** and press **U** |

Case does not matter: `^O` is Ctrl and the O key, not Ctrl+Shift+O.

## Reading the screen

```
┌──────────────────────────────────────────────────────┐
│ notes.txt  Modified                    ← status line │
│                                                      │
│ Everything above the bottom three rows is your text. │
│                                                      │
│ File Name to Write: notes.txt          ← prompt line │
│ ^G Help    ^O Write Out  ^W Where Is   ← footer      │
│ ^X Exit    ^R Read File  ^\ Replace                  │
└──────────────────────────────────────────────────────┘
```

The **status line** at the top names the file and says `Modified` when there
are unsaved changes. It is also where short messages appear — `Wrote 3 lines`,
`copied`, `settings reloaded` — for one keystroke, after which the filename
comes back.

The **prompt line** only exists when something is being asked.

The **footer** is two rows of shortcuts and never becomes three.

## Typing and moving around

Type. Backspace and Delete work. So does everything you expect:

| Key | Goes to |
|---|---|
| Arrows | One character or line |
| `Ctrl+Left` / `Ctrl+Right` | Previous or next word |
| `Home` / `End` | Start or end of the line |
| `PgUp` / `PgDn` | A screen at a time |
| `Ctrl+Home` / `Ctrl+End` | Top or bottom of the file |
| `^_` | A line number you type |

`^_` is Ctrl and the underscore key — on most keyboards, Ctrl+Shift+Minus.
It asks for a line, and `12:5` goes to line 12, column 5.

You can also start at a line from the command line:

```
aitch +42 notes.txt        opens at line 42
aitch +42:8 notes.txt      line 42, column 8
```

## Selecting, cutting and pasting

Hold **Shift** with any movement key to select as you go — Shift+Right,
Shift+Down, Shift+Ctrl+Right, Shift+End, all of them.

nano's way also works: `^6` sets a mark, and everything you move over
afterwards is selected until you cut it or press `^6` again.

`Ctrl+Shift+A` selects the whole file. (Plain `^A` goes to the start of the
line, as it does in nano.)

| Key | Does |
|---|---|
| `^K` | Cut. With no selection, cuts the whole line |
| `^U` | Uncut — pastes what `^K` took |
| `M-6` or `Ctrl+Shift+C` | Copy |
| `Ctrl+Shift+V` | Paste from the system clipboard |

The clipboard keys take Shift because plain `^C` and `^V` mean other things
in this profile — `^C` reports where the cursor is, `^V` is a page down, both
as in nano. The `modern` keymap gives you the usual `^C`/`^X`/`^V`; see
[Highlighting and the view](#highlighting-and-the-view).

`^K` and `^U` are a pair and use their own buffer, the way nano does: several
`^K`s in a row collect several lines, and one `^U` puts them all back. Copy
and paste use the system clipboard, so they work with other applications.

## Undo

`M-U` undoes and `M-E` redoes. Undo works in the units you would expect: a
run of typing is one step, not one step per letter, and a paste is one step
however much it pasted.

## Opening and saving

| Key | Does |
|---|---|
| `^O` | Write out. Saves; asks for a name if the buffer has none |
| `^R` | Read a file into a new buffer |
| `^X` | Exit. Asks first if anything is unsaved |

Saving is careful about two things. **Your file keeps the encoding and line
endings it arrived with** — open a UTF-16 file with Windows line endings,
change one word, save, and only that word is different. And if something else
changed the file while you had it open, Aitch says so and asks rather than
overwriting it.

From a terminal:

```
aitch                      an empty buffer, or what you had open last time
aitch notes.txt            a file — it need not exist yet
aitch src/                 a folder
git log | aitch            whatever was piped in
```

To make it your editor for other programs:

```sh
export EDITOR=aitch        # bash, zsh
setx EDITOR aitch          # Windows, once
```

## Finding things

`^W` — "where is" — searches as you type and wraps around the end of the
file. Once it has found something, `M-W` (or `F3`) jumps to the next match
and `M-Q` (or `Shift+F3`) to the previous one.

`^\` replaces. It asks what to find, then what to put in its place, then
walks you through the matches: **y** replaces this one, **n** skips it, **a**
does all the rest without asking.

Two things worth knowing:

- **Smart case.** A search in lower case ignores case; put a capital in it
  and the capital has to match. `todo` finds `TODO`, `Todo` does not.
- **Literal by default.** Searching for `foo(bar)` finds `foo(bar)`, not a
  regular expression. There is a toggle on the prompt when you want one.

## Folders and projects

Open a folder and Aitch becomes a project editor:

```
aitch .
aitch path/to/project
```

| Key | Does |
|---|---|
| `M-T` | Show or hide the file tree |
| `^T` | Find a file by typing part of its name |

The **tree** takes the arrow keys while it is up: Up and Down walk it, Enter
opens a file or expands a folder, and `M-T` puts it away. Nothing in
`.gitignore` appears in it.

**`^T` is the fast way.** Type any part of a path — `mainrs`, `srcmain`,
`config` — and the closest matches appear as you type. Enter opens the one at
the top; the arrows pick another. On a project of eighty thousand files this
still answers in single-digit milliseconds.

There is deliberately **no tab bar**. Open files live on the prompt line with
everything else — see [Several files at once](#several-files-at-once).

## Searching a whole project

`M-^W` — Alt and Ctrl and W together — searches every file under the folder.

Results appear as they are found, `file:line: text`, and Enter opens the one
you have selected at that exact line. It uses the same machinery ripgrep does,
so a first hit on a very large tree arrives in a few milliseconds, and it
respects `.gitignore` throughout.

Typing another character abandons the search in flight and starts again,
which is what makes it feel like searching rather than like waiting.

From the results, `^\` **replaces across the whole project**. It asks what to
put in place, works out the entire plan, and tells you how many occurrences
in how many files *before* writing anything. Nothing is changed until you
agree to that number.

## Several files at once

`^R` and `^T` and the tree all open files alongside what you have already got.

| Key | Does |
|---|---|
| `M-,` | Previous buffer |
| `M-.` | Next buffer |
| `M-B` | List them and pick one |
| `Ctrl+Shift+W` | Close the one you are on |

The status line always names the one you are looking at.

## Highlighting and the view

Syntax highlighting is automatic, from the file's extension, for thirteen
languages: Rust, C, C++, Python, JavaScript, TypeScript, Bash, HTML, CSS,
JSON, TOML, Markdown and YAML. It runs on its own thread, so it never makes typing
wait — a keystroke in a ten-thousand-line file costs about a microsecond of
the frame either way.

| Key | Shows |
|---|---|
| `M-N` | Line numbers |
| `M-P` | Tabs and trailing spaces |
| `M-M` | Switches between the `nano` and `modern` keymaps |

`M-M` is worth knowing about if the Ctrl keys here fight your muscle memory:
the `modern` profile is `^S` to save, `^F` to find, `^C`/`^X`/`^V` for the
clipboard, `^Z`/`^Y` for undo and redo, `^Q` to quit, `^A` to select all,
`^P` for quick open, `^B` for the tree and `Ctrl+Shift+F` to search the
folder. The footer updates to match, because it is generated from whichever
keymap is active.

## When something goes wrong

**You closed it without saving, or it crashed.** Unsaved work is written to a
recovery file a couple of seconds after you stop typing. Open the same file
again and Aitch asks whether to put it back:

```
Unsaved work found for notes.txt (1843 bytes, 2026-09-08 14:02:37 UTC). Restore it?
```

**y** restores it as a single undo step, left unsaved so you can look before
you keep it. **n** deletes it. **Esc** does neither — the work stays where it
is and you are asked again next time. Text typed into a piped or unnamed
buffer is protected the same way.

Recovery files are only ever deleted when you quit deliberately or answer the
question. Nothing tidies them away in the background.

**Something else changed the file while you had it open.** Aitch notices at
save time and asks instead of overwriting. `^R` reads the file back in if you
want their version.

**It is behaving oddly and you have a config file.** `aitch --no-config`
starts without it. If a setting is bad, Aitch says so on the status line and
carries on with the default — it will never refuse to open because of a
config file.

## Making it yours

One optional file. On Windows it goes in `%APPDATA%\aitch\aitchrc.toml`;
elsewhere in `~/.config/aitch/aitchrc.toml`.

```toml
theme = "dark"          # or "light"
keymap = "modern"       # or "nano", or a path to your own
tab_width = 4
expand_tabs = true      # Tab inserts spaces
line_numbers = true
ignore = ["target", "node_modules"]

[font]
family = "Cascadia Mono"
size = 14.0
```

Save it and it applies immediately — no restart, no reload command. Only the
font waits for the next start, and it tells you so.

[`docs/config.md`](config.md) has every setting, and
[`docs/keymap.md`](keymap.md) is how to bind your own keys — the two shipped
keymaps are written in exactly that format, so they are the examples.

## Every key, in one place

`^G` opens the help, which lists every binding in the keymap you are using —
including any you wrote yourself. It is generated, so it cannot drift out of
date.

This is the `nano` profile, which is the default:

| | | | |
|---|---|---|---|
| `^G` Help | `^O` Write Out | `^W` Where Is | `^K` Cut |
| `^X` Exit | `^R` Read File | `^\` Replace | `^U` Uncut |
| `^_` Go To Line | `^T` Quick Open | `M-W` Find Next | `M-6` Copy |
| `^6` Set Mark | `M-T` File Tree | `M-Q` Find Previous | `Ctrl+Shift+V` Paste |
| `^C` Where Am I | `M-^W` Search Folder | `M-U` Undo | `M-E` Redo |
| `M-N` Line Numbers | `M-P` Whitespace | `M-M` Switch Keymap | `M-B` Buffer List |
| `Ctrl+Shift+A` Select All | `M-,` `M-.` Buffers | `Ctrl+Shift+W` Close Buffer | `^L` Redraw |

Esc or `^C` backs out of any prompt without doing anything.
