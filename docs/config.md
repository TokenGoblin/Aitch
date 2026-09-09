# Configuring Aitch

Settings live in one TOML file. Every key is optional; the file itself is
optional. **A broken config never stops the editor opening** — the problem is
reported on the status line and everything falls back to its default, because
an editor that will not start is worse than an editor with the wrong theme.

## Where it lives

| Platform | Path |
|---|---|
| Windows | `%APPDATA%\aitch\aitchrc.toml` |
| Linux, BSD | `$XDG_CONFIG_HOME/aitch/aitchrc.toml`, or `~/.config/aitch/aitchrc.toml` |
| macOS | `~/.config/aitch/aitchrc.toml` |

`aitch --config path/to/other.toml` reads somewhere else instead.
`aitch --no-config` ignores it entirely, which is the thing to try first when
something behaves oddly. A `--config` path that is not there is reported —
unlike the default path, where having no config file is the normal state of a
fresh install.

Saving the file applies it immediately. There is no reload command and no
restart, apart from the font — see below.

## Everything it can say

```toml
# "dark" or "light".
theme = "dark"

# "nano", "modern", or a path to a keymap file. Writing your own uses the
# same format as the shipped ones: see docs/keymap.md.
keymap = "nano"

# What the Tab key puts in, and how wide a tab is drawn — both, so a file
# that already contains tabs lines up the way you set it to.
tab_width = 4
expand_tabs = false

# What the view shows besides the text. Both toggle at runtime too:
# M-N for numbers, M-P for whitespace.
line_numbers = false
whitespace = false

# Extra ignore rules for the folder tree, quick open and project search, on
# top of .gitignore. Same syntax as .gitignore, negation included.
ignore = ["target", "node_modules", "*.min.js"]

[font]
# A family installed on this machine. Omitted means the system monospace.
# The size is held between 4 and 200: a window that will not open is the
# worst way to find out about a typo.
family = "JetBrains Mono"
size = 14.0
```

An unknown key is an error rather than a shrug, so a typo tells you about
itself instead of quietly doing nothing.

Ignore rules follow `.gitignore` to the letter, which includes its one
surprise: an excluded folder is never opened, so nothing inside it can be
pulled back out. `["vendor/*", "!vendor/keep.js"]` keeps the one file;
`["vendor", "!vendor/keep.js"]` keeps nothing. A rule that will not compile
is skipped rather than fatal.

## What reloads and what does not

Everything above takes effect on save except `[font]`, which is settled when
the window's text atlas is built. Changing it says so on the status line and
applies at the next start.

`tab_width` and `expand_tabs` change what the **Tab key inserts** from that
moment on, and `tab_width` also changes how a tab already in the file is
drawn. Neither goes back and re-indents anything: Aitch never rewrites text
you did not ask it to.

## The command line

```
aitch [OPTIONS] [+LINE[:COLUMN]] [FILE|FOLDER]

    --config PATH   read settings from PATH
    --no-config     ignore aitchrc.toml entirely
    --no-session    do not reopen what was open last time
```

`aitch +42 notes.txt` opens at line 42, `+42:8` at line 42, column 8 — the
same as vi and every editor since. Text piped in becomes an unnamed buffer,
so `git log | aitch` works; naming a file wins over a pipe, so
`echo hi | aitch notes.txt` opens `notes.txt`.

Aitch runs until its window closes, which is what `$EDITOR` requires:

```sh
export EDITOR=aitch
```

## Sessions

Starting `aitch` with no file reopens what was open last time, at the cursor
positions it had. Naming a file, a folder, or passing `--no-session` opens
that instead — getting yesterday's five buffers as well would be a surprise.

A file that has since been deleted or renamed is skipped without comment. A
session is a convenience; being nagged about last week's scratch file every
morning is not one.

## Recovery

Unsaved work is written to a recovery file a couple of seconds after you stop
typing, and the files are deleted when you quit deliberately — surviving *not*
quitting deliberately is their whole purpose.

If the editor did not get to do that, opening the same file again asks:

```
Unsaved work found for notes.txt (1843 bytes, 2026-09-08 14:02:37 UTC). Restore it?
```

Yes puts it back as a single undoable edit, left unsaved so you can compare
before committing to it. No deletes it. **Esc does neither** — the file stays
where it is and you are asked again next time, because "let me think about
it" must never be read as "throw it away".

Work typed into a buffer that has no file — `git log | aitch`, or a new file
you have not saved yet — is protected the same way, and offered back into an
empty unnamed buffer. That is the case where the recovery file really is the
only copy.

Recovery files live beside the session, under `%LOCALAPPDATA%\aitch` or
`$XDG_STATE_HOME/aitch`. A recovery file whose buffer has not been reopened is
left alone, never deleted in the background: it may be the only copy of that
text anywhere.
