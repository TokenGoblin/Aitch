//! The prompt line: one line of input above the footer.
//!
//! Everything that would be a modal dialog in another editor happens here —
//! save-as, goto line, search, replace, and the yes/no questions. That is the
//! nano bargain: the answer is always in the same place, one line above the
//! footer, and it never covers the text you are working on.
//!
//! A prompt knows what it is for ([`Kind`]) and what has been typed into it.
//! What to *do* with the answer is `editor.rs`'s business — this module holds
//! no opinion about buffers.

use std::path::PathBuf;

use crate::search::Direction;

/// What a prompt is asking for, and what the answer means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A filename to write to.
    SaveAs,
    /// A line, or `line:column`.
    GotoLine,
    /// A search term. Incremental: the buffer follows as you type.
    Search { direction: Direction },
    /// A filename to insert at the cursor.
    InsertFile,
    /// The search half of a replace.
    ReplaceFind,
    /// The replacement half, once the search term is known.
    ReplaceWith { find: String },
    /// Per-match confirmation: yes, no, all, or cancel.
    ReplaceConfirm {
        find: String,
        replace: String,
        /// How many have been replaced so far, for the closing message.
        done: usize,
    },
    /// Save before quitting?
    SaveBeforeQuit,
}

impl Kind {
    /// Whether this prompt takes typed text, or a single-key answer.
    pub fn is_question(&self) -> bool {
        matches!(self, Kind::ReplaceConfirm { .. } | Kind::SaveBeforeQuit)
    }

    /// Whether the buffer should follow along as the term is typed.
    pub fn is_incremental(&self) -> bool {
        matches!(self, Kind::Search { .. })
    }

    /// Which history list this prompt draws on. Questions have none.
    pub fn history(&self) -> Option<HistoryKind> {
        match self {
            Kind::Search { .. } | Kind::ReplaceFind => Some(HistoryKind::Search),
            Kind::ReplaceWith { .. } => Some(HistoryKind::Replace),
            Kind::SaveAs | Kind::InsertFile => Some(HistoryKind::File),
            Kind::GotoLine => Some(HistoryKind::Goto),
            _ => None,
        }
    }

    /// The label shown before the input, as nano words it.
    pub fn label(&self) -> String {
        match self {
            Kind::SaveAs => "File Name to Write".to_string(),
            Kind::GotoLine => "Enter line number, column number".to_string(),
            Kind::Search { direction } => match direction {
                Direction::Forward => "Search".to_string(),
                Direction::Backward => "Search (backward)".to_string(),
            },
            Kind::InsertFile => "File to insert".to_string(),
            Kind::ReplaceFind => "Search (to replace)".to_string(),
            Kind::ReplaceWith { find } => format!("Replace {find:?} with"),
            Kind::ReplaceConfirm { .. } => "Replace this instance?".to_string(),
            Kind::SaveBeforeQuit => "Save modified buffer?".to_string(),
        }
    }
}

/// Which list of past answers a prompt remembers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HistoryKind {
    Search,
    Replace,
    File,
    Goto,
}

/// The answer to a single-key question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    All,
    Cancel,
}

impl Answer {
    /// Read a keystroke as an answer, the way nano reads it.
    pub fn from_key(c: char) -> Option<Answer> {
        match c.to_ascii_lowercase() {
            'y' => Some(Answer::Yes),
            'n' => Some(Answer::No),
            'a' => Some(Answer::All),
            _ => None,
        }
    }
}

/// One line of input, waiting for an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub kind: Kind,
    input: String,
    /// Cursor position within the input, counted in characters.
    cursor: usize,
    /// Where the buffer cursor was when this prompt opened, so cancelling an
    /// incremental search puts it back.
    pub origin: usize,
    /// How far into this prompt's history the user has walked.
    history_at: Option<usize>,
    /// What was typed before walking into history, to come back to.
    draft: Option<String>,
}

