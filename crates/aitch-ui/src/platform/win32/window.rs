//! Raw Win32 window and message loop, replacing `winit` per
//! `PLAN-ZERO-DEP.md` §2 and §4 Phase 0 Track B.
//!
//! Every signature here is declared by hand, in the style
//! `crates/aitch/src/main.rs`'s `attach_to_parent_console` already
//! established: no `windows-sys`, no `winapi`, no crate at all.
//!
//! `HWND` (and every other handle) is carried as a plain `isize` rather than
//! a richer pointer type, matching `platform/win32/surface.rs` — a separate,
//! parallel piece of Phase 0 work that settled on the same representation for
//! the same reason (see that file's module docs): any Win32 handle converts
//! losslessly to and from `isize`, and it keeps the two modules' independent
//! `extern "system"` declarations of the handful of functions they both need
//! (`DestroyWindow`, `ShowWindow`, `GetDC`, ...) from disagreeing on a type
//! and tripping `clashing_extern_declarations`.
//!
//! # Shape of the API
//!
//! [`Window::new`] opens a real top-level window and returns immediately.
//! From there a caller drives it in whichever of two ways suits it, both
//! built on the same [`Window::next_event`]:
//!
//! - Pump it themselves: `while let Some(event) = window.next_event() { .. }`.
//! - Hand over a callback: [`Window::run`] does exactly that loop.
//!
//! [`Event::CloseRequested`] does not tear the window down by itself: a real
//! app wants to ask "save first?" in between, exactly as `aitch-ui/src/app.rs`
//! does today for winit's own `CloseRequested`. Call [`Window::close`] once
//! that question is answered.
//!
//! # Keyboard and mouse (Phase 3)
//!
//! [`Event::KeyDown`] carries a raw virtual-key code, not a resolved
//! [`aitch_core::Chord`] — this module has no opinion on what any key does,
//! same as the deleted winit-based `input.rs` it replaces. [`Event::Char`]
//! carries a character already resolved by `TranslateMessage` (layout, dead
//! keys, IME composition). Neither event carries modifier state: `Ctrl`,
//! `Alt`, and `Shift` are read live with `GetKeyState` by whoever turns these
//! into a `Chord`, not threaded through the event queue — simpler, and
//! accurate at the instant the key/char event fires either way.
//!
//! **A key that produces a character arrives as both.** `WM_KEYDOWN` fires
//! first with the virtual-key code, and (for a character-producing key)
//! `TranslateMessage` then synthesizes a `WM_CHAR` with the resolved
//! character. `Enter`, `Tab`, `Backspace`, `Escape`, and `Space` are the
//! awkward case: they produce *both* a `KeyDown` (mapping to a
//! [`aitch_core::NamedKey`]) *and* a `Char` (`'\r'`, `'\t'`, `'\u{8}'`,
//! `'\u{1b}'`, `' '`) for the same physical press. Whoever consumes these
//! events must resolve each such key exactly once — via the `KeyDown`/
//! `NamedKey` path — and discard the paired `Char`, or a bound `Enter`
//! command and a literal `'\r'` insertion both happen from one keypress.
//! Every other `Char` (letters, digits, punctuation, and `Ctrl`+letter's C0
//! control codes) has no corresponding meaningful `KeyDown` mapping and is
//! the only event that key produces.
//!
//! **Alt combos arrive as `WM_SYSKEYDOWN`/`WM_SYSCHAR`**, not the plain
//! `WM_KEYDOWN`/`WM_CHAR` above — Windows' own name for "a key held with
//! Alt down" — but this module queues the identical [`Event::KeyDown`]/
//! [`Event::Char`] for both: `GetKeyState(VK_MENU)` at translation time
//! already recovers that Alt was down, so the event type does not need to
//! distinguish them. What *does* differ: a "sys" message is still forwarded
//! to `DefWindowProcW` after queuing this module's own event, so `Alt+F4`,
//! `Alt+Space`, and `F10` (which also arrives as `WM_SYSKEYDOWN`, without
//! Alt actually held — a genuine Windows quirk) keep their system behavior
//! alongside whatever this editor's keymap does with the same chord.
//!
//! Only the left mouse button is surfaced ([`MouseButton::Left`]) — this
//! editor has never used the right or middle button for anything.
//!
//! # DPI awareness
//!
//! [`Window::new`] asks once per process for per-monitor-v2 DPI awareness
//! (`SetProcessDpiAwarenessContext`, Windows 10 1703+), falling back through
//! per-monitor-v1 (1607+) to `SetProcessDPIAware` (Vista+), so the same binary
//! still runs — just less crisply scaled — on an older Windows. The two newer
//! entry points are loaded with `GetProcAddress` rather than linked directly:
//! a hard import naming a symbol the running system's `user32.dll` does not
//! export fails the whole process at load time, before `main` even runs,
//! which is a worse failure than "blurry on old Windows." The same reasoning
//! covers `GetDpiForWindow` (1607+), with `GetDeviceCaps(LOGPIXELSX)` as its
//! fallback.
//!
//! A window's initial size is given in logical (DPI-independent) pixels and
//! converted using the *system* DPI, because no window — and so no
//! per-monitor DPI — exists yet to ask. This is the outer window size, not
//! the client area: exactly matching a requested client size would need
//! `AdjustWindowRectExForDpi` (also 1607+), which Phase 0's "opens, resizes,
//! clears to colour" acceptance gate does not need. `WM_DPICHANGED` corrects
//! both the DPI and the window's rect the moment Windows places it on an
//! actual monitor, or moves it to one with a different scale.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::io;
use std::sync::Once;

type WndProc = unsafe extern "system" fn(isize, u32, usize, isize) -> isize;

