//! Raw GDI DIB-section software surface — Phase 0 Track C.
//!
//! `PLAN-ZERO-DEP.md` §3 makes the call this module implements: software
//! rendering instead of a hand-written Direct3D11/COM backend. Direct3D
//! without a crate means hand-written COM vtable calls *and* an HLSL
//! compiler dependency for basically no benefit in a text editor — see that
//! section for the full reasoning. So a frame here is just a CPU-side pixel
//! buffer, cleared and blitted with GDI: `StretchDIBits` onto a window's
//! device context, no Direct3D, no shaders, no crate.
//!
//! Every signature below is declared by hand, in the style
//! `crates/aitch/src/main.rs`'s `attach_to_parent_console` already
//! established: no `windows-sys`, no `winapi`, nothing from crates.io. An
//! `HWND` is carried as a plain `isize` rather than any richer type, which
//! keeps this module buildable and testable with no dependency on
//! `platform/win32/window.rs` (a separate, parallel piece of Phase 0 work) —
//! an integration step later wires the two together.
//!
//! This file also carries a small amount of throwaway window-creation code
//! (`open_throwaway_window` and friends) purely so this module's own tests
//! and its smoke example have a real `HWND` to blit into. It is deliberately
//! minimal and duplicates a little of what the real window module will do —
//! see `PLAN-ZERO-DEP.md` §4's parallel-tracks guidance, which expects
//! exactly that until the integration step replaces it.

use std::ffi::c_void;
use std::sync::Once;

use crate::text::raster::Coverage;

// ---- A CPU-side color, pixel buffer, and GDI presentation ----------------

/// A solid color, stored in the byte order GDI's DIB sections use natively:
/// blue, green, red, alpha.
///
/// Windows' 32bpp `BI_RGB` DIBs are BGRA in memory, not RGBA. Storing colors
/// in that order here means `Surface::clear` never has to reorder a single
/// channel per pixel — the one reorder (if the caller thinks in RGB) happens
/// exactly once, in the constructor below, rather than once per pixel in a
/// clear loop that might run over a million times a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    /// Already in on-the-wire order: blue, green, red, alpha.
    bgra: [u8; 4],
}

impl Color {
    /// Build an opaque color from red, green, blue — the order most callers
    /// think in — reordering it to BGRA once, here.
    #[must_use]
    pub const fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Color::from_rgba(r, g, b, 0xFF)
    }

    /// The same, with an explicit alpha.
    #[must_use]
    pub const fn from_rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Color { bgra: [b, g, r, a] }
    }

    /// The raw BGRA bytes this color fills a pixel with.
    #[must_use]
    pub const fn to_bgra_bytes(self) -> [u8; 4] {
        self.bgra
    }
}

/// A CPU-side pixel buffer plus a raw window handle to present it to.
///
/// The pixel buffer is BGRA (see [`Color`]), row-major, top row first —
/// GDI's native order for a top-down DIB, which `present` asks for via a
/// negative `biHeight` so no row-flip is ever needed.
///
/// `hwnd` is a plain `isize` rather than any richer handle type: the real
/// window (`platform/win32/window.rs`) is a separate, parallel piece of
/// Phase 0 work this module has no need to depend on. Any `HWND`-shaped
/// value converts losslessly to and from `isize` on Windows, so this is the
/// ordinary, dependency-free way to carry one.
pub struct Surface {
    hwnd: isize,
    width: u32,
    height: u32,
    /// BGRA, `width * height * 4` bytes.
    pixels: Vec<u8>,
}

