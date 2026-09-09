//! `aitch-core` — the editor core.
//!
//! This crate has no GUI dependencies and never will. Everything that can be
//! tested without a window lives here. See `CLAUDE.md` at the repo root.
//!
//! [`Command`] is the contract: the UI resolves input to one and hands it
//! over, and never touches a buffer itself. [`keymap`] turns chords into
//! commands, [`buffer`] runs them, [`edit`] is the only place text is
//! mutated, and [`fileio`] gets files in and out byte-exactly.

pub mod buffer;
pub mod command;
pub mod document;
pub mod edit;
pub mod fileio;
pub mod history;
pub mod keymap;
pub mod line_ending;

pub use buffer::{Applied, Buffer, Position, Viewport};
pub use command::Command;
pub use document::Document;
pub use edit::Edit;
pub use fileio::{Charset, Encoding, FileError, Loaded};
pub use history::History;
pub use keymap::{
    Binding, Chord, ChordParseError, Context, FooterEntry, Key, Keymap, KeymapError, Mods,
    NamedKey, MODERN_PROFILE, NANO_PROFILE,
};
pub use line_ending::LineEnding;
