//! The editing session: everything on screen except the pixels.
//!
//! Document, viewport, keymap, context, prompt line, status line and footer
//! all live here, and every command goes through [`Editor::run`]. That is what
//! lets `aitch-harness` drive the whole editor headlessly and assert on what a
//! user would see — which is how nano fidelity gets pinned down.
//!
//! Not in PLAN.md §3's module list, which has `workspace.rs` for "open folder,
//! buffer set, active buffer". Phase 4 will grow that around this; a session
//! with one document is what Phase 3 needs, and putting it in the UI would put
//! the status line and prompt beyond reach of the harness.

use std::path::{Path, PathBuf};

use crate::buffer::{Applied, Buffer, Position, Viewport};
use crate::command::Command;
use crate::document::Document;
use crate::fileio;
use crate::footer::{self, Footer};
use crate::keymap::{Context, Keymap};
use crate::prompt::{self, Answer, Histories, Kind, Prompt};
use crate::search::{self, Direction, Match, Query};

/// What the caller must do after a command.
///
/// The clipboard is the one thing the core cannot do for itself: only the UI
/// can talk to a window server. It is asked for rather than reached for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing changed on screen.
    Nothing,
    /// Something changed; draw again.
    Redraw,
    /// The editor is finished.
    Quit,
    /// Put this text on the system clipboard.
    Copy(String),
    /// Fetch the system clipboard and send it back as `InsertText`.
    Paste,
}

impl Outcome {
    pub fn redraws(&self) -> bool {
        !matches!(self, Outcome::Nothing)
    }
}

/// The help pane: a scrollable list of lines, not a dialog.
#[derive(Debug, Clone, Default)]
pub struct Help {
    pub scroll: usize,
}

/// One editing session.
pub struct Editor {
    document: Document,
    viewport: Viewport,
    keymap: Keymap,
    context: Context,
    prompt: Option<Prompt>,
    histories: Histories,
    /// A transient message. Cleared by the next keystroke, as nano does it,
    /// rather than by a timer — which also makes it testable.
    status: Option<String>,
    /// The last search, so repeating it needs no prompt.
    query: Option<Query>,
    help: Option<Help>,
    quitting: bool,
}

impl Editor {
    pub fn new(document: Document) -> Editor {
        Editor {
            document,
            viewport: Viewport::new(24),
            keymap: Keymap::nano(),
            context: Context::Editor,
            prompt: None,
            histories: Histories::default(),
            status: None,
            query: None,
            help: None,
            quitting: false,
        }
    }

    // -- what is on screen -------------------------------------------------

    pub fn document(&self) -> &Document {
        &self.document
    }

    pub fn buffer(&self) -> &Buffer {
        &self.document.buffer
    }

    pub fn buffer_mut(&mut self) -> &mut Buffer {
        &mut self.document.buffer
    }

    pub fn viewport(&self) -> &Viewport {
        &self.viewport
    }

    pub fn viewport_mut(&mut self) -> &mut Viewport {
        &mut self.viewport
    }

    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    /// Swap the active keymap. Used at startup and by `switch-profile`.
    pub fn set_keymap(&mut self, keymap: Keymap) {
        self.keymap = keymap;
    }

    pub fn context(&self) -> Context {
        self.context
    }

    pub fn prompt(&self) -> Option<&Prompt> {
        self.prompt.as_ref()
    }

    pub fn help(&self) -> Option<&Help> {
        self.help.as_ref()
    }

    pub fn should_quit(&self) -> bool {
        self.quitting
    }

    /// The footer, laid out for a window this many character cells wide.
    pub fn footer(&self, width: usize) -> Footer {
        footer::layout(&self.keymap.footer_entries(self.context), width)
    }

    /// The status line: a transient message if there is one, otherwise what
    /// file this is and whether it has been touched.
    pub fn status_line(&self) -> String {
        if let Some(message) = &self.status {
            return message.clone();
        }
        let modified = if self.document.is_dirty() {
            "  Modified"
        } else {
            ""
        };
        format!("{}{modified}", self.document.display_name())
    }

    /// Set a transient message. It lasts until the next keystroke.
    pub fn say(&mut self, message: impl Into<String>) {
        self.status = Some(message.into());
    }

