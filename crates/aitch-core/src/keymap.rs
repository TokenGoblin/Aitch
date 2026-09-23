//! `(profile, context, chord) -> Command`.
//!
//! Keymaps are data. The UI does not know what keys do: it builds a [`Chord`]
//! from a key event, asks the active [`Keymap`] to [`resolve`](Keymap::resolve)
//! it for the current [`Context`], and sends the resulting [`Command`] to core.
//! The footer is generated from the same data — see [`Keymap::footer_entries`].
//!
//! # Chord grammar
//!
//! A chord is zero or more modifiers plus one key. Two spellings are accepted
//! and mean exactly the same thing:
//!
//! | nano shorthand | long form              | parses to             |
//! |----------------|------------------------|-----------------------|
//! | `^X`           | `Ctrl+X`               | ctrl + `x`            |
//! | `M-U`          | `Alt+U`                | alt + `u`             |
//! | `M-^X`         | `Ctrl+Alt+X`           | ctrl + alt + `x`      |
//! | —              | `Shift+Left`           | shift + Left          |
//! | —              | `F5`                   | F5                    |
//!
//! Modifier names are case-insensitive: `Ctrl`/`Control`/`C`, `Alt`/`Meta`/`M`,
//! `Shift`/`S`. `M-` means Alt on both Windows and Linux.
//!
//! ## Keys are logical, not physical
//!
//! The key half of a chord is the *character the layout produces*, ignoring the
//! Ctrl modifier — winit's key-without-modifiers. Binding on physical scancodes
//! would put `^\` and `^_` in different places on every non-US layout; binding
//! on the logical character keeps a keymap file meaning what it says.
//!
//! Three consequences, all deliberate:
//!
//! - **ASCII letters are folded to lowercase**, so `^X` and `^x` are one chord.
//! - **Shift is significant for letters and named keys.** `Ctrl+Shift+F` is a
//!   different chord from `^F`, and `Shift+Left` from `Left`. A GUI can tell
//!   these apart where a terminal cannot, and the modern profile needs to.
//! - **Shift is dropped for every other character.** `_` implies Shift on a US
//!   layout and not on others, so the character itself is the identity and the
//!   flag is discarded: `^_` matches however the layout produced the `_`.
//!
//! A punctuation chord that a layout cannot reach (`^\` on a French AZERTY, for
//! instance) is a keymap problem, not a code problem: a binding accepts a list
//! of chords, so a profile can offer an alternate spelling for the same command.

use std::collections::HashMap;
use std::fmt;

use crate::command::{Command, UnknownCommand};
use crate::toml::{self, Value};

/// The `nano` profile, compiled in, so the editor and its tests never depend
/// on the working directory. Phase 7 adds loading one from `aitchrc`.
pub const NANO_PROFILE: &str = include_str!("../../../keymaps/nano.toml");

/// The `modern` profile, compiled in.
pub const MODERN_PROFILE: &str = include_str!("../../../keymaps/modern.toml");

/// Where a chord is being pressed. Keymap and footer are both context-scoped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Context {
    /// The text area — the default context.
    Editor,
    /// The single-line prompt above the footer (save-as, goto, search terms).
    Prompt,
    /// The file tree sidebar.
    Tree,
    /// An active incremental search or replace confirmation.
    Search,
    /// The help pane.
    Help,
}

impl Context {
    /// The name used in keymap files.
    pub fn name(self) -> &'static str {
        match self {
            Context::Editor => "editor",
            Context::Prompt => "prompt",
            Context::Tree => "tree",
            Context::Search => "search",
            Context::Help => "help",
        }
    }

    /// The inverse of [`Context::name`], for reading one back out of a
    /// keymap file. Replaces the `#[serde(rename_all = "kebab-case")]`
    /// derive this used to lean on — every name here is already one word,
    /// so kebab-casing it was never doing anything beyond matching `name()`
    /// verbatim.
    fn parse(text: &str) -> Option<Context> {
        match text {
            "editor" => Some(Context::Editor),
            "prompt" => Some(Context::Prompt),
            "tree" => Some(Context::Tree),
            "search" => Some(Context::Search),
            "help" => Some(Context::Help),
            _ => None,
        }
    }
}