impl Surface {
    /// A new surface for `hwnd`, with a zeroed (opaque black) pixel buffer.
    #[must_use]
    pub fn new(hwnd: isize, width: u32, height: u32) -> Self {
        Surface {
            hwnd,
            width,
            height,
            pixels: vec![0u8; buffer_len(width, height)],
        }
    }

    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Replace the pixel buffer for a new size, discarding old content.
    ///
    /// A resized window has a different row count and stride, so there is no
    /// sensible way to carry the old bytes forward; the next `clear` +
    /// `present` repaints the whole thing anyway.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.pixels = vec![0u8; buffer_len(width, height)];
    }

    /// The raw BGRA pixel buffer, for reading back (tests, and later a
    /// screenshot path).
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The same buffer, mutably — Phase 1's glyph rasterizer writes here.
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    /// Fill the whole buffer with one solid color.
    pub fn clear(&mut self, color: Color) {
        let bytes = color.to_bgra_bytes();
        for pixel in self.pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&bytes);
        }
    }

    /// Alpha-blend a rasterized glyph onto the buffer: standard "over"
    /// compositing, `result = dst * (1 - a) + color * a`, applied per BGRA
    /// byte (the destination's alpha byte included, so it stays consistent
    /// with whatever `clear`/a previous blend already put there).
    ///
    /// `a` is not `coverage`'s byte alone: it folds in `color`'s own alpha
    /// too, as `a = coverage_byte * color.alpha / 255`. A translucent
    /// `color` should still only ink a pixel in proportion to how much of
    /// the glyph covers it, not paint at full strength wherever the glyph
    /// has *any* ink. `Color::from_rgb` is always fully opaque
    /// (`alpha == 0xFF`), so for that constructor this reduces to `a =
    /// coverage_byte` exactly — the plain coverage-driven blend this method
    /// is specified against — and the two boundary cases stay exact:
    /// coverage `0` leaves the destination byte-for-byte unchanged (`a = 0`
    /// makes the formula collapse to `dst`, no rounding drift), coverage
    /// `255` with an opaque `color` sets the destination to exactly
    /// `color`'s bytes (`a = 255` makes it collapse to `color`).
    ///
    /// `(x, y)` is `coverage`'s top-left corner in surface pixel
    /// coordinates, signed because a glyph can hang off any edge — including
    /// being positioned fully off-surface — which happens constantly for
    /// glyphs near a window edge. Only pixels inside `0..width` /
    /// `0..height` are ever touched; out-of-bounds rows/columns of
    /// `coverage` are silently clipped rather than indexed, and a
    /// zero-sized `coverage` (an empty glyph, e.g. a space) draws nothing.
    pub fn draw_coverage(&mut self, x: i32, y: i32, coverage: &Coverage, color: Color) {
        if coverage.width == 0 || coverage.height == 0 {
            return;
        }

        // Intersect the coverage bitmap's rect with the surface's bounds, in
        // surface coordinates. Widened to i64 throughout so a negative or
        // huge x/y (a glyph fully off any edge) can never overflow or wrap
        // the way raw u32/i32 pointer-style arithmetic could.
        let surface_width = i64::from(self.width);
        let surface_height = i64::from(self.height);
        let cov_x0 = i64::from(x);
        let cov_y0 = i64::from(y);
        let cov_x1 = cov_x0 + i64::from(coverage.width);
        let cov_y1 = cov_y0 + i64::from(coverage.height);

        let dst_x0 = cov_x0.max(0);
        let dst_y0 = cov_y0.max(0);
        let dst_x1 = cov_x1.min(surface_width);
        let dst_y1 = cov_y1.min(surface_height);

        if dst_x0 >= dst_x1 || dst_y0 >= dst_y1 {
            return; // no overlap with the surface at all
        }

        let bgra = color.to_bgra_bytes();
        let stride = self.width as usize * 4;

        for surface_y in dst_y0..dst_y1 {
            // In range [0, coverage.height) by construction: surface_y is in
            // [dst_y0, dst_y1) which is clamped inside [cov_y0, cov_y1).
            let cov_row = (surface_y - cov_y0) as u32;
            let row_base = surface_y as usize * stride;

            for surface_x in dst_x0..dst_x1 {
                let cov_col = (surface_x - cov_x0) as u32;
                let glyph_alpha = coverage.pixel(cov_col, cov_row);
                if glyph_alpha == 0 {
                    continue; // leave this destination pixel untouched
                }

                let alpha = (u32::from(glyph_alpha) * u32::from(bgra[3]) + 127) / 255;
                let offset = row_base + surface_x as usize * 4;
                let dst = &mut self.pixels[offset..offset + 4];
                for channel in 0..4 {
                    let d = u32::from(dst[channel]);
                    let s = u32::from(bgra[channel]);
                    dst[channel] = ((s * alpha + d * (255 - alpha) + 127) / 255) as u8;
                }
            }
        }
    }

    /// Blit the pixel buffer onto the window with `StretchDIBits`.
    ///
    /// A DIB section (`CreateDIBSection`) is not needed here: `StretchDIBits`
    /// blits directly out of an ordinary memory buffer we already own, with
    /// no GDI bitmap object to create, select, or clean up. That keeps this
    /// path to one GDI call instead of `CreateDIBSection` +
    /// `CreateCompatibleDC` + `SelectObject` + `BitBlt` for the same result.
    pub fn present(&self) {
        if self.width == 0 || self.height == 0 {
            return;
        }

        let info = BitmapInfo {
            header: BitmapInfoHeader {
                size: std::mem::size_of::<BitmapInfoHeader>() as u32,
                width: self.width as i32,
                // Negative selects a top-down DIB, matching the row order
                // `pixels` is already stored in — otherwise this would need
                // to flip every row before every present.
                height: -(self.height as i32),
                planes: 1,
                bit_count: 32,
                compression: BI_RGB,
                size_image: 0,
                x_pels_per_meter: 0,
                y_pels_per_meter: 0,
                clr_used: 0,
                clr_important: 0,
            },
            colors: [0],
        };

        unsafe {
            let hdc = GetDC(self.hwnd);
            if hdc == 0 {
                return;
            }
            StretchDIBits(
                hdc,
                0,
                0,
                self.width as i32,
                self.height as i32,
                0,
                0,
                self.width as i32,
                self.height as i32,
                self.pixels.as_ptr().cast::<c_void>(),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
            ReleaseDC(self.hwnd, hdc);
        }
    }
}

