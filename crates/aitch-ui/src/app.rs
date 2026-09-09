//! The event loop.
//!
//! Input goes one way: winit event, to [`Chord`], to [`Command`] through the
//! active keymap, to the buffer. This file contains no opinion about what any
//! key does — swap the keymap and every binding changes with it. The one thing
//! it decides for itself is that a printable key with no binding is text, and
//! even that becomes a [`Command::InsertText`] before it reaches the buffer.
//!
//! [`Chord`]: aitch_core::Chord

use std::error::Error;
use std::sync::Arc;

use aitch_core::{Applied, Command, Context, Document, Keymap, Viewport};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::input::chord_from_event;
use crate::render::Surface;
use crate::theme::Theme;

/// How many lines one notch of a wheel scrolls, matching the usual desktop feel.
const WHEEL_LINES: f64 = 3.0;

/// Open a window on `document` and run until it closes.
pub fn run(document: Document) -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    // Event-driven redraw only. PLAN.md §6: idle CPU is 0%, and a spinning
    // render loop is the one way to fail that budget by construction.
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App::new(document);
    event_loop.run_app(&mut app)?;

    match app.failure {
        Some(e) => Err(e.into()),
        None => Ok(()),
    }
}

struct App {
    window: Option<Arc<Window>>,
    surface: Option<Surface>,
    theme: Theme,

    document: Document,
    viewport: Viewport,
    keymap: Keymap,
    context: Context,

    /// The system clipboard. `None` if the platform would not give us one —
    /// a headless session, say — in which case copy and paste do nothing
    /// rather than taking the editor down with them.
    clipboard: Option<arboard::Clipboard>,

    /// Set when quit was asked for on a modified buffer. The next quit goes
    /// through. Phase 3 replaces this with a real prompt on the prompt line;
    /// until then the window title carries the warning.
    quit_armed: bool,
    /// A transient message shown in the title bar, for the same reason.
    message: Option<String>,

    /// Distance from the top of the document in physical pixels.
    ///
    /// The source of truth for scrolling: the viewport's first line is derived
    /// from it, and the remainder is how far into that line the window has
    /// scrolled. Whole-line scrolling would make a touchpad feel like a ratchet.
    scroll: f64,
    modifiers: ModifiersState,
    pointer: PhysicalPosition<f64>,
    /// Whether the left button is down, so a drag extends the selection.
    dragging: bool,
    /// Bumped whenever the visible text could have changed, so the shaper
    /// knows when its cached layout is stale.
    generation: u64,

    failure: Option<String>,
}

impl App {
    fn new(document: Document) -> App {
        App {
            window: None,
            surface: None,
            theme: Theme::default(),
            document,
            viewport: Viewport::new(1),
            keymap: Keymap::nano(),
            context: Context::Editor,
            clipboard: arboard::Clipboard::new().ok(),
            quit_armed: false,
            message: None,
            scroll: 0.0,
            modifiers: ModifiersState::empty(),
            pointer: PhysicalPosition::new(0.0, 0.0),
            dragging: false,
            generation: 0,
            failure: None,
        }
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, message: String) {
        self.failure = Some(message);
        event_loop.exit();
    }

    fn redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// Filename, modified marker, and any transient message.
    ///
    /// The title is doing the status line's job until Phase 3 builds one.
    fn title(&self) -> String {
        let modified = if self.document.is_dirty() { "*" } else { "" };
        match &self.message {
            Some(message) => format!("{modified}{} — {message}", self.document.display_name()),
            None => format!("{modified}{} — Aitch", self.document.display_name()),
        }
    }

    fn refresh_title(&mut self) {
        let title = self.title();
        if let Some(window) = &self.window {
            window.set_title(&title);
        }
    }

    fn say(&mut self, message: impl Into<String>) {
        self.message = Some(message.into());
        self.refresh_title();
    }

    fn clear_message(&mut self) {
        if self.message.take().is_some() {
            self.refresh_title();
        }
    }

    /// The furthest the document can scroll: the last line at the top.
    fn max_scroll(&self, line_height: f64) -> f64 {
        (self.document.buffer.len_lines().saturating_sub(1)) as f64 * line_height
    }

    /// Derive the viewport from the scroll position.
    fn viewport_from_scroll(&mut self, line_height: f64) {
        let first = (self.scroll / line_height).floor() as usize;
        let lines = self.document.buffer.len_lines();
        self.viewport.scroll_to(first, lines);
    }

