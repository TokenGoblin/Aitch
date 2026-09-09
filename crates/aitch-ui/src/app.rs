//! The event loop.
//!
//! Input goes one way: winit event, to [`Chord`], to [`Command`] through the
//! active keymap, to [`Editor`]. This file has no opinion about what any key
//! does — swap the keymap and every binding changes with it. The one thing it
//! decides for itself is that a printable key with no binding is text, and even
//! that becomes a [`Command::InsertText`] before it reaches the editor.
//!
//! What is left here is the window: pixels, the pointer, the clipboard, and
//! scrolling in fractions of a line. Everything else moved into `aitch-core`
//! so the harness can drive it.
//!
//! [`Chord`]: aitch_core::Chord

use std::error::Error;
use std::sync::Arc;

use aitch_core::{
    Command, Config, ConfigError, Editor, Outcome, Position, Session, ThemeChoice, Watcher,
    Workspace,
};
use std::time::{Duration, Instant};
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

/// Why the event loop was woken from outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// Something in the open folder changed on disk.
    FolderChanged,
    /// A syntax parse finished and there is new colour to draw.
    HighlightsReady,
    /// A project-wide search found more matches.
    SearchResults,
    /// `aitchrc.toml` was saved; read it again.
    ConfigChanged,
}

/// Everything the binary worked out before there was a window.
pub struct Startup {
    pub workspace: Workspace,
    pub config: Config,
    /// A config that would not load. Shown once there is a status line.
    pub config_error: Option<ConfigError>,
    /// Where the config came from, so saving it can take effect live.
    pub config_path: Option<std::path::PathBuf>,
    /// What was open last time, when nothing else was asked for.
    pub session: Option<Session>,
    /// From `+LINE:COLUMN`, one-based.
    pub cursor: Option<(usize, usize)>,
}

/// How long after the last edit to write recovery files.
///
/// Long enough that a burst of typing writes once; short enough that the
/// window of work existing only in memory stays small. The timer is armed
/// only while something is unsaved, so an idle editor still costs nothing.
const RECOVERY_DELAY: Duration = Duration::from_secs(2);

