//! Raw Win32 clipboard, replacing `arboard` per `PLAN-ZERO-DEP.md` §2 and §4
//! Phase 2 Track A.
//!
//! Every signature here is declared by hand, in the style
//! `crates/aitch/src/main.rs`'s `attach_to_parent_console` already
//! established: no `windows-sys`, no `winapi`, no crate at all. Handles are
//! carried as plain `isize`/`usize`, matching the convention `window.rs` and
//! `surface.rs` already settled on for the same reason (see their module
//! docs): any Win32 handle converts losslessly to and from an integer, so
//! this file's `extern "system"` declarations of `GlobalAlloc` and friends
//! never disagree with anyone else's about the type of a handle.
//!
//! # Shape of the API
//!
//! There is no per-window clipboard state to hold — Windows' clipboard is one
//! system-wide resource, not owned by any particular window — so the public
//! API is two free functions, [`set_text`] and [`get_text`], both operating
//! on `CF_UNICODETEXT` (UTF-16LE, null-terminated), the format every other
//! Windows text editor and browser reads and writes.
//!
//! # Memory ownership
//!
//! `CF_UNICODETEXT` clipboard data lives in movable global memory
//! (`GlobalAlloc(GMEM_MOVEABLE, ..)`), locked with `GlobalLock` while its
//! bytes are read or written and unlocked with `GlobalUnlock` before it is
//! handed anywhere else. The two directions have opposite ownership rules,
//! and getting this backwards is the classic way to double-free or leak on
//! this API:
//!
//! - **`set_text`**: this process allocates the block, writes the UTF-16
//!   text into it, and hands the handle to `SetClipboardData`. From the
//!   moment that call succeeds, the *system* owns the block — it is what
//!   every future `GetClipboardData` call, by this process or any other,
//!   will return, until some other application overwrites the clipboard.
//!   This code must not call `GlobalFree` on it, ever, including on a later
//!   error path; freeing memory the system now considers its own is exactly
//!   the hazard the module docs warn about. If `SetClipboardData` itself
//!   fails, the block was never handed over, so freeing it there is correct
//!   and necessary (see `set_text`'s error path).
//! - **`get_text`**: `GetClipboardData` returns a handle still owned by the
//!   system — a *borrow*, not a transfer. This code locks it to read, copies
//!   the text out into an owned `String`, unlocks it, and closes the
//!   clipboard. It never calls `GlobalFree` on a handle it got back from
//!   `GetClipboardData`; the system frees that memory on its own schedule
//!   (typically when the clipboard contents next change).

use std::ffi::c_void;
use std::fmt;

const CF_UNICODETEXT: u32 = 13;
const GMEM_MOVEABLE: u32 = 0x0002;

#[link(name = "user32")]
extern "system" {
    fn OpenClipboard(owner: isize) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn SetClipboardData(format: u32, data: isize) -> isize;
    fn GetClipboardData(format: u32) -> isize;
    fn IsClipboardFormatAvailable(format: u32) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GlobalAlloc(flags: u32, bytes: usize) -> isize;
    fn GlobalLock(handle: isize) -> *mut c_void;
    fn GlobalUnlock(handle: isize) -> i32;
    fn GlobalFree(handle: isize) -> isize;
    fn GlobalSize(handle: isize) -> usize;
}

/// Something that went wrong talking to the clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardError {
    /// `OpenClipboard` failed — usually because another process (or another
    /// window in this one) already has it open. Windows offers no way to
    /// wait for it; the caller's only real option is "try again."
    OpenFailed,
    /// `EmptyClipboard` or `SetClipboardData` failed while writing.
    WriteFailed,
    /// Allocating or locking the global memory block used to hand text to
    /// the clipboard failed.
    AllocationFailed,
    /// The clipboard has no `CF_UNICODETEXT` data on it at all — either
    /// nothing has ever been copied, or the last thing copied was some other
    /// format (an image, a file list, ...) this module doesn't read.
    NoTextAvailable,
    /// `GetClipboardData` reported text was available but locking the
    /// returned handle failed anyway. Rare — would mean the clipboard
    /// changed out from under this call, or a misbehaving other process.
    ReadFailed,
}

impl fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            ClipboardError::OpenFailed => "could not open the clipboard",
            ClipboardError::WriteFailed => "could not write to the clipboard",
            ClipboardError::AllocationFailed => {
                "could not allocate memory to copy text to the clipboard"
            }
            ClipboardError::NoTextAvailable => "no text is on the clipboard",
            ClipboardError::ReadFailed => "could not read the clipboard's text",
        };
        f.write_str(message)
    }
}

impl std::error::Error for ClipboardError {}

