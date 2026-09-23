//! The `Command` enum — the entire editor API surface.
//!
//! The UI never mutates a buffer directly. It resolves input to a [`Command`]
//! and hands it to core. Keymap files name commands in kebab-case; the mapping
//! between those names and this enum lives here and nowhere else.
//!
//! Adding or reshaping a variant is a "stop and ask" change (see `CLAUDE.md`).

use std::fmt;

/// Every action the editor can perform.
///
/// Variants are grouped by the phase that gives them behavior. Phase 0 defines
/// the names only; no variant is implemented yet.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Command {
    // -- Movement (Phase 1) ------------------------------------------------
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    MoveWordLeft,
    MoveWordRight,
    MoveLineStart,
    MoveLineEnd,
    MovePageUp,
    MovePageDown,
    MoveBufferStart,
    MoveBufferEnd,

    // -- Selection and editing (Phase 2) -----------------------------------
    /// nano's `M-A`: start a selection anchored at the cursor.
    SetMark,
    SelectAll,
    /// Run a movement, dragging the selection along with it.
    ///
    /// Shift+arrow and its relatives, expressed once instead of as a twin for
    /// every movement command. Keymaps write `command = "select"` with the
    /// movement in `arg`.
    Select(Box<Command>),
    /// Text the user typed. Not a keybinding — no keymap binds ordinary
    /// letters — so the UI raises it directly from a key event's text.
    InsertText(String),
    InsertNewline,
    InsertTab,
    DeleteBackward,
    DeleteForward,
    DeleteWordBackward,
    DeleteWordForward,
    /// nano's `^K`. Consecutive cuts accumulate into one cut buffer.
    Cut,
    /// nano's `^U`. Pastes the cut buffer.
    Uncut,
    Copy,
    Paste,
    Undo,
    Redo,

    // -- File (Phase 2) ----------------------------------------------------
    /// nano's `^O`.
    WriteOut,
    /// nano's `^R`: insert another file at the cursor.
    ReadFile,
    Quit,

    // -- Search and navigation (Phase 3) -----------------------------------
    /// nano's `^W`.
    WhereIs,
    WhereIsNext,
    WhereIsPrev,
    /// nano's replace, bound to Ctrl+backslash in the nano profile.
    Replace,
    /// nano's goto-line, bound to Ctrl+underscore in the nano profile.
    GotoLine,
    /// nano's `^C`: report the cursor position on the status line.
    CursorPosition,
    Help,
    Refresh,
    /// Show or hide the line-number gutter.
    ToggleLineNumbers,
    /// Show or hide tabs and trailing spaces.
    ToggleWhitespace,

    // -- Prompt context (Phase 3) ------------------------------------------
    PromptAccept,
    PromptCancel,
    PromptHistoryPrev,
    PromptHistoryNext,

    // -- Workspace (Phase 4) -----------------------------------------------
    ToggleTree,
    QuickOpen,
    NextBuffer,
    PrevBuffer,
    BufferList,
    CloseBuffer,

    // -- Project search (Phase 6) ------------------------------------------
    ProjectSearch,

    // -- Meta --------------------------------------------------------------
    /// Switch the active keymap profile by name, e.g. `modern`.
    SwitchProfile(String),
}