fn buffer_len(width: u32, height: u32) -> usize {
    width as usize * height as usize * 4
}

const BI_RGB: u32 = 0;
const DIB_RGB_COLORS: u32 = 0;
const SRCCOPY: u32 = 0x00CC_0020;

#[repr(C)]
struct BitmapInfoHeader {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bit_count: u16,
    compression: u32,
    size_image: u32,
    x_pels_per_meter: i32,
    y_pels_per_meter: i32,
    clr_used: u32,
    clr_important: u32,
}

/// `BITMAPINFO` for a 32bpp `BI_RGB` image: no palette, but `StretchDIBits`
/// still expects the trailing color-table field to be present in the shape,
/// even though it reads none of it at this bit depth.
#[repr(C)]
struct BitmapInfo {
    header: BitmapInfoHeader,
    colors: [u32; 1],
}

#[link(name = "gdi32")]
extern "system" {
    fn StretchDIBits(
        hdc: isize,
        x_dst: i32,
        y_dst: i32,
        dst_width: i32,
        dst_height: i32,
        x_src: i32,
        y_src: i32,
        src_width: i32,
        src_height: i32,
        bits: *const c_void,
        bmi: *const BitmapInfo,
        usage: u32,
        rop: u32,
    ) -> i32;
}

// Only the "extra confidence" test reads a pixel back off a real window.
#[cfg(test)]
#[link(name = "gdi32")]
extern "system" {
    fn GetPixel(hdc: isize, x: i32, y: i32) -> u32;
}

#[link(name = "user32")]
extern "system" {
    fn GetDC(hwnd: isize) -> isize;
    fn ReleaseDC(hwnd: isize, hdc: isize) -> i32;
}