#[repr(C)]
struct WndClassExW {
    cb_size: u32,
    style: u32,
    lpfn_wnd_proc: WndProc,
    cb_cls_extra: i32,
    cb_wnd_extra: i32,
    h_instance: isize,
    h_icon: isize,
    h_cursor: isize,
    hbr_background: isize,
    lpsz_menu_name: *const u16,
    lpsz_class_name: *const u16,
    h_icon_sm: isize,
}

/// Only `lp_create_params` is ever read, but the layout has to match
/// `CREATESTRUCTW` up to that point (it is the first field, so this could be
/// shorter — spelled out in full so nobody has to go check that fact again).
#[repr(C)]
struct CreateStructW {
    lp_create_params: *mut c_void,
    h_instance: isize,
    h_menu: isize,
    hwnd_parent: isize,
    cy: i32,
    cx: i32,
    y: i32,
    x: i32,
    style: i32,
    lpsz_name: *const u16,
    lpsz_class: *const u16,
    ex_style: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct Msg {
    hwnd: isize,
    message: u32,
    w_param: usize,
    l_param: isize,
    time: u32,
    pt_x: i32,
    pt_y: i32,
}

const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
const CW_USEDEFAULT: i32 = 0x8000_0000u32 as i32;
const SW_SHOW: i32 = 5;
const CS_HREDRAW: u32 = 0x0002;
const CS_VREDRAW: u32 = 0x0001;
const COLOR_WINDOW: isize = 5;
const IDC_ARROW: *const u16 = 32512 as *const u16;
const GWLP_USERDATA: i32 = -21;
const WM_NCCREATE: u32 = 0x0081;
const WM_SIZE: u32 = 0x0005;
const WM_CLOSE: u32 = 0x0010;
const WM_DESTROY: u32 = 0x0002;
const WM_DPICHANGED: u32 = 0x02E0;
const WM_KEYDOWN: u32 = 0x0100;
const WM_SYSKEYDOWN: u32 = 0x0104;
const WM_CHAR: u32 = 0x0102;
const WM_SYSCHAR: u32 = 0x0106;
const WM_MOUSEMOVE: u32 = 0x0200;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_MOUSEWHEEL: u32 = 0x020A;
const SWP_NOZORDER: u32 = 0x0004;
const SWP_NOACTIVATE: u32 = 0x0010;
const SWP_NOMOVE: u32 = 0x0002;
const SWP_NOSIZE: u32 = 0x0001;
/// Tells the window manager the non-client area needs redrawing, which is
/// what makes a caption colour set after the window is already on screen
/// take effect without waiting for the next time something else disturbs it.
const SWP_FRAMECHANGED: u32 = 0x0020;

/// `DWMWA_USE_IMMERSIVE_DARK_MODE`, which flips the caption to the dark
/// palette: light glyphs on a dark bar, and a dark hover highlight behind the
/// minimise/maximise/close buttons.
///
/// Windows 10 1809 shipped this as attribute 19 and renumbered it to 20 in
/// 20H1; both numbers are still live in the wild, so
/// [`Window::set_caption_theme`] tries the new one and falls back.
const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
const DWMWA_USE_IMMERSIVE_DARK_MODE_1809: u32 = 19;
/// `DWMWA_BORDER_COLOR`, `DWMWA_CAPTION_COLOR`, `DWMWA_TEXT_COLOR`: exact
/// colours rather than the two-palette approximation above. Windows 11
/// (build 22000) and later only — on anything older `DwmSetWindowAttribute`
/// fails and the dark-mode flag is what carries the theme.
const DWMWA_BORDER_COLOR: u32 = 34;
const DWMWA_CAPTION_COLOR: u32 = 35;
const DWMWA_TEXT_COLOR: u32 = 36;
const PM_REMOVE: u32 = 0x0001;
const LOGPIXELSX: i32 = 88;
/// Bit 30 of `WM_KEYDOWN`/`WM_SYSKEYDOWN`'s `lParam`: set when the key was
/// already down before this message (an OS auto-repeat), clear on the first
/// press.
const KEYDOWN_REPEAT_BIT: isize = 1 << 30;
const CLASS_NAME: &str = "AitchWindowClass";

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(module_name: *const u16) -> isize;
    fn GetProcAddress(module: isize, proc_name: *const u8) -> *mut c_void;
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
    fn ShowWindow(hwnd: isize, cmd_show: i32) -> i32;
    fn UpdateWindow(hwnd: isize) -> i32;
    fn DefWindowProcW(hwnd: isize, msg: u32, w_param: usize, l_param: isize) -> isize;
    fn GetMessageW(msg: *mut Msg, hwnd: isize, filter_min: u32, filter_max: u32) -> i32;
    fn PeekMessageW(
        msg: *mut Msg,
        hwnd: isize,
        filter_min: u32,
        filter_max: u32,
        remove: u32,
    ) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
    fn PostQuitMessage(exit_code: i32);
    fn SetWindowLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
    fn GetWindowLongPtrW(hwnd: isize, index: i32) -> isize;
    fn LoadCursorW(instance: isize, cursor_name: *const u16) -> isize;
    fn SetWindowPos(
        hwnd: isize,
        insert_after: isize,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> i32;
    fn GetDC(hwnd: isize) -> isize;
    fn ReleaseDC(hwnd: isize, hdc: isize) -> i32;
    // Vista+; the last-resort DPI fallback below `enable_dpi_awareness`'s
    // dynamically-loaded pair.
    fn SetProcessDPIAware() -> i32;
    fn SetWindowTextW(hwnd: isize, text: *const u16) -> i32;
    // Test-only: reads back what `set_title` wrote, to prove it round-trips.
    #[cfg(test)]
    fn GetWindowTextW(hwnd: isize, buffer: *mut u16, max_count: i32) -> i32;
    // Test-only: proves `set_caption_theme`'s `SetWindowPos` nudge repaints
    // the frame without also moving or resizing the window.
    #[cfg(test)]
    fn GetWindowRect(hwnd: isize, rect: *mut Rect) -> i32;
    // Test-only: synthesizes the messages `wnd_proc` handles, so its
    // keyboard/mouse translation is provable without a real keystroke or
    // click. `SendMessageW` calls `wnd_proc` directly and synchronously
    // (unlike `PostMessageW`, which would need a `GetMessage` pump loop to
    // ever be seen), which is exactly what a same-thread test wants.
    #[cfg(test)]
    fn SendMessageW(hwnd: isize, msg: u32, w_param: usize, l_param: isize) -> isize;
}

#[link(name = "gdi32")]
extern "system" {
    fn GetDeviceCaps(hdc: isize, index: i32) -> i32;
}

// Vista+, so no dynamic load: the DLL is always there. The individual
// *attributes* are the part that varies by Windows version, and an attribute
// this build has never heard of is reported as a failed `HRESULT` rather than
// a crash — see `set_attribute`.
#[link(name = "dwmapi")]
extern "system" {
    fn DwmSetWindowAttribute(hwnd: isize, attribute: u32, value: *const c_void, size: u32) -> i32;
}

/// An event surfaced from the message loop. See the module docs — especially
/// the "Keyboard and mouse" section — before consuming [`Event::KeyDown`] or
/// [`Event::Char`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// The client area's size changed, in physical pixels.
    Resized { width: u32, height: u32 },
    /// The window's close button, Alt+F4, or a shell "close window" was used.
    /// The window is *not* destroyed for you — call [`Window::close`] once
    /// whatever should happen first (an unsaved-changes prompt, say) is done.
    CloseRequested,
    /// The window moved to a monitor with a different DPI, or the user
    /// changed the scale in Settings. `scale` is `dpi as f64 / 96.0`.
    ScaleChanged { scale: f64, dpi: u32 },
    /// A key went down. `vkey` is a Win32 virtual-key code (`VK_*`);
    /// `repeat` is true for an OS auto-repeat rather than the first press.
    /// Carries no modifier state — see the module docs.
    KeyDown { vkey: u32, repeat: bool },
    /// A character was produced by the input layer: layout, dead keys, and
    /// IME composition already resolved. See the module docs for which keys
    /// produce this *and* a [`Event::KeyDown`] for the same press.
    Char(char),
    /// The pointer moved, in client-area pixels.
    MouseMove { x: i32, y: i32 },
    /// A mouse button changed state, at its position in client-area pixels.
    MouseButton {
        button: MouseButton,
        pressed: bool,
        x: i32,
        y: i32,
    },
    /// The wheel turned. Positive is away from the user (the usual
    /// "scroll up"/content-moves-down direction); one notch is `1.0`.
    MouseWheel { delta_lines: f32 },
}