impl fmt::Display for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The modifier set of a [`Chord`].
///
/// `shift` is only meaningful for [`Key::Named`]; see the module docs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        ctrl: false,
        alt: false,
        shift: false,
    };

    pub const CTRL: Mods = Mods {
        ctrl: true,
        alt: false,
        shift: false,
    };

    pub const ALT: Mods = Mods {
        ctrl: false,
        alt: true,
        shift: false,
    };

    pub const SHIFT: Mods = Mods {
        ctrl: false,
        alt: false,
        shift: true,
    };

    pub fn with_ctrl(mut self) -> Self {
        self.ctrl = true;
        self
    }

    pub fn with_alt(mut self) -> Self {
        self.alt = true;
        self
    }

    pub fn with_shift(mut self) -> Self {
        self.shift = true;
        self
    }
}

/// A key that produces no character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NamedKey {
    Enter,
    Tab,
    Backspace,
    Delete,
    Escape,
    Space,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    F(u8),
}

impl NamedKey {
    fn parse(s: &str) -> Option<NamedKey> {
        use NamedKey::*;
        let lower = s.to_ascii_lowercase();
        let named = match lower.as_str() {
            "enter" | "return" => Enter,
            "tab" => Tab,
            "backspace" | "bksp" => Backspace,
            "delete" | "del" => Delete,
            "escape" | "esc" => Escape,
            "space" => Space,
            "left" => Left,
            "right" => Right,
            "up" => Up,
            "down" => Down,
            "home" => Home,
            "end" => End,
            "pageup" | "pgup" => PageUp,
            "pagedown" | "pgdn" => PageDown,
            "insert" | "ins" => Insert,
            _ => {
                let n: u8 = lower.strip_prefix('f')?.parse().ok()?;
                if (1..=12).contains(&n) {
                    F(n)
                } else {
                    return None;
                }
            }
        };
        Some(named)
    }

    /// The short label used on the footer.
    pub fn name(self) -> String {
        use NamedKey::*;
        match self {
            Enter => "Enter".into(),
            Tab => "Tab".into(),
            Backspace => "Bksp".into(),
            Delete => "Del".into(),
            Escape => "Esc".into(),
            Space => "Space".into(),
            Left => "Left".into(),
            Right => "Right".into(),
            Up => "Up".into(),
            Down => "Down".into(),
            Home => "Home".into(),
            End => "End".into(),
            PageUp => "PgUp".into(),
            PageDown => "PgDn".into(),
            Insert => "Ins".into(),
            F(n) => format!("F{n}"),
        }
    }
}

/// The key half of a [`Chord`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// A character the layout produces. ASCII letters are stored lowercase.
    Char(char),
    Named(NamedKey),
}

impl Key {
    fn parse(s: &str) -> Option<Key> {
        let mut chars = s.chars();
        let first = chars.next()?;
        if chars.next().is_none() {
            return Some(Key::Char(first.to_ascii_lowercase()));
        }
        NamedKey::parse(s).map(Key::Named)
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // nano writes `^X`, not `^x`.
            Key::Char(c) => write!(f, "{}", c.to_ascii_uppercase()),
            Key::Named(n) => f.write_str(&n.name()),
        }
    }
}

/// One modifier set plus one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub mods: Mods,
    pub key: Key,
}

impl Chord {
    /// A bare key with no modifiers, as a question's answer is typed.
    pub fn from_char(c: char) -> Chord {
        Chord::new(Mods::NONE, Key::Char(c))
    }

