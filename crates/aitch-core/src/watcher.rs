//! Noticing when the folder changes underneath the editor.
//!
//! A build runs, a branch is checked out, a file appears — the sidebar should
//! catch up without being asked. What it must never do is act on that: a file
//! that changed under an open buffer is reported, never reloaded over unsaved
//! work and never quietly overwritten on the next save. That guarantee lives
//! in [`crate::document::Document::changed_on_disk`]; this is only the nudge.
//!
//! Two things matter for the idle-CPU budget in PLAN.md §6:
//!
//! - **Nothing polls.** The thread blocks on the watcher's channel, and the
//!   editor's event loop blocks until something wakes it.
//! - **Events are debounced.** Saving one file can produce several events, and
//!   a build produces thousands; rebuilding the tree for each would be a waste
//!   and a stutter. They are collapsed into one notification per quiet period.
//!
//! # Raw `ReadDirectoryChangesW`, replacing `notify`
//!
//! Per `PLAN-ZERO-DEP.md` §2/§4 Phase 4 Track B. Every signature here is
//! declared by hand, in the style
//! `crates/aitch/src/main.rs`'s `attach_to_parent_console` and
//! `aitch-ui/src/platform/win32/clipboard.rs` already established: no
//! `windows-sys`, no `winapi`, no crate at all. Handles are carried as plain
//! `isize`, matching `clipboard.rs`'s convention for the same reason (any
//! Win32 handle converts losslessly to and from an integer) — here that also
//! means the handle is trivially `Send`, which matters because it crosses to
//! a background thread.
//!
//! Two threads are involved, mirroring the two jobs `notify` used to do in
//! one crate:
//!
//! - **The pump thread** opens the directory (`CreateFileW` with
//!   `FILE_FLAG_BACKUP_SEMANTICS`, required to get a handle to a *directory*
//!   rather than a file, and `FILE_SHARE_READ | FILE_SHARE_WRITE |
//!   FILE_SHARE_DELETE` so watching never locks anything) and loops calling
//!   `ReadDirectoryChangesW` **synchronously** (`lpOverlapped = NULL`): the
//!   call blocks the thread until something changes, then returns. This
//!   project never reads the returned `FILE_NOTIFY_INFORMATION` buffer's
//!   contents — only that a change happened, never what — so a successful
//!   return, overflow included, just forwards a `()` down a channel to:
//! - **The debounce thread**, whose loop is untouched from the `notify`
//!   version: block for the first signal, then drain the channel until
//!   [`QUIET`] passes, then call back once.
//!
//! # Stopping a thread blocked inside a synchronous syscall
//!
//! The pump thread spends nearly all its life blocked inside the kernel, not
//! waiting on a channel — a channel send cannot wake it the way dropping
//! `notify`'s watcher could. `CancelIoEx` is the documented way to interrupt
//! it: unlike `CancelIo`, it can cancel I/O issued by *another* thread, which
//! is exactly the shape of the problem here (`Drop::drop` runs on whichever
//! thread drops the `Watcher`, never the pump thread itself). Cancelling
//! makes the blocked `ReadDirectoryChangesW` call return `FALSE`, which the
//! pump loop treats as "stop" (see [`pump_changes`]) — closing its own handle
//! itself, from its own thread, right before returning, rather than racing a
//! `CloseHandle` from the dropping thread against a call that has not
//! actually unblocked yet.
//!
//! `CancelIoEx` only cancels I/O already submitted to the driver, though —
//! there is a real, small window (right after the pump thread starts, and
//! again between any two of its calls) where nothing is pending yet, and a
//! `CancelIoEx` landing there is a no-op that would leave the thread to block
//! forever on its *next* call. [`stop_pump`] closes that race by retrying
//! the cancellation every few milliseconds until the thread actually joins,
//! rather than firing once and hoping — see its doc comment and
//! `dropping_the_watcher_stops_it`, which drops the `Watcher` essentially
//! immediately after creating it and so reliably hits this window.
//! `Watcher::drop` calls it, so a dropped `Watcher` never leaves a thread
//! parked inside a Win32 call behind it — see also
//! `dropping_the_watcher_actually_ends_the_pump_thread` below.

use std::ffi::c_void;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

/// How long to wait for the noise to stop before reporting a change.
///
/// Long enough to collapse a build's worth of events into one, short enough
/// that a file someone just saved shows up while they are still looking.
const QUIET: Duration = Duration::from_millis(250);

const FILE_LIST_DIRECTORY: u32 = 0x0001;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const FILE_SHARE_DELETE: u32 = 0x0000_0004;
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const INVALID_HANDLE_VALUE: isize = -1;