/// Open a window and run until it closes.
pub fn run(startup: Startup) -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::<Wake>::with_user_event().build()?;
    // Event-driven redraw only. PLAN.md §6: idle CPU is 0%, and a spinning
    // render loop is the one way to fail that budget by construction. The
    // watcher below fits that: its thread blocks, and waking the loop is a
    // message rather than a poll.
    event_loop.set_control_flow(ControlFlow::Wait);

    let Startup {
        workspace,
        config,
        config_error,
        config_path,
        session,
        cursor,
    } = startup;

    let mut editor = Editor::with_workspace(workspace);
    editor.apply_config(config.clone());

    if let Some(session) = &session {
        editor.restore_session(session);
    }
    if let Some((line, column)) = cursor {
        let last = editor.buffer().len_lines().saturating_sub(1);
        editor.buffer_mut().set_cursor(Position::new(
            line.saturating_sub(1).min(last),
            column.saturating_sub(1),
        ));
    }
    if let Some(error) = &config_error {
        editor.say(error.to_string());
    }

    // Anything left behind by a run that did not end cleanly.
    editor.offer_recovery();

    let root = editor.workspace().root().map(std::path::Path::to_path_buf);
    let mut app = App::new(editor);
    app.config_path = config_path;
    app.config_complaint = config_error.as_ref().map(ToString::to_string);
    app.theme = match config.theme {
        ThemeChoice::Dark => Theme::dark(),
        ThemeChoice::Light => Theme::light(),
    };
    app.font = config.font.clone();

    if let Some(root) = root {
        let proxy = event_loop.create_proxy();
        app.watcher = Watcher::new(&root, move || {
            // The loop may already be gone; nothing to do about it here.
            let _ = proxy.send_event(Wake::FolderChanged);
        });
    }

    // The config is watched so that saving it takes effect without a restart.
    if let Some(path) = app.config_path.clone() {
        if let Some(directory) = path.parent().map(std::path::Path::to_path_buf) {
            let proxy = event_loop.create_proxy();
            app.config_watcher = Watcher::new(&directory, move || {
                let _ = proxy.send_event(Wake::ConfigChanged);
            });
        }
    }

    // Parsing and project search both run on their own threads; this is how
    // they say they have something. One channel serves both: the poll on the
    // other side is cheap and the alternative is two proxies to keep in step.
    let proxy = event_loop.create_proxy();
    app.editor.set_wake(move || {
        let _ = proxy.send_event(Wake::HighlightsReady);
    });
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
    editor: Editor,

    /// The system clipboard. `None` if the platform would not give us one —
    /// a headless session, say — in which case copy and paste say so rather
    /// than taking the editor down with them.
    clipboard: Option<arboard::Clipboard>,

    /// Watches the open folder. Held so that dropping the app stops it;
    /// `None` when there is no folder, or the platform will not watch.
    watcher: Option<Watcher>,

    /// The font to draw with, from the config.
    font: aitch_core::config::FontConfig,
    /// When to write recovery files, or `None` when nothing is unsaved.
    recovery_due: Option<Instant>,
    /// Where the config came from, so a change to it can be picked up.
    config_path: Option<std::path::PathBuf>,
    /// What the config last said, so that a change to some other file in the
    /// same directory does not announce itself as a settings reload.
    config_complaint: Option<String>,
    /// Watches that file. Held so dropping the app stops it.
    config_watcher: Option<Watcher>,

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
    fn new(editor: Editor) -> App {
        App {
            window: None,
            surface: None,
            theme: Theme::default(),
            editor,
            clipboard: arboard::Clipboard::new().ok(),
            watcher: None,
            font: aitch_core::config::FontConfig::default(),
            recovery_due: None,
            config_path: None,
            config_complaint: None,
            config_watcher: None,
            scroll: 0.0,
            modifiers: ModifiersState::empty(),
            pointer: PhysicalPosition::new(0.0, 0.0),
            dragging: false,
            generation: 0,
            failure: None,
        }
    }

    /// Leaving deliberately: remember what was open, and drop the recovery
    /// files, whose whole purpose was to survive *not* leaving deliberately.
    fn finish(&mut self) {
        let session = self.editor.session();
        if session.is_empty() {
            Session::clear();
        } else {
            session.save();
        }
        self.editor.abandon_recovery();
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

    /// The window title. The status line carries the detail; this is just
    /// enough to pick the window out of a task bar.
    fn title(&self) -> String {
        let modified = if self.editor.document().is_dirty() {
            "*"
        } else {
            ""
        };
        format!(
            "{modified}{} — Aitch",
            self.editor.document().display_name()
        )
    }

    fn refresh_title(&self) {
        if let Some(window) = &self.window {
            window.set_title(&self.title());
        }
    }

    /// The furthest the document can scroll: its last line at the top.
    fn max_scroll(&self, line_height: f64) -> f64 {
        self.editor.buffer().len_lines().saturating_sub(1) as f64 * line_height
    }

    /// Derive the viewport from the scroll position.
    fn viewport_from_scroll(&mut self, line_height: f64) {
        let first = (self.scroll / line_height).floor() as usize;
        let lines = self.editor.buffer().len_lines();
        self.editor.viewport_mut().scroll_to(first, lines);
    }

    /// Snap the scroll position to the viewport, after the cursor moved it.
    fn scroll_from_viewport(&mut self, line_height: f64) {
        self.scroll = self.editor.viewport().first_line() as f64 * line_height;
    }

    fn scroll_by(&mut self, delta: f64, line_height: f64) {
        let max = self.max_scroll(line_height);
        let target = (self.scroll + delta).clamp(0.0, max.max(0.0));
        if (target - self.scroll).abs() > f64::EPSILON {
            self.scroll = target;
            self.viewport_from_scroll(line_height);
            self.editor.view_moved();
            self.redraw();
        }
    }

    /// Run one command through the editor and act on what it asks for.
    fn dispatch(&mut self, event_loop: &ActiveEventLoop, command: Command, line_height: f64) {
        self.viewport_from_scroll(line_height);
        let outcome = self.editor.run(&command);

        let redraws = outcome.redraws();
        match outcome {
            Outcome::Quit => {
                self.finish();
                event_loop.exit();
                return;
            }
            Outcome::Copy(text) => self.copy(text),
            Outcome::Paste => self.paste(event_loop, line_height),
            Outcome::Redraw | Outcome::Nothing => {}
        }

        if redraws {
            self.generation += 1;
            self.scroll_from_viewport(line_height);
            self.refresh_title();
            self.redraw();
            self.arm_recovery(event_loop);
        }
    }

    /// Schedule a recovery write, or cancel one that is no longer needed.
    ///
    /// Waking the loop on a timer is the one thing that could break the
    /// zero-idle-CPU budget, so the timer exists only while there is unsaved
    /// work to protect and is stood down the moment there is not.
    fn arm_recovery(&mut self, event_loop: &ActiveEventLoop) {
        if self.editor.needs_recovery() {
            let due = Instant::now() + RECOVERY_DELAY;
            self.recovery_due = Some(due);
            event_loop.set_control_flow(ControlFlow::WaitUntil(due));
        } else if self.recovery_due.take().is_some() {
            // Everything is saved; the files that were holding it can go.
            self.editor.discard_recovery();
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }

    /// Read `aitchrc.toml` again and apply it.
    ///
    /// Live reload matters most for the settings you are experimenting with —
    /// the theme, the font, the tab width — and restarting to see each one is
    /// what makes people give up on a config file.
    fn reload_config(&mut self) {
        let (config, error) = match &self.config_path {
            Some(path) => Config::load_from(path),
            None => Config::load(),
        };

        // The watcher covers the whole directory, because that is the only
        // way to hear about a file being replaced rather than written in
        // place. Saving something else in there is not a settings change.
        let complaint = error.as_ref().map(ToString::to_string);
        if config == *self.editor.config() && complaint == self.config_complaint {
            return;
        }
        self.config_complaint = complaint;

        self.theme = match config.theme {
            ThemeChoice::Dark => Theme::dark(),
            ThemeChoice::Light => Theme::light(),
        };

        // The font is settled when the surface is built, so a change to it
        // needs a new one rather than a redraw.
        let font_changed = config.font != self.font;
        self.font = config.font.clone();
        let complaint = self.editor.apply_config(config);

        match (error, complaint) {
            // Whatever went wrong is more use than "reloaded". Saying the
            // latter over the former is how a typo in a keymap name comes to
            // look like it worked.
            (Some(error), _) => self.editor.say(error.to_string()),
            (None, Some(complaint)) => self.editor.say(complaint),
            (None, None) if font_changed => {
                self.editor.say("settings reloaded — restart for the font")
            }
            (None, None) => self.editor.say("settings reloaded"),
        }

        self.generation += 1;
        self.redraw();
    }

    fn copy(&mut self, text: String) {
        match self.clipboard.as_mut() {
            Some(clipboard) => match clipboard.set_text(text) {
                Ok(()) => self.editor.say("copied"),
                Err(e) => self.editor.say(format!("clipboard: {e}")),
            },
            None => self.editor.say("no clipboard on this system"),
        }
    }

    fn paste(&mut self, event_loop: &ActiveEventLoop, line_height: f64) {
        let text = match self.clipboard.as_mut() {
            Some(clipboard) => match clipboard.get_text() {
                Ok(text) => text,
                Err(e) => return self.editor.say(format!("clipboard: {e}")),
            },
            None => return self.editor.say("no clipboard on this system"),
        };
        if !text.is_empty() {
            // Back through the front door: the UI does not touch the buffer.
            self.dispatch(event_loop, Command::InsertText(text), line_height);
        }
    }

    /// Map a pointer position to a place in the document, if it is over one.
    fn position_at_pointer(&self, line_height: f64) -> Option<aitch_core::Position> {
        // The chrome at the bottom is not the document, and neither is the
        // help pane; a click on either must not move the cursor.
        let surface = self.surface.as_ref()?;
        if self.editor.help().is_some() {
            return None;
        }
        let text_height = surface.visible_lines() as f64 * line_height;
        if self.pointer.y >= text_height {
            return None;
        }

        let offset = (self.scroll % line_height) as f32;
        surface.hit(self.pointer.x as f32, self.pointer.y as f32, offset)
    }
}

impl ApplicationHandler<Wake> for App {
    /// Called before the loop sleeps. Writes recovery files when their delay
    /// has passed, then goes back to waiting for events.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(due) = self.recovery_due else {
            return;
        };
        if Instant::now() < due {
            // Not yet: keep sleeping until it is.
            event_loop.set_control_flow(ControlFlow::WaitUntil(due));
            return;
        }

        self.editor.write_recovery();
        self.recovery_due = None;
        event_loop.set_control_flow(ControlFlow::Wait);
    }

    /// Woken from outside: the folder changed.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: Wake) {
        match event {
            Wake::FolderChanged => {
                if self.editor.folder_changed().redraws() {
                    self.generation += 1;
                    self.refresh_title();
                    self.redraw();
                }
            }
            Wake::ConfigChanged => self.reload_config(),

            Wake::HighlightsReady | Wake::SearchResults => {
                let colours = self.editor.poll_highlights();
                let results = self.editor.poll_search();
                if colours || results {
                    // Only the colours moved, so the shaped layout still
                    // stands: no generation bump, just another pass.
                    self.redraw();
                }
            }
        }
    }

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

        match Surface::with_font(
            window.clone(),
            self.font.size(),
            self.font.family.clone(),
            self.editor.config().tab_width,
        ) {
            Ok(surface) => {
                let lines = surface.visible_lines();
                self.editor.viewport_mut().set_height_lines(lines);
                let line_height = surface.line_height() as f64;
                self.surface = Some(surface);

                // Only now is the window's height known, so this is the first
                // point at which "scroll to the cursor" means anything. A
                // `+LINE` or a restored session put the cursor somewhere
                // before there was a viewport to put it in.
                self.editor.scroll_to_cursor();
                self.scroll_from_viewport(line_height);
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
                // The window manager's close button asks the same question the
                // quit command does, unsaved-changes prompt and all.
                self.dispatch(event_loop, Command::Quit, line_height);
            }

            WindowEvent::Resized(size) => {
                let Some(surface) = self.surface.as_mut() else {
                    return;
                };
                surface.resize(size);
                let lines = surface.visible_lines();
                self.editor.viewport_mut().set_height_lines(lines);
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
                self.editor.viewport_mut().set_height_lines(lines);
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
                    let context = self.editor.context();
                    let command = self.editor.keymap().resolve(context, chord).cloned();
                    if let Some(command) = command {
                        self.dispatch(event_loop, command, line_height);
                        return;
                    }
                }

                // Unbound and printable: this is text, not a shortcut. Ctrl and
                // Alt combinations are never text, whatever the OS reports the
                // key as having produced.
                if self.modifiers.control_key() || self.modifiers.alt_key() {
                    return;
                }
                let Some(text) = event.text.as_ref() else {
                    return;
                };
                let typed: String = text.chars().filter(|c| !c.is_control()).collect();
                if !typed.is_empty() {
                    self.dispatch(event_loop, Command::InsertText(typed), line_height);
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = position;
                if self.dragging {
                    // Dragging extends from wherever the press landed, which
                    // the mark is already holding.
                    if let Some(target) = self.position_at_pointer(line_height) {
                        let at = self.editor.buffer().position_to_char(target);
                        if at != self.editor.buffer().cursor_char() {
                            self.editor.buffer_mut().set_cursor_extending(target);
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
                        self.editor.buffer_mut().set_cursor(position);
                        // Drop a mark here so a drag has something to drag from.
                        self.editor.buffer_mut().set_mark();
                        self.dragging = true;
                        self.redraw();
                    }
                }
                ElementState::Released => {
                    self.dragging = false;
                    // A click with no drag leaves a mark and no selection,
                    // which would surprise the next movement key.
                    if !self.editor.buffer().has_selection() {
                        self.editor.buffer_mut().clear_selection();
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
                let first_line = self.editor.viewport().first_line();
                let offset = (self.scroll - first_line as f64 * line_height) as f32;
                let theme = self.theme;
                let generation = self.generation;

                let Some(surface) = self.surface.as_mut() else {
                    return;
                };
                let result = surface.render(&self.editor, offset, generation, &theme);
                if let Err(e) = result {
                    self.fail(event_loop, e.to_string());
                }
            }

            _ => {}
        }
    }
}
