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
pub mod config;
pub mod document;
pub mod edit;
pub mod editor;
pub mod fileio;
pub mod footer;
pub mod highlighter;
pub mod history;
pub mod keymap;
pub mod line_ending;
pub mod project;
pub mod project_search;
pub mod prompt;
pub mod search;
pub mod session;
pub mod syntax;
pub mod watcher;
pub mod workspace;

pub use buffer::{Applied, Buffer, Position, TextEdit, Viewport};
pub use command::Command;
pub use config::{Config, ConfigError, ThemeChoice};
pub use document::Document;
pub use edit::Edit;
pub use editor::{Editor, Outcome, ViewOptions};
pub use fileio::{Charset, Encoding, FileError, Loaded};
pub use footer::Footer;
pub use highlighter::{Highlights, SyntaxThread};
pub use history::History;
pub use keymap::Binding as KeymapBinding;
pub use keymap::{
    Binding, Chord, ChordParseError, Context, FooterEntry, Key, Keymap, KeymapError, Mods,
    NamedKey, MODERN_PROFILE, NANO_PROFILE,
};
pub use line_ending::LineEnding;
pub use project::{PathIndex, Tree};
pub use project_search::{Hit, Pattern, ProjectSearch};
pub use prompt::{Answer, Histories, Prompt};
pub use search::{Direction, Match, Query};
pub use session::{OpenFile, Recovery, Session};
pub use syntax::{Highlighter, Language, Span, Token};
pub use watcher::Watcher;
pub use workspace::Workspace;