impl Prompt {
    pub fn new(kind: Kind, origin: usize) -> Prompt {
        Prompt {
            kind,
            input: String::new(),
            cursor: 0,
            origin,
            history_at: None,
            draft: None,
        }
    }

    /// Open a prompt already filled in — save-as offers the current name.
    pub fn with_input(mut self, input: impl Into<String>) -> Prompt {
        self.input = input.into();
        self.cursor = self.input.chars().count();
        self
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn label(&self) -> String {
        self.kind.label()
    }

    /// The whole line as it reads on screen: `Search: needle`.
    pub fn line(&self) -> String {
        format!("{}: {}", self.label(), self.input)
    }

    pub fn is_empty(&self) -> bool {
        self.input.is_empty()
    }

    /// The answer as a path, for the prompts that take one.
    pub fn as_path(&self) -> PathBuf {
        PathBuf::from(self.input.trim())
    }

    // -- editing the input -------------------------------------------------

    pub fn insert(&mut self, text: &str) {
        let at = self.byte_offset(self.cursor);
        self.input.insert_str(at, text);
        self.cursor += text.chars().count();
        self.leave_history();
    }

    pub fn delete_backward(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let from = self.byte_offset(self.cursor - 1);
        let to = self.byte_offset(self.cursor);
        self.input.replace_range(from..to, "");
        self.cursor -= 1;
        self.leave_history();
        true
    }

    pub fn delete_forward(&mut self) -> bool {
        if self.cursor >= self.input.chars().count() {
            return false;
        }
        let from = self.byte_offset(self.cursor);
        let to = self.byte_offset(self.cursor + 1);
        self.input.replace_range(from..to, "");
        self.leave_history();
        true
    }

    pub fn move_left(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        true
    }

    pub fn move_right(&mut self) -> bool {
        if self.cursor >= self.input.chars().count() {
            return false;
        }
        self.cursor += 1;
        true
    }

    pub fn move_start(&mut self) -> bool {
        let moved = self.cursor != 0;
        self.cursor = 0;
        moved
    }

    pub fn move_end(&mut self) -> bool {
        let end = self.input.chars().count();
        let moved = self.cursor != end;
        self.cursor = end;
        moved
    }

    // -- history -----------------------------------------------------------

    /// Walk back into past answers. `history` is oldest-first.
    pub fn history_previous(&mut self, history: &[String]) -> bool {
        if history.is_empty() {
            return false;
        }
        let next = match self.history_at {
            // Already at the oldest entry.
            Some(0) => return false,
            Some(at) => at - 1,
            None => {
                self.draft = Some(self.input.clone());
                history.len() - 1
            }
        };
        self.history_at = Some(next);
        self.set_input(history[next].clone());
        true
    }

    /// Walk forward again, and out the other side back to the draft.
    pub fn history_next(&mut self, history: &[String]) -> bool {
        let Some(at) = self.history_at else {
            return false;
        };
        if at + 1 < history.len() {
            self.history_at = Some(at + 1);
            self.set_input(history[at + 1].clone());
        } else {
            self.history_at = None;
            let draft = self.draft.take().unwrap_or_default();
            self.set_input(draft);
        }
        true
    }

    fn set_input(&mut self, input: String) {
        self.input = input;
        self.cursor = self.input.chars().count();
    }

    /// Typing after walking into history keeps what is on screen and forgets
    /// the way back, which is what every shell does.
    fn leave_history(&mut self) {
        self.history_at = None;
        self.draft = None;
    }

    fn byte_offset(&self, chars: usize) -> usize {
        self.input
            .char_indices()
            .nth(chars)
            .map(|(index, _)| index)
            .unwrap_or(self.input.len())
    }
}

/// Past answers, kept per kind so a search does not offer filenames.
#[derive(Debug, Clone, Default)]
pub struct Histories {
    search: Vec<String>,
    replace: Vec<String>,
    file: Vec<String>,
    goto: Vec<String>,
}

impl Histories {
    pub fn get(&self, kind: HistoryKind) -> &[String] {
        match kind {
            HistoryKind::Search => &self.search,
            HistoryKind::Replace => &self.replace,
            HistoryKind::File => &self.file,
            HistoryKind::Goto => &self.goto,
        }
    }

