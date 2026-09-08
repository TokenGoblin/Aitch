//! The event loop.
//!
//! Phase 0 opens a window and clears it. The input-to-[`Command`] translation
//! that belongs here arrives in Phase 1; the keymap is already data, so this
//! file will never learn what a key *does*.
//!
//! [`Command`]: nib_core::Command

use std::error::Error;
use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use crate::render::Surface;
use crate::theme::Theme;

/// Open a window and run until it closes.
pub fn run() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    // Event-driven redraw only. PLAN.md §6: idle CPU is 0%, and a spinning
    // render loop is the one way to fail that budget by construction.
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App::default();
    event_loop.run_app(&mut app)?;

    match app.failure {
        Some(e) => Err(e.into()),
        None => Ok(()),
    }
}

#[derive(Default)]
struct App {
    window: Option<Arc<Window>>,
    surface: Option<Surface>,
    theme: Theme,
    /// Set instead of panicking inside the loop, so `run` can report it.
    failure: Option<String>,
}

impl App {
    fn fail(&mut self, event_loop: &ActiveEventLoop, message: String) {
        self.failure = Some(message);
        event_loop.exit();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // On Android this fires again after a suspend; everywhere else, once.
        if self.window.is_some() {
            return;
        }

        let attributes = Window::default_attributes()
            .with_title("nib")
            .with_inner_size(LogicalSize::new(960.0, 640.0));

        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(e) => return self.fail(event_loop, format!("could not open a window: {e}")),
        };

        match Surface::new(window.clone()) {
            Ok(surface) => self.surface = Some(surface),
            Err(e) => return self.fail(event_loop, e.to_string()),
        }

        window.request_redraw();
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(surface) = self.surface.as_mut() else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                surface.resize(size);
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }

            // Moving between monitors, or a Wayland fractional-scale change.
            // Resized always follows, so this only has to record the factor.
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                surface.set_scale_factor(scale_factor);
            }

            WindowEvent::RedrawRequested => {
                let background = self.theme.background;
                if let Err(e) = surface.render(background) {
                    self.fail(event_loop, e.to_string());
                }
            }

            _ => {}
        }
    }
}