// ---- A throwaway window, for this module's own tests and smoke example ---
//
// The real window (`platform/win32/window.rs`) is a different, parallel
// Phase 0 track. Rather than wait on it or depend on its in-progress code,
// this is a few lines of the same `RegisterClassExW` / `CreateWindowExW`
// shape, kept intentionally minimal — an integration step later swaps it out
// for the real shared window.

type WndProc = unsafe extern "system" fn(isize, u32, usize, isize) -> isize;

#[repr(C)]
struct WndClassExW {
    cb_size: u32,
    style: u32,
    wnd_proc: WndProc,
    cls_extra: i32,
    wnd_extra: i32,
    instance: isize,
    icon: isize,
    cursor: isize,
    background: isize,
    menu_name: *const u16,
    class_name: *const u16,
    icon_sm: isize,
}

/// `MSG`. Field order (and so, on a `repr(C)` struct, layout) matches the
/// real Win32 struct exactly.
#[repr(C)]
#[derive(Default)]
struct Msg {
    hwnd: isize,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt_x: i32,
    pt_y: i32,
}

const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
const CW_USEDEFAULT: i32 = 0x8000_0000u32 as i32;
const SW_SHOW: i32 = 5;
const SW_SHOWNOACTIVATE: i32 = 4;
const WM_DESTROY: u32 = 0x0002;
const WM_QUIT: u32 = 0x0012;
const PM_REMOVE: u32 = 0x0001;
/// `MAKEINTRESOURCEW(32512)` — `IDC_ARROW`, the ordinary pointer cursor.
const IDC_ARROW: *const u16 = 32512usize as *const u16;

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> isize;
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassExW(class: *const WndClassExW) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class_name: *const u16,
        window_name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: isize,
        menu: isize,
        instance: isize,
        param: *const c_void,
    ) -> isize;
    fn DestroyWindow(hwnd: isize) -> i32;
    fn DefWindowProcW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn PostQuitMessage(exit_code: i32);
    fn ShowWindow(hwnd: isize, cmd_show: i32) -> i32;
    fn UpdateWindow(hwnd: isize) -> i32;
    fn LoadCursorW(instance: isize, name: *const u16) -> isize;
    fn PeekMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32, remove: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
}

unsafe extern "system" fn throwaway_wndproc(
    hwnd: isize,
    msg: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    if msg == WM_DESTROY {
        unsafe {
            PostQuitMessage(0);
        }
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Open a minimal top-level window for this module's own testing — not the
/// real editor window. Registers its window class at most once per process.
///
/// Returns `None` if window creation failed (for example, no desktop session
/// is available), so callers can skip rather than panic.
#[must_use]
pub fn open_throwaway_window(title: &str, width: i32, height: i32) -> Option<isize> {
    static REGISTER: Once = Once::new();
    let class_name = wide("AitchSurfaceThrowawayWindow");

    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());

        REGISTER.call_once(|| {
            let class = WndClassExW {
                cb_size: std::mem::size_of::<WndClassExW>() as u32,
                style: 0,
                wnd_proc: throwaway_wndproc,
                cls_extra: 0,
                wnd_extra: 0,
                instance,
                icon: 0,
                cursor: LoadCursorW(0, IDC_ARROW),
                background: 0,
                menu_name: std::ptr::null(),
                class_name: class_name.as_ptr(),
                icon_sm: 0,
            };
            RegisterClassExW(&class);
        });

        let window_name = wide(title);
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            window_name.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            width,
            height,
            0,
            0,
            instance,
            std::ptr::null(),
        );

        if hwnd == 0 {
            None
        } else {
            Some(hwnd)
        }
    }
}

/// Show a window opened with [`open_throwaway_window`] and paint it once.
pub fn show_window(hwnd: isize) {
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
    }
}