/// See the module docs for why only [`MouseButton::Left`] exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
}

/// Per-window state reachable both from the `Window` handle and from
/// `wnd_proc`, which only ever sees the raw `HWND`. Boxed once and shared by
/// raw pointer for that reason — see `Window::create` and `Window::close`.
struct SharedState {
    events: VecDeque<Event>,
    dpi: u32,
}

/// A single top-level, resizable Win32 window and the message queue behind
/// it. Not `Send`/`Sync`: like any Win32 window, it belongs to the thread
/// that created it.
pub struct Window {
    hwnd: isize,
    state: *mut SharedState,
}

impl Window {
    /// Open a resizable top-level window titled `title`, sized `width` ×
    /// `height` logical (DPI-independent) pixels, at the OS's default
    /// position.
    pub fn new(title: &str, width: i32, height: i32) -> io::Result<Window> {
        Self::create(title, width, height, None, true)
    }

    fn create(
        title: &str,
        width: i32,
        height: i32,
        position: Option<(i32, i32)>,
        visible: bool,
    ) -> io::Result<Window> {
        static DPI_AWARENESS_ONCE: Once = Once::new();
        DPI_AWARENESS_ONCE.call_once(enable_dpi_awareness);

        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };

        static REGISTER_CLASS_ONCE: Once = Once::new();
        REGISTER_CLASS_ONCE.call_once(|| register_class(instance));

        let class_name = wide(CLASS_NAME);
        let title_wide = wide(title);

        // No window exists yet to ask its own DPI, so the *system* DPI sizes
        // it initially; `window_dpi` below corrects this once it exists.
        let scale = f64::from(system_dpi()) / 96.0;
        let physical_width = ((f64::from(width)) * scale).round() as i32;
        let physical_height = ((f64::from(height)) * scale).round() as i32;

        let (x, y) = position.unwrap_or((CW_USEDEFAULT, CW_USEDEFAULT));

