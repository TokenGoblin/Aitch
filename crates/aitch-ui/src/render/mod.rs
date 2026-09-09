//! Rendering: the GPU surface, the glyph atlas, and the one pipeline that
//! draws both text and solid fills.
//!
//! Phase 3 adds the footer, prompt line and status line here; Phase 4 the
//! sidebar. All of them are quads and glyphs, so all of them go through
//! [`quads::Instances`].

pub mod atlas;
pub mod quads;
pub mod screen;
pub mod text;

mod surface;

pub use surface::{Surface, SurfaceError};
pub use text::TextRenderer;