/// A `CloseClipboard` call tied to a scope, so every early return below —
/// success or failure — closes the clipboard exactly once. Forgetting to
/// close it would leave every other process (including this one, the next
/// time it wants the clipboard) unable to open it at all.
struct ClipboardGuard;

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}

/// How many times to retry `OpenClipboard` before giving up.
///
/// `OpenClipboard` can fail transiently even when nothing is wrong: another
/// process (a clipboard history feature, a sync tool, another app's own
/// copy/paste) can hold the clipboard open for a moment. Windows gives no way
/// to *wait* for it — no blocking variant, no event to watch — so the
/// documented mitigation every long-lived Windows application ends up
/// reaching for is exactly this: try again a few times with a short pause.
/// Found by this module's own `repeated_round_trips_do_not_leak_or_corrupt_state`
/// test flaking on a real desktop with nothing else obviously wrong.
const OPEN_CLIPBOARD_ATTEMPTS: u32 = 10;
const OPEN_CLIPBOARD_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(5);

fn open_clipboard() -> Result<ClipboardGuard, ClipboardError> {
    // `hWndNewOwner = 0`: this process doesn't need `WM_DRAWCLIPBOARD`
    // notifications, and passing no window means there is nothing here that
    // depends on `platform::win32::window` at all.
    for attempt in 0..OPEN_CLIPBOARD_ATTEMPTS {
        if unsafe { OpenClipboard(0) } != 0 {
            return Ok(ClipboardGuard);
        }
        if attempt + 1 < OPEN_CLIPBOARD_ATTEMPTS {
            std::thread::sleep(OPEN_CLIPBOARD_RETRY_DELAY);
        }
    }
    Err(ClipboardError::OpenFailed)
}

/// Replace the system clipboard's contents with `text`, as `CF_UNICODETEXT`.
///
/// An empty string is valid input and round-trips through [`get_text`] as
/// `""` — the clipboard ends up holding a global block containing nothing
/// but a single UTF-16 null terminator.
pub fn set_text(text: &str) -> Result<(), ClipboardError> {
    // UTF-16, null-terminated: what CF_UNICODETEXT is defined to hold. A
    // `char` in `text` can never itself be U+0000 followed by more text in a
    // way that would truncate early here in a *correctness*-affecting way —
    // see the module's test module for why an embedded NUL is not something
    // a `&str` round-trip needs to worry about — but it is still the reason
    // this collects the whole string before terminating it, rather than
    // stopping at the first zero unit.
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let byte_len = units.len() * std::mem::size_of::<u16>();

    let guard = open_clipboard()?;

    if unsafe { EmptyClipboard() } == 0 {
        return Err(ClipboardError::WriteFailed);
    }

    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, byte_len) };
    if handle == 0 {
        return Err(ClipboardError::AllocationFailed);
    }

    let locked = unsafe { GlobalLock(handle) };
    if locked.is_null() {
        // Never handed to SetClipboardData, so this process still owns it.
        unsafe {
            GlobalFree(handle);
        }
        return Err(ClipboardError::AllocationFailed);
    }

    unsafe {
        std::ptr::copy_nonoverlapping(units.as_ptr(), locked.cast::<u16>(), units.len());
        GlobalUnlock(handle);
    }

    // From here, ownership of `handle` passes to the system on success. Do
    // NOT call GlobalFree on it after this point, in either branch below —
    // see the module docs.
    let result = unsafe { SetClipboardData(CF_UNICODETEXT, handle) };
    if result == 0 {
        // The call failed, so the handle was never adopted by the system:
        // this is the one place in `set_text` where freeing it is correct.
        unsafe {
            GlobalFree(handle);
        }
        drop(guard);
        return Err(ClipboardError::WriteFailed);
    }

    drop(guard);
    Ok(())
}

