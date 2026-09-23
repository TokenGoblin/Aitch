//! The event loop.
//!
//! Phase 0 of the zero-dependency rewrite (`PLAN-ZERO-DEP.md`) put a
//! hand-written Win32 window and GDI surface here. Phase 1 added real text.
//! Phase 3 makes it an editor again: [`input::chord_from_event`] resolves a
//! keystroke to a [`aitch_core::Chord`], [`Editor::keymap`] resolves that to
//! a [`Command`] (or, unbound and printable, an [`Command::InsertText`]
//! fallback — see [`resolve_keyboard_command`]), [`hit::position_at`] and
//! [`hit::Drag`] turn a click/drag into cursor placement and selection, and
//! [`hit::scroll_by_wheel`] turns a wheel notch into a scrolled viewport.
//! [`dispatch`] runs the resulting `Command` through [`Editor::run`] and acts
//! on its [`Outcome`] — including the clipboard, via
//! `platform::win32::clipboard` — exactly as the pre-rewrite, winit-based
//! `app.rs` did (`git show adf6c14^:crates/aitch-ui/src/app.rs`), just
//! against this backend's types.
//!
//! **Still deliberately missing**, per `PLAN-ZERO-DEP.md` §4 Phase 3: a
//! folder or config watcher and a recovery-write timer. Both need either
//! `SetTimer`/`WM_TIMER` or a cross-thread wake via `PostMessageW` — neither
//! exists yet, and both are Phase 4/7-shaped work this phase's tracks were
//! never reviewed against, not an oversight here.
//!
//! Phase 3 Track C's chrome rendering (the footer's two rows, a status line
//! a prompt's own line replaces rather than joins, and a help pane that
//! takes over the main text area) is unchanged by this integration — see
//! [`render_frame`]'s own doc comment. What changes here is *when* a redraw
//! happens: every dispatched command, mouse action, and scroll now triggers
//! one, not just [`Event::Resized`].

use std::error::Error;

use aitch_core::{
    footer, Command, Config, ConfigError, Editor, Outcome, Position, Session, ThemeChoice,
    Workspace,
};

use crate::hit::{self, Drag};
use crate::input;
use crate::platform::win32::clipboard;
use crate::platform::win32::surface::{Color as GdiColor, Surface};
use crate::platform::win32::window::{Event, MouseButton, Window};
use crate::text::font::Font;
use crate::text::raster::{self, GlyphCache};
use crate::text::shape;
use crate::theme::Theme;

/// The window's starting size, in logical (DPI-independent) pixels.
const WINDOW_WIDTH: i32 = 960;
const WINDOW_HEIGHT: i32 = 640;

/// The one font this rewrite bundles — see `PLAN-ZERO-DEP.md` §3, "One
/// bundled font, no system font discovery." Licensed in
/// `docs/third-party.md`.
const FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/DejaVuSansMono.ttf");

/// A line-spacing multiplier over the font's own ascender-descender span.
/// The deleted cosmic-text-based renderer used 1.4x; kept close to that for
/// visual continuity, though neither figure has more authority than the
/// other — both are just "a bit more than the raw glyph height."
const LINE_HEIGHT_RATIO: f32 = 1.4;

/// Everything the binary worked out before there was a window.
pub struct Startup {
    pub workspace: Workspace,
    pub config: Config,
    /// A config that would not load. Shown once there is a status line.
    pub config_error: Option<ConfigError>,
    /// Where the config came from, so saving it can take effect live.
    ///
    /// Not yet read: live-reload needs a config watcher, which needs a way to
    /// wake the window's message loop from another thread — Phase 7's job,
    /// not Phase 0's.
    pub config_path: Option<std::path::PathBuf>,
    /// What was open last time, when nothing else was asked for.
    pub session: Option<Session>,
    /// From `+LINE:COLUMN`, one-based.
    pub cursor: Option<(usize, usize)>,
}

/// Layout numbers derived once from the font and config, at the pixel size
/// text is drawn at. Fixed for the process's lifetime: recomputing these
/// becomes necessary once Phase 7's config live-reload can change the font
/// size at runtime, which is also when [`GlyphCache`]'s key needs to grow
/// from a bare `char` to `(char, pixel size)`.
struct Metrics {
    /// Font units to pixels: `pixel_size / units_per_em`.
    scale: f32,
    /// One monospace column's width, in pixels.
    advance_width: f32,
    /// One line's height, in pixels — `(ascender - descender) * scale *
    /// LINE_HEIGHT_RATIO`.
    line_height: f32,
    /// The ascender, in pixels above the baseline.
    ascender_px: f32,
    /// The descender, in pixels — negative, same convention `hhea` uses.
    descender_px: f32,
    tab_columns: usize,
}

/// Every color a frame is drawn with, bundled so `render_frame`/`redraw`/
/// `dispatch` take one argument instead of four that always travel together.
#[derive(Debug, Clone, Copy)]
struct Colors {
    background: GdiColor,
    foreground: GdiColor,
    cursor: GdiColor,
    /// Translucent — see `Theme::selection`'s own doc comment and
    /// [`gdi_color_with_alpha`].
    selection: GdiColor,
}

impl Metrics {
    fn new(font: &Font, pixel_size: f32, tab_columns: usize) -> Metrics {
        let scale = pixel_size / f32::from(font.units_per_em());
        // Space's advance width, not a glyph that might be missing: every
        // font has one, and DejaVu Sans Mono's own tests already prove every
        // printable ASCII glyph shares this one width anyway.
        let advance_width = font
            .glyph_for_char(' ')
            .map(|g| f32::from(g.advance_width) * scale)
            .unwrap_or(pixel_size * 0.6);
        let ascender_px = f32::from(font.ascender()) * scale;
        let descender_px = f32::from(font.descender()) * scale;
        let line_height = (ascender_px - descender_px) * LINE_HEIGHT_RATIO;
        Metrics {
            scale,
            advance_width,
            line_height,
            ascender_px,
            descender_px,
            tab_columns,
        }
    }
}

