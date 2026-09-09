//! The one pipeline aitch draws with: textured and solid quads.
//!
//! Instances are packed straight into a byte buffer rather than going through
//! a `#[repr(C)]` struct and a casting crate — the layout is written once, in
//! [`Instances::push`], and checked against the vertex attributes below.

use crate::render::atlas::Entry;
use crate::theme::Color;

/// How the fragment shader should treat an instance. Must match `quads.wgsl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A coverage mask tinted by the instance color.
    Mask = 0,
    /// A color glyph, drawn as rasterized.
    Color = 1,
    /// A solid rectangle.
    Solid = 2,
}

/// Bytes per instance: 12 floats and one u32.
const INSTANCE_SIZE: u64 = 13 * 4;

/// A frame's worth of quads, packed for the GPU.
#[derive(Debug, Default)]
pub struct Instances {
    bytes: Vec<u8>,
    count: u32,
}

impl Instances {
    pub fn clear(&mut self) {
        self.bytes.clear();
        self.count = 0;
    }

    pub fn count(&self) -> u32 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Add one quad at a physical-pixel position.
    pub fn push(&mut self, position: [f32; 2], entry: Entry, color: Color, kind: Kind) {
        let mut push_f32 = |v: f32| self.bytes.extend_from_slice(&v.to_le_bytes());

        push_f32(position[0]);
        push_f32(position[1]);
        push_f32(entry.size[0]);
        push_f32(entry.size[1]);
        push_f32(entry.uv_min[0]);
        push_f32(entry.uv_min[1]);
        push_f32(entry.uv_max[0]);
        push_f32(entry.uv_max[1]);
        push_f32(color.r as f32);
        push_f32(color.g as f32);
        push_f32(color.b as f32);
        push_f32(color.a as f32);
        self.bytes.extend_from_slice(&(kind as u32).to_le_bytes());

        self.count += 1;
    }

    /// Add a solid rectangle in physical pixels.
    pub fn push_rect(&mut self, position: [f32; 2], size: [f32; 2], white: Entry, color: Color) {
        let entry = Entry { size, ..white };
        self.push(position, entry, color, Kind::Solid);
    }
}

/// The render pipeline plus the buffers it reads from.
pub struct QuadPipeline {
    pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    globals_bind_group: wgpu::BindGroup,
    instances: wgpu::Buffer,
    instance_capacity: u64,
}

impl QuadPipeline {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        atlas_layout: &wgpu::BindGroupLayout,
    ) -> QuadPipeline {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aitch quads"),
            source: wgpu::ShaderSource::Wgsl(include_str!("quads.wgsl").into()),
        });

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aitch globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aitch globals layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aitch globals"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aitch quads layout"),
            bind_group_layouts: &[&globals_layout, atlas_layout],
            push_constant_ranges: &[],
        });

        // Must match `struct Instance` in quads.wgsl and `Instances::push`.
        let attributes = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 8,
                shader_location: 1,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 16,
                shader_location: 2,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 24,
                shader_location: 3,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 32,
                shader_location: 4,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 48,
                shader_location: 5,
            },
        ];

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("aitch quads"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: INSTANCE_SIZE,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attributes,
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        QuadPipeline {
            pipeline,
            globals,
            globals_bind_group,
            instances: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aitch instances"),
                size: INSTANCE_SIZE * 4096,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            instance_capacity: INSTANCE_SIZE * 4096,
        }
    }

    /// Upload this frame's instances, growing the buffer if it is too small.
    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: [f32; 2],
        instances: &Instances,
    ) {
        let mut globals = [0u8; 16];
        globals[0..4].copy_from_slice(&screen[0].to_le_bytes());
        globals[4..8].copy_from_slice(&screen[1].to_le_bytes());
        queue.write_buffer(&self.globals, 0, &globals);

        let needed = instances.bytes().len() as u64;
        if needed > self.instance_capacity {
            // Grow in powers of two so a slow ramp does not reallocate often.
            let capacity = needed.next_power_of_two();
            self.instances = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aitch instances"),
                size: capacity,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.instance_capacity = capacity;
        }

        if needed > 0 {
            queue.write_buffer(&self.instances, 0, instances.bytes());
        }
    }

    pub fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        atlas_bind_group: &wgpu::BindGroup,
        instances: &Instances,
    ) {
        if instances.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.globals_bind_group, &[]);
        pass.set_bind_group(1, atlas_bind_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        pass.draw(0..6, 0..instances.count());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> Entry {
        Entry {
            uv_min: [0.0, 0.25],
            uv_max: [0.5, 1.0],
            size: [3.0, 4.0],
            offset: [0.0, 0.0],
            color: false,
        }
    }

    #[test]
    fn an_instance_is_exactly_the_declared_stride() {
        let mut instances = Instances::default();
        instances.push([1.0, 2.0], entry(), Color::srgb(255, 255, 255), Kind::Mask);
        assert_eq!(instances.count(), 1);
        assert_eq!(instances.bytes().len() as u64, INSTANCE_SIZE);
    }

    #[test]
    fn instances_pack_back_to_back() {
        let mut instances = Instances::default();
        for _ in 0..10 {
            instances.push([0.0, 0.0], entry(), Color::srgb(0, 0, 0), Kind::Solid);
        }
        assert_eq!(instances.count(), 10);
        assert_eq!(instances.bytes().len() as u64, INSTANCE_SIZE * 10);

        instances.clear();
        assert!(instances.is_empty());
        assert_eq!(instances.bytes().len(), 0);
    }

    #[test]
    fn the_position_and_size_are_where_the_shader_expects_them() {
        let mut instances = Instances::default();
        instances.push([12.5, -3.0], entry(), Color::srgb(0, 0, 0), Kind::Mask);
        let bytes = instances.bytes();

        let read =
            |offset: usize| f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        assert_eq!(read(0), 12.5, "position.x at offset 0");
        assert_eq!(read(4), -3.0, "position.y at offset 4");
        assert_eq!(read(8), 3.0, "size.x at offset 8");
        assert_eq!(read(12), 4.0, "size.y at offset 12");

        let kind = u32::from_le_bytes(bytes[48..52].try_into().unwrap());
        assert_eq!(kind, Kind::Mask as u32, "kind at offset 48");
    }

    #[test]
    fn a_solid_rect_takes_its_size_from_the_caller_not_the_texel() {
        let white = Entry {
            size: [1.0, 1.0],
            ..entry()
        };
        let mut instances = Instances::default();
        instances.push_rect([0.0, 0.0], [2.0, 20.0], white, Color::srgb(255, 0, 0));

        let bytes = instances.bytes();
        let read =
            |offset: usize| f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        assert_eq!(read(8), 2.0);
        assert_eq!(read(12), 20.0);
    }
}
