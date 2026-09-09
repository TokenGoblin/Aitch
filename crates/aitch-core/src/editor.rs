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
use crate::project::{PathIndex, Tree};
use crate::prompt::{self, Answer, Histories, Kind, Prompt};
use crate::search::{self, Direction, Match, Query};
use crate::workspace::Workspace;

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

/// How many quick-open hits to show. More than fits above the prompt line is
/// wasted work, and the ranking means the answer is near the top or not there.
const RESULT_LIMIT: usize = 50;

/// The help pane: a scrollable list of lines, not a dialog.
#[derive(Debug, Clone, Default)]
pub struct Help {
    pub scroll: usize,
}

/// One editing session.
pub struct Editor {
    workspace: Workspace,
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
    /// The sidebar, when a folder is open and it has been toggled on.
    tree: Option<Tree>,
    /// Built on first use of quick open; walking is the slow half.
    index: Option<PathIndex>,
    /// The list shown above the prompt line, and which row is picked.
    results: Vec<String>,
    result: usize,
    quitting: bool,
}

impl Editor {
    pub fn new(document: Document) -> Editor {
        Editor::with_workspace(Workspace::new(document))
    }

    /// A session over a set of buffers, and optionally a folder.
    pub fn with_workspace(workspace: Workspace) -> Editor {
        Editor {
            workspace,
            viewport: Viewport::new(24),
            keymap: Keymap::nano(),
            context: Context::Editor,
            prompt: None,
            histories: Histories::default(),
            status: None,
            query: None,
            help: None,
            tree: None,
            index: None,
            results: Vec::new(),
            result: 0,
            quitting: false,
        }
    }

    // -- what is on screen -------------------------------------------------

    pub fn document(&self) -> &Document {
        self.workspace.active()
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    pub fn workspace_mut(&mut self) -> &mut Workspace {
        &mut self.workspace
    }

    pub fn buffer(&self) -> &Buffer {
        &self.workspace.active().buffer
    }

    pub fn buffer_mut(&mut self) -> &mut Buffer {
        &mut self.workspace.active_mut().buffer
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

    /// The sidebar, when it is showing.
    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }

    /// The list above the prompt line: quick-open hits, or open buffers.
    pub fn results(&self) -> &[String] {
        &self.results
    }

    /// Which row of that list is picked.
    pub fn result_index(&self) -> usize {
        self.result
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
        let modified = if self.workspace.active().is_dirty() {
            "  Modified"
        } else {
            ""
        };
        format!("{}{modified}", self.workspace.active().display_name())
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
            Context::Tree => self.run_in_tree(command),
            _ => self.run_in_editor(command),
        };

        match (outcome, had_status) {
            (Outcome::Nothing, true) => Outcome::Redraw,
            (outcome, _) => outcome,
        }
    }

