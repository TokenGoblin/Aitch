//! Turning Win32 keyboard events into [`Chord`]s.
//!
//! This is the whole of what the UI knows about the keyboard. It does not know
//! what any key *does* — that is the keymap's business, and the keymap is
//! data. Rebuilds what Phase 0 deleted (the winit-based `input.rs`) against
//! [`Event::KeyDown`]/[`Event::Char`] instead, per `PLAN-ZERO-DEP.md` §4
//! Phase 3 Track A. Read `platform::win32::window`'s module docs (the
//! "Keyboard and mouse" section) before touching this file — it is the
//! contract this module builds against, and explains the two rules below in
//! full.
//!
//! # Two rules the old winit-based file didn't need
//!
//! - **A key that produces a character arrives as both `KeyDown` and
//!   `Char`.** `Enter`, `Tab`, `Backspace`, `Escape`, and `Space` fire a
//!   `KeyDown` (resolved here to a [`NamedKey`]) *and* a `Char` (`'\r'`,
//!   `'\t'`, `'\u{8}'`, `'\u{1b}'`, `' '`) for the same physical press.
//!   [`chord_from_char`] discards those five characters — they are handled
//!   exclusively via [`chord_from_keydown`]'s `NamedKey` path — so a caller
//!   that feeds every event from the queue through [`chord_from_event`] never
//!   double-fires a bound `Enter` command and inserts a literal `'\r'` from
//!   one keypress.
//! - **No modifier state travels on the event at all.** `window.rs` reads
//!   `Ctrl`/`Alt`/`Shift` live with `GetKeyState` rather than threading them
//!   through the event queue, and this module does the same, at the moment
//!   it resolves a [`Chord`] — not stored anywhere.
//!
//! # Ctrl+letter, unchanged from the deleted file
//!
//! The awkward part is still Ctrl. Win32's `WM_CHAR` produces the same C0
//! control character a terminal would for Ctrl+letter (`^A` arrives as
//! `U+0001`), exactly like winit did — winit was just passing through the
//! OS's own behavior, and Win32 *is* that OS. [`unmap_control`] undoes it
//! unchanged from the deleted file, so a keymap file can say `^A` and mean
//! the `A` key everywhere.

use aitch_core::{Chord, Key, Mods, NamedKey};

use crate::platform::win32::window::Event;

#[link(name = "user32")]
extern "system" {
    fn GetKeyState(vkey: i32) -> i16;
}

const VK_SHIFT: i32 = 0x10;
const VK_CONTROL: i32 = 0x11;
const VK_MENU: i32 = 0x12;

/// Named virtual-key codes [`named_key_from_vkey`] maps. Not every `VK_*`
/// constant Win32 defines — only the ones with an [`aitch_core::NamedKey`]
/// counterpart; an ordinary letter/digit/punctuation `vkey` has no entry
/// here, because its character arrives separately via [`Event::Char`].
mod vk {
    pub const BACK: u32 = 0x08;
    pub const TAB: u32 = 0x09;
    pub const RETURN: u32 = 0x0D;
    pub const ESCAPE: u32 = 0x1B;
    pub const SPACE: u32 = 0x20;
    pub const PRIOR: u32 = 0x21;
    pub const NEXT: u32 = 0x22;
    pub const END: u32 = 0x23;
    pub const HOME: u32 = 0x24;
    pub const LEFT: u32 = 0x25;
    pub const UP: u32 = 0x26;
    pub const RIGHT: u32 = 0x27;
    pub const DOWN: u32 = 0x28;
    pub const INSERT: u32 = 0x2D;
    pub const DELETE: u32 = 0x2E;
    pub const F1: u32 = 0x70;
    pub const F12: u32 = 0x7B;
}