    /// Build a chord, applying the normalization rules from the module docs.
    pub fn new(mods: Mods, key: Key) -> Chord {
        match key {
            Key::Char(c) if c.is_ascii_alphabetic() => Chord {
                mods,
                key: Key::Char(c.to_ascii_lowercase()),
            },
            // For non-letters the character already encodes whether shift was
            // held, and which physical key produced it varies by layout.
            Key::Char(c) => Chord {
                mods: Mods {
                    shift: false,
                    ..mods
                },
                key: Key::Char(c),
            },
            named => Chord { mods, key: named },
        }
    }

    /// Parse a chord in either the nano shorthand or the long form.
    pub fn parse(s: &str) -> Result<Chord, ChordParseError> {
        let text = s.trim();
        let err = || ChordParseError {
            input: s.to_string(),
        };
        if text.is_empty() {
            return Err(err());
        }

        let mut mods = Mods::NONE;
        let mut rest = text;

        // nano shorthand prefixes: `^` for Ctrl, `M-` for Alt, in any order.
        loop {
            if rest.len() > 1 && rest.starts_with('^') {
                mods.ctrl = true;
                rest = &rest[1..];
                continue;
            }
            let prefix = if rest.len() > 2 && rest.is_char_boundary(2) {
                Some(&rest[..2])
            } else {
                None
            };
            match prefix {
                Some(p) if p.eq_ignore_ascii_case("m-") => {
                    mods.alt = true;
                    rest = &rest[2..];
                    continue;
                }
                Some(p) if p.eq_ignore_ascii_case("s-") => {
                    mods.shift = true;
                    rest = &rest[2..];
                    continue;
                }
                _ => {}
            }
            break;
        }

        // Long form: `Ctrl+Shift+Left`. A leading `+` is the key itself.
        while let Some(i) = rest.find('+') {
            if i == 0 {
                break;
            }
            let (head, tail) = rest.split_at(i);
            match head.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "c" => mods.ctrl = true,
                "alt" | "meta" | "m" => mods.alt = true,
                "shift" | "s" => mods.shift = true,
                _ => break,
            }
            rest = &tail[1..];
            if rest.is_empty() {
                return Err(err());
            }
        }

        let key = Key::parse(rest).ok_or_else(err)?;
        Ok(Chord::new(mods, key))
    }
}

impl fmt::Display for Chord {
    /// Renders the footer spelling: `^X`, `M-U`, `M-^X`, `^S-Left`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods.alt {
            f.write_str("M-")?;
        }
        if self.mods.ctrl {
            f.write_str("^")?;
        }
        if self.mods.shift {
            f.write_str("S-")?;
        }
        write!(f, "{}", self.key)
    }
}

/// A chord string in a keymap file could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChordParseError {
    input: String,
}

impl fmt::Display for ChordParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot parse chord `{}`", self.input)
    }
}

impl std::error::Error for ChordParseError {}

/// One entry in a keymap: some chords, a command, and the footer metadata that
/// lets the footer render itself without hardcoding a single string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub context: Context,
    /// At least one. The first is the one the footer shows.
    pub chords: Vec<Chord>,
    pub command: Command,
    /// Footer text. A binding with no label never appears on the footer.
    pub label: Option<String>,
    /// Higher survives longer when the footer reflows on a narrow window.
    pub priority: i32,
}

impl Binding {
    /// The chord the footer displays for this binding.
    pub fn primary_chord(&self) -> Chord {
        self.chords[0]
    }
}

/// A footer cell: `^X Exit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FooterEntry<'a> {
    pub chord: Chord,
    pub label: &'a str,
    /// The command this runs, when it came from the keymap.
    ///
    /// `None` for an entry the keymap knows nothing about: the answers to a
    /// question are read as characters rather than resolved through a
    /// binding, so there is no command behind `Y` or `N`.
    pub command: Option<&'a Command>,
    pub priority: i32,
}

impl FooterEntry<'_> {
    /// Rendered width in cells, as `^X Exit` would occupy.
    pub fn width(&self) -> usize {
        self.chord.to_string().chars().count() + 1 + self.label.chars().count()
    }
}