    /// The help pane's text, generated from the active keymap so it can never
    /// drift from what the keys actually do.
    pub fn help_text(&self) -> Vec<String> {
        let mut lines = vec![
            format!("Aitch help — {} keys", self.keymap.profile()),
            String::new(),
            "Press ^X to leave this help.".to_string(),
            String::new(),
        ];

        for context in [
            Context::Editor,
            Context::Prompt,
            Context::Search,
            Context::Help,
        ] {
            let bindings: Vec<_> = self
                .keymap
                .bindings()
                .iter()
                .filter(|b| b.context == context)
                .collect();
            if bindings.is_empty() {
                continue;
            }

            lines.push(format!("-- {context} --"));
            for binding in bindings {
                let chords: Vec<String> = binding.chords.iter().map(|c| c.to_string()).collect();
                let name = binding
                    .label
                    .clone()
                    .unwrap_or_else(|| binding.command.name().replace('-', " "));
                lines.push(format!("{:<18} {name}", chords.join(" or ")));
            }
            lines.push(String::new());
        }

        lines
    }

    // -- running commands --------------------------------------------------

    /// Run one command and report what the caller must do about it.
    pub fn run(&mut self, command: &Command) -> Outcome {
        // A message lasts until the next keystroke. nano does the same, and
        // it beats a timer that a headless test cannot wait out.
        let had_status = self.status.take().is_some();

        let outcome = match self.context {
            Context::Help => self.run_in_help(command),
            Context::Prompt | Context::Search => self.run_in_prompt(command),
            _ => self.run_in_editor(command),
        };

        match (outcome, had_status) {
            (Outcome::Nothing, true) => Outcome::Redraw,
            (outcome, _) => outcome,
        }
    }

    fn run_in_editor(&mut self, command: &Command) -> Outcome {
        match self.document.buffer.apply(command, &self.viewport) {
            Applied::Changed => {
                self.follow_cursor();
                return Outcome::Redraw;
            }
            Applied::Unchanged => return Outcome::Nothing,
            Applied::Unhandled => {}
        }

        match command {
            Command::Quit => self.begin_quit(),
            Command::WriteOut => self.write_out(),
            Command::ReadFile => self.open_prompt(Kind::InsertFile),
            Command::Help => self.open_help(),

            Command::WhereIs => self.open_prompt(Kind::Search {
                direction: Direction::Forward,
            }),
            Command::WhereIsPrev => self.open_prompt(Kind::Search {
                direction: Direction::Backward,
            }),
            Command::WhereIsNext => self.repeat_search(Direction::Forward),
            Command::Replace => self.open_prompt(Kind::ReplaceFind),
            Command::GotoLine => self.open_prompt(Kind::GotoLine),
            Command::CursorPosition => self.report_position(),

            Command::Copy => match self.document.buffer.selected_text() {
                Some(text) => Outcome::Copy(text),
                None => {
                    self.say("nothing is selected");
                    Outcome::Redraw
                }
            },
            Command::Paste => Outcome::Paste,

            Command::Refresh => Outcome::Redraw,
            Command::SwitchProfile(profile) => match Keymap::by_name(profile) {
                Some(keymap) => {
                    self.keymap = keymap;
                    self.say(format!("{profile} keys"));
                    Outcome::Redraw
                }
                None => {
                    self.say(format!("no keymap called {profile}"));
                    Outcome::Redraw
                }
            },

            // Phases 4 and 6. Saying so beats a key that does nothing.
            other => {
                self.say(format!(
                    "{} is not built yet",
                    other.name().replace('-', " ")
                ));
                Outcome::Redraw
            }
        }
    }

    fn run_in_help(&mut self, command: &Command) -> Outcome {
        if self.help.is_none() {
            self.context = Context::Editor;
            return Outcome::Redraw;
        }

        // Anything that leaves the pane is handled before the scroll state is
        // borrowed, so closing and scrolling never contend for it.
        if matches!(
            command,
            Command::PromptCancel | Command::Quit | Command::Help
        ) {
            self.help = None;
            self.context = Context::Editor;
            return Outcome::Redraw;
        }

        // Both of these read the whole editor, so they are taken before the
        // mutable borrow of the scroll offset.
        let page = self.viewport.height_lines().saturating_sub(1);
        let last = self.help_line_count().saturating_sub(1);

        let Some(help) = self.help.as_mut() else {
            return Outcome::Nothing;
        };
        let scrolled = match command {
            Command::MoveUp => step(&mut help.scroll, -1, last),
            Command::MoveDown => step(&mut help.scroll, 1, last),
            Command::MovePageUp => step(&mut help.scroll, -(page as isize), last),
            Command::MovePageDown => step(&mut help.scroll, page as isize, last),
            Command::MoveBufferStart => step(&mut help.scroll, isize::MIN / 2, last),
            Command::MoveBufferEnd => step(&mut help.scroll, isize::MAX / 2, last),
            _ => false,
        };

        if scrolled {
            Outcome::Redraw
        } else {
            Outcome::Nothing
        }
    }

