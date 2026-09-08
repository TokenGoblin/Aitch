//! Drive nib without a window.
//!
//! Phase 0 can only do the half of the pipeline that exists: chords in,
//! commands out, plus whatever the footer would show. Phase 3 grows this into
//! the real thing — feed a chord sequence, assert on buffer content, cursor
//! position, status text and footer — and every footer command gets a test.

use nib_core::{Chord, ChordParseError, Command, Context, Keymap, KeymapError};

/// The keymap profiles that ship with nib, compiled in so tests do not depend
/// on the working directory.
pub const NANO_KEYMAP: &str = include_str!("../../../keymaps/nano.toml");
pub const MODERN_KEYMAP: &str = include_str!("../../../keymaps/modern.toml");

/// A headless editor session.
pub struct Harness {
    keymap: Keymap,
    context: Context,
    commands: Vec<Command>,
    unbound: Vec<Chord>,
}

impl Harness {
    pub fn new(keymap: Keymap) -> Harness {
        Harness {
            keymap,
            context: Context::Editor,
            commands: Vec::new(),
            unbound: Vec::new(),
        }
    }

    /// A session using the shipped `nano` profile.
    pub fn nano() -> Harness {
        Harness::new(Keymap::from_toml(NANO_KEYMAP).expect("nano.toml is a compile-time asset"))
    }

    /// A session using the shipped `modern` profile.
    pub fn modern() -> Harness {
        Harness::new(Keymap::from_toml(MODERN_KEYMAP).expect("modern.toml is a compile-time asset"))
    }

    /// A session using a keymap written inline by a test.
    pub fn from_toml(src: &str) -> Result<Harness, KeymapError> {
        Ok(Harness::new(Keymap::from_toml(src)?))
    }

    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    pub fn context(&self) -> Context {
        self.context
    }

    /// Move to another context, as focus changes will once they exist.
    pub fn set_context(&mut self, context: Context) -> &mut Harness {
        self.context = context;
        self
    }

    /// Press one chord. Returns what it resolved to in the current context.
    ///
    /// Unbound chords are recorded too, so a test can assert that a profile
    /// leaves a chord free rather than silently swallowing it.
    pub fn press(&mut self, chord: &str) -> Result<Option<Command>, ChordParseError> {
        let chord = Chord::parse(chord)?;
        match self.keymap.resolve(self.context, chord) {
            Some(command) => {
                let command = command.clone();
                self.commands.push(command.clone());
                Ok(Some(command))
            }
            None => {
                self.unbound.push(chord);
                Ok(None)
            }
        }
    }

    /// Press a whitespace-separated sequence of chords.
    pub fn feed(&mut self, chords: &str) -> Result<&mut Harness, ChordParseError> {
        for chord in chords.split_whitespace() {
            self.press(chord)?;
        }
        Ok(self)
    }

    /// Every command resolved so far, in order.
    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    /// Every chord pressed so far that resolved to nothing.
    pub fn unbound(&self) -> &[Chord] {
        &self.unbound
    }

    pub fn clear(&mut self) -> &mut Harness {
        self.commands.clear();
        self.unbound.clear();
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
}
