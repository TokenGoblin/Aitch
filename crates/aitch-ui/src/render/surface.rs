//! The wgpu device, queue and swapchain for one window, and the frame it draws.

use std::fmt;
use std::sync::Arc;

use aitch_core::{Editor, Position};
use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::render::atlas::Atlas;
use crate::render::quads::{Instances, QuadPipeline};
use crate::render::screen;
use crate::render::text::TextRenderer;
use crate::theme::Theme;

/// Side of the square glyph atlas in texels. A full screen of monospace text
/// needs a few hundred distinct glyphs; this holds thousands.
const ATLAS_SIZE: u32 = 1024;

/// Default font size in logical pixels. `aitchrc` overrides it in Phase 7.
const FONT_SIZE: f32 = 14.0;

/// Everything needed to put pixels in a window.
pub struct Surface {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// Physical pixels per logical pixel.
    scale_factor: f64,
    atlas: Atlas,
    pipeline: QuadPipeline,
    text: TextRenderer,
    instances: Instances,
}

impl Surface {
    /// Create a surface for `window` with the default font.
    pub fn new(window: Arc<Window>) -> Result<Surface, SurfaceError> {
        Surface::with_font(window, FONT_SIZE, None)
    }

    /// Create a surface, choosing the font from the config.
    pub fn with_font(
        window: Arc<Window>,
        font_size: f32,
        font_family: Option<String>,
    ) -> Result<Surface, SurfaceError> {
        let size = window.inner_size();
        let scale_factor = window.scale_factor();

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance
            .create_surface(window)
            .map_err(|e| SurfaceError(format!("could not create a surface: {e}")))?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
        }))
        .map_err(|e| SurfaceError(format!("no usable GPU adapter: {e}")))?;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("aitch device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        }))
        .map_err(|e| SurfaceError(format!("could not open a GPU device: {e}")))?;

        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(capabilities.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            // Fifo is the only mode guaranteed everywhere, and it is what an
            // editor wants: no tearing, no spinning to produce frames nobody
            // asked for. See the idle-CPU budget in PLAN.md §6.
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: capabilities.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let atlas = Atlas::new(&device, &queue, ATLAS_SIZE);
        let pipeline = QuadPipeline::new(&device, format, atlas.bind_group_layout());
        let text = TextRenderer::with_family(font_size, scale_factor as f32, font_family);

        Ok(Surface {
            surface,
            device,
            queue,
            config,
            scale_factor,
            atlas,
            pipeline,
            text,
            instances: Instances::default(),
        })
    }

    /// Physical size of the drawable area.
    pub fn size(&self) -> PhysicalSize<u32> {
        PhysicalSize::new(self.config.width, self.config.height)
    }

    pub fn scale_factor(&self) -> f64 {
        self.scale_factor
    }

    /// Physical height of one line of text.
    pub fn line_height(&self) -> f32 {
        self.text.line_height()
    }

    /// How many lines of the document are visible.
    ///
    /// The bottom three rows are not the document's: one status or prompt
    /// line, then the two footer rows. That is the nano screen, and the text
    /// area is what is left over.
    pub fn visible_lines(&self) -> usize {
        screen::text_rows(&self.text, self.config.height as f32)
    }

    /// How many character cells fit across the window, for footer layout.
    pub fn columns(&self) -> usize {
        screen::columns(&self.text, self.config.width as f32)
    }

    /// React to a DPI change: re-derive the metrics and drop every cached
    /// glyph, since all of them were rasterized for the old scale.
    pub fn set_scale_factor(&mut self, scale_factor: f64) {
        self.scale_factor = scale_factor;
        if self.text.set_scale_factor(scale_factor as f32) {
            self.atlas.reset(&self.queue);
        }
    }

    /// Reconfigure after a resize. A zero-sized window is skipped rather than
    /// configured, which some backends treat as an error.
    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
    }

    /// Map a click in physical pixels to a position in the buffer.
    pub fn hit(&self, x: f32, y: f32, sub_line_offset: f32) -> Option<Position> {
        self.text.hit(x, y, -sub_line_offset)
    }

    /// Draw one frame: the text area, then the chrome under it.
    ///
    /// `sub_line_offset` is how far into the top visible line the window has
    /// scrolled, in physical pixels — the pair with the viewport's first line
    /// is what makes touchpad scrolling smooth rather than a line at a time.
    pub fn render(
        &mut self,
        editor: &Editor,
        sub_line_offset: f32,
        generation: u64,
        theme: &Theme,
    ) -> Result<(), SurfaceError> {
        let size = (self.config.width as f32, self.config.height as f32);

        self.instances.clear();
        screen::draw(
            &self.queue,
            &mut self.atlas,
            &mut self.text,
            &mut self.instances,
            editor,
            theme,
            screen::Layout {
                size,
                scale_factor: self.scale_factor as f32,
                sub_line_offset,
                generation,
            },
        );

        self.pipeline
            .upload(&self.device, &self.queue, [size.0, size.1], &self.instances);
        self.present(theme)
    }

    fn present(&mut self, theme: &Theme) -> Result<(), SurfaceError> {
        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            // The swapchain went stale (a resize or a display change raced us).
            // Reconfigure and let the next redraw request pick it up.
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            Err(e) => return Err(SurfaceError(format!("could not acquire a frame: {e}"))),
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aitch frame"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aitch text"),
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

            self.pipeline
                .draw(&mut pass, self.atlas.bind_group(), &self.instances);
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }
}

/// Something went wrong talking to the GPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceError(String);

impl fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SurfaceError {}
