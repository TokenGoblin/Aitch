//! Opens a real, resizable Win32 window and proves the Phase 0 Track B
//! window/message loop actually works end to end: it can be resized, closed
//! by hand, and it exits cleanly.
//!
//! Safe to run unattended too — if nobody closes it, it closes itself after
//! `TIMEOUT`, which is what lets this run in CI without hanging a build.
//!
//! ```text
//! cargo run -p aitch-ui --example win32_window_smoke
//! ```

use std::time::{Duration, Instant};

use aitch_ui::platform::win32::window::{Event, Window};

const TIMEOUT: Duration = Duration::from_secs(5);

fn main() {
    let mut window = Window::new("aitch win32 smoke", 800, 600).expect("failed to open a window");

    println!(
        "opened at {}% scale ({} dpi) — resize it, or wait {}s",
        (window.scale_factor() * 100.0).round(),
        window.dpi(),
        TIMEOUT.as_secs(),
    );

    let deadline = Instant::now() + TIMEOUT;
    loop {
        match window.poll_event() {
            Some(Event::CloseRequested) => {
                println!("close requested; closing");
                window.close();
                break;
            }
            Some(Event::Resized { width, height }) => {
                println!("resized to {width}x{height} (physical pixels)");
            }
            Some(Event::ScaleChanged { scale, dpi }) => {
                println!("scale changed to {scale} ({dpi} dpi)");
            }
            // This example predates Phase 3's keyboard/mouse events; it has
            // nothing to do with them yet.
            Some(
                Event::KeyDown { .. }
                | Event::Char(_)
                | Event::MouseMove { .. }
                | Event::MouseButton { .. }
                | Event::MouseWheel { .. },
            ) => {}
            None => {
                if Instant::now() >= deadline {
                    println!("timed out with nobody closing it; closing");
                    window.close();
                    break;
                }
                // Non-blocking poll, so this sleep is what keeps the loop
                // from spinning a core the whole five seconds.
                std::thread::sleep(Duration::from_millis(16));
            }
        }
    }

    println!("closed cleanly");
}