/// Resolve whichever of [`Event::KeyDown`]/[`Event::Char`] `event` is, to at
/// most one [`Chord`]. Every other [`Event`] variant (`Resized`,
/// `CloseRequested`, `ScaleChanged`, and the mouse events — Track B's
/// business, not this module's) returns `None`, as does a `Char` that
/// duplicates a same-keypress `KeyDown` (see the module docs).
///
/// This is the shape a real event loop wants: one function to call for
/// every queued [`Event`], regardless of which variant it turns out to be.
pub fn chord_from_event(event: &Event) -> Option<Chord> {
    match *event {
        Event::KeyDown { vkey, .. } => chord_from_keydown(vkey),
        Event::Char(c) => chord_from_char(c),
        _ => None,
    }
}

/// Build a chord from a `WM_KEYDOWN`/`WM_SYSKEYDOWN` virtual-key code.
/// Returns `None` for a `vkey` with no [`NamedKey`] mapping — an ordinary
/// letter, digit, or punctuation key, whose actual character arrives
/// separately via [`Event::Char`] and is handled by [`chord_from_char`]
/// instead. Reads live modifier state with `GetKeyState`.
pub fn chord_from_keydown(vkey: u32) -> Option<Chord> {
    let named = named_key_from_vkey(vkey)?;
    Some(Chord::new(live_mods(), Key::Named(named)))
}

/// Build a chord from a `WM_CHAR`/`WM_SYSCHAR` character. Returns `None` for
/// the five characters that duplicate a same-keypress [`Event::KeyDown`]
/// (`'\r'`, `'\t'`, `'\u{8}'`, `'\u{1b}'`, `' '` — see the module docs).
/// Every other character is run through [`unmap_control`] and becomes a
/// [`Key::Char`]. Reads live modifier state with `GetKeyState`.
pub fn chord_from_char(c: char) -> Option<Chord> {
    if is_keydown_duplicate(c) {
        return None;
    }
    Some(Chord::new(live_mods(), Key::Char(unmap_control(c))))
}

/// The five characters a `KeyDown`-producing key *also* sends as a `Char`
/// for the same press. See the module docs.
fn is_keydown_duplicate(c: char) -> bool {
    matches!(c, '\r' | '\t' | '\u{8}' | '\u{1b}' | ' ')
}

/// Undo the C0 control character a terminal-style Ctrl+key produces.
///
/// `U+0001`–`U+001A` are Ctrl+A through Ctrl+Z. The four above them are the
/// ones a nano keymap actually needs: `^\` and `^_` in particular, which is
/// why this matters beyond tidiness. Ported unchanged from the deleted
/// winit-based `input.rs` — Win32 produces the identical C0 codes.
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

/// Map a Win32 virtual-key code to a [`NamedKey`], for the keys that produce
/// no character of their own. `None` for anything else — including ordinary
/// letters and digits, whose `vkey` (e.g. `'A'` = `0x41`) carries no useful
/// information here since [`Event::Char`] carries the actual character.
fn named_key_from_vkey(vkey: u32) -> Option<NamedKey> {
    let named = match vkey {
        vk::RETURN => NamedKey::Enter,
        vk::TAB => NamedKey::Tab,
        vk::BACK => NamedKey::Backspace,
        vk::ESCAPE => NamedKey::Escape,
        vk::SPACE => NamedKey::Space,
        vk::LEFT => NamedKey::Left,
        vk::RIGHT => NamedKey::Right,
        vk::UP => NamedKey::Up,
        vk::DOWN => NamedKey::Down,
        vk::HOME => NamedKey::Home,
        vk::END => NamedKey::End,
        vk::PRIOR => NamedKey::PageUp,
        vk::NEXT => NamedKey::PageDown,
        vk::INSERT => NamedKey::Insert,
        vk::DELETE => NamedKey::Delete,
        vk::F1..=vk::F12 => NamedKey::F((vkey - vk::F1 + 1) as u8),
        _ => return None,
    };
    Some(named)
}

/// Live modifier state, read at the moment a [`Chord`] is resolved rather
/// than threaded through the event queue — see the module docs and
/// `window.rs`'s.
fn live_mods() -> Mods {
    Mods {
        ctrl: key_is_down(VK_CONTROL),
        alt: key_is_down(VK_MENU),
        shift: key_is_down(VK_SHIFT),
    }
}