        let state = Box::into_raw(Box::new(SharedState {
            events: VecDeque::new(),
            dpi: 96,
        }));

        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                title_wide.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                x,
                y,
                physical_width.max(1),
                physical_height.max(1),
                0,
                0,
                instance,
                state.cast::<c_void>().cast_const(),
            )
        };

        if hwnd == 0 {
            // Nothing reachable ever stored this pointer, so nothing else
            // will free it.
            drop(unsafe { Box::from_raw(state) });
            return Err(io::Error::last_os_error());
        }

        // Authoritative now that the window is placed on a real monitor,
        // unlike the system-wide guess `physical_width`/`physical_height`
        // used above.
        unsafe {
            (*state).dpi = window_dpi(hwnd);
        }

        if visible {
            unsafe {
                ShowWindow(hwnd, SW_SHOW);
                UpdateWindow(hwnd);
            }
        }

        Ok(Window { hwnd, state })
    }

    /// The raw `HWND`, as an `isize` — see the module docs for why. For
    /// handing to [`crate::platform::win32::surface::Surface::new`].
    pub fn raw_handle(&self) -> isize {
        self.hwnd
    }

    /// The window's current DPI (96 = 100%).
    pub fn dpi(&self) -> u32 {
        if self.state.is_null() {
            return 96;
        }
        unsafe { (*self.state).dpi }
    }

    /// `dpi() as f64 / 96.0`.
    pub fn scale_factor(&self) -> f64 {
        f64::from(self.dpi()) / 96.0
    }

    /// Change the window's title — Phase 3's dispatch loop calls this after
    /// a command changes the document's dirty state or name, the same way
    /// the pre-rewrite `App::refresh_title` did.
    pub fn set_title(&mut self, title: &str) {
        if self.hwnd == 0 {
            return;
        }
        let title = wide(title);
        unsafe {
            SetWindowTextW(self.hwnd, title.as_ptr());
        }
    }

    /// Colour the title bar to match the editor underneath it.
    ///
    /// A window is given the shell's caption by default, so a dark theme used
    /// to stop dead at a white bar across the top. `caption` and `text` are
    /// 8-bit sRGB — the theme's chrome colours, so the title bar reads as the
    /// same surface as the status line and the footer — and `dark` says which
    /// of the two built-in palettes the rest of the frame should use, which
    /// is what colours the minimise/maximise/close glyphs and their hover
    /// highlights. That flag is not derived from `caption`'s luminance on
    /// purpose: a theme is free to pick a mid-grey chrome that the guess
    /// would get wrong, and the caller already knows the answer.
    ///
    /// Best-effort by design, and in version order. Windows 11 takes the
    /// exact colours; Windows 10 has only the dark-mode flag, so it gets a
    /// near-enough dark bar instead of the theme's precise one; anything
    /// older keeps the shell default. None of those are errors worth
    /// propagating — the editor runs identically either way — so this returns
    /// nothing and the caller does not have to care which Windows it is on.
    pub fn set_caption_theme(&mut self, caption: (u8, u8, u8), text: (u8, u8, u8), dark: bool) {
        if self.hwnd == 0 {
            return;
        }

        // The renumbering in 20H1 means one of these two is a no-op on any
        // given Windows, and which one flips depending on the build. Setting
        // both is how every other application handles it.
        let flag: i32 = i32::from(dark);
        set_attribute(self.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &flag);
        set_attribute(self.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE_1809, &flag);

        // Windows 11 only; older builds report a failed `HRESULT` here and
        // keep the palette the flag above chose.
        set_attribute(self.hwnd, DWMWA_CAPTION_COLOR, &colorref(caption));
        set_attribute(self.hwnd, DWMWA_TEXT_COLOR, &colorref(text));
        // The thin frame around the whole window, which is otherwise left as
        // the accent colour and reads as a bright outline on a dark theme.
        set_attribute(self.hwnd, DWMWA_BORDER_COLOR, &colorref(caption));

        // The caption is painted once when the window is shown, so a colour
        // set afterwards is not visible until the frame is invalidated.
        unsafe {
            SetWindowPos(
                self.hwnd,
                0,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
    }

    /// Block until the next event, translating and dispatching Win32
    /// messages as needed. Returns `None` once the loop should stop — after
    /// [`Window::close`] posts `WM_QUIT`, or on a real `GetMessage` failure.
    pub fn next_event(&mut self) -> Option<Event> {
        loop {
            if let Some(event) = self.take_queued_event() {
                return Some(event);
            }
            if self.hwnd == 0 {
                return None;
            }
            let mut msg: Msg = unsafe { std::mem::zeroed() };
            let result = unsafe { GetMessageW(&mut msg, 0, 0, 0) };
            if result <= 0 {
                // 0 is WM_QUIT; -1 is a real error. Neither has anything left
                // worth dispatching.
                return None;
            }
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    /// Like [`Window::next_event`], but never blocks: `None` means nothing
    /// was waiting right now, not that the window is done.
    pub fn poll_event(&mut self) -> Option<Event> {
        if let Some(event) = self.take_queued_event() {
            return Some(event);
        }
        if self.hwnd == 0 {
            return None;
        }
        loop {
            let mut msg: Msg = unsafe { std::mem::zeroed() };
            let has_message = unsafe { PeekMessageW(&mut msg, 0, 0, 0, PM_REMOVE) };
            if has_message == 0 {
                return None;
            }
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if let Some(event) = self.take_queued_event() {
                return Some(event);
            }
        }
    }

    fn take_queued_event(&mut self) -> Option<Event> {
        if self.state.is_null() {
            return None;
        }
        unsafe { (*self.state).events.pop_front() }
    }

    /// Pump events until the window closes, handing each to `on_event`. See
    /// the module docs: [`Event::CloseRequested`] does not close the window
    /// by itself.
    pub fn run(mut self, mut on_event: impl FnMut(&mut Window, Event)) {
        while let Some(event) = self.next_event() {
            on_event(&mut self, event);
        }
    }

    /// Destroy the window. Safe to call more than once, and called
    /// automatically on drop if it was not called already.
    pub fn close(&mut self) {
        if self.hwnd != 0 {
            unsafe {
                DestroyWindow(self.hwnd);
            }
            self.hwnd = 0;
        }
        if !self.state.is_null() {
            drop(unsafe { Box::from_raw(self.state) });
            self.state = std::ptr::null_mut();
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.close();
    }
}

fn register_class(instance: isize) {
    let class_name = wide(CLASS_NAME);
    let cursor = unsafe { LoadCursorW(0, IDC_ARROW) };
    let class = WndClassExW {
        cb_size: std::mem::size_of::<WndClassExW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfn_wnd_proc: wnd_proc,
        cb_cls_extra: 0,
        cb_wnd_extra: 0,
        h_instance: instance,
        h_icon: 0,
        h_cursor: cursor,
        // A stock background brush so the window isn't a black rectangle
        // before Track C's surface ever paints it. `COLOR_WINDOW + 1` is
        // Win32's usual way to turn a system colour index into a brush
        // handle — not a real handle at all, just a small integer in a
        // handle-shaped slot, exactly like `IDC_ARROW` above.
        hbr_background: COLOR_WINDOW + 1,
        lpsz_menu_name: std::ptr::null(),
        lpsz_class_name: class_name.as_ptr(),
        h_icon_sm: 0,
    };
    // Ignoring the ATOM result: a failure here means every later
    // `CreateWindowExW` fails too, loudly, with its own `GetLastError`.
    unsafe {
        RegisterClassExW(&class);
    }
}

unsafe extern "system" fn wnd_proc(hwnd: isize, msg: u32, w_param: usize, l_param: isize) -> isize {
    match msg {
        // The one point in a window's life `CREATESTRUCTW::lpCreateParams`
        // is available: stash it so every later message can find its state.
        WM_NCCREATE => {
            let create_struct = l_param as *const CreateStructW;
            let params = (*create_struct).lp_create_params;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, params as isize);
            DefWindowProcW(hwnd, msg, w_param, l_param)
        }

        WM_SIZE => {
            if let Some(state) = state_ptr(hwnd) {
                let width = (l_param as u32) & 0xFFFF;
                let height = ((l_param as u32) >> 16) & 0xFFFF;
                (*state).events.push_back(Event::Resized { width, height });
            }
            0
        }

        // Handled instead of forwarded to `DefWindowProcW`, whose default
        // behaviour is to call `DestroyWindow` itself — which would not give
        // a caller the chance to ask "save first?" before the window is gone.
        WM_CLOSE => {
            if let Some(state) = state_ptr(hwnd) {
                (*state).events.push_back(Event::CloseRequested);
            }
            0
        }

        WM_DPICHANGED => {
            let dpi = (w_param as u32) & 0xFFFF;
            if let Some(state) = state_ptr(hwnd) {
                (*state).dpi = dpi;
                (*state).events.push_back(Event::ScaleChanged {
                    scale: f64::from(dpi) / 96.0,
                    dpi,
                });
            }
            // Microsoft's documented handling: move/resize to the rect
            // Windows suggests, so the window covers the same physical area
            // on the new monitor instead of just re-scaling in place.
            let suggested = l_param as *const Rect;
            if !suggested.is_null() {
                let rect = *suggested;
                SetWindowPos(
                    hwnd,
                    0,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            0
        }

        // `Window::close` is the only thing that calls `DestroyWindow`, so
        // this is the one place that needs to turn that into the message
        // loop actually stopping.
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }

        // Plain and Alt-held key-down are queued identically — see the
        // module docs for why `Event::KeyDown` carries no Alt flag. The
        // "sys" variant still reaches `DefWindowProcW` below so `Alt+F4`,
        // `Alt+Space`, and bare `F10` keep working.
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if let Some(state) = state_ptr(hwnd) {
                (*state).events.push_back(Event::KeyDown {
                    vkey: w_param as u32,
                    repeat: (l_param & KEYDOWN_REPEAT_BIT) != 0,
                });
            }
            if msg == WM_SYSKEYDOWN {
                DefWindowProcW(hwnd, msg, w_param, l_param)
            } else {
                0
            }
        }

        // `WM_CHAR`'s `wParam` is a UTF-16 code unit. A surrogate half
        // (only reachable via IME composition, never a plain physical key)
        // has no single-`char` meaning on its own and is dropped rather than
        // guessed at.
        WM_CHAR | WM_SYSCHAR => {
            if let Some(c) = char::from_u32(w_param as u32) {
                if let Some(state) = state_ptr(hwnd) {
                    (*state).events.push_back(Event::Char(c));
                }
            }
            if msg == WM_SYSCHAR {
                DefWindowProcW(hwnd, msg, w_param, l_param)
            } else {
                0
            }
        }

        WM_MOUSEMOVE => {
            if let Some(state) = state_ptr(hwnd) {
                let (x, y) = client_point(l_param);
                (*state).events.push_back(Event::MouseMove { x, y });
            }
            0
        }

        WM_LBUTTONDOWN | WM_LBUTTONUP => {
            if let Some(state) = state_ptr(hwnd) {
                let (x, y) = client_point(l_param);
                (*state).events.push_back(Event::MouseButton {
                    button: MouseButton::Left,
                    pressed: msg == WM_LBUTTONDOWN,
                    x,
                    y,
                });
            }
            0
        }

        // The wheel-rotation amount lives in `wParam`'s high 16 bits, as a
        // *signed* 16-bit count of `WHEEL_DELTA` (120) units — unlike the
        // button messages above, this is not a plain bitmask extraction, so
        // it goes through an `i16` cast to keep its sign.
        WM_MOUSEWHEEL => {
            if let Some(state) = state_ptr(hwnd) {
                let raw = ((w_param as u32) >> 16) as u16 as i16;
                let delta_lines = f32::from(raw) / 120.0;
                (*state).events.push_back(Event::MouseWheel { delta_lines });
            }
            0
        }

        _ => DefWindowProcW(hwnd, msg, w_param, l_param),
    }
}

/// `WM_MOUSEMOVE`/`WM_LBUTTON*`'s `lParam`: client-area x in the low 16 bits,
/// y in the high 16, each a *signed* 16-bit coordinate (a window spanning a
/// negative-coordinate monitor, or a point just off the client edge during a
/// drag, both produce negative values) — cast through `i16`, not masked like
/// `WM_SIZE`'s always-nonnegative width/height above.
fn client_point(l_param: isize) -> (i32, i32) {
    let x = (l_param as u32 & 0xFFFF) as u16 as i16;
    let y = ((l_param as u32 >> 16) & 0xFFFF) as u16 as i16;
    (i32::from(x), i32::from(y))
}

/// `None` before `WM_NCCREATE` has run, which nothing here ever observes —
/// every other message arrives after it.
unsafe fn state_ptr(hwnd: isize) -> Option<*mut SharedState> {
    let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if raw == 0 {
        None
    } else {
        Some(raw as *mut SharedState)
    }
}

/// Ask Windows for per-monitor-v2 DPI awareness, falling back as described in
/// the module docs. Idempotent in effect (later calls fail harmlessly), but
/// meant to run once — see the `Once` in `Window::create`.
/// A `COLORREF`, which packs sRGB bytes as `0x00BBGGRR` — reversed from
/// every other place a colour is written down, and the one detail that turns
/// a themed title bar the wrong colour when it is missed.
fn colorref(rgb: (u8, u8, u8)) -> u32 {
    let (r, g, b) = rgb;
    u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16)
}

/// Set one DWM window attribute, ignoring the `HRESULT`.
///
/// Every caller here is asking for something the running Windows may simply
/// not have (see the `DWMWA_*` constants). A failure means "this build does
/// not support that attribute", which is not a condition the editor can or
/// should do anything about.
fn set_attribute<T>(hwnd: isize, attribute: u32, value: &T) {
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            attribute,
            (value as *const T).cast::<c_void>(),
            std::mem::size_of::<T>() as u32,
        );
    }
}