    fn run_in_editor(&mut self, command: &Command) -> Outcome {
        match self
            .workspace
            .active_mut()
            .buffer
            .apply(command, &self.viewport)
        {
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

            Command::Copy => match self.workspace.active().buffer.selected_text() {
                Some(text) => Outcome::Copy(text),
                None => {
                    self.say("nothing is selected");
                    Outcome::Redraw
                }
            },
            Command::Paste => Outcome::Paste,

            Command::ToggleTree => self.toggle_tree(),
            Command::QuickOpen => self.open_quick_open(),
            Command::BufferList => self.open_buffer_list(),
            Command::NextBuffer => self.cycle_buffer(true),
            Command::PrevBuffer => self.cycle_buffer(false),
            Command::CloseBuffer => self.close_buffer(),

            Command::Refresh => {
                if let Some(tree) = self.tree.as_mut() {
                    tree.refresh();
                }
                if self.workspace.active().changed_on_disk() {
                    self.say("this file has changed on disk since you opened it");
                }
                Outcome::Redraw
            }
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

    /// The sidebar has focus: move about it, open what is picked, or leave.
    fn run_in_tree(&mut self, command: &Command) -> Outcome {
        if self.tree.is_none() {
            self.context = Context::Editor;
            return Outcome::Redraw;
        }

        // Leaving is handled before the tree is borrowed.
        match command {
            // From inside, M-T puts the sidebar away entirely.
            Command::ToggleTree => {
                self.tree = None;
                self.context = Context::Editor;
                return Outcome::Redraw;
            }
            // Escape only gives the keys back; the sidebar stays.
            Command::PromptCancel => {
                self.context = Context::Editor;
                return Outcome::Redraw;
            }
            Command::QuickOpen => return self.open_quick_open(),
            Command::Help => return self.open_help(),
            Command::Quit => return self.begin_quit(),
            _ => {}
        }

        let page = self.viewport.height_lines().saturating_sub(1) as isize;
        let tree = self.tree.as_mut().expect("checked above");

        let moved = match command {
            Command::MoveUp => tree.move_up(),
            Command::MoveDown => tree.move_down(),
            Command::MovePageUp => tree.move_by(-page),
            Command::MovePageDown => tree.move_by(page),
            Command::MoveBufferStart => tree.select(0),
            Command::MoveBufferEnd => tree.select(usize::MAX),
            Command::Refresh => {
                tree.refresh();
                true
            }
            Command::PromptAccept => match tree.activate() {
                Some(path) => return self.open_path(&path),
                // A folder opened or shut; the rows changed.
                None => true,
            },
            _ => false,
        };

        if moved {
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

            // With a list on screen the arrows move through it. Past answers
            // are the obvious meaning only when there is nothing to pick from.
            Command::PromptHistoryPrev if prompt.kind.has_results() => {
                let moved = self.result > 0;
                self.result = self.result.saturating_sub(1);
                moved
            }
            Command::PromptHistoryNext if prompt.kind.has_results() => {
                let last = self.results.len().saturating_sub(1);
                let moved = self.result < last;
                self.result = (self.result + 1).min(last);
                moved
            }

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
            if self.prompt.as_ref().is_some_and(|p| p.kind.has_results()) {
                self.refresh_results();
            }
            Outcome::Redraw
        } else {
            Outcome::Nothing
        }
    }

    // -- prompts -----------------------------------------------------------

    fn open_prompt(&mut self, kind: Kind) -> Outcome {
        let origin = self.workspace.active().buffer.cursor_char();
        let prefill = match &kind {
            Kind::SaveAs => self
                .workspace
                .active()
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
                let position = self.workspace.active().buffer.char_to_position(origin);
                self.workspace.active_mut().buffer.clear_selection();
                self.workspace.active_mut().buffer.set_cursor(position);
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
                self.workspace
                    .active_mut()
                    .set_path(PathBuf::from(input.trim()));
                self.write_out()
            }

            Kind::GotoLine => match prompt::parse_goto(&input) {
                Some((line, column)) => {
                    let last = self.workspace.active().buffer.len_lines().saturating_sub(1);
                    self.workspace
                        .active_mut()
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

            Kind::OverwriteChanged => Outcome::Redraw,

            Kind::QuickOpen => match self.results.get(self.result).cloned() {
                Some(relative) => {
                    let path = match &self.index {
                        Some(index) => index.resolve(&relative),
                        None => PathBuf::from(&relative),
                    };
                    self.results.clear();
                    self.open_path(&path)
                }
                None => {
                    self.say("nothing to open");
                    Outcome::Redraw
                }
            },

            Kind::BufferList => {
                let picked = self.result;
                self.results.clear();
                self.workspace.activate(picked);
                self.follow_cursor();
                Outcome::Redraw
            }

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
                    if self.workspace.active().is_dirty() {
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

            Kind::OverwriteChanged => match answer {
                Answer::Yes => self.write_out_now(),
                _ => {
                    self.say("not saved — press ^R to read the file back in");
                    Outcome::Redraw
                }
            },

            _ => Outcome::Redraw,
        }
    }

    // -- the commands that do work ----------------------------------------

    fn begin_quit(&mut self) -> Outcome {
        if !self.workspace.active().is_dirty() {
            self.quitting = true;
            return Outcome::Quit;
        }
        self.open_prompt(Kind::SaveBeforeQuit)
    }

    fn write_out(&mut self) -> Outcome {
        if self.workspace.active().path().is_none() {
            return self.open_prompt(Kind::SaveAs);
        }
        // Never silently overwrite: something else may have written the file
        // since it was read, and the buffer knows nothing about that change.
        if self.workspace.active().changed_on_disk() {
            return self.open_prompt(Kind::OverwriteChanged);
        }
        self.write_out_now()
    }

    /// Write without asking. Only reached once the question is settled.
    fn write_out_now(&mut self) -> Outcome {
        match self.workspace.active_mut().save() {
            Ok(()) => {
                let lines = self.workspace.active().buffer.len_lines();
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
                self.workspace
                    .active_mut()
                    .buffer
                    .apply(&command, &self.viewport);
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
        let position = self.workspace.active().buffer.cursor();
        let lines = self.workspace.active().buffer.len_lines();
        let characters = self.workspace.active().buffer.len_chars();
        let at = self.workspace.active().buffer.cursor_char();
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
            self.workspace.active_mut().buffer.clear_selection();
            let position = self.workspace.active().buffer.char_to_position(origin);
            self.workspace.active_mut().buffer.set_cursor(position);
            self.follow_cursor();
            return;
        }

        let query = Query::new(term).direction(direction);
        if let Some(found) = search::find(self.workspace.active().buffer.text(), &query, origin) {
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
            Direction::Forward => self.workspace.active().buffer.cursor_char() + 1,
            Direction::Backward => self
                .workspace
                .active()
                .buffer
                .cursor_char()
                .saturating_sub(1),
        };
        self.query = Some(query);
        self.run_search(from)
    }

    fn run_search(&mut self, from: usize) -> Outcome {
        let Some(query) = self.query.clone() else {
            return Outcome::Redraw;
        };
        match search::find(self.workspace.active().buffer.text(), &query, from) {
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
        self.workspace
            .active_mut()
            .buffer
            .select_range(found.start, found.end);
        self.follow_cursor();
    }

    // -- replacing ---------------------------------------------------------

    fn begin_replace(&mut self, find: String, replace: String) -> Outcome {
        let query = Query::new(find.clone());
        let from = self.workspace.active().buffer.cursor_char();
        self.query = Some(query.clone());

        match search::find(self.workspace.active().buffer.text(), &query, from) {
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
                self.workspace.active_mut().buffer.clear_selection();
                self.say(format!("Replaced {done} occurrences"));
                return Outcome::Redraw;
            }

            Answer::All => {
                // "All" means this one too. The match under the cursor is
                // already selected, so it is replaced before the loop starts —
                // otherwise answering `a` on the first match would skip it.
                self.workspace
                    .active_mut()
                    .buffer
                    .apply(&Command::InsertText(replace.clone()), &self.viewport);
                let mut count = done + 1;
                let mut from = self.workspace.active().buffer.cursor_char();
                while let Some(found) =
                    search::find(self.workspace.active().buffer.text(), &query, from)
                {
                    if found.wrapped {
                        break;
                    }
                    self.workspace
                        .active_mut()
                        .buffer
                        .select_range(found.start, found.end);
                    self.workspace
                        .active_mut()
                        .buffer
                        .apply(&Command::InsertText(replace.clone()), &self.viewport);
                    from = self.workspace.active().buffer.cursor_char();
                    count += 1;
                }
                self.workspace.active_mut().buffer.clear_selection();
                self.follow_cursor();
                self.say(format!("Replaced {count} occurrences"));
                return Outcome::Redraw;
            }

            Answer::Yes => {
                self.workspace
                    .active_mut()
                    .buffer
                    .apply(&Command::InsertText(replace.clone()), &self.viewport);
                done + 1
            }

            // Skip this one and look for the next.
            Answer::No => {
                let at = self.workspace.active().buffer.cursor_char();
                let position = self.workspace.active().buffer.char_to_position(at);
                self.workspace.active_mut().buffer.clear_selection();
                self.workspace.active_mut().buffer.set_cursor(position);
                done
            }
        };

        let from = self.workspace.active().buffer.cursor_char();
        match search::find(self.workspace.active().buffer.text(), &query, from) {
            Some(found) if !found.wrapped => {
                self.show_match(found);
                self.open_prompt(Kind::ReplaceConfirm {
                    find,
                    replace,
                    done,
                })
            }
            _ => {
                self.workspace.active_mut().buffer.clear_selection();
                self.follow_cursor();
                self.say(format!("Replaced {done} occurrences"));
                Outcome::Redraw
            }
        }
    }

    // -- the folder --------------------------------------------------------

    /// `M-T` from the text area: show the sidebar, or step back into it.
    ///
    /// Three states, not two. Opening a file from the tree leaves the sidebar
    /// showing and moves focus to the file, so the next `M-T` should put focus
    /// back rather than hide the thing the user is looking at. Closing it is
    /// `M-T` again from inside — see [`Editor::run_in_tree`].
    fn toggle_tree(&mut self) -> Outcome {
        if self.tree.is_some() {
            self.context = Context::Tree;
            return Outcome::Redraw;
        }

        let Some(root) = self.workspace.root().map(Path::to_path_buf) else {
            self.say("no folder is open — start Aitch with a folder");
            return Outcome::Redraw;
        };
        self.tree = Some(Tree::new(root));
        self.context = Context::Tree;
        Outcome::Redraw
    }

    fn open_quick_open(&mut self) -> Outcome {
        let Some(root) = self.workspace.root().map(Path::to_path_buf) else {
            self.say("no folder is open — start Aitch with a folder");
            return Outcome::Redraw;
        };

        // Built once. The walk is the slow half and the matching is the fast
        // half, and only the fast half runs on a keystroke.
        if self.index.is_none() {
            self.index = Some(PathIndex::build(&root));
        }

        let outcome = self.open_prompt(Kind::QuickOpen);
        self.refresh_results();
        outcome
    }

    fn open_buffer_list(&mut self) -> Outcome {
        let outcome = self.open_prompt(Kind::BufferList);
        self.result = self.workspace.active_index();
        self.refresh_results();
        outcome
    }

    /// Recompute the list above the prompt line for whatever is being asked.
    fn refresh_results(&mut self) {
        let Some(prompt) = &self.prompt else {
            self.results.clear();
            return;
        };
        let query = prompt.input().to_string();

        self.results = match prompt.kind {
            Kind::QuickOpen => match &self.index {
                Some(index) => index.search(&query, RESULT_LIMIT),
                None => Vec::new(),
            },
            Kind::BufferList => self.workspace.listing(),
            _ => Vec::new(),
        };
        self.result = self.result.min(self.results.len().saturating_sub(1));
    }

    fn cycle_buffer(&mut self, forward: bool) -> Outcome {
        let moved = if forward {
            self.workspace.next_buffer()
        } else {
            self.workspace.previous_buffer()
        };
        if !moved {
            self.say("only one buffer is open");
            return Outcome::Redraw;
        }
        self.follow_cursor();
        let name = self.workspace.active().display_name();
        self.say(name);
        Outcome::Redraw
    }

    fn close_buffer(&mut self) -> Outcome {
        if self.workspace.active().is_dirty() {
            self.say("this buffer has unsaved changes");
            return Outcome::Redraw;
        }
        if !self.workspace.close_active() {
            // The last buffer: closing it means leaving.
            return self.begin_quit();
        }
        self.follow_cursor();
        Outcome::Redraw
    }

    /// Open a file into the workspace and show it.
    fn open_path(&mut self, path: &Path) -> Outcome {
        match self.workspace.open(path) {
            Ok(()) => {
                self.context = Context::Editor;
                self.close_prompt();
                self.follow_cursor();
                let name = self.workspace.active().display_name();
                self.say(name);
                Outcome::Redraw
            }
            Err(e) => {
                self.say(format!("{e}"));
                Outcome::Redraw
            }
        }
    }

    /// Something in the folder changed. Catch the tree up and say so if the
    /// file being edited is one of the things that moved.
    ///
    /// Never reloads: unsaved work is not ours to throw away, and the save
    /// path asks before overwriting. This only tells the truth about what is
    /// on screen.
    pub fn folder_changed(&mut self) -> Outcome {
        if let Some(tree) = self.tree.as_mut() {
            tree.refresh();
        }
        // The quick-open index is now stale too; rebuild it on next use.
        self.index = None;

        if self.workspace.active().changed_on_disk() {
            self.say("this file has changed on disk since you opened it");
        }
        Outcome::Redraw
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
        self.workspace
            .active_mut()
            .buffer
            .follow_cursor(&mut self.viewport);
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