    fn run_in_prompt(&mut self, command: &Command) -> Outcome {
        let Some(prompt) = self.prompt.as_mut() else {
            self.context = Context::Editor;
            return Outcome::Redraw;
        };

        // A question takes one key, not a line of text.
        if prompt.kind.is_question() {
            return match command {
                Command::PromptCancel => self.answer(Answer::Cancel),
                Command::PromptAccept => self.answer(Answer::Yes),
                Command::InsertText(text) => match text.chars().next().and_then(Answer::from_key) {
                    Some(answer) => self.answer(answer),
                    None => Outcome::Nothing,
                },
                _ => Outcome::Nothing,
            };
        }

        let changed = match command {
            Command::PromptAccept => return self.accept(),
            Command::PromptCancel => return self.cancel(),

            Command::InsertText(text) => {
                prompt.insert(text);
                true
            }
            Command::DeleteBackward => prompt.delete_backward(),
            Command::DeleteForward => prompt.delete_forward(),
            Command::MoveLeft => prompt.move_left(),
            Command::MoveRight => prompt.move_right(),
            Command::MoveLineStart => prompt.move_start(),
            Command::MoveLineEnd => prompt.move_end(),

            Command::PromptHistoryPrev => {
                let kind = prompt.kind.history();
                match kind {
                    Some(kind) => {
                        let past = self.histories.get(kind).to_vec();
                        self.prompt.as_mut().unwrap().history_previous(&past)
                    }
                    None => false,
                }
            }
            Command::PromptHistoryNext => {
                let kind = prompt.kind.history();
                match kind {
                    Some(kind) => {
                        let past = self.histories.get(kind).to_vec();
                        self.prompt.as_mut().unwrap().history_next(&past)
                    }
                    None => false,
                }
            }

            _ => false,
        };

        if changed {
            // An incremental search follows along as the term is typed.
            if self
                .prompt
                .as_ref()
                .is_some_and(|p| p.kind.is_incremental())
            {
                self.search_from_prompt();
            }
            Outcome::Redraw
        } else {
            Outcome::Nothing
        }
    }

    // -- prompts -----------------------------------------------------------

    fn open_prompt(&mut self, kind: Kind) -> Outcome {
        let origin = self.document.buffer.cursor_char();
        let prefill = match &kind {
            Kind::SaveAs => self
                .document
                .path()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            _ => String::new(),
        };

        self.context = match kind {
            Kind::Search { .. } | Kind::ReplaceConfirm { .. } => Context::Search,
            _ => Context::Prompt,
        };
        self.prompt = Some(Prompt::new(kind, origin).with_input(prefill));
        Outcome::Redraw
    }

    fn close_prompt(&mut self) {
        self.prompt = None;
        self.context = Context::Editor;
    }

    fn cancel(&mut self) -> Outcome {
        if let Some(prompt) = &self.prompt {
            // A cancelled incremental search leaves the cursor where it began.
            if prompt.kind.is_incremental() {
                let origin = prompt.origin;
                self.document.buffer.clear_selection();
                self.document
                    .buffer
                    .set_cursor(self.document.buffer.char_to_position(origin));
                self.follow_cursor();
            }
        }
        self.close_prompt();
        self.say("cancelled");
        Outcome::Redraw
    }

    fn accept(&mut self) -> Outcome {
        let Some(prompt) = self.prompt.take() else {
            self.context = Context::Editor;
            return Outcome::Redraw;
        };
        let input = prompt.input().to_string();
        if let Some(kind) = prompt.kind.history() {
            self.histories.remember(kind, &input);
        }
        self.context = Context::Editor;

        match prompt.kind {
            Kind::SaveAs => {
                if input.trim().is_empty() {
                    self.say("cancelled");
                    return Outcome::Redraw;
                }
                self.document.set_path(PathBuf::from(input.trim()));
                self.write_out()
            }

            Kind::GotoLine => match prompt::parse_goto(&input) {
                Some((line, column)) => {
                    let last = self.document.buffer.len_lines().saturating_sub(1);
                    self.document
                        .buffer
                        .set_cursor(Position::new(line.min(last), column));
                    self.follow_cursor();
                    Outcome::Redraw
                }
                None => {
                    self.say(format!("{input:?} is not a line number"));
                    Outcome::Redraw
                }
            },

            Kind::Search { direction } => {
                // Enter on an empty term repeats the last search, as nano does.
                if input.is_empty() {
                    return self.repeat_search(direction);
                }
                self.query = Some(Query::new(input).direction(direction));
                // From where the prompt opened, not from the cursor: the
                // incremental search has already moved the cursor onto the
                // match, and searching from there would skip past the very
                // match the user is looking at and pressed Enter to accept.
                self.run_search(prompt.origin)
            }

            Kind::InsertFile => self.insert_file(Path::new(input.trim())),

            Kind::ReplaceFind => {
                if input.is_empty() {
                    self.say("cancelled");
                    return Outcome::Redraw;
                }
                self.open_prompt(Kind::ReplaceWith { find: input })
            }

            Kind::ReplaceWith { find } => self.begin_replace(find, input),

            // Questions never reach here; they are answered a key at a time.
            Kind::ReplaceConfirm { .. } | Kind::SaveBeforeQuit => Outcome::Redraw,
        }
    }

