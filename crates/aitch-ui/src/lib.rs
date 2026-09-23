//! `aitch-ui` — window, input, and rendering.
//!
//! Runs on a hand-written Win32 window and GDI software surface
//! (`platform::win32`), zero crates.io dependencies — see
//! `PLAN-ZERO-DEP.md`. It never mutates a buffer: it resolves input to a
//! `aitch_core::Command` and hands it to core. See `CLAUDE.md` at the repo
//! root.
//!
//! Phase 0 landed the window and surface with no text or input yet — real
//! rendering (Phase 1) and keyboard/mouse handling (Phase 3) rebuild `render`
//! and `input` modules against this backend.

pub mod app;
pub mod hit;
pub mod input;
pub mod platform;
pub mod text;
pub mod theme;

pub use app::{run, Startup};
pub use theme::Theme;