const FILE_NOTIFY_CHANGE_FILE_NAME: u32 = 0x0000_0001;
const FILE_NOTIFY_CHANGE_DIR_NAME: u32 = 0x0000_0002;
const FILE_NOTIFY_CHANGE_LAST_WRITE: u32 = 0x0000_0010;

/// What this project needs to know about: a name appearing, disappearing, or
/// being renamed, and a file's content changing (its last-write time) — the
/// only signal [`crate::document::Document::changed_on_disk`] reacts to.
const NOTIFY_FILTER: u32 =
    FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE;

/// `ERROR_NOTIFY_ENUM_DIR`: `ReadDirectoryChangesW`'s buffer overflowed
/// because too much happened between two calls. Still means "something
/// changed" — just not what — so [`pump_changes`] treats it as a signal to
/// forward, not a reason to stop.
const ERROR_NOTIFY_ENUM_DIR: u32 = 1022;

/// How large a buffer to hand `ReadDirectoryChangesW` for each call. Its
/// contents are never read (see the module docs), so this only needs to be
/// large enough that an ordinary burst of changes does not overflow it —
/// bigger just means fewer `ERROR_NOTIFY_ENUM_DIR` round trips under load.
const NOTIFY_BUFFER_BYTES: usize = 64 * 1024;

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *mut c_void,
        disposition: u32,
        flags: u32,
        template: isize,
    ) -> isize;
    fn ReadDirectoryChangesW(
        directory: isize,
        buffer: *mut c_void,
        buffer_length: u32,
        watch_subtree: i32,
        notify_filter: u32,
        bytes_returned: *mut u32,
        overlapped: *mut c_void,
        completion_routine: *mut c_void,
    ) -> i32;
    fn CancelIoEx(handle: isize, overlapped: *mut c_void) -> i32;
    fn CloseHandle(handle: isize) -> i32;
    fn GetLastError() -> u32;
}

/// Watches a folder and calls back when it settles after a change.
///
/// Dropping it stops the watch and ends the thread.
pub struct Watcher {
    /// Kept only so `Drop` can hand it to `CancelIoEx`; the handle itself is
    /// closed by [`pump_changes`], on the pump thread, once that call
    /// actually returns — see the module docs for why closing it from here
    /// instead would race the still-blocked call.
    handle: isize,
    pump: Option<JoinHandle<()>>,
    /// Dropping this closes the channel, which is what ends the debounce
    /// thread if it is not already ending via the pump thread's `events`
    /// sender being dropped — same double role the `notify`-based version's
    /// `_stop` field played.
    _stop: mpsc::Sender<()>,
}

impl Watcher {
    /// Start watching `root`, calling `on_change` after each quiet period.
    ///
    /// Returns `None` if the platform cannot watch — a network share, a
    /// permissions problem, a path that does not exist. The editor works
    /// without it; the tree just needs `^L` to catch up, which is why this
    /// is not an error.
    pub fn new<F>(root: &Path, on_change: F) -> Option<Watcher>
    where
        F: Fn() + Send + 'static,
    {
        let path = root.to_str()?;
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                0,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }

        let (events, incoming) = mpsc::channel::<()>();
        // A one-shot readiness signal: see `pump_changes` for why `new` must
        // not return until the pump thread has actually issued its first
        // `ReadDirectoryChangesW` call.
        let (ready, ready_arrived) = mpsc::channel::<()>();
        let pump = match std::thread::Builder::new()
            .name("aitch-watcher-io".to_string())
            .spawn(move || pump_changes(handle, events, ready))
        {
            Ok(pump) => pump,
            Err(_) => {
                unsafe {
                    CloseHandle(handle);
                }
                return None;
            }
        };
        // The kernel only starts watching a directory from the moment a
        // `ReadDirectoryChangesW` call is actually issued against it — not
        // from `CreateFileW`. Without waiting here, a caller that writes a
        // file immediately after `Watcher::new` returns can race the pump
        // thread's first call and lose that change forever, since nothing
        // else will ever report it. A short bound rather than an unbounded
        // `recv` so a pathological failure to schedule the pump thread at
        // all cannot hang `new` outright; either way, this returns almost
        // immediately in the overwhelmingly common case.
        let _ = ready_arrived.recv_timeout(Duration::from_secs(2));

        let (stop, stopped) = mpsc::channel::<()>();