    fn answer(&mut self, answer: Answer) -> Outcome {
        let Some(prompt) = self.prompt.take() else {
            self.context = Context::Editor;
            return Outcome::Redraw;
        };
        self.context = Context::Editor;

        match prompt.kind {
            Kind::SaveBeforeQuit => match answer {
                Answer::Yes => {
                    let outcome = self.write_out();
                    // Only leave if it actually got written.
                    if self.document.is_dirty() {
                        outcome
                    } else {
                        self.quitting = true;
                        Outcome::Quit
                    }
                }
                Answer::No => {
                    self.quitting = true;
                    Outcome::Quit
                }
                Answer::All | Answer::Cancel => {
                    self.say("cancelled");
                    Outcome::Redraw
                }
            },

            Kind::ReplaceConfirm {
                find,
                replace,
                done,
            } => self.continue_replace(find, replace, done, answer),

            _ => Outcome::Redraw,
        }
    }

    // -- the commands that do work ----------------------------------------

    fn begin_quit(&mut self) -> Outcome {
        if !self.document.is_dirty() {
            self.quitting = true;
            return Outcome::Quit;
        }
        self.open_prompt(Kind::SaveBeforeQuit)
    }

    fn write_out(&mut self) -> Outcome {
        if self.document.path().is_none() {
            return self.open_prompt(Kind::SaveAs);
        }
        match self.document.save() {
            Ok(()) => {
                let lines = self.document.buffer.len_lines();
                self.say(format!("Wrote {lines} lines"));
            }
            Err(e) => self.say(format!("{e}")),
        }
        Outcome::Redraw
    }

    fn insert_file(&mut self, path: &Path) -> Outcome {
        if path.as_os_str().is_empty() {
            self.say("cancelled");
            return Outcome::Redraw;
        }
        match fileio::load(path) {
            Ok(loaded) => {
                let lines = loaded.text.lines().count();
                // Through the same door as typing: nothing bypasses edit.rs.
                let command = Command::InsertText(loaded.text);
                self.document.buffer.apply(&command, &self.viewport);
                self.follow_cursor();
                self.say(format!("Inserted {lines} lines"));
                Outcome::Redraw
            }
            Err(e) => {
                self.say(format!("{e}"));
                Outcome::Redraw
            }
        }
    }

    fn report_position(&mut self) -> Outcome {
        let position = self.document.buffer.cursor();
        let lines = self.document.buffer.len_lines();
        let characters = self.document.buffer.len_chars();
        let at = self.document.buffer.cursor_char();
        // An empty buffer is 0%, not a division by zero.
        let percent = (at * 100).checked_div(characters).unwrap_or(0);
        self.say(format!(
            "line {}/{lines}, col {}, char {at}/{characters} ({percent}%)",
            position.line + 1,
            position.column + 1
        ));
        Outcome::Redraw
    }

    // -- searching ---------------------------------------------------------

    /// Re-run the search as the term is typed, from where the prompt opened.
    fn search_from_prompt(&mut self) {
        let Some(prompt) = &self.prompt else { return };
        let Kind::Search { direction } = prompt.kind else {
            return;
        };
        let term = prompt.input().to_string();
        let origin = prompt.origin;

        if term.is_empty() {
            self.document.buffer.clear_selection();
            let position = self.document.buffer.char_to_position(origin);
            self.document.buffer.set_cursor(position);
            self.follow_cursor();
            return;
        }

        let query = Query::new(term).direction(direction);
        if let Some(found) = search::find(self.document.buffer.text(), &query, origin) {
            self.show_match(found);
        }
        self.query = Some(query);
    }

