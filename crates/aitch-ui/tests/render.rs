//! Render text to an offscreen texture and read the pixels back.
//!
//! Manual inspection is supposed to be the *only* thing left for rendering
//! (PLAN.md §7), and it nearly is — but "did any glyph actually reach the
//! screen" is checkable without a screen, and it is the one rendering bug that
//! would otherwise reach a human every time.
//!
//! Needs a GPU adapter. Where there is none — a bare CI runner — the test
//! reports that it did not run rather than passing quietly.

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

/// `None` when the machine has no usable GPU adapter at all.
fn gpu() -> Option<Gpu> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .ok()?;

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("aitch test device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::downlevel_defaults(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    }))
    .ok()?;

    Some(Gpu { device, queue })
}

/// Draw `buffer` offscreen and return the frame as RGBA rows.
fn draw(gpu: &Gpu, buffer: &Buffer, theme: &Theme) -> Vec<u8> {
    let mut atlas = Atlas::new(&gpu.device, &gpu.queue, 1024);
    let mut pipeline = QuadPipeline::new(&gpu.device, FORMAT, atlas.bind_group_layout());
    let mut text = TextRenderer::new(14.0, 1.0);
    let mut instances = Instances::default();

    text.prepare(buffer, 0, (WIDTH as f32, HEIGHT as f32), 0);
    text.push_instances(&gpu.queue, &mut atlas, &mut instances, buffer, 0.0, theme);
    assert!(
        !instances.is_empty(),
        "nothing was queued to draw, so nothing could appear"
    );
    pipeline.upload(
        &gpu.device,
        &gpu.queue,
        [WIDTH as f32, HEIGHT as f32],
        &instances,
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
        pipeline.draw(&mut pass, atlas.bind_group(), &instances);
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
    let Some(gpu) = gpu() else {
        eprintln!("SKIPPED: no GPU adapter on this machine");
        return;
    };

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
    let Some(gpu) = gpu() else {
        eprintln!("SKIPPED: no GPU adapter on this machine");
        return;
    };

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
    let Some(gpu) = gpu() else {
        eprintln!("SKIPPED: no GPU adapter on this machine");
        return;
    };

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
