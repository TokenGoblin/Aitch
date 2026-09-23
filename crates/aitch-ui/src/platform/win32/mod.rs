//! Raw Win32 backend: window/message loop, a GDI-backed software surface,
//! and the system clipboard.
//!
//! Every FFI signature in `window`, `surface`, and `clipboard` is declared by
//! hand, in the style `crates/aitch/src/main.rs`'s `attach_to_parent_console`
//! already established: no `windows-sys`, no `winapi`, no crate at all. See
//! `PLAN-ZERO-DEP.md` §2 and §4 Phases 0 and 2.

pub mod clipboard;
pub mod surface;
pub mod window;
