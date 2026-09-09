//! Drive Aitch without a window.
//!
//! Feed it chord strings, assert on what came out. This is where keymap,
//! command dispatch and buffer meet, so it is where a wiring mistake shows up
//! that no single unit test would catch.
//!
//! Phase 3 grows it to cover the status line, the prompt line and the footer,
//! and every footer command gets a test here.

use aitch_core::{Applied, Buffer, Chord, ChordParseError, Command, Context, Keymap, KeymapError};
use aitch_core::{Position, Viewport};

/// A headless editor session: a keymap, a buffer, and a log of what happened.
pub struct Harness {
    keymap: Keymap,
    context: Context,
    buffer: Buffer,
    viewport: Viewport,
    commands: Vec<Command>,
    unbound: Vec<Chord>,
    /// Commands the buffer refused, which the real UI would have to handle:
    /// saving, quitting, the system clipboard.
    unhandled: Vec<Command>,
}

impl Harness {
    pub fn new(keymap: Keymap) -> Harness {
        Harness {
            keymap,
            context: Context::Editor,
            buffer: Buffer::new(),
            viewport: Viewport::new(24),
            commands: Vec::new(),
            unbound: Vec::new(),
            unhandled: Vec::new(),
        }
    }

    /// A session using the shipped `nano` profile.
    pub fn nano() -> Harness {
        Harness::new(Keymap::nano())
    }

    /// A session using the shipped `modern` profile.
    pub fn modern() -> Harness {
        Harness::new(Keymap::modern())
    }

    /// A session using a keymap written inline by a test.
    pub fn from_toml(src: &str) -> Result<Harness, KeymapError> {
        Ok(Harness::new(Keymap::from_toml(src)?))
    }

