//! Render text to an offscreen texture and read the pixels back.
//!
//! Manual inspection is supposed to be the *only* thing left for rendering
//! (PLAN.md §7), and it nearly is — but "did any glyph actually reach the
//! screen" is checkable without a screen, and it is the one rendering bug that
//! would otherwise reach a human every time.
//!
//! Needs a GPU adapter. Where there is none, the test skips — but a skipped
//! test that reports "ok" is indistinguishable from a passing one, and libtest
//! swallows the message. So CI sets `AITCH_REQUIRE_GPU=1`, which turns a
//! missing adapter into a failure. Without that, coverage can quietly vanish
//! from a platform and nobody finds out.

use aitch_core::{Buffer, Position};
use aitch_ui::render::atlas::Atlas;
use aitch_ui::render::quads::{Instances, QuadPipeline};
use aitch_ui::render::text::TextRenderer;
use aitch_ui::theme::Theme;

const WIDTH: u32 = 512;
const HEIGHT: u32 = 256;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

/// Why there is no GPU to test against.
///
/// The two cases want different advice, so they are kept apart: telling
/// someone to install a driver that is already installed and enumerating
/// sends them a long way in the wrong direction.
enum NoGpu {
    NoAdapter(String),
    NoDevice(String),
}

impl std::fmt::Display for NoGpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NoGpu::NoAdapter(why) => write!(
                f,
                "no GPU adapter: {why}. On Linux this usually means the loader \
                 found no Vulkan ICD — install mesa-vulkan-drivers for lavapipe, \
                 and do not set VK_ICD_FILENAMES unless the path really exists, \
                 because it replaces the driver list rather than adding to it"
            ),
            NoGpu::NoDevice(why) => write!(
                f,
                "an adapter was found but no device could be opened: {why}. The \
                 driver is present and enumerating; it did not meet \
                 Limits::downlevel_defaults() or could not allocate"
            ),
        }
    }
}

/// Whether a missing GPU should fail rather than skip.
///
/// The value is read, not merely its presence: `AITCH_REQUIRE_GPU=0` is what
/// someone reproducing a CI failure locally will reach for to get the skip
/// behaviour back, and it should work.
fn gpu_is_required() -> bool {
    required_from(std::env::var("AITCH_REQUIRE_GPU").ok().as_deref())
}

/// The decision, separated from where the value comes from, so it can be
/// tested without writing to the process environment — these tests run in
/// parallel, and a test that unset the variable could make a rendering test
/// skip silently, which is the failure this whole guard exists to prevent.
fn required_from(value: Option<&str>) -> bool {
    match value {
        Some(value) => !matches!(value.trim(), "" | "0" | "false" | "no"),
        None => false,
    }
}

/// `None` when the machine has no usable GPU, after saying why.
///
/// Panics instead when `AITCH_REQUIRE_GPU` is set, so CI cannot pass by
/// skipping every rendering test.
fn gpu() -> Option<Gpu> {
    match adapter_and_device() {
        Ok(gpu) => Some(gpu),
        Err(why) if gpu_is_required() => {
            panic!("{why}, and AITCH_REQUIRE_GPU is set")
        }
        Err(why) => {
            eprintln!("SKIPPED: {why}");
            None
        }
    }
}

fn adapter_and_device() -> Result<Gpu, NoGpu> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .map_err(|e| NoGpu::NoAdapter(e.to_string()))?;

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("aitch test device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::downlevel_defaults(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    }))
    .map_err(|e| NoGpu::NoDevice(e.to_string()))?;

    Ok(Gpu { device, queue })
}

/// Draw `buffer` offscreen and return the frame as RGBA rows.
fn draw(gpu: &Gpu, buffer: &Buffer, theme: &Theme) -> Vec<u8> {
    draw_with_tab_width(gpu, buffer, theme, 4)
}

/// The same, drawing a tab as `tab_width` columns wide.
fn draw_with_tab_width(gpu: &Gpu, buffer: &Buffer, theme: &Theme, tab_width: usize) -> Vec<u8> {
    let mut atlas = Atlas::new(&gpu.device, &gpu.queue, 1024);
    let mut pipeline = QuadPipeline::new(&gpu.device, FORMAT, atlas.bind_group_layout());
    let mut text = TextRenderer::with_family(14.0, 1.0, None, tab_width);
    let mut instances = Instances::default();

    text.prepare(buffer, 0, (WIDTH as f32, HEIGHT as f32), 0);
    text.push_instances(&gpu.queue, &mut atlas, &mut instances, buffer, 0.0, theme);
    assert!(
        !instances.is_empty(),
        "nothing was queued to draw, so nothing could appear"
    );
    render_instances(gpu, &mut pipeline, &atlas, &instances, theme)
}