impl Command {
    /// The kebab-case name used in keymap files.
    ///
    /// For [`Command::SwitchProfile`] this returns the bare name; the profile
    /// travels in the binding's `arg` field.
    pub fn name(&self) -> &'static str {
        use Command::*;
        match self {
            MoveLeft => "move-left",
            MoveRight => "move-right",
            MoveUp => "move-up",
            MoveDown => "move-down",
            MoveWordLeft => "move-word-left",
            MoveWordRight => "move-word-right",
            MoveLineStart => "move-line-start",
            MoveLineEnd => "move-line-end",
            MovePageUp => "move-page-up",
            MovePageDown => "move-page-down",
            MoveBufferStart => "move-buffer-start",
            MoveBufferEnd => "move-buffer-end",
            SetMark => "set-mark",
            SelectAll => "select-all",
            Select(_) => "select",
            InsertText(_) => "insert-text",
            InsertNewline => "insert-newline",
            InsertTab => "insert-tab",
            DeleteBackward => "delete-backward",
            DeleteForward => "delete-forward",
            DeleteWordBackward => "delete-word-backward",
            DeleteWordForward => "delete-word-forward",
            Cut => "cut",
            Uncut => "uncut",
            Copy => "copy",
            Paste => "paste",
            Undo => "undo",
            Redo => "redo",
            WriteOut => "write-out",
            ReadFile => "read-file",
            Quit => "quit",
            WhereIs => "where-is",
            WhereIsNext => "where-is-next",
            WhereIsPrev => "where-is-prev",
            Replace => "replace",
            GotoLine => "goto-line",
            CursorPosition => "cursor-position",
            Help => "help",
            Refresh => "refresh",
            ToggleLineNumbers => "toggle-line-numbers",
            ToggleWhitespace => "toggle-whitespace",
            PromptAccept => "prompt-accept",
            PromptCancel => "prompt-cancel",
            PromptHistoryPrev => "prompt-history-prev",
            PromptHistoryNext => "prompt-history-next",
            ToggleTree => "toggle-tree",
            QuickOpen => "quick-open",
            NextBuffer => "next-buffer",
            PrevBuffer => "prev-buffer",
            BufferList => "buffer-list",
            CloseBuffer => "close-buffer",
            ProjectSearch => "project-search",
            SwitchProfile(_) => "switch-profile",
        }
    }

    /// Whether a command name requires an `arg` in its keymap binding.
    pub fn takes_arg(name: &str) -> bool {
        matches!(name, "switch-profile" | "select" | "insert-text")
    }

    /// Whether this command only moves the cursor, leaving the text alone.
    ///
    /// [`Command::Select`] wraps one of these; anything else would mean
    /// "extend the selection by deleting a word", which is not a thing.
    pub fn is_movement(&self) -> bool {
        use Command::*;
        matches!(
            self,
            MoveLeft
                | MoveRight
                | MoveUp
                | MoveDown
                | MoveWordLeft
                | MoveWordRight
                | MoveLineStart
                | MoveLineEnd
                | MovePageUp
                | MovePageDown
                | MoveBufferStart
                | MoveBufferEnd
        )
    }

    /// Build a command from its keymap name and optional argument.
    pub fn from_name(name: &str, arg: Option<&str>) -> Result<Command, UnknownCommand> {
        use Command::*;

        if arg.is_some() && !Command::takes_arg(name) {
            return Err(UnknownCommand {
                name: name.to_string(),
                reason: Reason::UnexpectedArg,
            });
        }

        let cmd = match name {
            "move-left" => MoveLeft,
            "move-right" => MoveRight,
            "move-up" => MoveUp,
            "move-down" => MoveDown,
            "move-word-left" => MoveWordLeft,
            "move-word-right" => MoveWordRight,
            "move-line-start" => MoveLineStart,
            "move-line-end" => MoveLineEnd,
            "move-page-up" => MovePageUp,
            "move-page-down" => MovePageDown,
            "move-buffer-start" => MoveBufferStart,
            "move-buffer-end" => MoveBufferEnd,
            "set-mark" => SetMark,
            "select-all" => SelectAll,
            "select" => {
                let movement = arg.ok_or_else(|| UnknownCommand {
                    name: name.to_string(),
                    reason: Reason::MissingArg,
                })?;
                let inner = Command::from_name(movement, None)?;
                if !inner.is_movement() {
                    return Err(UnknownCommand {
                        name: name.to_string(),
                        reason: Reason::NotAMovement,
                    });
                }
                Select(Box::new(inner))
            }
            "insert-text" => {
                let text = arg.ok_or_else(|| UnknownCommand {
                    name: name.to_string(),
                    reason: Reason::MissingArg,
                })?;
                InsertText(text.to_string())
            }
            "insert-newline" => InsertNewline,
            "insert-tab" => InsertTab,
            "delete-backward" => DeleteBackward,
            "delete-forward" => DeleteForward,
            "delete-word-backward" => DeleteWordBackward,
            "delete-word-forward" => DeleteWordForward,
            "cut" => Cut,
            "uncut" => Uncut,
            "copy" => Copy,
            "paste" => Paste,
            "undo" => Undo,
            "redo" => Redo,
            "write-out" => WriteOut,
            "read-file" => ReadFile,
            "quit" => Quit,
            "where-is" => WhereIs,
            "where-is-next" => WhereIsNext,
            "where-is-prev" => WhereIsPrev,
            "replace" => Replace,
            "goto-line" => GotoLine,
            "cursor-position" => CursorPosition,
            "help" => Help,
            "refresh" => Refresh,
            "toggle-line-numbers" => ToggleLineNumbers,
            "toggle-whitespace" => ToggleWhitespace,
            "prompt-accept" => PromptAccept,
            "prompt-cancel" => PromptCancel,
            "prompt-history-prev" => PromptHistoryPrev,
            "prompt-history-next" => PromptHistoryNext,
            "toggle-tree" => ToggleTree,
            "quick-open" => QuickOpen,
            "next-buffer" => NextBuffer,
            "prev-buffer" => PrevBuffer,
            "buffer-list" => BufferList,
            "close-buffer" => CloseBuffer,
            "project-search" => ProjectSearch,
            "switch-profile" => {
                let profile = arg.ok_or_else(|| UnknownCommand {
                    name: name.to_string(),
                    reason: Reason::MissingArg,
                })?;
                SwitchProfile(profile.to_string())
            }
            _ => {
                return Err(UnknownCommand {
                    name: name.to_string(),
                    reason: Reason::NoSuchCommand,
                })
            }
        };

        Ok(cmd)
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Command::SwitchProfile(p) => write!(f, "switch-profile({p})"),
            Command::Select(inner) => write!(f, "select({inner})"),
            Command::InsertText(text) => write!(f, "insert-text({text:?})"),
            other => f.write_str(other.name()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Reason {
    NoSuchCommand,
    MissingArg,
    UnexpectedArg,
    NotAMovement,
}

/// A keymap file named a command that does not exist, or misused its argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownCommand {
    name: String,
    reason: Reason,
}

impl fmt::Display for UnknownCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.reason {
            Reason::NoSuchCommand => write!(f, "unknown command `{}`", self.name),
            Reason::MissingArg => write!(f, "command `{}` requires an `arg`", self.name),
            Reason::UnexpectedArg => write!(f, "command `{}` takes no `arg`", self.name),
            Reason::NotAMovement => write!(
                f,
                "command `{}` can only wrap a movement command",
                self.name
            ),
        }
    }
}

