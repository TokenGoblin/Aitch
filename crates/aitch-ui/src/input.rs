//! Turning winit key events into [`Chord`]s.
//!
//! This is the whole of what the UI knows about the keyboard. It does not know
//! what any key *does* — that is the keymap's business, and the keymap is data.
//!
//! The awkward part is Ctrl. Some platforms report Ctrl+letter as the C0
//! control character it produces in a terminal (`^A` arrives as `U+0001`)
//! rather than as the letter. `unmap_control` puts those back, so a keymap
//! file can say `^A` and mean the A key everywhere.

use aitch_core::{Chord, Key, Mods, NamedKey};
use winit::event::KeyEvent;
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey as WinitNamedKey};

/// Build a chord from a key press. Returns `None` for keys with no chord
/// spelling, such as a bare modifier or a dead key.
pub fn chord_from_event(event: &KeyEvent, modifiers: ModifiersState) -> Option<Chord> {
    let key = key_from(&event.logical_key)?;
    let mods = Mods {
        ctrl: modifiers.control_key(),
        alt: modifiers.alt_key(),
        shift: modifiers.shift_key(),
    };
    Some(Chord::new(mods, key))
}

fn key_from(logical: &WinitKey) -> Option<Key> {
    match logical {
        WinitKey::Character(text) => {
            let c = text.chars().next()?;
            Some(Key::Char(unmap_control(c)))
        }
        WinitKey::Named(named) => named_key_from(*named).map(Key::Named),
        _ => None,
    }
}

/// Undo the C0 control character a terminal-style Ctrl+key produces.
///
/// `U+0001`–`U+001A` are Ctrl+A through Ctrl+Z. The four above them are the
/// ones a nano keymap actually needs: `^\` and `^_` in particular, which is
/// why this matters beyond tidiness.
fn unmap_control(c: char) -> char {
    match c as u32 {
        0x00 => '@',
        code @ 0x01..=0x1a => char::from(b'a' + (code as u8 - 1)),
        0x1b => '[',
        0x1c => '\\',
        0x1d => ']',
        0x1e => '^',
        0x1f => '_',
        _ => c,
    }
}

fn named_key_from(named: WinitNamedKey) -> Option<NamedKey> {
    let key = match named {
        WinitNamedKey::Enter => NamedKey::Enter,
        WinitNamedKey::Tab => NamedKey::Tab,
        WinitNamedKey::Backspace => NamedKey::Backspace,
        WinitNamedKey::Delete => NamedKey::Delete,
        WinitNamedKey::Escape => NamedKey::Escape,
        WinitNamedKey::Space => NamedKey::Space,
        WinitNamedKey::ArrowLeft => NamedKey::Left,
        WinitNamedKey::ArrowRight => NamedKey::Right,
        WinitNamedKey::ArrowUp => NamedKey::Up,
        WinitNamedKey::ArrowDown => NamedKey::Down,
        WinitNamedKey::Home => NamedKey::Home,
        WinitNamedKey::End => NamedKey::End,
        WinitNamedKey::PageUp => NamedKey::PageUp,
        WinitNamedKey::PageDown => NamedKey::PageDown,
        WinitNamedKey::Insert => NamedKey::Insert,
        WinitNamedKey::F1 => NamedKey::F(1),
        WinitNamedKey::F2 => NamedKey::F(2),
        WinitNamedKey::F3 => NamedKey::F(3),
        WinitNamedKey::F4 => NamedKey::F(4),
        WinitNamedKey::F5 => NamedKey::F(5),
        WinitNamedKey::F6 => NamedKey::F(6),
        WinitNamedKey::F7 => NamedKey::F(7),
        WinitNamedKey::F8 => NamedKey::F(8),
        WinitNamedKey::F9 => NamedKey::F(9),
        WinitNamedKey::F10 => NamedKey::F(10),
        WinitNamedKey::F11 => NamedKey::F(11),
        WinitNamedKey::F12 => NamedKey::F(12),
        _ => return None,
    };
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_letters_come_back_as_letters() {
        assert_eq!(unmap_control('\u{1}'), 'a');
        assert_eq!(unmap_control('\u{18}'), 'x');
        assert_eq!(unmap_control('\u{1a}'), 'z');
    }

    #[test]
    fn the_punctuation_chords_nano_needs_come_back() {
        // ^\ is Replace and ^_ is Go To Line in the nano profile. Without
        // this they arrive as unprintable control characters and go unbound.
        assert_eq!(unmap_control('\u{1c}'), '\\');
        assert_eq!(unmap_control('\u{1f}'), '_');
    }

    #[test]
    fn ordinary_characters_are_left_alone() {
        for c in ['a', 'Z', '5', '_', '\\', 'é', '→'] {
            assert_eq!(unmap_control(c), c);
        }
    }

    #[test]
    fn unmapped_control_chords_resolve_the_way_the_keymap_spells_them() {
        // The end-to-end claim: what the OS hands us for Ctrl+X, run through
        // unmapping and normalization, is the chord `^X` in a keymap file.
        let chord = Chord::new(Mods::CTRL, Key::Char(unmap_control('\u{18}')));
        assert_eq!(chord, Chord::parse("^X").unwrap());

        let goto = Chord::new(Mods::CTRL, Key::Char(unmap_control('\u{1f}')));
        assert_eq!(goto, Chord::parse("^_").unwrap());
    }

    #[test]
    fn shift_survives_for_letters_but_not_for_punctuation() {
        let find_all = Chord::new(Mods::CTRL.with_shift(), Key::Char('f'));
        assert_eq!(find_all, Chord::parse("Ctrl+Shift+F").unwrap());
        assert_ne!(find_all, Chord::parse("^F").unwrap());

        // `_` needs Shift on a US layout, and that must not change the chord.
        let goto = Chord::new(Mods::CTRL.with_shift(), Key::Char('_'));
        assert_eq!(goto, Chord::parse("^_").unwrap());
    }
}