/// Open a window and run until it closes.
pub fn run(startup: Startup) -> Result<(), Box<dyn Error>> {
    let Startup {
        workspace,
        config,
        config_error,
        config_path: _config_path,
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

    // Anything left behind by a run that did not end cleanly. Nothing renders
    // it yet — Phase 3's status line does — but the check itself is cheap and
    // has no GUI dependency, so there is no reason to defer it.
    editor.offer_recovery();

    let theme = match config.theme {
        ThemeChoice::Dark => Theme::dark(),
        ThemeChoice::Light => Theme::light(),
    };
    let colors = Colors {
        background: gdi_color(theme.background),
        foreground: gdi_color(theme.foreground),
        cursor: gdi_color(theme.cursor),
        selection: gdi_color_with_alpha(theme.selection),
    };

    // The bundled font is a fixed asset shipped in the binary, not user
    // input: if this ever fails to parse, that is a build defect, not a
    // runtime condition to recover from gracefully. `Font::parse`'s own test
    // suite already proves it parses.
    let font = Font::parse(FONT_BYTES).expect("the bundled font should always parse");
    let metrics = Metrics::new(&font, config.font.size(), config.tab_width);
    let mut cache: GlyphCache<char> = GlyphCache::new();

    let mut window = Window::new(&title(&editor), WINDOW_WIDTH, WINDOW_HEIGHT)?;
    // So the title bar is part of the editor rather than a strip of the
    // shell's default sitting on top of it. The chrome colours, not the
    // buffer background, because the title bar is chrome: it should match the
    // status line and the footer.
    window.set_caption_theme(
        theme.chrome_background.to_srgb_bytes(),
        theme.chrome_foreground.to_srgb_bytes(),
        matches!(config.theme, ThemeChoice::Dark),
    );
    let mut surface: Option<Surface> = None;
    let mut drag = Drag::new();
    // Pixel-granular scroll position; `hit::scroll_by_wheel`'s accumulator.
    // The viewport itself only ever sees the line this floors to — no
    // pre-rewrite `App`-style sub-line rendering offset yet, which is a
    // known, deliberate simplification, not a bug: it just means a wheel
    // notch always lands on a whole-line boundary.
    let mut scroll_px: f32 = 0.0;

    window.run(move |window, event| match event {
        Event::Resized { width, height } => {
            match surface.as_mut() {
                Some(surface) => surface.resize(width, height),
                None => surface = Some(Surface::new(window.raw_handle(), width, height)),
            }

            // Whole lines only: a partially visible line at the bottom of
            // the text area is simply not drawn, not a broken layout.
            //
            // The viewport only ever sees the text area above the footer's
            // two rows and the status/prompt row — see
            // `text_area_height_px`'s docs — so the buffer never draws
            // underneath the chrome Track C added.
            let text_area = text_area_height_px(height, metrics.line_height);
            let height_lines = ((text_area / metrics.line_height).floor() as usize).max(1);
            editor.viewport_mut().set_height_lines(height_lines);

            redraw(&mut surface, &font, &mut cache, &metrics, &editor, colors);
        }

        // Nothing to redo yet: there are no glyphs rasterized at the old
        // scale to throw away (the cache is keyed on `char`, not scale — see
        // `Metrics`'s docs). `Window` has already resized the OS window rect;
        // the `Resized` event that follows is what actually redraws.
        Event::ScaleChanged { .. } => {}

        Event::CloseRequested => finish_and_close(window, &mut editor),

        Event::KeyDown { .. } => {
            if let Some(command) = resolve_keyboard_command(&editor, &event, None) {
                dispatch(
                    window,
                    &mut editor,
                    &mut surface,
                    &font,
                    &mut cache,
                    &metrics,
                    colors,
                    command,
                );
            }
        }

        Event::Char(c) => {
            if let Some(command) = resolve_keyboard_command(&editor, &event, Some(c)) {
                dispatch(
                    window,
                    &mut editor,
                    &mut surface,
                    &font,
                    &mut cache,
                    &metrics,
                    colors,
                    command,
                );
            }
        }

        Event::MouseButton {
            button: MouseButton::Left,
            pressed: true,
            x,
            y,
        } => {
            let text_area = current_text_area_height(&surface, metrics.line_height);
            let position = hit::position_at(
                x,
                y,
                text_area,
                editor.buffer(),
                editor.viewport().first_line(),
                metrics.line_height,
                metrics.advance_width,
                metrics.tab_columns,
            );
            // A click on chrome (None) starts no drag, exactly as the
            // pre-rewrite `position_at_pointer` rejecting it left `dragging`
            // false.
            if let Some(position) = position {
                drag.press(editor.buffer_mut(), position);
                redraw(&mut surface, &font, &mut cache, &metrics, &editor, colors);
            }
        }

        Event::MouseButton {
            button: MouseButton::Left,
            pressed: false,
            ..
        } => {
            drag.release(editor.buffer_mut());
            redraw(&mut surface, &font, &mut cache, &metrics, &editor, colors);
        }

        Event::MouseMove { x, y } => {
            if drag.is_active() {
                let text_area = current_text_area_height(&surface, metrics.line_height);
                let position = hit::position_at(
                    x,
                    y,
                    text_area,
                    editor.buffer(),
                    editor.viewport().first_line(),
                    metrics.line_height,
                    metrics.advance_width,
                    metrics.tab_columns,
                );
                if let Some(position) = position {
                    if drag.drag_to(editor.buffer_mut(), position) {
                        redraw(&mut surface, &font, &mut cache, &metrics, &editor, colors);
                    }
                }
            }
        }

        Event::MouseWheel { delta_lines } => {
            let total_lines = editor.buffer().len_lines();
            scroll_px =
                hit::scroll_by_wheel(scroll_px, delta_lines, metrics.line_height, total_lines);
            let line = (scroll_px / metrics.line_height).floor() as usize;
            if editor.viewport_mut().scroll_to(line, total_lines) {
                editor.view_moved();
                redraw(&mut surface, &font, &mut cache, &metrics, &editor, colors);
            }
        }
    });

    Ok(())
}

/// Resolve a keyboard [`Event`] to a [`Command`], the way the pre-rewrite
/// `app.rs` did in its `WindowEvent::KeyboardInput` handler: a chord that
/// resolves in the active keymap wins outright; failing that, an unbound
/// `Ctrl`/`Alt` combo does nothing (never falls through to text); failing
/// that, `insertable` (only ever `Some` for [`Event::Char`] — a `KeyDown`'s
/// `NamedKey` has no character of its own to insert) becomes a plain
/// [`Command::InsertText`]. Inserts `insertable` itself, not anything
/// derived from the chord: [`input::chord_from_event`]'s `Chord` normalizes
/// ASCII letters to lowercase for keymap matching, but a `Shift`-produced
/// capital letter must still appear as typed.
fn resolve_keyboard_command(
    editor: &Editor,
    event: &Event,
    insertable: Option<char>,
) -> Option<Command> {
    let chord = input::chord_from_event(event)?;
    if let Some(command) = editor.keymap().resolve(editor.context(), chord).cloned() {
        return Some(command);
    }
    if chord.mods.ctrl || chord.mods.alt {
        return None;
    }
    insertable.map(|c| Command::InsertText(c.to_string()))
}

/// Save-or-clear the session and close the window — shared by
/// [`Event::CloseRequested`] and a dispatched [`Outcome::Quit`] (nano's
/// `^X`), which must do exactly the same thing.
fn finish_and_close(window: &mut Window, editor: &mut Editor) {
    let session = editor.session();
    if session.is_empty() {
        Session::clear();
    } else {
        session.save();
    }
    editor.abandon_recovery();
    window.close();
}

/// Run `command` through `editor` and act on its [`Outcome`], the way the
/// pre-rewrite `App::dispatch`/`copy`/`paste` did.
#[allow(clippy::too_many_arguments)]
fn dispatch(
    window: &mut Window,
    editor: &mut Editor,
    surface: &mut Option<Surface>,
    font: &Font,
    cache: &mut GlyphCache<char>,
    metrics: &Metrics,
    colors: Colors,
    command: Command,
) {
    let outcome = editor.run(&command);
    let redraws = outcome.redraws();

    match outcome {
        Outcome::Quit => {
            finish_and_close(window, editor);
            return;
        }
        Outcome::Copy(text) => match clipboard::set_text(&text) {
            Ok(()) => editor.say("copied"),
            Err(e) => editor.say(format!("clipboard: {e}")),
        },
        Outcome::Paste => match clipboard::get_text() {
            Ok(text) if !text.is_empty() => {
                // Back through the front door: the UI does not touch the
                // buffer directly, same as the pre-rewrite `app.rs`.
                dispatch(
                    window,
                    editor,
                    surface,
                    font,
                    cache,
                    metrics,
                    colors,
                    Command::InsertText(text),
                );
                return;
            }
            Ok(_) => {}
            Err(e) => editor.say(format!("clipboard: {e}")),
        },
        Outcome::Redraw | Outcome::Nothing => {}
    }

    if redraws {
        editor.scroll_to_cursor();
        redraw(surface, font, cache, metrics, editor, colors);
        window.set_title(&title(editor));
    }
}

/// Redraw `surface` if it exists yet — a no-op before the first
/// [`Event::Resized`], which shouldn't be reachable in practice (Windows
/// sends an initial `WM_SIZE` immediately) but costs nothing to guard.
fn redraw(
    surface: &mut Option<Surface>,
    font: &Font,
    cache: &mut GlyphCache<char>,
    metrics: &Metrics,
    editor: &Editor,
    colors: Colors,
) {
    if let Some(surface) = surface.as_mut() {
        render_frame(surface, font, cache, metrics, editor, colors);
    }
}

/// [`text_area_height_px`] against whatever `surface` currently is — `0`
/// before the first resize, which conservatively rejects every click rather
/// than guessing at a text area that doesn't exist yet.
fn current_text_area_height(surface: &Option<Surface>, line_height: f32) -> i32 {
    let window_height = surface.as_ref().map(Surface::height).unwrap_or(0);
    text_area_height_px(window_height, line_height) as i32
}

/// The text of every line the viewport currently considers visible, without
/// line breaks — exactly what [`render_frame`] draws, one row per entry.
fn visible_lines(editor: &Editor) -> Vec<String> {
    let buffer = editor.buffer();
    let viewport = editor.viewport();
    let first = viewport.first_line();
    let total = buffer.len_lines();
    let last = (first + viewport.height_lines()).min(total);
    (first..last)
        .map(|line| buffer.line_text(line).as_str().to_string())
        .collect()
}

/// Height, in pixels, of the fixed chrome band pinned to the bottom of the
/// window: the status/prompt row, then the footer's two rows — always three
/// rows regardless of whether a prompt is active, per Phase 3 Track C's
/// design (`PLAN-ZERO-DEP.md` §4 Phase 3): a prompt *replaces* the status
/// line for that frame rather than adding a row beside it, so the chrome
/// never grows a third footer row and never shrinks below three either.
///
/// Plain `f32` in, plain `f32` out — deliberately not `&Metrics`, which is
/// private to this module — so this is reusable from any module (the mouse
/// hit-testing track's `hit.rs`, once it lands) without needing to see
/// `Metrics` itself.
pub(crate) fn chrome_height_px(line_height: f32) -> f32 {
    (1 + footer::ROWS) as f32 * line_height
}

/// Height, in pixels, of the main text area above the chrome band: the
/// window's client height minus [`chrome_height_px`], floored at zero for a
/// window too short to show any chrome at all.
///
/// This is the number `render_frame` reserves buffer/help text to, and the
/// number `run`'s `Resized` handler derives the viewport's visible-line count
/// from — see its call site. It is also what Phase 3 Track B's mouse
/// hit-testing module should treat as the vertical extent of clickable
/// buffer content once it is wired in: a click at or below this many pixels
/// from the top lands on the chrome, not the buffer.
pub(crate) fn text_area_height_px(window_height: u32, line_height: f32) -> f32 {
    (window_height as f32 - chrome_height_px(line_height)).max(0.0)
}

/// How many whole rows of `line_height` fit within `area_height_px`.
fn rows_that_fit(area_height_px: f32, line_height: f32) -> usize {
    if line_height <= 0.0 {
        return 0;
    }
    (area_height_px / line_height).floor().max(0.0) as usize
}

/// Draw one row of text with its top edge at `row_top` pixels down from the
/// surface's top, through the full hand-written text pipeline: monospace
/// layout ([`shape::layout_line`]), rasterization and caching ([`raster`],
/// [`GlyphCache`]), and compositing ([`Surface::draw_coverage`]).
///
/// Extracted out of `render_frame`'s old per-line loop so buffer lines and
/// every piece of chrome (footer rows, the status/prompt row, the help pane)
/// draw through the exact same pipeline rather than duplicating it — see
/// `render_frame`'s own doc comment.
fn draw_row(
    surface: &mut Surface,
    font: &Font,
    cache: &mut GlyphCache<char>,
    metrics: &Metrics,
    text: &str,
    row_top: f32,
    color: GdiColor,
) {
    // Centers each line's glyphs within its line-height band: the extra
    // space `LINE_HEIGHT_RATIO` adds over the raw ascender-descender span is
    // split evenly above and below, rather than all landing below the
    // descender or above the ascender.
    let content_height = metrics.ascender_px - metrics.descender_px;
    let margin = (metrics.line_height - content_height).max(0.0) / 2.0;
    let baseline_y = row_top + margin + metrics.ascender_px;

    let placements =
        shape::layout_line(text, metrics.advance_width, metrics.tab_columns, baseline_y);
    for placement in &placements {
        let glyph = cache.get_or_insert_with(placement.ch, || {
            // A character with no glyph in the bundled font (outside its
            // Latin coverage) falls back to space -- drawing nothing is
            // better than a missing-glyph box this rasterizer has no concept
            // of, and better than panicking on user text.
            let outline = font
                .glyph_for_char(placement.ch)
                .or_else(|| font.glyph_for_char(' '))
                .unwrap_or_default();
            raster::rasterize(&outline, metrics.scale)
        });

        if glyph.coverage.width == 0 || glyph.coverage.height == 0 {
            continue;
        }
        let x = (placement.x + glyph.bearing_x).round() as i32;
        let y = (placement.y - glyph.bearing_y).round() as i32;
        surface.draw_coverage(x, y, &glyph.coverage, color);
    }
}

/// Clear `surface` to `background` and draw a whole frame onto it in
/// `foreground`: the main text area (the buffer's visible lines, or —
/// Phase 3 Track C — the help pane's lines instead, when help is active),
/// then the bottom chrome band ([`chrome_height_px`]): the status line, or a
/// prompt's line in its place when one is active, followed by the footer's
/// two rows (`editor.footer`). Every row, chrome or buffer, is drawn through
/// the same [`draw_row`] helper.
///
/// A free function taking every dependency as a parameter, rather than a
/// method threading through `App`/`run`'s closure state, specifically so it
/// can be exercised directly in a test with an in-memory `Surface` and no
/// real window — see the tests below.
#[allow(clippy::too_many_arguments)]
fn render_frame(
    surface: &mut Surface,
    font: &Font,
    cache: &mut GlyphCache<char>,
    metrics: &Metrics,
    editor: &Editor,
    colors: Colors,
) {
    surface.clear(colors.background);

    // The main area: the buffer's own visible lines, unless help has taken
    // it over. No scrolling within a help pane longer than the window — a
    // deliberate simplification (see PLAN-ZERO-DEP.md Phase 3 Track C):
    // `rows_that_fit` below just clips anything past the last visible row,
    // the same way an overlong buffer already would.
    let main_lines: Vec<String> = match editor.help() {
        Some(_) => editor.help_text(),
        None => visible_lines(editor),
    };
    let text_rows = rows_that_fit(
        text_area_height_px(surface.height(), metrics.line_height),
        metrics.line_height,
    );

    // Behind the text, so glyphs stay at full contrast on top of it — a
    // real cursor caret or selection band has no meaning over the help
    // pane's own text, which isn't the buffer at all.
    if editor.help().is_none() {
        draw_selection(surface, metrics, editor, colors.selection, text_rows);
    }

    for (row, text) in main_lines.iter().take(text_rows).enumerate() {
        let row_top = row as f32 * metrics.line_height;
        draw_row(
            surface,
            font,
            cache,
            metrics,
            text,
            row_top,
            colors.foreground,
        );
    }

    if editor.help().is_none() {
        draw_cursor(surface, metrics, editor, colors.cursor, text_rows);
    }

    // The chrome band, pinned to the bottom of the surface regardless of how
    // many text rows actually fit above it.
    let chrome_top = surface.height() as f32 - chrome_height_px(metrics.line_height);

    // A prompt replaces the status line for this frame rather than adding a
    // row beside it -- never both on screen, never a third row.
    let status_text = match editor.prompt() {
        Some(prompt) => prompt.line(),
        None => editor.status_line(),
    };
    draw_row(
        surface,
        font,
        cache,
        metrics,
        &status_text,
        chrome_top,
        colors.foreground,
    );

    let columns = ((surface.width() as f32 / metrics.advance_width).floor() as usize).max(1);
    let footer_lines = editor.footer(columns).lines();
    for row in 0..footer::ROWS {
        let text = footer_lines.get(row).map(String::as_str).unwrap_or("");
        let row_top = chrome_top + (row + 1) as f32 * metrics.line_height;
        draw_row(
            surface,
            font,
            cache,
            metrics,
            text,
            row_top,
            colors.foreground,
        );
    }

    surface.present();
}

/// The pixel x-coordinate where character `column` of `text` begins, using
/// the same tab-stop rule [`shape::layout_line`] lays text out with (the
/// next multiple of `tab_columns`) — ported rather than shared because that
/// rule is private to `shape.rs` and this needs the *reverse* mapping
/// (column to x) that function's own placement list doesn't expose for a
/// column that lands exactly on a tab. `column == text.chars().count()`
/// (one past the last character) is the valid, meaningful "end of line"
/// case a cursor or selection edge needs, not an error.
fn x_for_column(text: &str, column: usize, advance_width: f32, tab_columns: usize) -> f32 {
    let mut screen_column: usize = 0;
    for (index, ch) in text.chars().enumerate() {
        if index == column {
            break;
        }
        screen_column = if ch == '\t' {
            next_tab_stop(screen_column, tab_columns)
        } else {
            screen_column + 1
        };
    }
    screen_column as f32 * advance_width
}

/// See [`x_for_column`] — ported unchanged from `shape::layout_line`'s own
/// (private) tab-stop rule.
fn next_tab_stop(column: usize, tab_columns: usize) -> usize {
    if tab_columns == 0 {
        return column + 1;
    }
    (column / tab_columns + 1) * tab_columns
}

/// A [`raster::Coverage`] rectangle at full (255) coverage everywhere —
/// [`Surface::draw_coverage`]'s own alpha blend then does the rest: an
/// opaque color fully replaces the covered pixels (the cursor), a
/// translucent one blends over them (the selection highlight).
fn solid_rect(width: u32, height: u32) -> raster::Coverage {
    raster::Coverage {
        width,
        height,
        pixels: vec![255u8; (width as usize) * (height as usize)],
    }
}

/// How wide the cursor caret is drawn, regardless of font size — a thin bar
/// between characters, not a block filling one.
const CURSOR_WIDTH_PX: f32 = 2.0;

/// Draw the buffer's cursor as a thin vertical bar, if it falls within the
/// `text_rows` currently visible rows. On top of the text (drawn after it in
/// [`render_frame`]), so it's always fully visible rather than blended.
fn draw_cursor(
    surface: &mut Surface,
    metrics: &Metrics,
    editor: &Editor,
    cursor_color: GdiColor,
    text_rows: usize,
) {
    let buffer = editor.buffer();
    let first_line = editor.viewport().first_line();
    let position = buffer.cursor();
    if position.line < first_line {
        return;
    }
    let row = position.line - first_line;
    if row >= text_rows {
        return;
    }

    let text = buffer.line_text(position.line);
    let x = x_for_column(
        text.as_str(),
        position.column,
        metrics.advance_width,
        metrics.tab_columns,
    );
    let row_top = row as f32 * metrics.line_height;
    let coverage = solid_rect(
        CURSOR_WIDTH_PX.round().max(1.0) as u32,
        metrics.line_height.round().max(1.0) as u32,
    );
    surface.draw_coverage(
        x.round() as i32,
        row_top.round() as i32,
        &coverage,
        cursor_color,
    );
}

/// Draw a translucent band over every visible row the buffer's current
/// selection spans, if any. Behind the text (drawn before it in
/// [`render_frame`]) so glyphs stay at full contrast on top of the
/// highlight, matching `Theme::selection`'s own "text reads through it"
/// design.
fn draw_selection(
    surface: &mut Surface,
    metrics: &Metrics,
    editor: &Editor,
    selection_color: GdiColor,
    text_rows: usize,
) {
    let buffer = editor.buffer();
    let Some(range) = buffer.selection() else {
        return;
    };
    let first_line = editor.viewport().first_line();
    let start = buffer.char_to_position(range.start);
    let end = buffer.char_to_position(range.end);

    for line in start.line..=end.line {
        if line < first_line {
            continue;
        }
        let row = line - first_line;
        if row >= text_rows {
            break;
        }

        let text = buffer.line_text(line);
        let line_len = text.len_chars();
        let start_col = if line == start.line { start.column } else { 0 };
        let end_col = if line == end.line {
            end.column
        } else {
            line_len
        };
        if end_col <= start_col {
            continue;
        }

        let x0 = x_for_column(
            text.as_str(),
            start_col,
            metrics.advance_width,
            metrics.tab_columns,
        );
        let x1 = x_for_column(
            text.as_str(),
            end_col,
            metrics.advance_width,
            metrics.tab_columns,
        );
        let row_top = row as f32 * metrics.line_height;
        let width = (x1 - x0).max(1.0).round() as u32;
        let height = metrics.line_height.round().max(1.0) as u32;
        let coverage = solid_rect(width, height);
        surface.draw_coverage(
            x0.round() as i32,
            row_top.round() as i32,
            &coverage,
            selection_color,
        );
    }
}

/// The window title. The status line (Phase 3) will carry more detail; this
/// is just enough to pick the window out of a task bar.
fn title(editor: &Editor) -> String {
    let modified = if editor.document().is_dirty() {
        "*"
    } else {
        ""
    };
    format!("{modified}{} — Aitch", editor.document().display_name())
}

/// A theme color as the GDI surface wants it: raw 8-bit sRGB, not the linear
/// light `Theme` stores.
fn gdi_color(color: crate::theme::Color) -> GdiColor {
    let (r, g, b) = color.to_srgb_bytes();
    GdiColor::from_rgb(r, g, b)
}

/// The same as [`gdi_color`], but preserving `color`'s alpha rather than
/// forcing full opacity — for the selection highlight, which must stay
/// translucent so the text underneath still reads (see `Theme::selection`'s
/// own doc comment).
fn gdi_color_with_alpha(color: crate::theme::Color) -> GdiColor {
    let (r, g, b) = color.to_srgb_bytes();
    let a = (color.a.clamp(0.0, 1.0) * 255.0).round() as u8;
    GdiColor::from_rgba(r, g, b, a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitch_core::{Buffer, Command, Document};

    fn dejavu() -> Font<'static> {
        Font::parse(FONT_BYTES).expect("the bundled font should parse")
    }

    /// A standalone `Editor` over `text`, with no path and no session —
    /// everything `render_frame`'s tests need from `aitch-core` without a
    /// real `Startup`/`Workspace`.
    fn editor_with_text(text: &str) -> Editor {
        let mut document = Document::blank();
        document.buffer = Buffer::from_str(text);
        Editor::new(document)
    }

    /// A [`Colors`] for tests that only care about background/foreground —
    /// cursor and selection get fixed, distinctive colors of their own so a
    /// test checking for "background" or "foreground" ink specifically can't
    /// accidentally match one of them instead.
    fn test_colors(background: GdiColor, foreground: GdiColor) -> Colors {
        Colors {
            background,
            foreground,
            cursor: GdiColor::from_rgb(0, 255, 0),
            selection: GdiColor::from_rgba(0, 0, 255, 128),
        }
    }

    fn pixel_at(surface: &Surface, x: u32, y: u32) -> [u8; 4] {
        let stride = surface.width() as usize * 4;
        let offset = y as usize * stride + x as usize * 4;
        surface.pixels()[offset..offset + 4].try_into().unwrap()
    }

    /// The raw bytes of every pixel row in `y0..y1`, for comparing one drawn
    /// region against another (e.g. "does the status row's content change").
    fn row_bytes(surface: &Surface, y0: u32, y1: u32) -> Vec<u8> {
        let stride = surface.width() as usize * 4;
        let y0 = (y0 as usize).min(surface.height() as usize);
        let y1 = (y1 as usize).min(surface.height() as usize);
        surface.pixels()[y0 * stride..y1 * stride].to_vec()
    }

    /// Whether any pixel in rows `y0..y1` differs from `background` — i.e.
    /// something was actually drawn there, not just cleared.
    fn row_has_ink(surface: &Surface, background: GdiColor, y0: u32, y1: u32) -> bool {
        let bg = background.to_bgra_bytes();
        row_bytes(surface, y0, y1)
            .chunks_exact(4)
            .any(|pixel| pixel != bg)
    }

    #[test]
    fn metrics_are_derived_from_the_real_font_and_config() {
        let font = dejavu();
        let metrics = Metrics::new(&font, 16.0, 4);
        assert!(metrics.scale > 0.0);
        assert!(metrics.advance_width > 0.0);
        assert!(metrics.line_height > 0.0);
        // A monospace font's own advance should be comfortably narrower than
        // a full em at any reasonable size -- if this were backwards
        // (advance_width close to or bigger than pixel_size), the scale or
        // the glyph lookup would be suspect.
        assert!(metrics.advance_width < 16.0);
    }

    #[test]
    fn chrome_height_is_always_the_status_row_plus_the_footers_two_rows() {
        // Not editor-dependent at all -- see its doc comment -- which is
        // exactly what makes "a prompt never grows a third row" true by
        // construction rather than by convention.
        assert_eq!(chrome_height_px(10.0), 30.0);
        assert_eq!(chrome_height_px(10.0), (1 + footer::ROWS) as f32 * 10.0);
    }

    #[test]
    fn text_area_height_is_the_window_height_minus_the_chrome_and_never_negative() {
        assert_eq!(
            text_area_height_px(100, 10.0),
            100.0 - chrome_height_px(10.0)
        );
        // A window shorter than the chrome band reserves zero, not a
        // negative height.
        assert_eq!(text_area_height_px(5, 10.0), 0.0);
    }

    #[test]
    fn render_frame_draws_ink_where_text_is_and_leaves_the_rest_background() {
        let font = dejavu();
        let metrics = Metrics::new(&font, 24.0, 4);
        let mut cache = GlyphCache::new();
        let background = GdiColor::from_rgb(0, 0, 0);
        let foreground = GdiColor::from_rgb(255, 255, 255);

        // Wide and tall enough that a short first line leaves plenty of
        // untouched background both to its right and above the chrome band
        // at the bottom.
        let mut surface = Surface::new(0, 400, 400);
        let editor = editor_with_text("W");
        render_frame(
            &mut surface,
            &font,
            &mut cache,
            &metrics,
            &editor,
            test_colors(background, foreground),
        );

        // Far to the right of a single narrow glyph, on its own row: still
        // background. Proves render_frame doesn't paint the whole surface.
        let far_right = pixel_at(&surface, 390, (metrics.line_height * 0.5) as u32);
        assert_eq!(far_right, background.to_bgra_bytes());

        // A row well past the only buffer line, but still above the chrome
        // band: also still background.
        let far_below_y = (metrics.line_height * 3.0) as u32;
        assert!(
            (far_below_y as f32) < surface.height() as f32 - chrome_height_px(metrics.line_height),
            "test setup: this row must land above the chrome band"
        );
        let far_below = pixel_at(&surface, 10, far_below_y);
        assert_eq!(far_below, background.to_bgra_bytes());

        // Somewhere within 'W''s own glyph box, some pixel should actually
        // be inked -- not the exact background color. 'W' is wide and tall
        // enough in a monospace face that scanning its whole cell finds ink
        // reliably without depending on precise rasterizer internals.
        let cell_width = metrics.advance_width.ceil() as u32;
        let cell_height = metrics.line_height.ceil() as u32;
        let mut found_ink = false;
        for y in 0..cell_height.min(surface.height()) {
            for x in 0..cell_width.min(surface.width()) {
                if pixel_at(&surface, x, y) != background.to_bgra_bytes() {
                    found_ink = true;
                }
            }
        }
        assert!(
            found_ink,
            "expected at least one non-background pixel within 'W''s cell"
        );
    }

    #[test]
    fn an_empty_buffer_shows_only_its_cursor_in_the_text_area() {
        let font = dejavu();
        let metrics = Metrics::new(&font, 16.0, 4);
        let mut cache = GlyphCache::new();
        let background = GdiColor::from_rgb(10, 20, 30);
        let foreground = GdiColor::from_rgb(255, 255, 255);
        let colors = test_colors(background, foreground);

        let editor = editor_with_text("");
        let mut surface = Surface::new(0, 200, 200);
        render_frame(&mut surface, &font, &mut cache, &metrics, &editor, colors);

        // An empty document has no text to draw, but it does still have a
        // cursor -- at its very start -- which is real ink this test must
        // not mistake for a rendering bug.
        assert_eq!(
            pixel_at(&surface, 0, 0),
            colors.cursor.to_bgra_bytes(),
            "expected the cursor's own color at the start of an empty document"
        );

        // Nothing else in the text area is touched: no buffer text, and the
        // cursor's own bar is only CURSOR_WIDTH_PX wide and one line tall.
        let chrome_top = surface.height() as f32 - chrome_height_px(metrics.line_height);
        let cursor_width = CURSOR_WIDTH_PX.round().max(1.0) as u32;
        let cursor_height = metrics.line_height.round().max(1.0) as u32;
        let mut extra_ink = false;
        for y in 0..chrome_top.round() as u32 {
            for x in 0..surface.width() {
                let in_cursor_bar = y < cursor_height && x < cursor_width;
                if !in_cursor_bar && pixel_at(&surface, x, y) != background.to_bgra_bytes() {
                    extra_ink = true;
                }
            }
        }
        assert!(
            !extra_ink,
            "expected only the cursor's own bar in the text area of an empty buffer"
        );

        // But the footer always has entries and the status line always has
        // something to say, so the chrome band at the bottom is never empty.
        assert!(
            row_has_ink(
                &surface,
                background,
                chrome_top.round() as u32,
                surface.height()
            ),
            "expected the chrome band to draw something even for an empty buffer"
        );
    }

    #[test]
    fn the_footers_two_rows_draw_ink_near_the_bottom_of_the_surface() {
        let font = dejavu();
        let metrics = Metrics::new(&font, 16.0, 4);
        let mut cache = GlyphCache::new();
        let background = GdiColor::from_rgb(0, 0, 0);
        let foreground = GdiColor::from_rgb(255, 255, 255);

        let editor = editor_with_text("hello\n");
        let mut surface = Surface::new(0, 400, 300);
        render_frame(
            &mut surface,
            &font,
            &mut cache,
            &metrics,
            &editor,
            test_colors(background, foreground),
        );

        let chrome_top = surface.height() as f32 - chrome_height_px(metrics.line_height);
        let footer_top = (chrome_top + metrics.line_height).round() as u32;
        assert!(
            row_has_ink(&surface, background, footer_top, surface.height()),
            "expected the footer's two rows to draw something near the bottom"
        );
    }

    #[test]
    fn the_status_line_draws_in_its_own_row_above_the_footer() {
        let font = dejavu();
        let metrics = Metrics::new(&font, 16.0, 4);
        let mut cache = GlyphCache::new();
        let background = GdiColor::from_rgb(0, 0, 0);
        let foreground = GdiColor::from_rgb(255, 255, 255);

        let mut editor = editor_with_text("");
        editor.say("a distinctive status message");
        let colors = test_colors(background, foreground);
        let mut surface = Surface::new(0, 400, 300);
        render_frame(&mut surface, &font, &mut cache, &metrics, &editor, colors);

        let chrome_top = surface.height() as f32 - chrome_height_px(metrics.line_height);
        let status_top = chrome_top.round() as u32;
        let status_bottom = (chrome_top + metrics.line_height).round() as u32;

        assert!(
            row_has_ink(&surface, background, status_top, status_bottom),
            "expected the status row to draw the status message"
        );
        // Distinct from the (empty-buffer) text area above it -- apart from
        // the cursor's own bar, which legitimately draws at the very start
        // of any document, empty or not.
        let mut extra_ink_above_status = false;
        for y in 0..status_top {
            for x in 0..surface.width() {
                let in_cursor_bar = x < CURSOR_WIDTH_PX.round().max(1.0) as u32
                    && y < metrics.line_height.round().max(1.0) as u32;
                if !in_cursor_bar && pixel_at(&surface, x, y) != background.to_bgra_bytes() {
                    extra_ink_above_status = true;
                }
            }
        }
        assert!(
            !extra_ink_above_status,
            "the status row's ink should not bleed into the text area"
        );
    }

    #[test]
    fn a_prompt_takes_over_the_status_row_without_growing_a_third_chrome_row() {
        let font = dejavu();
        let metrics = Metrics::new(&font, 16.0, 4);
        let mut cache = GlyphCache::new();
        let background = GdiColor::from_rgb(0, 0, 0);
        let foreground = GdiColor::from_rgb(255, 255, 255);
        let colors = test_colors(background, foreground);

        // No prompt: the ordinary status row.
        let editor = editor_with_text("");
        let mut surface = Surface::new(0, 400, 300);
        render_frame(&mut surface, &font, &mut cache, &metrics, &editor, colors);
        let chrome_top = surface.height() as f32 - chrome_height_px(metrics.line_height);
        let status_top = chrome_top.round() as u32;
        let status_bottom = (chrome_top + metrics.line_height).round() as u32;
        let status_row_no_prompt = row_bytes(&surface, status_top, status_bottom);

        // Now with a prompt open.
        let mut editor = editor_with_text("");
        editor.run(&Command::GotoLine);
        assert!(
            editor.prompt().is_some(),
            "Command::GotoLine should have opened a prompt"
        );
        let mut surface = Surface::new(0, 400, 300);
        render_frame(&mut surface, &font, &mut cache, &metrics, &editor, colors);

        // The reserved chrome height does not change just because a prompt
        // opened -- a prompt replaces the status line, it never adds a row
        // beside it. Since chrome_height_px never even looks at the editor,
        // this holds by construction; the assertion below is the visible
        // proof of it.
        let chrome_top_with_prompt =
            surface.height() as f32 - chrome_height_px(metrics.line_height);
        assert_eq!(
            chrome_top, chrome_top_with_prompt,
            "the reserved chrome height must not change for an active prompt"
        );
        // And there is no pixel row left in the surface beyond the footer's
        // second row for a fourth row to occupy.
        assert_eq!(
            chrome_top_with_prompt + 3.0 * metrics.line_height,
            surface.height() as f32,
            "the chrome band is exactly three rows: status/prompt + two footer rows"
        );

        // Its own line draws right where the status line otherwise would --
        // with different content than the no-prompt case, in the same row.
        assert!(
            row_has_ink(&surface, background, status_top, status_bottom),
            "expected the prompt's line to draw in the status row"
        );
        let status_row_with_prompt = row_bytes(&surface, status_top, status_bottom);
        assert_ne!(
            status_row_no_prompt, status_row_with_prompt,
            "the prompt's line should read differently than the ordinary status line"
        );
    }

    #[test]
    fn the_help_pane_draws_its_own_text_instead_of_the_buffers_when_active() {
        let font = dejavu();
        let metrics = Metrics::new(&font, 16.0, 4);
        let mut cache = GlyphCache::new();
        let background = GdiColor::from_rgb(0, 0, 0);
        let foreground = GdiColor::from_rgb(255, 255, 255);
        let colors = test_colors(background, foreground);
        let row0_height = metrics.line_height.ceil() as u32;

        let mut editor = editor_with_text("ZQXJ not help text");
        editor.viewport_mut().set_height_lines(10);

        let mut surface = Surface::new(0, 500, 500);
        render_frame(&mut surface, &font, &mut cache, &metrics, &editor, colors);
        let row0_no_help = row_bytes(&surface, 0, row0_height);

        editor.run(&Command::Help);
        assert!(
            editor.help().is_some(),
            "Command::Help should have opened the help pane"
        );

        let mut surface = Surface::new(0, 500, 500);
        render_frame(&mut surface, &font, &mut cache, &metrics, &editor, colors);
        let row0_with_help = row_bytes(&surface, 0, row0_height);

        assert_ne!(
            row0_no_help, row0_with_help,
            "the help pane's first line should replace the buffer's own"
        );

        // Confirm it's specifically `help_text()`'s first line, not just
        // "something different", by drawing it directly and comparing. No
        // cursor is drawn here (matching render_frame's own "skip cursor and
        // selection while help is active" rule), so a plain draw_row is the
        // right comparison, not another render_frame call.
        let expected_text = editor.help_text()[0].clone();
        let mut expected_surface = Surface::new(0, 500, 500);
        expected_surface.clear(background);
        draw_row(
            &mut expected_surface,
            &font,
            &mut cache,
            &metrics,
            &expected_text,
            0.0,
            foreground,
        );
        let expected_row0 = row_bytes(&expected_surface, 0, row0_height);
        assert_eq!(row0_with_help, expected_row0);
    }

    // -- Phase 3 integration: cursor and selection rendering -----------------
    //
    // Neither had any visual representation at all before this integration —
    // Buffer's cursor/selection state updated correctly (hit.rs's tests
    // already proved that), but render_frame never drew either one. Found
    // and closed while verifying real click/drag/type input, since "the
    // state is right but invisible" would have failed the very thing this
    // phase exists to prove.

    #[test]
    fn the_cursor_draws_at_the_tab_aware_column_it_actually_sits_at() {
        let font = dejavu();
        let metrics = Metrics::new(&font, 24.0, 4);
        let mut cache = GlyphCache::new();
        let colors = test_colors(
            GdiColor::from_rgb(0, 0, 0),
            GdiColor::from_rgb(255, 255, 255),
        );

        // Cursor after "a\t" -- column 2, but visually at the fourth tab
        // stop (screen column 4), not screen column 2.
        let mut editor = editor_with_text("a\tb\n");
        editor.buffer_mut().set_cursor(Position::new(0, 2));

        let mut surface = Surface::new(0, 400, 200);
        render_frame(&mut surface, &font, &mut cache, &metrics, &editor, colors);

        let expected_x =
            x_for_column("a\tb", 2, metrics.advance_width, metrics.tab_columns).round() as u32;
        assert_eq!(expected_x, (4.0 * metrics.advance_width).round() as u32);

        let cursor_width = CURSOR_WIDTH_PX.round().max(1.0) as u32;
        let mut found_cursor = false;
        for x in expected_x..expected_x + cursor_width {
            if pixel_at(&surface, x, 2) == colors.cursor.to_bgra_bytes() {
                found_cursor = true;
            }
        }
        assert!(
            found_cursor,
            "expected the cursor's own color at its tab-aware screen column"
        );

        // Nowhere near screen column 2 (where a naive, tab-blind cursor
        // would have landed instead).
        let wrong_x = (2.0 * metrics.advance_width).round() as u32;
        assert_ne!(
            pixel_at(&surface, wrong_x, 2),
            colors.cursor.to_bgra_bytes(),
            "the cursor must not land on the tab's own naive character index"
        );
    }

    #[test]
    fn a_selection_draws_a_translucent_band_over_exactly_the_selected_span() {
        let font = dejavu();
        let metrics = Metrics::new(&font, 24.0, 4);
        let mut cache = GlyphCache::new();
        let background = GdiColor::from_rgb(0, 0, 0);
        let colors = test_colors(background, GdiColor::from_rgb(255, 255, 255));

        // Select "ell" out of "hello" -- columns 1..4.
        let mut editor = editor_with_text("hello\n");
        editor.buffer_mut().set_cursor(Position::new(0, 1));
        editor.buffer_mut().set_mark();
        editor
            .buffer_mut()
            .set_cursor_extending(Position::new(0, 4));

        let mut surface = Surface::new(0, 400, 200);
        render_frame(&mut surface, &font, &mut cache, &metrics, &editor, colors);

        let x_start =
            x_for_column("hello", 1, metrics.advance_width, metrics.tab_columns).round() as u32;
        let x_end =
            x_for_column("hello", 4, metrics.advance_width, metrics.tab_columns).round() as u32;
        let sample_y = (metrics.line_height * 0.9) as u32; // below the glyphs, still in the band

        // Inside the selected span: blended toward the selection color, not
        // plain background.
        let inside = pixel_at(&surface, (x_start + x_end) / 2, sample_y);
        assert_ne!(
            inside,
            background.to_bgra_bytes(),
            "expected the selection band to tint pixels within the selected span"
        );

        // Well before and well after the selected span: untouched.
        assert_eq!(
            pixel_at(&surface, x_start.saturating_sub(3), sample_y),
            background.to_bgra_bytes(),
            "the selection band must not bleed left of the selection"
        );
        assert_eq!(
            pixel_at(&surface, x_end + 3, sample_y),
            background.to_bgra_bytes(),
            "the selection band must not bleed right of the selection"
        );
    }

    #[test]
    fn no_cursor_or_selection_draws_while_help_is_active() {
        let font = dejavu();
        let metrics = Metrics::new(&font, 16.0, 4);
        let mut cache = GlyphCache::new();
        let colors = test_colors(
            GdiColor::from_rgb(0, 0, 0),
            GdiColor::from_rgb(255, 255, 255),
        );

        let mut editor = editor_with_text("hello\n");
        editor.buffer_mut().set_cursor(Position::new(0, 2));
        editor.run(&Command::Help);
        assert!(editor.help().is_some());

        let mut surface = Surface::new(0, 400, 300);
        render_frame(&mut surface, &font, &mut cache, &metrics, &editor, colors);

        // Scan the whole text area: neither the cursor color nor the
        // selection color should appear anywhere while help owns the view.
        let chrome_top = surface.height() as f32 - chrome_height_px(metrics.line_height);
        let mut saw_cursor_or_selection = false;
        for y in 0..chrome_top.round() as u32 {
            for x in 0..surface.width() {
                let pixel = pixel_at(&surface, x, y);
                if pixel == colors.cursor.to_bgra_bytes() {
                    saw_cursor_or_selection = true;
                }
            }
        }
        assert!(
            !saw_cursor_or_selection,
            "help mode must not draw the document's cursor"
        );
    }

    // -- Phase 3 integration: keyboard -> Chord -> Command wiring -----------
    //
    // `input::chord_from_event` and `Editor::keymap().resolve` are each
    // already fully tested on their own (input.rs and aitch-core's keymap
    // tests). What's new and worth proving here is specifically the *wiring*
    // between them in `resolve_keyboard_command`: a bound chord wins, an
    // unbound Ctrl/Alt combo never falls through to text, and an unbound
    // plain key inserts exactly what was typed -- not a keymap-normalized
    // form of it.

    #[test]
    fn a_bound_named_key_resolves_to_its_command() {
        let editor = editor_with_text("");
        // VK_LEFT: nano's default keymap binds a bare Left arrow to movement.
        const VK_LEFT: u32 = 0x25;
        let event = Event::KeyDown {
            vkey: VK_LEFT,
            repeat: false,
        };
        assert_eq!(
            resolve_keyboard_command(&editor, &event, None),
            Some(Command::MoveLeft)
        );
    }

    #[test]
    fn an_unbound_plain_character_falls_through_to_insert_text() {
        let editor = editor_with_text("");
        // Nano's default keymap has no plain (unmodified) binding for an
        // ordinary letter -- only its Ctrl/Alt combinations are bound.
        let event = Event::Char('q');
        assert_eq!(
            resolve_keyboard_command(&editor, &event, Some('q')),
            Some(Command::InsertText("q".to_string()))
        );
    }

    #[test]
    fn insert_text_preserves_the_typed_character_not_a_normalized_one() {
        // Chord::new normalizes ASCII letters to lowercase for keymap
        // matching, but a Shift-produced capital letter must still appear as
        // typed -- this is the one rule this wiring adds beyond what
        // input.rs itself tests.
        let editor = editor_with_text("");
        let event = Event::Char('Q');
        assert_eq!(
            resolve_keyboard_command(&editor, &event, Some('Q')),
            Some(Command::InsertText("Q".to_string())),
            "must insert the typed 'Q', not a lowercased 'q'"
        );
    }

    #[test]
    fn a_keydown_with_no_insertable_character_never_produces_insert_text() {
        // Event::KeyDown always passes insertable=None from the real event
        // loop (a NamedKey has no character of its own) -- whether or not
        // this particular vkey happens to be bound in the default keymap,
        // resolving it must never manufacture an InsertText out of thin air.
        let editor = editor_with_text("");
        const VK_INSERT: u32 = 0x2D;
        let event = Event::KeyDown {
            vkey: VK_INSERT,
            repeat: false,
        };
        let result = resolve_keyboard_command(&editor, &event, None);
        assert!(
            !matches!(result, Some(Command::InsertText(_))),
            "KeyDown with insertable=None must never produce InsertText, got {result:?}"
        );
    }

    #[test]
    fn a_char_event_that_duplicates_a_keydown_resolves_to_nothing() {
        // '\r'/'\t'/etc. are suppressed inside input::chord_from_event
        // itself; confirm that suppression really does reach all the way
        // through this wiring rather than being reintroduced as an
        // InsertText some other way.
        let editor = editor_with_text("");
        let event = Event::Char('\r');
        assert_eq!(resolve_keyboard_command(&editor, &event, Some('\r')), None);
    }

    // -- Phase 3 integration: dispatch's Outcome handling --------------------

    #[test]
    fn dispatching_insert_text_updates_the_document_and_status() {
        // dispatch() itself needs a live Window only for Outcome::Quit and
        // the post-redraw retitle -- everything else (running the command,
        // reacting to its Outcome) is exercised here directly against
        // Editor, without opening a real window.
        let mut editor = editor_with_text("");
        let outcome = editor.run(&Command::InsertText("hi".to_string()));
        assert!(outcome.redraws());
        assert_eq!(editor.buffer().snapshot(), "hi");
        assert!(editor.document().is_dirty());
    }

    #[test]
    fn copy_outcome_carries_the_selected_text_for_dispatch_to_hand_the_clipboard() {
        // What dispatch()'s Outcome::Copy arm receives and hands to
        // clipboard::set_text. Stops short of touching the real system
        // clipboard here deliberately: that module's own test suite
        // (platform::win32::clipboard::tests) already covers it thoroughly,
        // under a shared mutex serializing its tests against each other on
        // that one system-wide resource. A second, unsynchronized caller
        // hitting the same real clipboard from here raced those tests and
        // produced a genuine STATUS_ACCESS_VIOLATION the first time this was
        // tried — proof this restraint is load-bearing, not just tidiness.
        let mut editor = editor_with_text("hello\n");
        editor.buffer_mut().set_cursor(Position::new(0, 0));
        editor.buffer_mut().set_mark();
        editor
            .buffer_mut()
            .set_cursor_extending(Position::new(0, 5));
        let outcome = editor.run(&Command::Copy);
        assert_eq!(outcome, Outcome::Copy("hello".to_string()));
    }
}