    /// Snap the scroll position to the viewport, after the cursor moved it.
    fn scroll_from_viewport(&mut self, line_height: f64) {
        self.scroll = self.viewport.first_line() as f64 * line_height;
    }

    fn scroll_by(&mut self, delta: f64, line_height: f64) {
        let max = self.max_scroll(line_height);
        let target = (self.scroll + delta).clamp(0.0, max.max(0.0));
        if (target - self.scroll).abs() > f64::EPSILON {
            self.scroll = target;
            self.viewport_from_scroll(line_height);
            self.redraw();
        }
    }

    /// Run one command, then let the view follow.
    fn dispatch(&mut self, event_loop: &ActiveEventLoop, command: Command, line_height: f64) {
        // Any command that is not a second quit disarms the confirmation.
        if !matches!(command, Command::Quit) {
            self.quit_armed = false;
            self.clear_message();
        }

        self.viewport_from_scroll(line_height);
        match self.document.buffer.apply(&command, &self.viewport) {
            Applied::Changed => {
                self.document.buffer.follow_cursor(&mut self.viewport);
                self.scroll_from_viewport(line_height);
                self.generation += 1;
                self.refresh_title();
                self.redraw();
                return;
            }
            Applied::Unchanged => return,
            // Not the buffer's business. Ours, then.
            Applied::Unhandled => {}
        }

        match command {
            Command::Quit => self.quit(event_loop),
            Command::WriteOut => self.save(),
            Command::Copy => self.copy(),
            Command::Paste => self.paste(line_height),
            Command::SwitchProfile(profile) => {
                if let Some(keymap) = Keymap::by_name(&profile) {
                    self.keymap = keymap;
                    self.say(format!("{profile} keys"));
                }
            }
            // Everything else is a later phase: search, the tree, the prompt.
            // Saying so beats a key that silently does nothing.
            other => self.say(format!("{other} is not built yet")),
        }
    }

    fn quit(&mut self, event_loop: &ActiveEventLoop) {
        if !self.document.is_dirty() || self.quit_armed {
            event_loop.exit();
            return;
        }
        // Phase 3 turns this into a proper prompt-line question.
        self.quit_armed = true;
        self.say("unsaved changes — press quit again to discard");
    }

    fn save(&mut self) {
        match self.document.save() {
            Ok(()) => {
                let lines = self.document.buffer.len_lines();
                self.say(format!("wrote {lines} lines"));
            }
            Err(e) => self.say(format!("{e}")),
        }
        self.refresh_title();
        self.redraw();
    }

    fn copy(&mut self) {
        let Some(text) = self.document.buffer.selected_text() else {
            self.say("nothing selected");
            return;
        };
        match self.clipboard.as_mut() {
            Some(clipboard) => match clipboard.set_text(text) {
                Ok(()) => self.say("copied"),
                Err(e) => self.say(format!("clipboard: {e}")),
            },
            None => self.say("no clipboard on this system"),
        }
    }

    fn paste(&mut self, line_height: f64) {
        let text = match self.clipboard.as_mut() {
            Some(clipboard) => match clipboard.get_text() {
                Ok(text) => text,
                Err(e) => return self.say(format!("clipboard: {e}")),
            },
            None => return self.say("no clipboard on this system"),
        };
        if text.is_empty() {
            return;
        }
        // Back through the front door: the UI does not mutate the buffer.
        self.dispatch_text(text, line_height);
    }

    /// Insert text as a command, the way a keystroke would.
    fn dispatch_text(&mut self, text: String, line_height: f64) {
        self.viewport_from_scroll(line_height);
        let command = Command::InsertText(text);
        if self.document.buffer.apply(&command, &self.viewport) == Applied::Changed {
            self.document.buffer.follow_cursor(&mut self.viewport);
            self.scroll_from_viewport(line_height);
            self.generation += 1;
            self.refresh_title();
            self.redraw();
        }
    }

    /// Map a pointer position to a buffer position, if it lands on text.
    fn position_at_pointer(&self, line_height: f64) -> Option<aitch_core::Position> {
        let surface = self.surface.as_ref()?;
        let offset = (self.scroll % line_height) as f32;
        surface.hit(self.pointer.x as f32, self.pointer.y as f32, offset)
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // On Android this fires again after a suspend; everywhere else, once.
        if self.window.is_some() {
            return;
        }

        let attributes = Window::default_attributes()
            .with_title(self.title())
            .with_inner_size(LogicalSize::new(960.0, 640.0));

        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(e) => return self.fail(event_loop, format!("could not open a window: {e}")),
        };

