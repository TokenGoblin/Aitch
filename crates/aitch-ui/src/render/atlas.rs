//! A glyph atlas: one texture, a shelf packer, and a cache of what is in it.
//!
//! Glyphs are rasterized by swash at their exact physical size and packed as
//! they are first asked for. The cache key is cosmic-text's [`CacheKey`], which
//! already includes the physical font size, so a DPI change re-keys every glyph
//! instead of scaling a blurry one — which is the Wayland fractional-scaling
//! trap PLAN.md §10 warns about.
//!
//! The texture is `Rgba8UnormSrgb` rather than a coverage-only `R8`, so color
//! emoji land in the same atlas as mask glyphs. A mask is stored as white with
//! the coverage in alpha; alpha is linear in an sRGB format, so the two kinds
//! of glyph can share one sampler and one pipeline.

use std::collections::HashMap;

use cosmic_text::{CacheKey, FontSystem, SwashCache, SwashContent};

/// A glyph's place in the atlas, in the units the vertex shader wants.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Entry {
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
    /// Size of the rasterized image in physical pixels.
    pub size: [f32; 2],
    /// Where the image sits relative to the glyph's pen position.
    pub offset: [f32; 2],
    /// A color glyph is drawn as-is; a mask is tinted by the instance color.
    pub color: bool,
}

/// One pixel of transparent margin around each entry, so that a neighbouring
/// glyph can never bleed in.
const PADDING: u32 = 1;

pub struct Atlas {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    layout: wgpu::BindGroupLayout,
    size: u32,
    /// Shelf packer state: the open shelf's cursor and height.
    shelf_x: u32,
    shelf_y: u32,
    shelf_height: u32,
    /// `None` means the glyph has no image at all — a space, most often.
    entries: HashMap<CacheKey, Option<Entry>>,
    /// A single opaque texel, so solid fills can share the glyph pipeline.
    white: Entry,
    /// Set when a frame asked for more than the atlas can hold. Reported once.
    overflowed: bool,
}

impl Atlas {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, size: u32) -> Atlas {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aitch glyph atlas"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("aitch atlas sampler"),
            // Glyphs are rasterized at their exact physical size and drawn at
            // 1:1, so nearest is both correct and sharper than linear.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aitch atlas layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aitch atlas"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let mut atlas = Atlas {
            texture,
            bind_group,
            layout,
            size,
            shelf_x: 0,
            shelf_y: 0,
            shelf_height: 0,
            entries: HashMap::new(),
            white: Entry {
                uv_min: [0.0, 0.0],
                uv_max: [0.0, 0.0],
                size: [1.0, 1.0],
                offset: [0.0, 0.0],
                color: false,
            },
            overflowed: false,
        };

        atlas.white = atlas
            .insert(queue, 1, 1, &[255, 255, 255, 255], [0.0, 0.0], false)
            .expect("a fresh atlas has room for one texel");

        atlas
    }

    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    pub fn bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    /// The opaque texel that solid fills — the cursor — sample.
    pub fn white(&self) -> Entry {
        self.white
    }

    /// Look a glyph up, rasterizing and packing it on first sight.
    pub fn glyph(
        &mut self,
        queue: &wgpu::Queue,
        font_system: &mut FontSystem,
        swash: &mut SwashCache,
        key: CacheKey,
    ) -> Option<Entry> {
        if let Some(entry) = self.entries.get(&key) {
            return *entry;
        }

        let image = swash.get_image(font_system, key).as_ref()?;
        let width = image.placement.width;
        let height = image.placement.height;
        let offset = [image.placement.left as f32, -image.placement.top as f32];

        if width == 0 || height == 0 {
            // A space, or a glyph that rasterizes to nothing. Remember that,
            // so we do not ask swash about it again on every frame.
            self.entries.insert(key, None);
            return None;
        }

        let (rgba, color) = match image.content {
            SwashContent::Mask => {
                let mut rgba = Vec::with_capacity(image.data.len() * 4);
                for coverage in &image.data {
                    rgba.extend_from_slice(&[255, 255, 255, *coverage]);
                }
                (rgba, false)
            }
            SwashContent::Color => (image.data.clone(), true),
            // Subpixel masks are three coverage values per pixel and need a
            // different blend mode to be worth anything. Phase 1 does not do
            // subpixel antialiasing, so take the green channel as coverage.
            SwashContent::SubpixelMask => {
                let mut rgba = Vec::with_capacity(image.data.len() / 4 * 4);
                for texel in image.data.chunks_exact(4) {
                    rgba.extend_from_slice(&[255, 255, 255, texel[1]]);
                }
                (rgba, false)
            }
        };

        let entry = self.insert(queue, width, height, &rgba, offset, color);
        if entry.is_none() && !self.overflowed {
            self.overflowed = true;
        }
        self.entries.insert(key, entry);
        entry
    }

    /// Whether the atlas ran out of room since it was last reset.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// Forget every glyph and start packing from the top left again.
    ///
    /// Called when the scale factor changes: every cached glyph was rasterized
    /// for the old one and none of them will be asked for again.
    pub fn reset(&mut self, queue: &wgpu::Queue) {
        self.entries.clear();
        self.shelf_x = 0;
        self.shelf_y = 0;
        self.shelf_height = 0;
        self.overflowed = false;
        self.white = self
            .insert(queue, 1, 1, &[255, 255, 255, 255], [0.0, 0.0], false)
            .expect("a reset atlas has room for one texel");
    }

    /// Pack one image, returning `None` if the atlas is full.
    fn insert(
        &mut self,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        rgba: &[u8],
        offset: [f32; 2],
        color: bool,
    ) -> Option<Entry> {
        let stride = width + PADDING;
        if self.shelf_x + stride > self.size {
            // Close this shelf and open one below it.
            self.shelf_y += self.shelf_height + PADDING;
            self.shelf_x = 0;
            self.shelf_height = 0;
        }
        if self.shelf_y + height + PADDING > self.size {
            return None;
        }

        let x = self.shelf_x;
        let y = self.shelf_y;
        self.shelf_x += stride;
        self.shelf_height = self.shelf_height.max(height);

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        let scale = self.size as f32;
        Some(Entry {
            uv_min: [x as f32 / scale, y as f32 / scale],
            uv_max: [(x + width) as f32 / scale, (y + height) as f32 / scale],
            size: [width as f32, height as f32],
            offset,
            color,
        })
    }
}