/// A parsed keymap profile.
#[derive(Debug, Clone)]
pub struct Keymap {
    profile: String,
    description: String,
    bindings: Vec<Binding>,
    lookup: HashMap<(Context, Chord), usize>,
}

impl Keymap {
    /// The shipped `nano` profile.
    pub fn nano() -> Keymap {
        Keymap::from_toml(NANO_PROFILE).expect("nano.toml is a compile-time asset")
    }

    /// The shipped `modern` profile.
    pub fn modern() -> Keymap {
        Keymap::from_toml(MODERN_PROFILE).expect("modern.toml is a compile-time asset")
    }

    /// One of the shipped profiles, by name.
    pub fn by_name(name: &str) -> Option<Keymap> {
        match name {
            "nano" => Some(Keymap::nano()),
            "modern" => Some(Keymap::modern()),
            _ => None,
        }
    }

    /// Parse a keymap profile from TOML.
    pub fn from_toml(src: &str) -> Result<Keymap, KeymapError> {
        let root = toml::parse(src).map_err(|e| KeymapError::Toml(e.to_string()))?;
        let raw = RawKeymap::from_value(&root).map_err(KeymapError::Toml)?;

        let mut bindings: Vec<Binding> = Vec::with_capacity(raw.binding.len());
        let mut lookup: HashMap<(Context, Chord), usize> = HashMap::new();

        for rb in raw.binding {
            let command = Command::from_name(&rb.command, rb.arg.as_deref())?;

            let mut chord_strings = rb.chords;
            if let Some(single) = rb.chord {
                chord_strings.insert(0, single);
            }
            if chord_strings.is_empty() {
                return Err(KeymapError::NoChords {
                    command: command.to_string(),
                });
            }

            let mut chords = Vec::with_capacity(chord_strings.len());
            for text in &chord_strings {
                chords.push(Chord::parse(text)?);
            }

            let index = bindings.len();
            for chord in &chords {
                if let Some(&existing) = lookup.get(&(rb.context, *chord)) {
                    return Err(KeymapError::DuplicateChord {
                        context: rb.context,
                        chord: chord.to_string(),
                        first: bindings[existing].command.to_string(),
                        second: command.to_string(),
                    });
                }
                lookup.insert((rb.context, *chord), index);
            }

            bindings.push(Binding {
                context: rb.context,
                chords,
                command,
                label: rb.label,
                priority: rb.priority.unwrap_or(0),
            });
        }

        Ok(Keymap {
            profile: raw.profile,
            description: raw.description,
            bindings,
            lookup,
        })
    }

    pub fn profile(&self) -> &str {
        &self.profile
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// Resolve a chord pressed in `context` to a command.
    pub fn resolve(&self, context: Context, chord: Chord) -> Option<&Command> {
        self.lookup
            .get(&(context, chord))
            .map(|&i| &self.bindings[i].command)
    }

    /// Every labelled binding for a context, highest priority first.
    ///
    /// Ties keep file order, so a keymap file's layout is what the footer shows.
    /// Reflow (Phase 3) truncates this list from the end.
    pub fn footer_entries(&self, context: Context) -> Vec<FooterEntry<'_>> {
        let mut entries: Vec<(usize, FooterEntry<'_>)> = self
            .bindings
            .iter()
            .enumerate()
            .filter(|(_, b)| b.context == context)
            .filter_map(|(i, b)| {
                b.label.as_deref().map(|label| {
                    (
                        i,
                        FooterEntry {
                            chord: b.primary_chord(),
                            label,
                            command: Some(&b.command),
                            priority: b.priority,
                        },
                    )
                })
            })
            .collect();

        entries.sort_by(|(ia, a), (ib, b)| b.priority.cmp(&a.priority).then(ia.cmp(ib)));
        entries.into_iter().map(|(_, e)| e).collect()
    }
}

/// Anything that can go wrong loading a keymap file.
#[derive(Debug)]
pub enum KeymapError {
    /// A `crate::toml::TomlError` or a schema problem (unknown/missing/
    /// wrong-shaped key), both flattened to a message — `Keymap::from_toml`
    /// is the only place that tells them apart, and nothing downstream
    /// needs to.
    Toml(String),
    Command(UnknownCommand),
    Chord(ChordParseError),
    NoChords {
        command: String,
    },
    DuplicateChord {
        context: Context,
        chord: String,
        first: String,
        second: String,
    },
}

impl fmt::Display for KeymapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeymapError::Toml(e) => write!(f, "invalid keymap TOML: {e}"),
            KeymapError::Command(e) => write!(f, "{e}"),
            KeymapError::Chord(e) => write!(f, "{e}"),
            KeymapError::NoChords { command } => {
                write!(f, "binding for `{command}` has no chords")
            }
            KeymapError::DuplicateChord {
                context,
                chord,
                first,
                second,
            } => write!(
                f,
                "chord `{chord}` is bound twice in context `{context}`: `{first}` and `{second}`"
            ),
        }
    }
}

