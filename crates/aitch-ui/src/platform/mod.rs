//! Platform backends.
//!
//! Windows only, by decision (see `PLAN-ZERO-DEP.md`'s platform scope). Every
//! module under here is hand-written against raw OS APIs — no crates.io
//! dependency, ever. See `CLAUDE.md`.

#[cfg(windows)]
pub mod win32;