    fn repeat_search(&mut self, direction: Direction) -> Outcome {
        let Some(query) = self.query.clone() else {
            self.say("no search to repeat");
            return Outcome::Redraw;
        };
        let query = query.direction(direction);
        let from = match direction {
            // Start one past the cursor, or the same match is found again.
            Direction::Forward => self.document.buffer.cursor_char() + 1,
            Direction::Backward => self.document.buffer.cursor_char().saturating_sub(1),
        };
        self.query = Some(query);
        self.run_search(from)
    }

    fn run_search(&mut self, from: usize) -> Outcome {
        let Some(query) = self.query.clone() else {
            return Outcome::Redraw;
        };
        match search::find(self.document.buffer.text(), &query, from) {
            Some(found) => {
                self.show_match(found);
                if found.wrapped {
                    self.say("Search wrapped");
                }
                Outcome::Redraw
            }
            None => {
                self.say(format!("{:?} not found", query.term));
                Outcome::Redraw
            }
        }
    }

    /// Put the cursor on a match and select it, so it is visibly highlighted.
    fn show_match(&mut self, found: Match) {
        self.document.buffer.select_range(found.start, found.end);
        self.follow_cursor();
    }

    // -- replacing ---------------------------------------------------------

    fn begin_replace(&mut self, find: String, replace: String) -> Outcome {
        let query = Query::new(find.clone());
        let from = self.document.buffer.cursor_char();
        self.query = Some(query.clone());

        match search::find(self.document.buffer.text(), &query, from) {
            Some(found) => {
                self.show_match(found);
                self.open_prompt(Kind::ReplaceConfirm {
                    find,
                    replace,
                    done: 0,
                })
            }
            None => {
                self.say(format!("{find:?} not found"));
                Outcome::Redraw
            }
        }
    }

    fn continue_replace(
        &mut self,
        find: String,
        replace: String,
        done: usize,
        answer: Answer,
    ) -> Outcome {
        let query = Query::new(find.clone());

        let done = match answer {
            Answer::Cancel => {
                self.document.buffer.clear_selection();
                self.say(format!("Replaced {done} occurrences"));
                return Outcome::Redraw;
            }

            Answer::All => {
                // "All" means this one too. The match under the cursor is
                // already selected, so it is replaced before the loop starts —
                // otherwise answering `a` on the first match would skip it.
                self.document
                    .buffer
                    .apply(&Command::InsertText(replace.clone()), &self.viewport);
                let mut count = done + 1;
                let mut from = self.document.buffer.cursor_char();
                while let Some(found) = search::find(self.document.buffer.text(), &query, from) {
                    if found.wrapped {
                        break;
                    }
                    self.document.buffer.select_range(found.start, found.end);
                    self.document
                        .buffer
                        .apply(&Command::InsertText(replace.clone()), &self.viewport);
                    from = self.document.buffer.cursor_char();
                    count += 1;
                }
                self.document.buffer.clear_selection();
                self.follow_cursor();
                self.say(format!("Replaced {count} occurrences"));
                return Outcome::Redraw;
            }

            Answer::Yes => {
                self.document
                    .buffer
                    .apply(&Command::InsertText(replace.clone()), &self.viewport);
                done + 1
            }

            // Skip this one and look for the next.
            Answer::No => {
                let at = self.document.buffer.cursor_char();
                self.document.buffer.clear_selection();
                self.document
                    .buffer
                    .set_cursor(self.document.buffer.char_to_position(at));
                done
            }
        };

        let from = self.document.buffer.cursor_char();
        match search::find(self.document.buffer.text(), &query, from) {
            Some(found) if !found.wrapped => {
                self.show_match(found);
                self.open_prompt(Kind::ReplaceConfirm {
                    find,
                    replace,
                    done,
                })
            }
            _ => {
                self.document.buffer.clear_selection();
                self.follow_cursor();
                self.say(format!("Replaced {done} occurrences"));
                Outcome::Redraw
            }
        }
    }

    // -- help --------------------------------------------------------------

    fn open_help(&mut self) -> Outcome {
        self.help = Some(Help::default());
        self.context = Context::Help;
        Outcome::Redraw
    }

    fn help_line_count(&self) -> usize {
        self.help_text().len()
    }

    // -- housekeeping ------------------------------------------------------

    fn follow_cursor(&mut self) {
        self.document.buffer.follow_cursor(&mut self.viewport);
    }
}

/// Move a scroll offset by a signed amount, clamped. True if it moved.
fn step(scroll: &mut usize, delta: isize, last: usize) -> bool {
    let target = if delta >= 0 {
        scroll.saturating_add(delta as usize)
    } else {
        scroll.saturating_sub(delta.unsigned_abs())
    }
    .min(last);
    let moved = target != *scroll;
    *scroll = target;
    moved
}
