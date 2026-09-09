//! Render one frame offscreen and dump it as raw RGBA, for looking at.
//!
//! PLAN.md §7 leaves visual checks as the only manual step in testing. This
//! makes that step repeatable and headless: no window, no display, no GPU
//! driver quirks about screenshots. It draws through exactly the same
//! `render::screen` code the real window uses, so what comes out is what the
//! editor would show.
//!
//! ```text
//! cargo run -p aitch-ui --example dump_frame -- src/main.rs frame.raw [chords]
//! ```
//!
//! `chords` is an optional whitespace-separated sequence fed to the editor
//! first, so a prompt or the help pane can be captured: `"^W"` opens a search.
//!
//! The output is `u32` width, `u32` height, then `width * height` RGBA pixels.
//! Width must be a multiple of 64 to satisfy the GPU copy alignment.

use std::io::Write;

use aitch_core::{Buffer, Chord, Command, Document, Editor, Workspace};
use aitch_ui::render::atlas::Atlas;
use aitch_ui::render::quads::{Instances, QuadPipeline};
use aitch_ui::render::screen::{self, Layout};
use aitch_ui::render::text::TextRenderer;
use aitch_ui::theme::Theme;

const W: u32 = 896;
const H: u32 = 384;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(out)) = (args.next(), args.next()) else {
        eprintln!("usage: dump_frame <text file> <output.raw> [chords]");
        std::process::exit(2);
    };
    let chords = args.next().unwrap_or_default();

    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("a GPU adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: None,
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::downlevel_defaults(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    }))
    .expect("a GPU device");

    let theme = Theme::dark();
    let text_src = std::fs::read_to_string(&input).expect("could not read the input file");

    let mut document = Document::blank();
    document.buffer = Buffer::from_str(&text_src);
    document.set_path(std::path::PathBuf::from(&input));

    // Root the workspace at the file's folder, so M-T and ^T have something
    // to show.
    let mut workspace = Workspace::new(document);
    if let Some(parent) = std::path::Path::new(&input)
        .canonicalize()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
    {
        workspace.set_root(parent);
    }
    let mut editor = Editor::with_workspace(workspace);

    let mut atlas = Atlas::new(&device, &queue, 1024);
    let mut pipeline = QuadPipeline::new(&device, FORMAT, atlas.bind_group_layout());
    let mut text = TextRenderer::new(14.0, 1.0);
    let mut instances = Instances::default();

    let rows = screen::text_rows(&text, H as f32);
    editor.viewport_mut().set_height_lines(rows);

    for chord in chords.split_whitespace() {
        let parsed = Chord::parse(chord).expect("a valid chord");
        let context = editor.context();
        match editor.keymap().resolve(context, parsed).cloned() {
            Some(command) => {
                editor.run(&command);
            }
            None => {
                // Unbound: treat it as typed text, as the real UI does.
                editor.run(&Command::InsertText(chord.to_string()));
            }
        }
    }

    screen::draw(
        &queue,
        &mut atlas,
        &mut text,
        &mut instances,
        &editor,
        &theme,
        Layout {
            size: (W as f32, H as f32),
            scale_factor: 1.0,
            sub_line_offset: 0.0,
            generation: 0,
        },
    );
    eprintln!("instances: {}", instances.count());
    pipeline.upload(&device, &queue, [W as f32, H as f32], &instances);

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let bpr = W * 4;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (bpr * H) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut enc = device.create_command_encoder(&Default::default());
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
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
    enc.copy_texture_to_buffer(
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
                bytes_per_row: Some(bpr),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(enc.finish()));

    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::Wait).unwrap();
    let pixels = slice.get_mapped_range().to_vec();

    let mut f = std::fs::File::create(&out).expect("could not create the output file");
    f.write_all(&W.to_le_bytes()).unwrap();
    f.write_all(&H.to_le_bytes()).unwrap();
    f.write_all(&pixels).unwrap();
    eprintln!("wrote {out}");
}