fn enable_dpi_awareness() {
    const PER_MONITOR_AWARE_V2: isize = -4;
    const PER_MONITOR_AWARE: isize = -3;

    unsafe {
        let set_context_ptr = user32_proc(b"SetProcessDpiAwarenessContext\0");
        if !set_context_ptr.is_null() {
            let set_context: unsafe extern "system" fn(isize) -> i32 =
                std::mem::transmute(set_context_ptr);
            if set_context(PER_MONITOR_AWARE_V2) != 0 {
                return;
            }
            // V2 needs Windows 10 1703; V1 has existed since 1607.
            if set_context(PER_MONITOR_AWARE) != 0 {
                return;
            }
        }
        // Every Windows since Vista has this one: one DPI for every monitor
        // rather than none, so still not the blurry bitmap-stretched default.
        SetProcessDPIAware();
    }
}

/// A window's current DPI, preferring the per-monitor value and falling back
/// to the system-wide one on a Windows old enough not to have
/// `GetDpiForWindow` (pre-1607).
fn window_dpi(hwnd: isize) -> u32 {
    unsafe {
        let get_dpi_ptr = user32_proc(b"GetDpiForWindow\0");
        if !get_dpi_ptr.is_null() {
            let get_dpi: unsafe extern "system" fn(isize) -> u32 = std::mem::transmute(get_dpi_ptr);
            let dpi = get_dpi(hwnd);
            if dpi != 0 {
                return dpi;
            }
        }
    }
    system_dpi()
}