        let debounce = std::thread::Builder::new()
            .name("aitch-watcher".to_string())
            .spawn(move || {
                loop {
                    // Block until something happens. No timeout, no polling.
                    match incoming.recv() {
                        Ok(_) => {}
                        // The pump thread stopped (the watcher was dropped).
                        Err(_) => return,
                    }

                    // Something did. Drain until it goes quiet, so a build
                    // that touches ten thousand files reports once.
                    loop {
                        match incoming.recv_timeout(QUIET) {
                            Ok(_) => continue,
                            Err(RecvTimeoutError::Timeout) => break,
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }

                    if stopped.try_recv() == Err(mpsc::TryRecvError::Disconnected) {
                        return;
                    }
                    on_change();
                }
            });
        if debounce.is_err() {
            stop_pump(handle, pump);
            return None;
        }

        Some(Watcher {
            handle,
            pump: Some(pump),
            _stop: stop,
        })
    }
}

/// Runs on its own thread for the `Watcher`'s whole lifetime, blocked inside
/// the kernel almost the entire time. Each successful return means something
/// changed somewhere under `handle`'s directory tree; the raw signal — not
/// its contents — is forwarded to the debounce loop over `events`.
///
/// Ends, closing `handle` itself, when `ReadDirectoryChangesW` fails for any
/// reason other than a buffer overflow. In practice that only happens when
/// `Watcher::drop` cancels the pending I/O.
///
/// Signals `ready` once, right before the first call: the directory is not
/// actually being watched by the kernel until that call is issued, so
/// `Watcher::new` waits for this to avoid handing back a `Watcher` that can
/// silently miss a change made in the gap between thread creation and this
/// thread actually getting scheduled.
fn pump_changes(handle: isize, events: mpsc::Sender<()>, ready: mpsc::Sender<()>) {
    let mut buffer = vec![0u8; NOTIFY_BUFFER_BYTES];
    let _ = ready.send(());
    loop {
        let mut bytes_returned: u32 = 0;
        let ok = unsafe {
            ReadDirectoryChangesW(
                handle,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                1, // bWatchSubtree = TRUE: the whole tree, not just this directory.
                NOTIFY_FILTER,
                &mut bytes_returned,
                std::ptr::null_mut(), // lpOverlapped = NULL: block synchronously.
                std::ptr::null_mut(), // No completion routine: nothing overlapped to complete.
            )
        };

        if ok != 0 {
            if events.send(()).is_err() {
                // The debounce thread is gone; nothing left to forward to.
                break;
            }
            continue;
        }

        // A buffer overflow still means something happened, just too much of
        // it to fit — keep watching rather than falling through to "stop".
        if unsafe { GetLastError() } == ERROR_NOTIFY_ENUM_DIR {
            if events.send(()).is_err() {
                break;
            }
            continue;
        }

        // Anything else — in practice, `Drop` cancelling this call — means
        // it is time to stop.
        break;
    }
    unsafe {
        CloseHandle(handle);
    }
}

