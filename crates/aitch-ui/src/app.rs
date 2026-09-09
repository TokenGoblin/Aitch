//! The event loop.
//!
//! Input goes one way: winit event, to [`Chord`], to [`Command`] through the
//! active keymap, to the buffer. This file contains no opinion about what any
//! key does — swap the keymap and every binding changes with it.
//!
//! [`Chord`]: aitch_core::Chord

use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;

use aitch_core::{Applied, Buffer, Command, Context, Keymap, Viewport};
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

/// Open a window on `buffer` and run until it closes.
pub fn run(buffer: Buffer, title: Option<PathBuf>) -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    // Event-driven redraw only. PLAN.md §6: idle CPU is 0%, and a spinning
    // render loop is the one way to fail that budget by construction.
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App::new(buffer, title);
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

    buffer: Buffer,
    viewport: Viewport,
    keymap: Keymap,
    context: Context,
    title: String,

    /// Distance from the top of the document in physical pixels.
    ///
    /// The source of truth for scrolling: the viewport's first line is derived
    /// from it, and the remainder is how far into that line the window has
    /// scrolled. Whole-line scrolling would make a touchpad feel like a ratchet.
    scroll: f64,
    modifiers: ModifiersState,
    pointer: PhysicalPosition<f64>,
    /// Bumped whenever the visible text could have changed, so the shaper
    /// knows when its cached layout is stale.
    generation: u64,

    failure: Option<String>,
}

impl App {
    fn new(buffer: Buffer, path: Option<PathBuf>) -> App {
        let title = match &path {
            Some(path) => format!("{} — Aitch", path.display()),
            None => "Aitch".to_string(),
        };

        App {
            window: None,
            surface: None,
            theme: Theme::default(),
            buffer,
            viewport: Viewport::new(1),
            keymap: Keymap::nano(),
            context: Context::Editor,
            title,
            scroll: 0.0,
            modifiers: ModifiersState::empty(),
            pointer: PhysicalPosition::new(0.0, 0.0),
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

    /// The furthest the document can scroll: the last line at the top.
    fn max_scroll(&self, line_height: f64) -> f64 {
        (self.buffer.len_lines().saturating_sub(1)) as f64 * line_height
    }

    /// Derive the viewport from the scroll position.
    fn viewport_from_scroll(&mut self, line_height: f64) {
        let first = (self.scroll / line_height).floor() as usize;
        self.viewport.scroll_to(first, self.buffer.len_lines());
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

    /// Run one command through the buffer, then let the view follow.
    fn dispatch(&mut self, event_loop: &ActiveEventLoop, command: Command, line_height: f64) {
        // Quit is the UI's to honor; the buffer has no opinion about windows.
        if command == Command::Quit {
            // Phase 2 adds the unsaved-changes prompt in front of this.
            event_loop.exit();
            return;
        }

        if let Command::SwitchProfile(profile) = &command {
            match Keymap::by_name(profile) {
                Some(keymap) => self.keymap = keymap,
                None => return,
            }
            return;
        }

        self.viewport_from_scroll(line_height);
        match self.buffer.apply(&command, &self.viewport) {
            Applied::Changed => {
                self.buffer.follow_cursor(&mut self.viewport);
                self.scroll_from_viewport(line_height);
                self.generation += 1;
                self.redraw();
            }
            // Nothing moved, and Phase 1 has nothing else to do about the
            // commands it cannot run yet.
            Applied::Unchanged | Applied::Unhandled => {}
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // On Android this fires again after a suspend; everywhere else, once.
        if self.window.is_some() {
            return;
        }

        let attributes = Window::default_attributes()
            .with_title(&self.title)
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
            WindowEvent::CloseRequested => event_loop.exit(),

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
                let Some(chord) = chord_from_event(&event, self.modifiers) else {
                    return;
                };
                let Some(command) = self.keymap.resolve(self.context, chord).cloned() else {
                    return;
                };
                self.dispatch(event_loop, command, line_height);
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = position;
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let Some(surface) = self.surface.as_ref() else {
                    return;
                };
                let offset = (self.scroll % line_height) as f32;
                let hit = surface.hit(self.pointer.x as f32, self.pointer.y as f32, offset);
                if let Some(position) = hit {
                    if self.buffer.set_cursor(position) {
                        self.redraw();
                    }
                }
            }

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
                let result = surface.render(&self.buffer, first_line, offset, generation, &theme);
                if let Err(e) = result {
                    self.fail(event_loop, e.to_string());
                }
            }

            _ => {}
        }
    }
}
