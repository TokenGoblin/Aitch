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

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use notify::{RecursiveMode, Watcher as _};

/// How long to wait for the noise to stop before reporting a change.
///
/// Long enough to collapse a build's worth of events into one, short enough
/// that a file someone just saved shows up while they are still looking.
const QUIET: Duration = Duration::from_millis(250);

/// Watches a folder and calls back when it settles after a change.
///
/// Dropping it stops the watch and ends the thread.
pub struct Watcher {
    _inner: notify::RecommendedWatcher,
    /// Dropping this closes the channel, which is what ends the thread.
    _stop: mpsc::Sender<()>,
}

impl Watcher {
    /// Start watching `root`, calling `on_change` after each quiet period.
    ///
    /// Returns `None` if the platform cannot watch — a network share, a
    /// permissions problem. The editor works without it; the tree just needs
    /// `^L` to catch up, which is why this is not an error.
    pub fn new<F>(root: &Path, on_change: F) -> Option<Watcher>
    where
        F: Fn() + Send + 'static,
    {
        let (events, incoming) = mpsc::channel();
        let mut inner = notify::recommended_watcher(move |result| {
            // A send failure means the debouncer has gone; nothing to do.
            let _ = events.send(result);
        })
        .ok()?;
        inner.watch(root, RecursiveMode::Recursive).ok()?;

        let (stop, stopped) = mpsc::channel::<()>();

        std::thread::Builder::new()
            .name("aitch-watcher".to_string())
            .spawn(move || {
                loop {
                    // Block until something happens. No timeout, no polling.
                    match incoming.recv() {
                        Ok(_) => {}
                        // The watcher was dropped.
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
            })
            .ok()?;

        Some(Watcher {
            _inner: inner,
            _stop: stop,
        })
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
}
