//! `nib-core` — the editor core.
//!
//! This crate has no GUI dependencies and never will. Everything that can be
//! tested without a window lives here. See `CLAUDE.md` at the repo root.
//!
//! Phase 0 ships two modules: the [`Command`] contract and the [`keymap`]
//! data model that resolves input chords to commands. Nothing is wired to
//! behavior yet.

pub mod command;
pub mod keymap;

pub use command::Command;
pub use keymap::{
    Binding, Chord, ChordParseError, Context, FooterEntry, Key, Keymap, KeymapError, Mods, NamedKey,
};