impl std::error::Error for KeymapError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            KeymapError::Command(e) => Some(e),
            KeymapError::Chord(e) => Some(e),
            _ => None,
        }
    }
}

impl From<UnknownCommand> for KeymapError {
    fn from(e: UnknownCommand) -> Self {
        KeymapError::Command(e)
    }
}

impl From<ChordParseError> for KeymapError {
    fn from(e: ChordParseError) -> Self {
        KeymapError::Chord(e)
    }
}

/// What a keymap file's top level can say, read out of a parsed
/// [`Value`] the same way [`crate::config::Config::from_value`] reads
/// `aitchrc.toml` — an unknown key is an error rather than a silent skip,
/// replacing the `#[serde(deny_unknown_fields)]` this used to lean on.
struct RawKeymap {
    profile: String,
    description: String,
    binding: Vec<RawBinding>,
}

struct RawBinding {
    context: Context,
    chord: Option<String>,
    chords: Vec<String>,
    command: String,
    arg: Option<String>,
    label: Option<String>,
    priority: Option<i32>,
}

impl RawKeymap {
    fn from_value(root: &Value) -> Result<RawKeymap, String> {
        let mut profile: Option<String> = None;
        let mut description = String::new();
        let mut binding = Vec::new();

        let entries = root
            .as_table()
            .ok_or_else(|| "expected a table at the top level".to_string())?;
        for (key, value) in entries {
            match key.as_str() {
                "profile" => profile = Some(value.expect_str("profile")?.to_string()),
                "description" => description = value.expect_str("description")?.to_string(),
                "binding" => {
                    for item in value.expect_array("binding")? {
                        binding.push(RawBinding::from_value(item)?);
                    }
                }
                other => return Err(format!("unknown key `{other}`")),
            }
        }

        Ok(RawKeymap {
            profile: profile.ok_or_else(|| "missing `profile`".to_string())?,
            description,
            binding,
        })
    }
}

