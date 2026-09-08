//! Rendering. Phase 0 owns the GPU surface and clears it.
//!
//! Phase 1 adds the text surface here; Phase 3 the footer, prompt line and
//! status line; Phase 4 the sidebar.

mod surface;

pub use surface::{Surface, SurfaceError};