/// The same, without stealing focus — for automated tests that show a window
/// only so GDI reads back real content, not for a human to look at.
pub fn show_window_without_activating(hwnd: isize) {
    unsafe {
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
}

pub fn destroy_window(hwnd: isize) {
    unsafe {
        DestroyWindow(hwnd);
    }
}

/// Drain whatever messages are already queued for this thread's windows,
/// without blocking. Returns `true` once `WM_QUIT` has been seen (the window
/// was closed), so a caller's own loop knows to stop.
pub fn drain_messages() -> bool {
    let mut msg = Msg::default();
    unsafe {
        while PeekMessageW(&mut msg, 0, 0, 0, PM_REMOVE) != 0 {
            if msg.message == WM_QUIT {
                return true;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- reliable everywhere: no window, no display, just the buffer -------

    #[test]
    fn clear_fills_every_pixel_with_the_exact_bgra_bytes() {
        let mut surface = Surface::new(0, 4, 3);
        let color = Color::from_rgba(0x11, 0x22, 0x33, 0x44);
        surface.clear(color);

        assert_eq!(surface.pixels().len(), 4 * 3 * 4);
        for pixel in surface.pixels().chunks_exact(4) {
            // BGRA order: blue, green, red, alpha -- not the RGBA the color
            // was specified in.
            assert_eq!(pixel, [0x33, 0x22, 0x11, 0x44]);
        }
    }

    #[test]
    fn from_rgb_defaults_to_fully_opaque() {
        let color = Color::from_rgb(0x01, 0x02, 0x03);
        assert_eq!(color.to_bgra_bytes(), [0x03, 0x02, 0x01, 0xFF]);
    }

    #[test]
    fn resize_reallocates_to_the_new_pixel_count_and_starts_cleared() {
        let mut surface = Surface::new(0, 2, 2);
        surface.clear(Color::from_rgb(0xFF, 0xFF, 0xFF));

        surface.resize(5, 7);

        assert_eq!(surface.width(), 5);
        assert_eq!(surface.height(), 7);
        assert_eq!(surface.pixels().len(), 5 * 7 * 4);
        assert!(
            surface.pixels().iter().all(|&b| b == 0),
            "a resize must not carry stale pixels forward at the wrong stride"
        );
    }

    #[test]
    fn pixels_mut_is_writable_and_visible_through_pixels() {
        let mut surface = Surface::new(0, 1, 1);
        surface.pixels_mut().copy_from_slice(&[9, 8, 7, 6]);
        assert_eq!(surface.pixels(), [9, 8, 7, 6]);
    }

    #[test]
    fn present_on_an_invalid_handle_does_not_panic() {
        // hwnd 0 is never a real window; GetDC on it fails, and present must
        // treat that as nothing to do rather than unwrap a null DC.
        let mut surface = Surface::new(0, 4, 4);
        surface.clear(Color::from_rgb(1, 2, 3));
        surface.present();
    }

    #[test]
    fn present_on_a_zero_sized_surface_does_not_panic() {
        let surface = Surface::new(0, 0, 0);
        surface.present();
    }

    // -- draw_coverage -------------------------------------------------

    /// A solid-color `Coverage` rectangle: every pixel set to `value`.
    fn solid_coverage(width: u32, height: u32, value: u8) -> Coverage {
        Coverage {
            width,
            height,
            pixels: vec![value; (width as usize) * (height as usize)],
        }
    }

    fn clear_color() -> Color {
        Color::from_rgba(0xAA, 0xBB, 0xCC, 0xDD)
    }

    #[test]
    fn fully_opaque_coverage_replaces_covered_pixels_exactly_and_leaves_the_rest() {
        let mut surface = Surface::new(0, 10, 8);
        surface.clear(clear_color());
        let color = Color::from_rgb(0x10, 0x20, 0x30);
        let coverage = solid_coverage(4, 3, 255);

        surface.draw_coverage(2, 2, &coverage, color);

        let expected = color.to_bgra_bytes();
        let background = clear_color().to_bgra_bytes();
        for y in 0..8u32 {
            for x in 0..10u32 {
                let inside = (2..6).contains(&x) && (2..5).contains(&y);
                let offset = ((y * 10 + x) * 4) as usize;
                let pixel = &surface.pixels()[offset..offset + 4];
                if inside {
                    assert_eq!(pixel, expected, "covered pixel ({x},{y})");
                } else {
                    assert_eq!(pixel, background, "untouched pixel ({x},{y})");
                }
            }
        }
    }

    #[test]
    fn zero_coverage_byte_leaves_the_destination_pixel_byte_for_byte_unchanged() {
        let mut surface = Surface::new(0, 3, 3);
        surface.clear(clear_color());
        let background = clear_color().to_bgra_bytes();

        let coverage = solid_coverage(3, 3, 0);
        surface.draw_coverage(0, 0, &coverage, Color::from_rgb(0xFF, 0x00, 0x00));

        for pixel in surface.pixels().chunks_exact(4) {
            assert_eq!(pixel, background);
        }
    }

    #[test]
    fn partial_coverage_blends_to_the_exact_hand_computed_bytes() {
        let mut surface = Surface::new(0, 1, 1);
        // Background pixel, chosen so the arithmetic below is easy to check
        // by hand: BGRA = (10, 20, 30, 40).
        surface.pixels_mut().copy_from_slice(&[10, 20, 30, 40]);

        // Fully opaque color, BGRA = (200, 150, 100, 255) via from_rgb(100,
        // 150, 200) (R, G, B).
        let color = Color::from_rgb(100, 150, 200);
        let coverage = solid_coverage(1, 1, 128);

        surface.draw_coverage(0, 0, &coverage, color);

        // alpha = (128 * 255 + 127) / 255 = (32640 + 127) / 255 = 32767/255 = 128
        // per channel: (s*alpha + d*(255-alpha) + 127) / 255, with alpha=128,
        // 255-alpha=127.
        let expect_channel = |s: u32, d: u32| -> u8 { ((s * 128 + d * 127 + 127) / 255) as u8 };
        let expected = [
            expect_channel(200, 10), // B
            expect_channel(150, 20), // G
            expect_channel(100, 30), // R
            expect_channel(255, 40), // A
        ];
        // Sanity: hand-expand so the test doesn't just re-derive the
        // production formula under a different name.
        // B: (200*128 + 10*127 + 127) / 255 = 26997 / 255 = 105
        // G: (150*128 + 20*127 + 127) / 255 = 21867 / 255 = 85
        // R: (100*128 + 30*127 + 127) / 255 = 16737 / 255 = 65
        // A: (255*128 + 40*127 + 127) / 255 = 37847 / 255 = 148
        assert_eq!(expected, [105, 85, 65, 148]);
        assert_eq!(surface.pixels(), expected);
    }

    #[test]
    fn coverage_hanging_off_the_top_left_draws_only_the_in_bounds_portion() {
        let mut surface = Surface::new(0, 4, 4);
        surface.clear(clear_color());
        let color = Color::from_rgb(1, 2, 3);
        let coverage = solid_coverage(3, 3, 255);

        // Top-left corner of the coverage sits at (-1, -1): only its bottom-
        // right 2x2 quadrant (coverage-local (1,1)..(3,3)) lands on the
        // surface, covering surface pixels (0,0)..(2,2).
        surface.draw_coverage(-1, -1, &coverage, color);

        let expected = color.to_bgra_bytes();
        let background = clear_color().to_bgra_bytes();
        assert_eq!(surface.pixels().len(), 4 * 4 * 4, "buffer size unchanged");
        for y in 0..4u32 {
            for x in 0..4u32 {
                let offset = ((y * 4 + x) * 4) as usize;
                let pixel = &surface.pixels()[offset..offset + 4];
                if x < 2 && y < 2 {
                    assert_eq!(pixel, expected, "covered pixel ({x},{y})");
                } else {
                    assert_eq!(pixel, background, "untouched pixel ({x},{y})");
                }
            }
        }
    }

    #[test]
    fn coverage_hanging_off_the_bottom_right_draws_only_the_in_bounds_portion() {
        let mut surface = Surface::new(0, 4, 4);
        surface.clear(clear_color());
        let color = Color::from_rgb(1, 2, 3);
        let coverage = solid_coverage(3, 3, 255);

        // Top-left corner of the coverage sits at (2, 2): it extends to
        // (5, 5), so only its top-left 2x2 quadrant lands on the surface,
        // covering surface pixels (2,2)..(4,4).
        surface.draw_coverage(2, 2, &coverage, color);

        let expected = color.to_bgra_bytes();
        let background = clear_color().to_bgra_bytes();
        assert_eq!(surface.pixels().len(), 4 * 4 * 4, "buffer size unchanged");
        for y in 0..4u32 {
            for x in 0..4u32 {
                let offset = ((y * 4 + x) * 4) as usize;
                let pixel = &surface.pixels()[offset..offset + 4];
                if x >= 2 && y >= 2 {
                    assert_eq!(pixel, expected, "covered pixel ({x},{y})");
                } else {
                    assert_eq!(pixel, background, "untouched pixel ({x},{y})");
                }
            }
        }
    }

    #[test]
    fn coverage_entirely_off_surface_draws_nothing_and_does_not_panic() {
        let mut surface = Surface::new(0, 4, 4);
        surface.clear(clear_color());
        let background = clear_color().to_bgra_bytes();
        let coverage = solid_coverage(5, 5, 255);

        surface.draw_coverage(-1000, -1000, &coverage, Color::from_rgb(9, 9, 9));
        surface.draw_coverage(1000, 1000, &coverage, Color::from_rgb(9, 9, 9));
        surface.draw_coverage(-1000, 0, &coverage, Color::from_rgb(9, 9, 9));
        surface.draw_coverage(0, 1000, &coverage, Color::from_rgb(9, 9, 9));

        for pixel in surface.pixels().chunks_exact(4) {
            assert_eq!(pixel, background);
        }
    }

    #[test]
    fn zero_sized_coverage_draws_nothing_and_does_not_panic() {
        let mut surface = Surface::new(0, 4, 4);
        surface.clear(clear_color());
        let background = clear_color().to_bgra_bytes();

        surface.draw_coverage(0, 0, &Coverage::empty(), Color::from_rgb(9, 9, 9));

        for pixel in surface.pixels().chunks_exact(4) {
            assert_eq!(pixel, background);
        }
    }

    // -- extra confidence: a real window, skipped if one can't be made -----

    #[test]
    fn present_actually_reaches_the_window() {
        let Some(hwnd) = open_throwaway_window("aitch surface test", 200, 150) else {
            eprintln!("SKIPPED: could not create a test window");
            return;
        };
        show_window_without_activating(hwnd);

        let mut surface = Surface::new(hwnd, 200, 150);
        let color = Color::from_rgb(0x10, 0x80, 0xF0);
        surface.clear(color);
        surface.present();

        let pixel = unsafe {
            let hdc = GetDC(hwnd);
            let value = GetPixel(hdc, 100, 75);
            ReleaseDC(hwnd, hdc);
            value
        };
        destroy_window(hwnd);

        const CLR_INVALID: u32 = 0xFFFF_FFFF;
        if pixel == CLR_INVALID {
            eprintln!("SKIPPED: GetPixel could not read the test window");
            return;
        }

        // COLORREF is 0x00bbggrr.
        let r = (pixel & 0xFF) as u8;
        let g = ((pixel >> 8) & 0xFF) as u8;
        let b = ((pixel >> 16) & 0xFF) as u8;
        assert_eq!((r, g, b), (0x10, 0x80, 0xF0));
    }
}