impl RawBinding {
    fn from_value(value: &Value) -> Result<RawBinding, String> {
        let mut context: Option<Context> = None;
        let mut chord: Option<String> = None;
        let mut chords: Vec<String> = Vec::new();
        let mut command: Option<String> = None;
        let mut arg: Option<String> = None;
        let mut label: Option<String> = None;
        let mut priority: Option<i32> = None;

        for (key, value) in value.expect_table("binding")? {
            match key.as_str() {
                "context" => {
                    let text = value.expect_str("context")?;
                    context = Some(
                        Context::parse(text).ok_or_else(|| format!("unknown context `{text}`"))?,
                    );
                }
                "chord" => chord = Some(value.expect_str("chord")?.to_string()),
                "chords" => chords = value.expect_string_array("chords")?,
                "command" => command = Some(value.expect_str("command")?.to_string()),
                "arg" => arg = Some(value.expect_str("arg")?.to_string()),
                "label" => label = Some(value.expect_str("label")?.to_string()),
                "priority" => priority = Some(value.expect_integer("priority")? as i32),
                other => return Err(format!("unknown key `{other}` in `[[binding]]`")),
            }
        }

        Ok(RawBinding {
            context: context.ok_or_else(|| "binding is missing `context`".to_string())?,
            chord,
            chords,
            command: command.ok_or_else(|| "binding is missing `command`".to_string())?,
            arg,
            label,
            priority,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NANO: &str = NANO_PROFILE;
    const MODERN: &str = MODERN_PROFILE;

    fn chord(s: &str) -> Chord {
        Chord::parse(s).expect("test chord should parse")
    }

    // -- chord grammar -----------------------------------------------------

    #[test]
    fn nano_shorthand_and_long_form_agree() {
        assert_eq!(chord("^X"), chord("Ctrl+X"));
        assert_eq!(chord("M-U"), chord("Alt+U"));
        assert_eq!(chord("M-^X"), chord("Ctrl+Alt+X"));
        assert_eq!(chord("^M-X"), chord("Ctrl+Alt+X"));
    }

    #[test]
    fn letter_keys_fold_case() {
        assert_eq!(chord("^X"), chord("^x"));
        assert_eq!(chord("^X").key, Key::Char('x'));
    }

    #[test]
    fn shift_is_significant_for_letters() {
        // The modern profile needs Ctrl+Shift+F to be its own chord.
        assert_ne!(chord("Ctrl+Shift+F"), chord("^F"));
        assert!(chord("Ctrl+Shift+F").mods.shift);
        assert_eq!(chord("Ctrl+Shift+F").key, Key::Char('f'));
    }

    #[test]
    fn shift_is_dropped_for_other_characters() {
        // `_` is Shift+minus on a US layout and something else elsewhere; the
        // character is the identity, so the flag cannot be part of it.
        assert_eq!(chord("Ctrl+Shift+_"), chord("^_"));
        assert!(!chord("Ctrl+Shift+_").mods.shift);
    }

    #[test]
    fn shift_is_significant_for_named_keys() {
        assert_ne!(chord("Shift+Left"), chord("Left"));
        assert_eq!(chord("Shift+Left").mods, Mods::SHIFT);
        assert_eq!(chord("Shift+Left").key, Key::Named(NamedKey::Left));
    }

    #[test]
    fn punctuation_chords_parse() {
        // The two chords the plan calls out as layout-hostile.
        assert_eq!(chord("^\\").key, Key::Char('\\'));
        assert_eq!(chord("^_").key, Key::Char('_'));
        // `+` as a key, not a separator.
        assert_eq!(chord("Ctrl++").key, Key::Char('+'));
        assert_eq!(chord("^,").key, Key::Char(','));
    }

    #[test]
    fn named_keys_parse() {
        assert_eq!(chord("F5").key, Key::Named(NamedKey::F(5)));
        assert_eq!(chord("PgUp").key, Key::Named(NamedKey::PageUp));
        assert_eq!(chord("pagedown").key, Key::Named(NamedKey::PageDown));
        assert_eq!(chord("Ctrl+Home").mods, Mods::CTRL);
    }

    #[test]
    fn bad_chords_are_rejected() {
        for bad in ["", "   ", "Ctrl+", "F13", "Hyper+X", "notakey"] {
            assert!(Chord::parse(bad).is_err(), "`{bad}` should not parse");
        }
    }

    #[test]
    fn chords_display_in_nano_style() {
        assert_eq!(chord("Ctrl+x").to_string(), "^X");
        assert_eq!(chord("Alt+u").to_string(), "M-U");
        assert_eq!(chord("Ctrl+Alt+x").to_string(), "M-^X");
        assert_eq!(chord("Ctrl+PageUp").to_string(), "^PgUp");
        assert_eq!(chord("^_").to_string(), "^_");
    }

    #[test]
    fn display_round_trips_through_parse() {
        for text in ["^X", "M-U", "M-^X", "^PgUp", "S-Left", "F5", "^\\"] {
            let c = chord(text);
            assert_eq!(chord(&c.to_string()), c, "round trip failed for {text}");
        }
    }

    // -- file parsing ------------------------------------------------------

    #[test]
    fn nano_profile_is_faithful() {
        let km = Keymap::from_toml(NANO).expect("nano.toml should parse");
        assert_eq!(km.profile(), "nano");

        let expect = [
            ("^X", Command::Quit),
            ("^O", Command::WriteOut),
            ("^W", Command::WhereIs),
            ("^K", Command::Cut),
            ("^U", Command::Uncut),
            ("^G", Command::Help),
            ("^R", Command::ReadFile),
            ("^C", Command::CursorPosition),
            ("^\\", Command::Replace),
            ("^_", Command::GotoLine),
            ("M-U", Command::Undo),
            ("M-E", Command::Redo),
        ];
        for (text, cmd) in expect {
            assert_eq!(
                km.resolve(Context::Editor, chord(text)),
                Some(&cmd),
                "nano profile should bind {text} to {cmd}"
            );
        }
    }

    #[test]
    fn modern_profile_uses_gui_conventions() {
        let km = Keymap::from_toml(MODERN).expect("modern.toml should parse");
        assert_eq!(km.profile(), "modern");

        let expect = [
            ("^S", Command::WriteOut),
            ("^F", Command::WhereIs),
            ("^C", Command::Copy),
            ("^X", Command::Cut),
            ("^V", Command::Paste),
            ("^Z", Command::Undo),
            ("^Y", Command::Redo),
            ("^Q", Command::Quit),
        ];
        for (text, cmd) in expect {
            assert_eq!(
                km.resolve(Context::Editor, chord(text)),
                Some(&cmd),
                "modern profile should bind {text} to {cmd}"
            );
        }
    }

    #[test]
    fn both_profiles_cover_the_same_commands() {
        let nano = Keymap::from_toml(NANO).unwrap();
        let modern = Keymap::from_toml(MODERN).unwrap();

        let names = |km: &Keymap| {
            let mut v: Vec<String> = km
                .bindings()
                .iter()
                // Names, not `Display`: `switch-profile` carries a different
                // target profile in each file and that is the point of it.
                .map(|b| format!("{}:{}", b.context, b.command.name()))
                .collect();
            v.sort();
            v.dedup();
            v
        };

        // A profile that drops a command makes it unreachable. Both must bind
        // every command the other does.
        assert_eq!(names(&nano), names(&modern));
    }

    #[test]
    fn first_run_hint_switches_to_the_modern_profile() {
        let km = Keymap::from_toml(NANO).unwrap();
        assert_eq!(
            km.resolve(Context::Editor, chord("M-M")),
            Some(&Command::SwitchProfile("modern".into()))
        );
    }

    #[test]
    fn contexts_are_scoped() {
        let km = Keymap::from_toml(NANO).unwrap();
        // Enter accepts the prompt, and does nothing of the sort in the editor.
        assert_eq!(
            km.resolve(Context::Prompt, chord("Enter")),
            Some(&Command::PromptAccept)
        );
        assert_ne!(
            km.resolve(Context::Editor, chord("Enter")),
            Some(&Command::PromptAccept)
        );
    }

    #[test]
    fn unbound_chord_resolves_to_nothing() {
        let km = Keymap::from_toml(NANO).unwrap();
        assert_eq!(km.resolve(Context::Editor, chord("M-^Q")), None);
    }

    #[test]
    fn alternate_chords_resolve_to_the_same_command() {
        let src = r#"
            profile = "alt"
            [[binding]]
            context = "editor"
            chords = ["^_", "M-G"]
            command = "goto-line"
        "#;
        let km = Keymap::from_toml(src).unwrap();
        assert_eq!(
            km.resolve(Context::Editor, chord("^_")),
            km.resolve(Context::Editor, chord("M-G"))
        );
    }

    #[test]
    fn duplicate_chord_in_one_context_is_rejected() {
        let src = r#"
            profile = "dupe"
            [[binding]]
            context = "editor"
            chord = "^K"
            command = "cut"
            [[binding]]
            context = "editor"
            chord = "^K"
            command = "copy"
        "#;
        let err = Keymap::from_toml(src).unwrap_err().to_string();
        assert!(err.contains("bound twice"), "unexpected error: {err}");
    }

    #[test]
    fn the_same_chord_in_two_contexts_is_fine() {
        let src = r#"
            profile = "ok"
            [[binding]]
            context = "editor"
            chord = "^K"
            command = "cut"
            [[binding]]
            context = "tree"
            chord = "^K"
            command = "move-up"
        "#;
        assert!(Keymap::from_toml(src).is_ok());
    }

    #[test]
    fn a_binding_needs_at_least_one_chord() {
        let src = r#"
            profile = "empty"
            [[binding]]
            context = "editor"
            command = "cut"
        "#;
        let err = Keymap::from_toml(src).unwrap_err().to_string();
        assert!(err.contains("no chords"), "unexpected error: {err}");
    }

    #[test]
    fn unknown_command_is_rejected() {
        let src = r#"
            profile = "bad"
            [[binding]]
            context = "editor"
            chord = "^K"
            command = "summon-lsp"
        "#;
        let err = Keymap::from_toml(src).unwrap_err().to_string();
        assert!(err.contains("unknown command"), "unexpected error: {err}");
    }

    #[test]
    fn unknown_field_is_rejected() {
        let src = r#"
            profile = "bad"
            [[binding]]
            context = "editor"
            chord = "^K"
            command = "cut"
            colour = "red"
        "#;
        assert!(Keymap::from_toml(src).is_err());
    }

    // -- footer ------------------------------------------------------------

    #[test]
    fn footer_is_generated_from_the_keymap() {
        let km = Keymap::from_toml(NANO).unwrap();
        let entries = km.footer_entries(Context::Editor);

        // nano's first two cells, and they come from the data, not a literal.
        assert_eq!(entries[0].chord.to_string(), "^G");
        assert_eq!(entries[0].label, "Help");
        assert_eq!(entries[1].chord.to_string(), "^X");
        assert_eq!(entries[1].label, "Exit");
        assert_eq!(entries[0].width(), "^G Help".len());
    }

    #[test]
    fn footer_entries_are_sorted_by_priority() {
        let km = Keymap::from_toml(NANO).unwrap();
        let entries = km.footer_entries(Context::Editor);
        assert!(entries.len() >= 12, "nano footer should be well populated");
        for pair in entries.windows(2) {
            assert!(
                pair[0].priority >= pair[1].priority,
                "footer entries must be highest priority first"
            );
        }
    }

    #[test]
    fn footer_reflow_drops_the_lowest_priority_entries() {
        let km = Keymap::from_toml(NANO).unwrap();
        let full = km.footer_entries(Context::Editor);
        let narrow: Vec<_> = full.iter().take(6).collect();
        let dropped = &full[6..];
        let lowest_kept = narrow.last().unwrap().priority;
        for entry in dropped {
            assert!(entry.priority <= lowest_kept);
        }
    }

    #[test]
    fn unlabelled_bindings_stay_off_the_footer() {
        let km = Keymap::from_toml(NANO).unwrap();
        let entries = km.footer_entries(Context::Editor);
        // Arrow keys are bound but must never take a footer cell.
        assert!(entries
            .iter()
            .all(|e| e.command != Some(&Command::MoveLeft)));
    }

    #[test]
    fn every_context_has_a_footer() {
        let km = Keymap::from_toml(NANO).unwrap();
        for context in [
            Context::Editor,
            Context::Prompt,
            Context::Tree,
            Context::Search,
            Context::Help,
        ] {
            assert!(
                !km.footer_entries(context).is_empty(),
                "context `{context}` has no footer entries"
            );
        }
    }
}