/// Draw prepared instances to an offscreen texture and read the pixels back.
fn render_instances(
    gpu: &Gpu,
    pipeline: &mut QuadPipeline,
    atlas: &Atlas,
    instances: &Instances,
    theme: &Theme,
) -> Vec<u8> {
    // Easy to forget when this was split out of `draw`, and the symptom is a
    // frame of pure background with no hint that anything was skipped.
    pipeline.upload(
        &gpu.device,
        &gpu.queue,
        [WIDTH as f32, HEIGHT as f32],
        instances,
    );

    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("aitch test target"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());

    // 512 * 4 is already a multiple of the 256-byte copy alignment.
    let bytes_per_row = WIDTH * 4;
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("aitch test readback"),
        size: (bytes_per_row * HEIGHT) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("aitch test pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(theme.background.into()),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pipeline.draw(&mut pass, atlas.bind_group(), instances);
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(Some(encoder.finish()));

    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    gpu.device.poll(wgpu::PollType::Wait).expect("poll");
    let pixels = slice.get_mapped_range().to_vec();
    readback.unmap();
    pixels
}

/// Pixels that are not the background color, and the rows they appear on.
fn lit_pixels(pixels: &[u8]) -> (usize, Vec<usize>) {
    let mut count = 0;
    let mut rows = Vec::new();
    for y in 0..HEIGHT as usize {
        let mut row_has_ink = false;
        for x in 0..WIDTH as usize {
            let i = (y * WIDTH as usize + x) * 4;
            // The dark background is under 0x20 on every channel; anything
            // brighter than that came from a glyph or the cursor.
            if pixels[i] > 0x40 || pixels[i + 1] > 0x40 || pixels[i + 2] > 0x40 {
                count += 1;
                row_has_ink = true;
            }
        }
        if row_has_ink {
            rows.push(y);
        }
    }
    (count, rows)
}

#[test]
fn text_actually_reaches_the_framebuffer() {
    let Some(gpu) = gpu() else { return };

    let theme = Theme::dark();
    let buffer = Buffer::from_str("hello world\nsecond line\nthird line\n");
    let pixels = draw(&gpu, &buffer, &theme);

    let (count, rows) = lit_pixels(&pixels);
    assert!(
        count > 200,
        "only {count} lit pixels — the text did not render"
    );

    // Three lines of text plus a cursor should occupy three separate bands,
    // not one smear: check there are gaps between the rows that have ink.
    let bands = rows.windows(2).filter(|pair| pair[1] - pair[0] > 1).count() + 1;
    assert!(
        bands >= 3,
        "expected at least 3 bands of text, found {bands}"
    );
}

#[test]
fn an_empty_buffer_draws_only_the_cursor() {
    let Some(gpu) = gpu() else { return };

    let theme = Theme::dark();
    let buffer = Buffer::new();
    let pixels = draw(&gpu, &buffer, &theme);

    let (count, rows) = lit_pixels(&pixels);
    assert!(count > 0, "the cursor should be visible in an empty buffer");
    assert!(
        count < 200,
        "{count} lit pixels is too many for a bare cursor"
    );
    assert!(
        rows.first().copied().unwrap_or(usize::MAX) < 40,
        "the cursor should be on the first line"
    );
}

#[test]
fn the_cursor_moves_with_the_buffer() {
    let Some(gpu) = gpu() else { return };

    let theme = Theme::dark();
    let mut buffer = Buffer::from_str("\n\n\n\n");

    let top = lit_pixels(&draw(&gpu, &buffer, &theme)).1;
    buffer.set_cursor(Position::new(3, 0));
    let moved = lit_pixels(&draw(&gpu, &buffer, &theme)).1;

    assert!(
        moved.first() > top.first(),
        "the cursor did not move down: {top:?} then {moved:?}"
    );
}

#[test]
fn a_selection_is_drawn_behind_the_text() {
    let Some(gpu) = gpu() else { return };

    let theme = Theme::dark();
    let text = "hello world\nsecond line\n";

    let plain = draw(&gpu, &Buffer::from_str(text), &theme);
    let (plain_lit, _) = lit_pixels(&plain);

    let mut selected = Buffer::from_str(text);
    selected.select_all();
    let highlighted = draw(&gpu, &selected, &theme);
    let (selected_lit, rows) = lit_pixels(&highlighted);

    // The selection is a translucent band behind the glyphs, so a great many
    // more pixels are brighter than the background than before.
    assert!(
        selected_lit > plain_lit * 3,
        "selection barely showed: {plain_lit} lit pixels became {selected_lit}"
    );

    // And it covers whole lines rather than only where glyphs are.
    let first = *rows.first().expect("something was drawn");
    let last = *rows.last().expect("something was drawn");
    assert!(
        last - first > 20,
        "the highlight should span both lines, not one band"
    );
}

/// Leftmost and rightmost lit column on a given row, if any.
fn lit_span(pixels: &[u8], y: usize) -> Option<(usize, usize)> {
    let mut first = None;
    let mut last = None;
    for x in 0..WIDTH as usize {
        let i = (y * WIDTH as usize + x) * 4;
        if pixels[i] > 0x40 || pixels[i + 1] > 0x40 || pixels[i + 2] > 0x40 {
            first.get_or_insert(x);
            last = Some(x);
        }
    }
    Some((first?, last?))
}

#[test]
fn the_first_selected_line_highlights_from_its_start() {
    let Some(gpu) = gpu() else { return };

    let theme = Theme::dark();
    let mut buffer = Buffer::from_str("aaaaaaaa\nbbbbbbbb\ncccccccc\n");
    // Select from the very start, across into the third line.
    buffer.set_mark();
    for _ in 0..20 {
        buffer.move_right();
    }
    let pixels = draw(&gpu, &buffer, &theme);

    let (_, rows) = lit_pixels(&pixels);
    let top = *rows.first().expect("something was drawn");

    // The first row of the first line must be lit from near the left margin,
    // not just where a line-break sliver would sit at the far right.
    let (first, last) = lit_span(&pixels, top + 2).expect("the first line has ink");
    assert!(
        first < 8,
        "the first selected line starts at x={first}, so its highlight is missing"
    );
    assert!(last > 40, "the highlight should cover the whole line");
}

#[test]
fn the_no_gpu_messages_read_as_sentences() {
    // A `\` continuation in a Rust string strips the newline and the leading
    // whitespace after it; forgetting one bakes the source indentation into
    // the message. This caught exactly that, in the one message a maintainer
    // sees when CI hard-fails.
    for message in [
        NoGpu::NoAdapter("adapter request failed".to_string()),
        NoGpu::NoDevice("device request failed".to_string()),
    ] {
        let text = message.to_string();
        assert!(
            !text.contains("  "),
            "run of spaces in a diagnostic message: {text:?}"
        );
        assert!(!text.contains('\n'), "message should be one line: {text:?}");
        // And it must carry the underlying cause, not just advice.
        assert!(text.contains("failed"), "cause was dropped: {text:?}");
    }
}

#[test]
fn the_gpu_requirement_reads_its_value() {
    // Someone reproducing a CI failure locally reaches for =0 to get the skip
    // back. Checking presence alone would ignore them.
    for (value, expected) in [
        (None, false),
        (Some("1"), true),
        (Some("true"), true),
        (Some("yes"), true),
        (Some("0"), false),
        (Some("false"), false),
        (Some("no"), false),
        (Some(""), false),
        (Some("  "), false),
    ] {
        assert_eq!(required_from(value), expected, "for {value:?}");
    }
}

#[test]
fn tab_width_changes_how_wide_a_tab_is_drawn() {
    // `tab_width` was only ever read to decide what the Tab key inserts, so a
    // file that already contains tabs ignored the setting entirely and drew
    // them at the shaper's default of eight.
    let Some(gpu) = gpu() else { return };
    let theme = Theme::dark();
    let buffer = Buffer::from_str("\tX\n");

    let narrow = draw_with_tab_width(&gpu, &buffer, &theme, 2);
    let wide = draw_with_tab_width(&gpu, &buffer, &theme, 8);

    // The rightmost ink, not the leftmost: a cursor block sits at column 0 in
    // both frames and would make the two look identical.
    let rightmost = |pixels: &[u8]| {
        (0..HEIGHT as usize)
            .filter_map(|y| lit_span(pixels, y))
            .map(|(_, last)| last)
            .max()
            .expect("the X has to land somewhere")
    };

    let narrow_x = rightmost(&narrow);
    let wide_x = rightmost(&wide);
    assert!(
        wide_x > narrow_x,
        "an eight-column tab must push the X further right than a two-column \
         one: {wide_x} vs {narrow_x}"
    );
}

// -- the chrome: status line, prompt line, footer ---------------------------

/// Draw a whole editor screen offscreen, through the same code the window uses.
fn draw_screen(gpu: &Gpu, editor: &aitch_core::Editor, theme: &Theme) -> Vec<u8> {
    use aitch_ui::render::screen::{self, Layout};

    let mut atlas = Atlas::new(&gpu.device, &gpu.queue, 1024);
    let mut pipeline = QuadPipeline::new(&gpu.device, FORMAT, atlas.bind_group_layout());
    let mut text = TextRenderer::new(14.0, 1.0);
    let mut instances = Instances::default();

    screen::draw(
        &gpu.queue,
        &mut atlas,
        &mut text,
        &mut instances,
        editor,
        theme,
        Layout {
            size: (WIDTH as f32, HEIGHT as f32),
            scale_factor: 1.0,
            sub_line_offset: 0.0,
            generation: 0,
        },
    );
    assert!(!instances.is_empty(), "nothing was queued to draw");

    render_instances(gpu, &mut pipeline, &atlas, &instances, theme)
}

/// The bottom three rows: one status or prompt line, then the two footer rows.
fn chrome_rows(gpu: &Gpu) -> std::ops::Range<usize> {
    let text = TextRenderer::new(14.0, 1.0);
    let line_height = text.line_height();
    let _ = gpu;
    let rows = aitch_ui::render::screen::text_rows(&text, HEIGHT as f32);
    let top = (rows as f32 * line_height) as usize;
    top..HEIGHT as usize
}

#[test]
fn the_footer_reaches_the_framebuffer() {
    let Some(gpu) = gpu() else { return };
    let theme = Theme::dark();

    let mut document = aitch_core::Document::blank();
    document.buffer = Buffer::from_str("some text\n");
    let editor = aitch_core::Editor::new(document);

    let pixels = draw_screen(&gpu, &editor, &theme);
    let chrome = chrome_rows(&gpu);

    // The footer is drawn on a bar with reversed chords, so the bottom rows
    // carry a great deal more ink than the mostly empty text area above them.
    let lit_in_chrome: usize = chrome.clone().filter_map(|y| lit_span(&pixels, y)).count();
    assert!(
        lit_in_chrome >= 3,
        "the bottom of the window has no footer on it"
    );

    // And it spans the width rather than hugging the left margin.
    let widest = chrome
        .filter_map(|y| lit_span(&pixels, y))
        .map(|(_, last)| last)
        .max()
        .unwrap_or(0);
    assert!(
        widest > WIDTH as usize / 2,
        "the footer only reached x={widest}, so it is not laid out across"
    );
}

#[test]
fn a_prompt_takes_over_the_status_line_without_a_third_footer_row() {
    let Some(gpu) = gpu() else { return };
    let theme = Theme::dark();

    let mut document = aitch_core::Document::blank();
    document.buffer = Buffer::from_str("alpha beta gamma\n");
    let mut editor = aitch_core::Editor::new(document);
    editor.viewport_mut().set_height_lines(10);

    let before = draw_screen(&gpu, &editor, &theme);

    // Open a search and type into it.
    editor.run(&aitch_core::Command::WhereIs);
    for c in "beta".chars() {
        editor.run(&aitch_core::Command::InsertText(c.to_string()));
    }
    let after = draw_screen(&gpu, &editor, &theme);

    assert!(
        lit_pixels(&before).0 != lit_pixels(&after).0,
        "the prompt did not change what is on screen"
    );

    // The chrome is still exactly three rows: the prompt replaced the status
    // line rather than pushing the footer down or growing a third row.
    let chrome = chrome_rows(&gpu);
    let rows_with_ink = chrome.clone().filter_map(|y| lit_span(&after, y)).count();
    let before_rows = chrome.filter_map(|y| lit_span(&before, y)).count();
    assert!(
        rows_with_ink > 0 && before_rows > 0,
        "the chrome should have ink in both frames"
    );

    // A third footer row would put ink below the three-row chrome bar. The
    // window is taller than the bar, so those rows must stay empty.
    let text = TextRenderer::new(14.0, 1.0);
    let line_height = text.line_height();
    let rows = aitch_ui::render::screen::text_rows(&text, HEIGHT as f32);
    let below_chrome =
        ((rows as f32 + aitch_ui::render::screen::CHROME_ROWS as f32) * line_height) as usize;
    for y in below_chrome..HEIGHT as usize {
        assert!(
            lit_span(&after, y).is_none(),
            "ink at row {y}, below the three rows of chrome"
        );
    }
}
