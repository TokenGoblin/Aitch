//! Opens a throwaway window, clears the GDI surface to a visible color, and
//! keeps presenting it -- a manual, visual companion to
//! `surface.rs`'s automated tests, in the same spirit as `dump_frame.rs`
//! (PLAN.md §7's "visual checks stay manual, but make them repeatable").
//!
//! ```text
//! cargo run -p aitch-ui --example win32_surface_smoke
//! ```
//!
//! There is no real editor window to open yet -- `platform/win32/window.rs`
//! is a separate, parallel piece of Phase 0 work -- so this uses `surface.rs`'s
//! own minimal throwaway window rather than importing from that module. An
//! integration step later points this at the real window instead.
//!
//! The window closes itself after a few seconds, or immediately if closed by
//! hand.

use std::time::{Duration, Instant};

use aitch_ui::platform::win32::surface::{self, Color, Surface};

const WIDTH: i32 = 640;
const HEIGHT: i32 = 480;

fn main() {
    let Some(hwnd) = surface::open_throwaway_window(
        "aitch -- GDI surface smoke test (closes itself in 5s)",
        WIDTH,
        HEIGHT,
    ) else {
        eprintln!("could not create a window");
        std::process::exit(1);
    };
    surface::show_window(hwnd);

    let mut frame = Surface::new(hwnd, WIDTH as u32, HEIGHT as u32);
    // A saturated orange: nowhere near the black or white a wired-wrong
    // buffer or an unset clear color would default to, so it is obvious at a
    // glance whether this worked.
    let color = Color::from_rgb(0xFF, 0x8A, 0x00);

    eprintln!("clearing to r=0xFF g=0x8A b=0x00 -- an orange window should appear");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if surface::drain_messages() {
            eprintln!("window closed");
            break;
        }
        if Instant::now() >= deadline {
            eprintln!("5 seconds elapsed, closing");
            break;
        }
        // Re-presenting every frame (rather than once) means the color stays
        // on screen even though this window has no `WM_PAINT` handler to
        // repaint itself if briefly obscured -- out of scope for the Phase 0
        // clear-to-color surface.
        frame.clear(color);
        frame.present();
        std::thread::sleep(Duration::from_millis(16));
    }

    surface::destroy_window(hwnd);
}