        match Surface::new(window.clone()) {
            Ok(surface) => {
                self.viewport.set_height_lines(surface.visible_lines());
                self.surface = Some(surface);
            }
            Err(e) => return self.fail(event_loop, e.to_string()),
        }

        window.request_redraw();
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Taken by value so the borrow ends here: every arm below mutates
        // both the surface and the rest of `self`, in varying orders.
        let Some(line_height) = self.surface.as_ref().map(|s| s.line_height() as f64) else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => {
                // The window manager's close button asks the same question
                // the quit command does.
                self.quit(event_loop);
            }

            WindowEvent::Resized(size) => {
                let Some(surface) = self.surface.as_mut() else {
                    return;
                };
                surface.resize(size);
                let lines = surface.visible_lines();
                self.viewport.set_height_lines(lines);
                self.generation += 1;
                self.redraw();
            }

            // Moving between monitors, or a Wayland fractional-scale change.
            // Resized always follows, so this only has to re-derive the metrics
            // and drop the glyphs rasterized at the old scale.
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let Some(surface) = self.surface.as_mut() else {
                    return;
                };
                surface.set_scale_factor(scale_factor);
                let lines = surface.visible_lines();
                self.viewport.set_height_lines(lines);
                self.generation += 1;
            }

            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }

                if let Some(chord) = chord_from_event(&event, self.modifiers) {
                    if let Some(command) = self.keymap.resolve(self.context, chord).cloned() {
                        self.dispatch(event_loop, command, line_height);
                        return;
                    }
                }

                // Unbound and printable: this is text, not a shortcut. Ctrl
                // and Alt combinations are never text, whatever the OS says
                // the key produced.
                if self.modifiers.control_key() || self.modifiers.alt_key() {
                    return;
                }
                let Some(text) = event.text.as_ref() else {
                    return;
                };
                let typed: String = text.chars().filter(|c| !c.is_control()).collect();
                if !typed.is_empty() {
                    self.clear_message();
                    self.quit_armed = false;
                    self.dispatch_text(typed, line_height);
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = position;
                if self.dragging {
                    // Dragging extends from wherever the press landed, which
                    // the mark is already holding.
                    if let Some(target) = self.position_at_pointer(line_height) {
                        let at = self.document.buffer.position_to_char(target);
                        if at != self.document.buffer.cursor_char() {
                            self.document.buffer.set_cursor_extending(target);
                            self.redraw();
                        }
                    }
                }
            }

            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => {
                    if let Some(position) = self.position_at_pointer(line_height) {
                        self.document.buffer.set_cursor(position);
                        // Drop a mark here so a drag has something to drag from.
                        self.document.buffer.set_mark();
                        self.dragging = true;
                        self.redraw();
                    }
                }
                ElementState::Released => {
                    self.dragging = false;
                    // A click with no drag leaves a mark and no selection,
                    // which would surprise the next movement key.
                    if !self.document.buffer.has_selection() {
                        self.document.buffer.clear_selection();
                    }
                }
            },

            WindowEvent::MouseWheel { delta, .. } => {
                // Positive means scrolling up, which moves the document down.
                let pixels = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => {
                        -(lines as f64) * line_height * WHEEL_LINES
                    }
                    // A touchpad reports real pixels, including the tail of a
                    // kinetic fling, so passing them straight through is what
                    // makes the fling feel like the rest of the desktop.
                    MouseScrollDelta::PixelDelta(position) => -position.y,
                };
                self.scroll_by(pixels, line_height);
            }

            WindowEvent::RedrawRequested => {
                self.viewport_from_scroll(line_height);
                let first_line = self.viewport.first_line();
                let offset = (self.scroll - first_line as f64 * line_height) as f32;
                let theme = self.theme;
                let generation = self.generation;

                let Some(surface) = self.surface.as_mut() else {
                    return;
                };
                let result = surface.render(
                    &self.document.buffer,
                    first_line,
                    offset,
                    generation,
                    &theme,
                );
                if let Err(e) = result {
                    self.fail(event_loop, e.to_string());
                }
            }

            _ => {}
        }
    }
}