/// `GetKeyState`'s return value packs "currently down" in the high bit and a
/// toggle flag (relevant only for `CapsLock`/`NumLock`/`ScrollLock`) in the
/// low bit. This extracts just the high bit, as its own function so the bit
/// arithmetic is testable without a real keyboard.
fn is_down(state: i16) -> bool {
    (state as u16) & 0x8000 != 0
}

fn key_is_down(vkey: i32) -> bool {
    is_down(unsafe { GetKeyState(vkey) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::win32::window::MouseButton;

    fn chord(s: &str) -> Chord {
        Chord::parse(s).expect("test chord should parse")
    }

    // -- unmap_control, ported from the deleted input.rs --------------------

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
        let ctrl_x = Chord::new(Mods::CTRL, Key::Char(unmap_control('\u{18}')));
        assert_eq!(ctrl_x, chord("^X"));

        let goto = Chord::new(Mods::CTRL, Key::Char(unmap_control('\u{1f}')));
        assert_eq!(goto, chord("^_"));
    }

    #[test]
    fn shift_survives_for_letters_but_not_for_punctuation() {
        let find_all = Chord::new(Mods::CTRL.with_shift(), Key::Char('f'));
        assert_eq!(find_all, chord("Ctrl+Shift+F"));
        assert_ne!(find_all, chord("^F"));

        // `_` needs Shift on a US layout, and that must not change the chord.
        let goto = Chord::new(Mods::CTRL.with_shift(), Key::Char('_'));
        assert_eq!(goto, chord("^_"));
    }

    // -- virtual-key -> NamedKey mapping (new) -------------------------------

    #[test]
    fn every_named_key_is_reachable_from_some_vkey() {
        let expect = [
            (vk::RETURN, NamedKey::Enter),
            (vk::TAB, NamedKey::Tab),
            (vk::BACK, NamedKey::Backspace),
            (vk::ESCAPE, NamedKey::Escape),
            (vk::SPACE, NamedKey::Space),
            (vk::LEFT, NamedKey::Left),
            (vk::RIGHT, NamedKey::Right),
            (vk::UP, NamedKey::Up),
            (vk::DOWN, NamedKey::Down),
            (vk::HOME, NamedKey::Home),
            (vk::END, NamedKey::End),
            (vk::PRIOR, NamedKey::PageUp),
            (vk::NEXT, NamedKey::PageDown),
            (vk::INSERT, NamedKey::Insert),
            (vk::DELETE, NamedKey::Delete),
        ];
        for (vkey, named) in expect {
            assert_eq!(
                named_key_from_vkey(vkey),
                Some(named),
                "vkey 0x{vkey:02X} should map to {named:?}"
            );
        }
        for n in 1..=12u8 {
            let vkey = vk::F1 + (n as u32 - 1);
            assert_eq!(
                named_key_from_vkey(vkey),
                Some(NamedKey::F(n)),
                "vkey 0x{vkey:02X} should map to F{n}"
            );
        }
    }

    #[test]
    fn ordinary_letter_and_digit_vkeys_map_to_nothing() {
        // 'A' and '5' as Win32 virtual-key codes: their character comes via
        // Event::Char instead, so KeyDown has nothing useful to say here.
        const VK_A: u32 = 0x41;
        const VK_5: u32 = 0x35;
        assert_eq!(named_key_from_vkey(VK_A), None);
        assert_eq!(named_key_from_vkey(VK_5), None);
    }

    #[test]
    fn chord_from_keydown_returns_none_for_unmapped_vkeys() {
        const VK_A: u32 = 0x41;
        assert_eq!(chord_from_keydown(VK_A), None);
    }

    #[test]
    fn chord_from_keydown_resolves_named_keys() {
        assert_eq!(
            chord_from_keydown(vk::ESCAPE).map(|c| c.key),
            Some(Key::Named(NamedKey::Escape))
        );
        assert_eq!(
            chord_from_keydown(vk::F1).map(|c| c.key),
            Some(Key::Named(NamedKey::F(1)))
        );
    }

    // -- duplicate Char suppression (the one genuinely new rule) ------------

    #[test]
    fn the_five_keydown_duplicates_are_suppressed() {
        for c in ['\r', '\t', '\u{8}', '\u{1b}', ' '] {
            assert_eq!(
                chord_from_char(c),
                None,
                "Char({c:?}) duplicates a KeyDown and must not also produce a chord"
            );
        }
    }

    #[test]
    fn every_other_char_still_produces_a_chord() {
        for c in ['a', 'Z', '5', '_', '\\', '\u{1}', '\u{18}', 'é', '→'] {
            assert!(
                chord_from_char(c).is_some(),
                "Char({c:?}) should still produce a chord"
            );
        }
    }

    #[test]
    fn chord_from_char_applies_unmap_control() {
        assert_eq!(
            chord_from_char('\u{18}').map(|c| c.key),
            Some(Key::Char('x'))
        );
        assert_eq!(
            chord_from_char('\u{1f}').map(|c| c.key),
            Some(Key::Char('_'))
        );
        assert_eq!(chord_from_char('q').map(|c| c.key), Some(Key::Char('q')));
    }

    // -- printable, unbound text falls through with the right Chord ---------

    #[test]
    fn an_ordinary_unbound_character_produces_a_plain_chord() {
        // What the later integration's `Command::InsertText` fallback needs:
        // an unbound printable character resolves to exactly the chord an
        // ordinary keypress with no Ctrl/Alt produces.
        let c = chord_from_char('q').expect("plain letter should produce a chord");
        assert_eq!(c.key, Key::Char('q'));
        // Whether ctrl/alt happen to be held is live keyboard state this
        // test doesn't control, but an ordinary typed key has neither.
    }

    // -- chord_from_event dispatch --------------------------------------------

    #[test]
    fn non_keyboard_events_resolve_to_no_chord() {
        let events = [
            Event::Resized {
                width: 100,
                height: 100,
            },
            Event::CloseRequested,
            Event::ScaleChanged {
                scale: 1.5,
                dpi: 144,
            },
            Event::MouseMove { x: 1, y: 2 },
            Event::MouseButton {
                button: MouseButton::Left,
                pressed: true,
                x: 1,
                y: 2,
            },
            Event::MouseWheel { delta_lines: 1.0 },
        ];
        for event in events {
            assert_eq!(
                chord_from_event(&event),
                None,
                "{event:?} should not resolve to a chord"
            );
        }
    }

    #[test]
    fn chord_from_event_dispatches_keydown_and_char() {
        assert_eq!(
            chord_from_event(&Event::KeyDown {
                vkey: vk::TAB,
                repeat: false,
            })
            .map(|c| c.key),
            Some(Key::Named(NamedKey::Tab))
        );
        assert_eq!(
            chord_from_event(&Event::Char('q')).map(|c| c.key),
            Some(Key::Char('q'))
        );
        // The duplicate-suppression rule, reached through the dispatcher too.
        assert_eq!(chord_from_event(&Event::Char('\t')), None);
    }

    // -- GetKeyState's high-bit "currently down" arithmetic ------------------

    #[test]
    fn is_down_reads_only_the_high_bit() {
        assert!(!is_down(0x0000u16 as i16), "nothing set: not down");
        assert!(is_down(0x8000u16 as i16), "high bit only: down");
        assert!(
            !is_down(0x0001u16 as i16),
            "toggle bit alone must not read as down"
        );
        assert!(
            is_down(0x8001u16 as i16),
            "high bit plus toggle bit: still down"
        );
        assert!(
            is_down(0xFFFFu16 as i16),
            "every bit set: down (toggled and pressed)"
        );
        assert!(
            !is_down(0x7FFFu16 as i16),
            "every bit but the high one: not down"
        );
    }
}