impl std::error::Error for UnknownCommand {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        let commands = [
            Command::MoveLeft,
            Command::MoveBufferEnd,
            Command::Cut,
            Command::Uncut,
            Command::WriteOut,
            Command::Replace,
            Command::GotoLine,
            Command::PromptAccept,
            Command::ToggleTree,
            Command::ProjectSearch,
        ];
        for cmd in commands {
            assert_eq!(Command::from_name(cmd.name(), None).unwrap(), cmd);
        }
    }

    #[test]
    fn switch_profile_carries_its_arg() {
        let cmd = Command::from_name("switch-profile", Some("modern")).unwrap();
        assert_eq!(cmd, Command::SwitchProfile("modern".to_string()));
        assert_eq!(cmd.to_string(), "switch-profile(modern)");
    }

    #[test]
    fn switch_profile_without_arg_is_an_error() {
        let err = Command::from_name("switch-profile", None).unwrap_err();
        assert!(err.to_string().contains("requires an `arg`"));
    }

    #[test]
    fn arg_on_a_command_that_takes_none_is_an_error() {
        let err = Command::from_name("quit", Some("now")).unwrap_err();
        assert!(err.to_string().contains("takes no `arg`"));
    }

    #[test]
    fn unknown_command_names_itself() {
        let err = Command::from_name("launch-missiles", None).unwrap_err();
        assert_eq!(err.to_string(), "unknown command `launch-missiles`");
    }
}