    /// Start from some text instead of an empty buffer.
    pub fn with_text(mut self, text: &str) -> Harness {
        self.buffer = Buffer::from_str(text);
        self
    }

    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    pub fn context(&self) -> Context {
        self.context
    }

    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    pub fn buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffer
    }

    /// The buffer's whole text, for asserting against.
    pub fn text(&self) -> String {
        self.buffer.text().to_string()
    }

    pub fn cursor(&self) -> Position {
        self.buffer.cursor()
    }

    pub fn selection(&self) -> Option<String> {
        self.buffer.selected_text()
    }

    /// Move to another context, as focus changes will once they exist.
    pub fn set_context(&mut self, context: Context) -> &mut Harness {
        self.context = context;
        self
    }

    /// Press one chord: resolve it, run it, and record what happened.
    ///
    /// Unbound chords are recorded too, so a test can assert that a profile
    /// leaves a chord free rather than silently swallowing it.
    pub fn press(&mut self, chord: &str) -> Result<Option<Command>, ChordParseError> {
        let chord = Chord::parse(chord)?;
        let Some(command) = self.keymap.resolve(self.context, chord).cloned() else {
            self.unbound.push(chord);
            return Ok(None);
        };

        self.commands.push(command.clone());
        if self.buffer.apply(&command, &self.viewport) == Applied::Unhandled {
            self.unhandled.push(command.clone());
        }
        self.buffer.follow_cursor(&mut self.viewport);
        Ok(Some(command))
    }

    /// Press a whitespace-separated sequence of chords.
    pub fn feed(&mut self, chords: &str) -> Result<&mut Harness, ChordParseError> {
        for chord in chords.split_whitespace() {
            self.press(chord)?;
        }
        Ok(self)
    }

    /// Type text, one character at a time, the way a keyboard delivers it.
    ///
    /// Typing is not a keybinding, so it does not go through the keymap — but
    /// it does go through a command, exactly as the UI sends it.
    pub fn type_text(&mut self, text: &str) -> &mut Harness {
        for c in text.chars() {
            let command = Command::InsertText(c.to_string());
            self.commands.push(command.clone());
            self.buffer.apply(&command, &self.viewport);
        }
        self.buffer.follow_cursor(&mut self.viewport);
        self
    }

    /// Every command resolved so far, in order.
    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    /// Every chord pressed so far that resolved to nothing.
    pub fn unbound(&self) -> &[Chord] {
        &self.unbound
    }

    /// Commands that reached the buffer and were handed back for the UI.
    pub fn unhandled(&self) -> &[Command] {
        &self.unhandled
    }

    pub fn clear(&mut self) -> &mut Harness {
        self.commands.clear();
        self.unbound.clear();
        self.unhandled.clear();
        self
    }

    /// The footer as it would read right now: `["^G Help", "^X Exit", ..]`.
    pub fn footer(&self) -> Vec<String> {
        self.keymap
            .footer_entries(self.context)
            .into_iter()
            .map(|e| format!("{} {}", e.chord, e.label))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chord_sequence_resolves_to_commands() {
        let mut h = Harness::nano();
        h.feed("^O ^W ^X").unwrap();
        assert_eq!(
            h.commands(),
            [Command::WriteOut, Command::WhereIs, Command::Quit]
        );
    }

    #[test]
    fn the_same_sequence_means_something_else_in_the_modern_profile() {
        let mut h = Harness::modern();
        h.feed("^O ^W ^X").unwrap();
        // ^O is unbound in the modern profile; ^W closes, ^X cuts.
        assert_eq!(h.commands(), [Command::CloseBuffer, Command::Cut]);
        assert_eq!(h.unbound().len(), 1);
    }

    #[test]
    fn context_decides_what_a_chord_means() {
        let mut h = Harness::nano();
        h.set_context(Context::Prompt);
        assert_eq!(h.press("Enter").unwrap(), Some(Command::PromptAccept));

        h.set_context(Context::Editor);
        assert_eq!(h.press("Enter").unwrap(), Some(Command::InsertNewline));
    }

    #[test]
    fn the_footer_reads_like_nano() {
        let h = Harness::nano();
        let footer = h.footer();
        assert_eq!(footer[0], "^G Help");
        assert_eq!(footer[1], "^X Exit");
        assert_eq!(footer[2], "^O Write Out");
        assert!(footer.contains(&"^K Cut".to_string()));
    }

    #[test]
    fn the_modern_footer_reads_like_a_gui() {
        let h = Harness::modern();
        let footer = h.footer();
        assert_eq!(footer[0], "F1 Help");
        assert_eq!(footer[1], "^Q Quit");
        assert_eq!(footer[2], "^S Save");
    }

    #[test]
    fn a_bad_chord_is_an_error_not_a_silent_miss() {
        let mut h = Harness::nano();
        assert!(h.press("Hyper+X").is_err());
    }

    // -- editing, end to end through the keymap ----------------------------

    #[test]
    fn typing_lands_in_the_buffer() {
        let mut h = Harness::nano();
        h.type_text("hello");
        assert_eq!(h.text(), "hello");
        assert_eq!(h.cursor(), Position::new(0, 5));
    }

    #[test]
    fn enter_inserts_a_line_break_in_both_profiles() {
        for mut h in [Harness::nano(), Harness::modern()] {
            h.type_text("one");
            h.press("Enter").unwrap();
            h.type_text("two");
            assert_eq!(h.text(), "one\ntwo", "{}", h.keymap().profile());
        }
    }

    #[test]
    fn backspace_is_bound_the_same_way_in_both_profiles() {
        for mut h in [Harness::nano(), Harness::modern()] {
            h.type_text("hello");
            h.feed("Backspace Backspace").unwrap();
            assert_eq!(h.text(), "hel", "{}", h.keymap().profile());
        }
    }

    #[test]
    fn the_nano_cut_and_uncut_chords_move_a_line() {
        let mut h = Harness::nano().with_text("one\ntwo\nthree\n");
        h.feed("^K").unwrap();
        assert_eq!(h.text(), "two\nthree\n");

        h.feed("Down ^U").unwrap();
        assert_eq!(h.text(), "two\none\nthree\n");
    }

    #[test]
    fn undo_and_redo_are_bound_in_both_profiles() {
        // M-U / M-E in nano, ^Z / ^Y in modern: same commands, different keys.
        let mut nano = Harness::nano();
        nano.type_text("hello");
        nano.feed("M-U").unwrap();
        assert_eq!(nano.text(), "");
        nano.feed("M-E").unwrap();
        assert_eq!(nano.text(), "hello");

        let mut modern = Harness::modern();
        modern.type_text("hello");
        modern.feed("^Z").unwrap();
        assert_eq!(modern.text(), "");
        modern.feed("^Y").unwrap();
        assert_eq!(modern.text(), "hello");
    }

    #[test]
    fn shift_arrows_select_in_both_profiles() {
        for mut h in [Harness::nano(), Harness::modern()] {
            h.type_text("hello world");
            h.feed("Home Shift+Right Shift+Right Shift+Right").unwrap();
            assert_eq!(
                h.selection().as_deref(),
                Some("hel"),
                "{}",
                h.keymap().profile()
            );
        }
    }

    #[test]
    fn the_nano_mark_selects_without_shift() {
        let mut h = Harness::nano().with_text("hello world");
        // ^6 sets the mark, then ordinary movement extends from it.
        h.feed("^6 Ctrl+Right").unwrap();
        assert_eq!(h.selection().as_deref(), Some("hello "));
    }

    #[test]
    fn typing_over_a_selection_replaces_it() {
        let mut h = Harness::nano().with_text("hello world");
        h.feed("Shift+Right Shift+Right Shift+Right Shift+Right Shift+Right")
            .unwrap();
        h.type_text("goodbye");
        assert_eq!(h.text(), "goodbye world");
    }

    #[test]
    fn saving_and_quitting_are_handed_back_to_the_ui() {
        let mut h = Harness::nano();
        h.type_text("hello");
        h.feed("^O ^X").unwrap();
        // The buffer has no filename and no window; both belong to the caller.
        assert_eq!(h.unhandled(), [Command::WriteOut, Command::Quit]);
    }

    #[test]
    fn a_run_of_typing_undoes_as_one_step_through_the_keymap() {
        let mut h = Harness::nano();
        h.type_text("hello world");
        h.feed("M-U").unwrap();
        assert_eq!(h.text(), "", "the whole run went at once");
    }
}