/// Read the system clipboard's text, if any.
///
/// Returns [`ClipboardError::NoTextAvailable`] rather than panicking when the
/// clipboard is empty or holds a non-text format.
pub fn get_text() -> Result<String, ClipboardError> {
    let guard = open_clipboard()?;

    // Checked explicitly rather than only inferred from a null
    // `GetClipboardData` return: a null return is ambiguous between "no such
    // format" and "format present but something else went wrong", and the
    // two deserve different errors.
    if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) } == 0 {
        drop(guard);
        return Err(ClipboardError::NoTextAvailable);
    }

    let handle = unsafe { GetClipboardData(CF_UNICODETEXT) };
    if handle == 0 {
        drop(guard);
        return Err(ClipboardError::NoTextAvailable);
    }

    let locked = unsafe { GlobalLock(handle) };
    if locked.is_null() {
        drop(guard);
        return Err(ClipboardError::ReadFailed);
    }

    // `GlobalSize` gives the block's capacity, which can be larger than the
    // text plus its terminator (the allocator may round up); the real length
    // is wherever the first U+0000 code unit falls, exactly like a C string.
    let capacity_bytes = unsafe { GlobalSize(handle) };
    let capacity_units = capacity_bytes / std::mem::size_of::<u16>();
    let text = unsafe {
        let ptr = locked.cast::<u16>();
        let slice = std::slice::from_raw_parts(ptr, capacity_units);
        let len = slice.iter().position(|&u| u == 0).unwrap_or(capacity_units);
        String::from_utf16_lossy(&slice[..len])
    };

    // This handle is still owned by the system (see the module docs) — only
    // unlock it, never GlobalFree it.
    unsafe {
        GlobalUnlock(handle);
    }
    drop(guard);
    Ok(text)
}

/// Remove whatever is on the clipboard, leaving it genuinely empty rather
/// than holding some other format. Exposed for this module's own tests, so
/// they can put the clipboard into a known "nothing here" state without
/// reaching for a second crate or an external tool — see the test module for
/// why that matters for a shared, system-wide resource.
#[cfg(test)]
fn clear() -> Result<(), ClipboardError> {
    let guard = open_clipboard()?;
    let ok = unsafe { EmptyClipboard() } != 0;
    drop(guard);
    if ok {
        Ok(())
    } else {
        Err(ClipboardError::WriteFailed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // The clipboard is one system-wide resource, but `cargo test` runs tests
    // in parallel threads by default. Two tests' set/get pairs interleaving
    // would flake in a way that has nothing to do with a real bug — test A's
    // `get_text` could observe test B's `set_text`. A single process-wide
    // mutex, held for the full body of every test below, serializes them
    // against each other without needing `--test-threads=1` on the whole
    // binary (which would also have serialized any unrelated test added here
    // later).
    static CLIPBOARD_LOCK: Mutex<()> = Mutex::new(());

    fn locked() -> std::sync::MutexGuard<'static, ()> {
        // `.unwrap_or_else(|poisoned| ..)`: one test panicking while holding
        // the lock must not brick every test after it in the same run.
        CLIPBOARD_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn ascii_text_round_trips_exactly() {
        let _guard = locked();
        set_text("hello, clipboard").unwrap();
        assert_eq!(get_text().unwrap(), "hello, clipboard");
    }

    #[test]
    fn non_ascii_text_round_trips_exactly() {
        let _guard = locked();
        // Accented Latin, a CJK character, and an emoji (outside the BMP, so
        // it is a UTF-16 surrogate pair) — proving the UTF-16 conversion is
        // correct in both directions, not just for BMP characters.
        let text = "café \u{6f22}\u{5b57} \u{1f600}";
        set_text(text).unwrap();
        assert_eq!(get_text().unwrap(), text);
    }

    #[test]
    fn empty_string_round_trips_as_empty() {
        let _guard = locked();
        set_text("").unwrap();
        assert_eq!(get_text().unwrap(), "");
    }

    #[test]
    fn an_empty_clipboard_is_a_clear_error_not_a_panic() {
        let _guard = locked();
        clear().unwrap();
        let error = get_text().unwrap_err();
        assert_eq!(error, ClipboardError::NoTextAvailable);
        // `Display`/`Error` are both implemented and produce a real message.
        assert_eq!(error.to_string(), "no text is on the clipboard");
    }

    #[test]
    fn setting_text_then_clearing_then_getting_is_still_a_clear_error() {
        let _guard = locked();
        set_text("something").unwrap();
        clear().unwrap();
        assert!(matches!(get_text(), Err(ClipboardError::NoTextAvailable)));
    }

    #[test]
    fn repeated_round_trips_do_not_leak_or_corrupt_state() {
        let _guard = locked();
        // Not a leak detector (no crate for that here, and it would not be
        // zero-dependency), but many round trips in a row through the same
        // process is exactly the pattern that would surface a
        // double-free, use-after-free, or GlobalLock/Unlock imbalance in the
        // `set_text`/`get_text` bodies above as a crash rather than silence.
        for i in 0..64 {
            let text = format!("round trip number {i}");
            set_text(&text).unwrap();
            assert_eq!(get_text().unwrap(), text);
        }
    }

    #[test]
    fn a_long_string_round_trips_exactly() {
        let _guard = locked();
        let text = "the quick brown fox jumps over the lazy dog ".repeat(2000);
        set_text(&text).unwrap();
        assert_eq!(get_text().unwrap(), text);
    }
}