/// Cancels the pump thread's pending I/O and blocks until it has actually
/// exited, retrying the cancellation until that happens.
///
/// `CancelIoEx` only cancels I/O already submitted to the driver — there is
/// a small window, right after the pump thread starts (and again between any
/// two of its `ReadDirectoryChangesW` calls), where it has not yet reached
/// the blocking syscall. A `CancelIoEx` landing in that window is a no-op,
/// and the thread would then block on its next call forever: exactly the
/// "leaked thread parked inside a Win32 call" this whole scheme exists to
/// avoid. That window is a handful of instructions wide, not a meaningful
/// amount of wall-clock time, so retrying at a short interval turns the race
/// into, at worst, one extra attempt rather than a real hang — proven by
/// `dropping_the_watcher_stops_it`, which drops the `Watcher` essentially
/// immediately after creating it and so reliably hits this window.
fn stop_pump(handle: isize, pump: JoinHandle<()>) {
    let (done, arrived) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = pump.join();
        let _ = done.send(());
    });
    loop {
        unsafe {
            CancelIoEx(handle, std::ptr::null_mut());
        }
        match arrived.recv_timeout(Duration::from_millis(5)) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => continue,
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // Wait for the pump thread to actually exit — not just to have fired
        // its last callback — so no thread is left parked inside a Win32
        // call once this returns. This also guarantees `events` has been
        // dropped, so the debounce thread's `incoming.recv()` is already
        // unblocking (or about to) even before `_stop` closes below.
        if let Some(pump) = self.pump.take() {
            stop_pump(self.handle, pump);
        }
        // `_stop` drops right after this function returns (see the struct's
        // field order), closing `stopped` — the debounce loop's
        // belt-and-suspenders check against firing `on_change` one last time
        // if it is mid-drain of a real burst right as this happens.
    }
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Watcher")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("aitch-watch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Wait for a condition, up to a limit. Filesystem events are not instant
    /// and their timing is the operating system's business, not ours.
    fn wait_for(mut done: impl FnMut() -> bool, limit: Duration) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < limit {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        done()
    }

    #[test]
    fn a_new_file_wakes_the_watcher() {
        let dir = scratch("new-file");
        let count = Arc::new(AtomicUsize::new(0));

        let seen = count.clone();
        let watcher = Watcher::new(&dir, move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        let Some(_watcher) = watcher else {
            eprintln!("SKIPPED: this platform will not watch {dir:?}");
            return;
        };

        std::fs::write(dir.join("appeared.txt"), "hello").unwrap();

        assert!(
            wait_for(|| count.load(Ordering::SeqCst) > 0, Duration::from_secs(5)),
            "the watcher never reported the new file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_burst_of_changes_is_reported_once() {
        let dir = scratch("burst");
        let count = Arc::new(AtomicUsize::new(0));

        let seen = count.clone();
        let Some(_watcher) = Watcher::new(&dir, move || {
            seen.fetch_add(1, Ordering::SeqCst);
        }) else {
            eprintln!("SKIPPED: this platform will not watch {dir:?}");
            return;
        };

        // A build's worth of churn, all inside one quiet period.
        for i in 0..200 {
            std::fs::write(dir.join(format!("file-{i}.o")), "output").unwrap();
        }

        assert!(
            wait_for(|| count.load(Ordering::SeqCst) > 0, Duration::from_secs(5)),
            "the watcher never reported the burst"
        );
        // Let any straggler debounce window close.
        std::thread::sleep(QUIET * 3);

        let reports = count.load(Ordering::SeqCst);
        assert!(
            reports <= 3,
            "200 files produced {reports} notifications; they should collapse"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dropping_the_watcher_stops_it() {
        let dir = scratch("dropped");
        let count = Arc::new(AtomicUsize::new(0));

        let seen = count.clone();
        let watcher = Watcher::new(&dir, move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        if watcher.is_none() {
            eprintln!("SKIPPED: this platform will not watch {dir:?}");
            return;
        }
        drop(watcher);

        std::fs::write(dir.join("after.txt"), "hello").unwrap();
        std::thread::sleep(QUIET * 3);

        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "a dropped watcher should not still be reporting"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The regression this rewrite is really about: a `notify`-based watcher
    /// could never leave a thread stuck, because dropping it dropped a
    /// channel sender the pump thread was blocked on. A hand-rolled
    /// synchronous `ReadDirectoryChangesW` pump has no such channel to wake
    /// it — only cancelling the pending I/O does — so
    /// `dropping_the_watcher_stops_it` above proving no more callbacks fire
    /// is not, by itself, proof the thread is gone; a leaked thread parked
    /// forever inside the blocked call would pass that test too.
    ///
    /// `Watcher::drop` joins the pump thread before returning (see the
    /// module docs), so dropping it on its own thread and waiting with a
    /// bound turns "the thread is stuck forever" into a clean test failure
    /// instead of a hang: if cancellation ever stopped actually unblocking
    /// the call, `drop` itself would never return.
    #[test]
    fn dropping_the_watcher_actually_ends_the_pump_thread() {
        let dir = scratch("join");
        let watcher = Watcher::new(&dir, || {});
        let Some(watcher) = watcher else {
            eprintln!("SKIPPED: this platform will not watch {dir:?}");
            return;
        };

        let (done, arrived) = mpsc::channel();
        std::thread::spawn(move || {
            drop(watcher);
            let _ = done.send(());
        });

        assert!(
            arrived.recv_timeout(Duration::from_secs(5)).is_ok(),
            "dropping the watcher never returned — the pump thread is stuck \
             blocked inside ReadDirectoryChangesW instead of actually ending"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A path that does not exist at all cannot be watched — `CreateFileW`
    /// fails outright. This must come back as `None`, the same "the editor
    /// works without it" contract as a network share that refuses
    /// `FILE_FLAG_BACKUP_SEMANTICS`, never a hang or a panic. `notify`'s own
    /// tests never had to cover this case explicitly since `notify::Watcher`
    /// took care of it internally; it is worth covering directly now that
    /// `CreateFileW`'s failure path is this file's own code.
    #[test]
    fn a_path_that_does_not_exist_returns_none() {
        let dir = std::env::temp_dir().join(format!(
            "aitch-watch-missing-{}-does-not-exist",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        assert!(Watcher::new(&dir, || {}).is_none());
    }
}
