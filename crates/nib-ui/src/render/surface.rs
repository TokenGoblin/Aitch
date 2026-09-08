//! The wgpu device, queue and swapchain for one window.

use std::fmt;
use std::sync::Arc;

use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::theme::Color;

/// Everything needed to put pixels in a window.
pub struct Surface {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// Physical pixels per logical pixel. Phase 1's glyph cache keys on this.
    scale_factor: f64,
}

impl Surface {
    /// Create a surface for `window` and configure it at its current size.
    pub fn new(window: Arc<Window>) -> Result<Surface, SurfaceError> {
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
            label: Some("nib device"),
            required_features: wgpu::Features::empty(),
            // Phase 1 raises these only if cosmic-text's atlas needs it.
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

        Ok(Surface {
            surface,
            device,
            queue,
            config,
            scale_factor,
        })
    }

    /// Physical size of the drawable area.
    pub fn size(&self) -> PhysicalSize<u32> {
        PhysicalSize::new(self.config.width, self.config.height)
    }

    pub fn scale_factor(&self) -> f64 {
        self.scale_factor
    }

    pub fn set_scale_factor(&mut self, scale_factor: f64) {
        self.scale_factor = scale_factor;
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

    /// Clear the window to `color`.
    ///
    /// Phase 1 turns this into a real render pass with text in it.
    pub fn render(&mut self, color: Color) -> Result<(), SurfaceError> {
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
                label: Some("nib frame"),
            });

        // The pass exists only for its clear value in Phase 0.
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("nib clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(color.into()),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

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
