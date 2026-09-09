//! `aitch-ui` — window, input, and rendering.
//!
//! This crate owns winit, wgpu and (from Phase 1) cosmic-text. It never
//! mutates a buffer: it resolves input to a `aitch_core::Command` and hands it
//! to core. See `CLAUDE.md` at the repo root.

pub mod app;
pub mod input;
pub mod render;
pub mod theme;

pub use app::{run, Startup};
pub use theme::Theme;
