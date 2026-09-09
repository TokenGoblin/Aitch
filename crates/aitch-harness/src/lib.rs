//! Drive Aitch without a window.
//!
//! Feed it chord strings; assert on the buffer, the status line, the prompt
//! and the footer — everything a user would see except the pixels. This is
//! where nano fidelity is pinned down, so the tests read like someone sitting
//! at the editor rather than like unit tests of a struct.

use aitch_core::{
    Chord, ChordParseError, Command, Context, Document, Editor, Footer, Keymap, KeymapError,
    Outcome, Position,
};

/// A headless editor session.
pub struct Harness {
    editor: Editor,
    commands: Vec<Command>,
    unbound: Vec<Chord>,
    outcomes: Vec<Outcome>,
}

impl Harness {
    pub fn new(keymap: Keymap, document: Document) -> Harness {
        let mut editor = Editor::new(document);
        editor.set_keymap(keymap);
        Harness {
            editor,
            commands: Vec::new(),
            unbound: Vec::new(),
            outcomes: Vec::new(),
        }
    }

    /// A session using the shipped `nano` profile.
    pub fn nano() -> Harness {
        Harness::new(Keymap::nano(), Document::blank())
    }

    /// A session using the shipped `modern` profile.
    pub fn modern() -> Harness {
        Harness::new(Keymap::modern(), Document::blank())
    }

    /// A session using a keymap written inline by a test.
    pub fn from_toml(src: &str) -> Result<Harness, KeymapError> {
        Ok(Harness::new(Keymap::from_toml(src)?, Document::blank()))
    }

    /// Start from some text instead of an empty buffer.
    pub fn with_text(self, text: &str) -> Harness {
        let mut document = Document::blank();
        document.buffer = aitch_core::Buffer::from_str(text);
        self.with_document(document)
    }

    /// Start from a file on disk.
    pub fn with_document(mut self, document: Document) -> Harness {
        let keymap = self.editor.keymap().clone();
        self.editor = Editor::new(document);
        self.editor.set_keymap(keymap);
        self
    }

    /// Start from a folder, as `aitch some/folder` does.
    pub fn with_workspace(self, workspace: aitch_core::Workspace) -> Harness {
        let keymap = self.editor.keymap().clone();
        let mut harness = Harness {
            editor: Editor::with_workspace(workspace),
            commands: self.commands,
            unbound: self.unbound,
            outcomes: self.outcomes,
        };
        harness.editor.set_keymap(keymap);
        harness
    }

    /// The sidebar rows as they would read, when it is showing.
    pub fn tree_rows(&self) -> Vec<String> {
        self.editor
            .tree()
            .map(|tree| tree.rows().iter().map(|row| row.label()).collect())
            .unwrap_or_default()
    }

    /// The list above the prompt line: quick-open hits or open buffers.
    pub fn results(&self) -> &[String] {
        self.editor.results()
    }

    /// Set the window height in lines, since paging depends on it.
    pub fn with_height(mut self, lines: usize) -> Harness {
        self.editor.viewport_mut().set_height_lines(lines);
        self
    }

    // -- what a user would see ---------------------------------------------

    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    pub fn editor_mut(&mut self) -> &mut Editor {
        &mut self.editor
    }

    /// The buffer's whole text.
    pub fn text(&self) -> String {
        self.editor.buffer().text().to_string()
    }

    pub fn cursor(&self) -> Position {
        self.editor.buffer().cursor()
    }

    pub fn selection(&self) -> Option<String> {
        self.editor.buffer().selected_text()
    }

    pub fn context(&self) -> Context {
        self.editor.context()
    }

    /// The status line as it reads right now.
    pub fn status(&self) -> String {
        self.editor.status_line()
    }

    /// The prompt line, or `None` when no prompt is open.
    pub fn prompt_line(&self) -> Option<String> {
        self.editor.prompt().map(|p| p.line())
    }

    /// The footer laid out for an 80-cell window.
    pub fn footer(&self) -> Footer {
        self.editor.footer(80)
    }

    /// The footer as two lines of text.
    pub fn footer_lines(&self) -> Vec<String> {
        self.footer().lines()
    }

    /// Every footer cell as `^X Exit`, in priority order.
    pub fn footer_cells(&self) -> Vec<String> {
        self.footer().cells().iter().map(|c| c.text()).collect()
    }

    pub fn is_help_open(&self) -> bool {
        self.editor.help().is_some()
    }

    pub fn should_quit(&self) -> bool {
        self.editor.should_quit()
    }

    // -- driving it --------------------------------------------------------

    /// Press one chord: resolve it in the current context and run it.
    ///
    /// Unbound chords are recorded, so a test can assert that a profile leaves
    /// a chord free rather than silently swallowing it.
    pub fn press(&mut self, chord: &str) -> Result<Option<Command>, ChordParseError> {
        let chord = Chord::parse(chord)?;
        let context = self.editor.context();
        let Some(command) = self.editor.keymap().resolve(context, chord).cloned() else {
            self.unbound.push(chord);
            return Ok(None);
        };

        self.commands.push(command.clone());
        let outcome = self.editor.run(&command);
        self.outcomes.push(outcome);
        Ok(Some(command))
    }

    /// Press a whitespace-separated sequence of chords.
    pub fn feed(&mut self, chords: &str) -> Result<&mut Harness, ChordParseError> {
        for chord in chords.split_whitespace() {
            self.press(chord)?;
        }
        Ok(self)
    }

    /// Type text one character at a time, the way a keyboard delivers it.
    ///
    /// Where it lands depends on the context, exactly as it does in the real
    /// editor: into the buffer, or into the prompt line.
    pub fn type_text(&mut self, text: &str) -> &mut Harness {
        for c in text.chars() {
            let command = Command::InsertText(c.to_string());
            self.commands.push(command.clone());
            let outcome = self.editor.run(&command);
            self.outcomes.push(outcome);
        }
        self
    }

    /// Every command run so far, in order.
    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    /// Every chord pressed that resolved to nothing.
    pub fn unbound(&self) -> &[Chord] {
        &self.unbound
    }

    /// Every outcome, for asserting on clipboard requests and quitting.
    pub fn outcomes(&self) -> &[Outcome] {
        &self.outcomes
    }

    pub fn clear(&mut self) -> &mut Harness {
        self.commands.clear();
        self.unbound.clear();
        self.outcomes.clear();
        self
    }
}