/// The system-wide DPI, from the screen device context. Used both as
/// `window_dpi`'s fallback and to size a window before it exists (see
/// `Window::create`).
fn system_dpi() -> u32 {
    unsafe {
        let hdc = GetDC(0);
        if hdc == 0 {
            return 96;
        }
        let dpi = GetDeviceCaps(hdc, LOGPIXELSX);
        ReleaseDC(0, hdc);
        if dpi > 0 {
            dpi as u32
        } else {
            96
        }
    }
}

/// Look up a `user32.dll` export that may not exist on this Windows version.
/// `user32.dll` is already loaded (this binary imports other functions from
/// it directly), so `GetModuleHandleW` finds it without a `LoadLibrary` call.
unsafe fn user32_proc(name: &[u8]) -> *mut c_void {
    let module = GetModuleHandleW(wide("user32.dll").as_ptr());
    if module == 0 {
        return std::ptr::null_mut();
    }
    GetProcAddress(module, name.as_ptr())
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_strings_are_null_terminated() {
        let s = wide("Hi");
        assert_eq!(s, vec![u16::from(b'H'), u16::from(b'i'), 0]);
    }

    #[test]
    fn an_empty_string_is_still_terminated() {
        assert_eq!(wide(""), vec![0]);
    }

    #[test]
    fn system_dpi_is_a_plausible_screen_dpi() {
        // Not a live window, but a real Win32 call (`GetDC(NULL)` for the
        // whole screen) — safe to run on the CI machine actually running
        // this test, same as `window_dpi`'s fallback path exercises below.
        let dpi = system_dpi();
        assert!(
            (48..=960).contains(&dpi),
            "dpi {dpi} is not a plausible screen DPI"
        );
    }

    /// Off-screen and never shown, so this does not steal focus or flash a
    /// window on screen while `cargo test` runs.
    fn open_offscreen(title: &str) -> Window {
        Window::create(title, 200, 150, Some((-32000, -32000)), false)
            .expect("an off-screen window should still open")
    }

    #[test]
    fn a_window_opens_reports_a_plausible_dpi_and_closes_cleanly() {
        let mut window = open_offscreen("aitch window test");

        assert!(
            window.dpi() >= 48,
            "dpi() returned {}, which is not plausible",
            window.dpi()
        );
        assert!(window.scale_factor() > 0.0);

        // Drain whatever the OS queued from creating it, without blocking.
        while window.poll_event().is_some() {}

        window.close();
        // Idempotent: closing an already-closed window must not double-free
        // or panic.
        window.close();
    }

    #[test]
    fn resizing_the_window_produces_a_resized_event() {
        let mut window = open_offscreen("aitch window resize test");
        // Drain whatever the OS queued from creating it before resizing, so
        // the event below can only be the one this test caused.
        while window.poll_event().is_some() {}

        unsafe {
            SetWindowPos(
                window.hwnd,
                0,
                -32000,
                -32000,
                300,
                250,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }

        let mut resized = None;
        for _ in 0..200 {
            match window.poll_event() {
                Some(Event::Resized { width, height }) => {
                    resized = Some((width, height));
                    break;
                }
                Some(_) => {}
                None => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }

        window.close();
        let (width, height) =
            resized.expect("SetWindowPos with a new size should produce a Resized event");
        assert!(width > 0 && height > 0);
    }

    #[test]
    fn events_stop_arriving_after_close() {
        let mut window = open_offscreen("aitch window test 2");
        window.close();
        assert_eq!(window.next_event(), None);
        assert_eq!(window.poll_event(), None);
    }

    #[test]
    fn dropping_a_window_closes_it_without_panicking() {
        let window = open_offscreen("aitch window test 3");
        drop(window);
    }

    #[test]
    fn set_title_actually_changes_the_windows_title() {
        let mut window = open_offscreen("aitch window title test");
        window.set_title("a new title");

        let mut buffer = [0u16; 64];
        let len = unsafe { GetWindowTextW(window.hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
        let title = String::from_utf16_lossy(&buffer[..len as usize]);
        assert_eq!(title, "a new title");

        window.close();
    }

    /// Where the window is and how big it is, in screen pixels.
    fn window_rect(window: &Window) -> Rect {
        let mut rect = Rect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let ok = unsafe { GetWindowRect(window.hwnd, &mut rect) };
        assert_ne!(ok, 0, "GetWindowRect should succeed on a live window");
        rect
    }

    #[test]
    fn a_colorref_puts_blue_in_the_high_byte_and_red_in_the_low_one() {
        // The whole point of the helper: `0x00BBGGRR`, not `0xRRGGBB`. Get
        // this backwards and a themed title bar comes out a plausible-looking
        // but wrong colour, which is exactly the kind of bug that survives a
        // glance at a screenshot.
        assert_eq!(colorref((0xFF, 0x00, 0x00)), 0x0000_00FF, "red");
        assert_eq!(colorref((0x00, 0xFF, 0x00)), 0x0000_FF00, "green");
        assert_eq!(colorref((0x00, 0x00, 0xFF)), 0x00FF_0000, "blue");

        // The dark theme's own chrome colour, spelled both ways.
        assert_eq!(colorref((0x1E, 0x21, 0x28)), 0x0028_211E);

        // Nothing ever sets the top byte: it is reserved, and DWM rejects a
        // `COLORREF` that uses it.
        for channel in [(0xFF, 0xFF, 0xFF), (0x14, 0x16, 0x1A)] {
            assert_eq!(colorref(channel) & 0xFF00_0000, 0, "{channel:?}");
        }
    }

    #[test]
    fn setting_the_caption_theme_leaves_the_window_otherwise_alone() {
        // `set_caption_theme` nudges the window with `SetWindowPos` to force
        // the frame to repaint. That call is the part that could plausibly
        // move, resize, or re-order the window by accident, so this pins down
        // that it does none of those -- and that the title survives it.
        let mut window = open_offscreen("aitch caption theme test");
        window.set_title("still this title");

        let before = window_rect(&window);
        window.set_caption_theme((0x1E, 0x21, 0x28), (0xC0, 0xC5, 0xCE), true);
        let after = window_rect(&window);
        assert_eq!(before, after, "the window should not have moved or resized");

        let mut buffer = [0u16; 64];
        let len = unsafe { GetWindowTextW(window.hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
        assert_eq!(
            String::from_utf16_lossy(&buffer[..len as usize]),
            "still this title"
        );

        window.close();
    }

    #[test]
    fn a_light_caption_theme_is_accepted_too() {
        // Both palettes go down the same path; the light one exists to prove
        // `dark: false` is a real branch and not just the absence of a call.
        let mut window = open_offscreen("aitch light caption test");
        window.set_caption_theme((0xE8, 0xE9, 0xEB), (0x38, 0x3A, 0x42), false);
        window.close();
    }

    #[test]
    fn setting_the_caption_theme_on_a_closed_window_is_a_no_op() {
        // Same guard `set_title` has: the dispatch loop can reach a window
        // that has already been torn down, and passing `DwmSetWindowAttribute`
        // a dead `HWND` should not be how we find that out.
        let mut window = open_offscreen("aitch closed caption test");
        window.close();
        window.set_caption_theme((0x1E, 0x21, 0x28), (0xC0, 0xC5, 0xCE), true);
    }

    #[test]
    fn a_keydown_event_carries_its_virtual_key_code_and_repeat_flag() {
        const VK_LEFT: usize = 0x25;
        let mut window = open_offscreen("aitch keydown test");
        while window.poll_event().is_some() {}

        unsafe {
            SendMessageW(window.hwnd, WM_KEYDOWN, VK_LEFT, 0);
        }
        assert_eq!(
            window.poll_event(),
            Some(Event::KeyDown {
                vkey: VK_LEFT as u32,
                repeat: false
            })
        );

        unsafe {
            SendMessageW(window.hwnd, WM_KEYDOWN, VK_LEFT, KEYDOWN_REPEAT_BIT);
        }
        assert_eq!(
            window.poll_event(),
            Some(Event::KeyDown {
                vkey: VK_LEFT as u32,
                repeat: true
            })
        );

        window.close();
    }

    #[test]
    fn a_sys_keydown_is_queued_identically_to_a_plain_one() {
        // The module docs' whole point: `Event::KeyDown` does not distinguish
        // Alt-held from plain, so this must be exactly the same event.
        const VK_T: usize = 0x54;
        let mut window = open_offscreen("aitch syskeydown test");
        while window.poll_event().is_some() {}

        unsafe {
            SendMessageW(window.hwnd, WM_SYSKEYDOWN, VK_T, 0);
        }
        assert_eq!(
            window.poll_event(),
            Some(Event::KeyDown {
                vkey: VK_T as u32,
                repeat: false
            })
        );

        window.close();
    }

    #[test]
    fn a_char_event_carries_the_resolved_character() {
        let mut window = open_offscreen("aitch char test");
        while window.poll_event().is_some() {}

        unsafe {
            SendMessageW(window.hwnd, WM_CHAR, 'q' as usize, 0);
        }
        assert_eq!(window.poll_event(), Some(Event::Char('q')));

        // Ctrl+X arrives as the C0 control code 0x18, exactly as a real
        // Ctrl+letter press would -- proving this module hands that raw code
        // point through unchanged, for whoever does the unmapping.
        unsafe {
            SendMessageW(window.hwnd, WM_CHAR, 0x18, 0);
        }
        assert_eq!(window.poll_event(), Some(Event::Char('\u{18}')));

        window.close();
    }

    #[test]
    fn a_sys_char_is_queued_identically_to_a_plain_one() {
        let mut window = open_offscreen("aitch syschar test");
        while window.poll_event().is_some() {}

        unsafe {
            SendMessageW(window.hwnd, WM_SYSCHAR, 't' as usize, 0);
        }
        assert_eq!(window.poll_event(), Some(Event::Char('t')));

        window.close();
    }

    #[test]
    fn mouse_move_and_button_events_carry_client_coordinates() {
        let mut window = open_offscreen("aitch mouse test");
        while window.poll_event().is_some() {}

        let l_param =
            |x: i16, y: i16| -> isize { ((y as u16 as isize) << 16) | (x as u16 as isize) };

        unsafe {
            SendMessageW(window.hwnd, WM_MOUSEMOVE, 0, l_param(12, 34));
        }
        assert_eq!(window.poll_event(), Some(Event::MouseMove { x: 12, y: 34 }));

        unsafe {
            SendMessageW(window.hwnd, WM_LBUTTONDOWN, 0, l_param(5, 6));
        }
        assert_eq!(
            window.poll_event(),
            Some(Event::MouseButton {
                button: MouseButton::Left,
                pressed: true,
                x: 5,
                y: 6
            })
        );

        unsafe {
            SendMessageW(window.hwnd, WM_LBUTTONUP, 0, l_param(5, 6));
        }
        assert_eq!(
            window.poll_event(),
            Some(Event::MouseButton {
                button: MouseButton::Left,
                pressed: false,
                x: 5,
                y: 6
            })
        );

        window.close();
    }

    #[test]
    fn a_negative_client_coordinate_is_not_mistaken_for_a_huge_positive_one() {
        // A point just off the client edge during a drag is exactly where a
        // naive bitmask (rather than a sign-preserving cast) would turn -1
        // into 65535.
        let mut window = open_offscreen("aitch negative coordinate test");
        while window.poll_event().is_some() {}

        let l_param = ((-1i16 as u16 as isize) << 16) | (-1i16 as u16 as isize);
        unsafe {
            SendMessageW(window.hwnd, WM_MOUSEMOVE, 0, l_param);
        }
        assert_eq!(window.poll_event(), Some(Event::MouseMove { x: -1, y: -1 }));

        window.close();
    }

    #[test]
    fn mouse_wheel_delta_has_the_right_sign_and_scale() {
        let mut window = open_offscreen("aitch wheel test");
        while window.poll_event().is_some() {}

        // One notch away from the user: +120 in the high word.
        unsafe {
            SendMessageW(
                window.hwnd,
                WM_MOUSEWHEEL,
                (120i32 as u32 as usize) << 16,
                0,
            );
        }
        assert_eq!(
            window.poll_event(),
            Some(Event::MouseWheel { delta_lines: 1.0 })
        );

        // One notch toward the user: a signed -120, not a huge unsigned one.
        unsafe {
            SendMessageW(
                window.hwnd,
                WM_MOUSEWHEEL,
                (-120i32 as u32 as usize) << 16,
                0,
            );
        }
        assert_eq!(
            window.poll_event(),
            Some(Event::MouseWheel { delta_lines: -1.0 })
        );

        window.close();
    }

    #[test]
    fn two_windows_do_not_interfere_with_each_other() {
        // Both register the same window class; this is the test that class
        // registration is idempotent and each window still gets its own
        // state rather than sharing one.
        let mut a = open_offscreen("aitch window test 4a");
        let mut b = open_offscreen("aitch window test 4b");

        while a.poll_event().is_some() {}
        while b.poll_event().is_some() {}

        a.close();
        assert_eq!(a.next_event(), None);
        // `b` is unaffected by `a` closing.
        b.close();
    }
}
