//! A hand-written font stack: parsing, shaping, and rasterization.
//!
//! Replaces `cosmic-text` (already removed from `Cargo.toml` in Phase 0) per
//! `PLAN-ZERO-DEP.md` §2 and §4 Phase 1. One bundled monospace font
//! (`assets/fonts/DejaVuSansMono.ttf`), no system font discovery.

pub mod font;
pub mod outline;
pub mod raster;
pub mod shape;