    /// Remember an answer. Blank answers and immediate repeats are not worth
    /// a slot, the same way a shell skips them.
    pub fn remember(&mut self, kind: HistoryKind, answer: &str) {
        if answer.is_empty() {
            return;
        }
        let list = match kind {
            HistoryKind::Search => &mut self.search,
            HistoryKind::Replace => &mut self.replace,
            HistoryKind::File => &mut self.file,
            HistoryKind::Goto => &mut self.goto,
        };
        if list.last().map(String::as_str) == Some(answer) {
            return;
        }
        list.push(answer.to_string());
    }
}

/// Parse a `line` or `line:column` answer into a zero-based position.
///
/// Users count from one, so `1` is the first line. An empty answer, or one
/// that is not a number, is not a position.
pub fn parse_goto(input: &str) -> Option<(usize, usize)> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let (line, column) = match input.split_once(&[':', ','][..]) {
        Some((line, column)) => (line.trim(), column.trim()),
        None => (input, ""),
    };

    let line: usize = line.parse().ok()?;
    let column: usize = if column.is_empty() {
        1
    } else {
        column.parse().ok()?
    };

    Some((line.saturating_sub(1), column.saturating_sub(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt() -> Prompt {
        Prompt::new(Kind::SaveAs, 0)
    }

    #[test]
    fn typing_and_deleting_move_the_cursor_with_the_text() {
        let mut p = prompt();
        p.insert("hello");
        assert_eq!(p.input(), "hello");
        assert_eq!(p.cursor(), 5);

        p.move_left();
        p.insert("X");
        assert_eq!(p.input(), "hellXo");
        assert_eq!(p.cursor(), 5);

        assert!(p.delete_backward());
        assert_eq!(p.input(), "hello");
    }

    #[test]
    fn the_cursor_stops_at_both_ends() {
        let mut p = prompt();
        p.insert("ab");
        assert!(!p.move_right());
        p.move_start();
        assert!(!p.move_left());
        assert!(!p.delete_backward());
    }

    #[test]
    fn multibyte_input_is_edited_by_character() {
        let mut p = prompt();
        p.insert("café");
        assert_eq!(p.cursor(), 4);
        assert!(p.delete_backward());
        assert_eq!(p.input(), "caf");

        p.insert("→日");
        assert_eq!(p.input(), "caf→日");
        p.move_start();
        assert!(p.delete_forward());
        assert_eq!(p.input(), "af→日");
    }

    #[test]
    fn a_prompt_can_open_prefilled() {
        let p = Prompt::new(Kind::SaveAs, 0).with_input("notes.txt");
        assert_eq!(p.input(), "notes.txt");
        assert_eq!(p.cursor(), 9, "cursor waits at the end, ready to edit");
    }

    #[test]
    fn the_line_reads_the_way_nano_words_it() {
        let mut p = Prompt::new(
            Kind::Search {
                direction: Direction::Forward,
            },
            0,
        );
        p.insert("needle");
        assert_eq!(p.line(), "Search: needle");

        let save = Prompt::new(Kind::SaveAs, 0).with_input("a.txt");
        assert_eq!(save.line(), "File Name to Write: a.txt");
    }

    // -- history -----------------------------------------------------------

    #[test]
    fn history_walks_back_through_past_answers() {
        let mut histories = Histories::default();
        for term in ["first", "second", "third"] {
            histories.remember(HistoryKind::Search, term);
        }
        let past = histories.get(HistoryKind::Search);

        let mut p = Prompt::new(
            Kind::Search {
                direction: Direction::Forward,
            },
            0,
        );
        assert!(p.history_previous(past));
        assert_eq!(p.input(), "third", "most recent first");
        p.history_previous(past);
        assert_eq!(p.input(), "second");
        p.history_previous(past);
        assert_eq!(p.input(), "first");
        assert!(!p.history_previous(past), "no further back to go");
    }

    #[test]
    fn walking_forward_comes_back_to_what_was_typed() {
        let mut histories = Histories::default();
        histories.remember(HistoryKind::Search, "old");
        let past = histories.get(HistoryKind::Search);

        let mut p = Prompt::new(
            Kind::Search {
                direction: Direction::Forward,
            },
            0,
        );
        p.insert("draft");
        p.history_previous(past);
        assert_eq!(p.input(), "old");

        p.history_next(past);
        assert_eq!(p.input(), "draft", "the draft was kept");
    }

    #[test]
    fn typing_after_walking_into_history_forgets_the_way_back() {
        let mut histories = Histories::default();
        histories.remember(HistoryKind::Search, "old");
        let past = histories.get(HistoryKind::Search);

        let mut p = Prompt::new(
            Kind::Search {
                direction: Direction::Forward,
            },
            0,
        );
        p.history_previous(past);
        p.insert("er");
        assert_eq!(p.input(), "older");
        assert!(!p.history_next(past), "no longer walking history");
    }

    #[test]
    fn histories_are_kept_apart_by_kind() {
        let mut histories = Histories::default();
        histories.remember(HistoryKind::Search, "needle");
        histories.remember(HistoryKind::File, "notes.txt");

        assert_eq!(histories.get(HistoryKind::Search), ["needle"]);
        assert_eq!(histories.get(HistoryKind::File), ["notes.txt"]);
        assert!(histories.get(HistoryKind::Replace).is_empty());
    }

    #[test]
    fn blank_and_repeated_answers_are_not_remembered() {
        let mut histories = Histories::default();
        histories.remember(HistoryKind::Search, "");
        histories.remember(HistoryKind::Search, "same");
        histories.remember(HistoryKind::Search, "same");
        assert_eq!(histories.get(HistoryKind::Search), ["same"]);
    }

    // -- answers and goto --------------------------------------------------

    #[test]
    fn single_key_answers_are_read_the_way_nano_reads_them() {
        assert_eq!(Answer::from_key('y'), Some(Answer::Yes));
        assert_eq!(Answer::from_key('Y'), Some(Answer::Yes));
        assert_eq!(Answer::from_key('n'), Some(Answer::No));
        assert_eq!(Answer::from_key('a'), Some(Answer::All));
        assert_eq!(Answer::from_key('q'), None);
    }

    #[test]
    fn goto_counts_from_one_because_people_do() {
        assert_eq!(parse_goto("1"), Some((0, 0)));
        assert_eq!(parse_goto("42"), Some((41, 0)));
        assert_eq!(parse_goto("10:5"), Some((9, 4)));
        assert_eq!(parse_goto("10,5"), Some((9, 4)));
        assert_eq!(parse_goto("  7 : 3 "), Some((6, 2)));
    }

    #[test]
    fn goto_line_zero_does_not_underflow() {
        assert_eq!(parse_goto("0"), Some((0, 0)));
        assert_eq!(parse_goto("0:0"), Some((0, 0)));
    }

    #[test]
    fn a_goto_that_is_not_a_number_is_refused() {
        for bad in ["", "   ", "abc", "1:x", "x:1", "-4", "1.5"] {
            assert_eq!(parse_goto(bad), None, "{bad:?} should not parse");
        }
    }

    #[test]
    fn questions_are_told_apart_from_text_prompts() {
        assert!(Kind::SaveBeforeQuit.is_question());
        assert!(Kind::ReplaceConfirm {
            find: "a".into(),
            replace: "b".into(),
            done: 0
        }
        .is_question());
        assert!(!Kind::SaveAs.is_question());
        assert!(Kind::Search {
            direction: Direction::Forward
        }
        .is_incremental());
        assert!(!Kind::SaveAs.is_incremental());
    }
}
