// One pipeline draws everything aitch puts on screen: glyphs sample the atlas,
// solid fills sample its opaque texel. Six vertices per instance, no vertex
// buffer of positions — the corner comes from the vertex index.

struct Globals {
    // Physical size of the drawable area, in pixels.
    screen: vec2<f32>,
    _pad: vec2<f32>,
}

@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var atlas_texture: texture_2d<f32>;
@group(1) @binding(1) var atlas_sampler: sampler;

// Mask glyph: white with coverage in alpha, tinted by the instance color.
const KIND_MASK: u32 = 0u;
// Color glyph: drawn as it was rasterized.
const KIND_COLOR: u32 = 1u;
// Solid fill: the instance color, ignoring the texture.
const KIND_SOLID: u32 = 2u;

var<private> CORNERS: array<vec2<f32>, 6> = array<vec2<f32>, 6>(
    vec2<f32>(0.0, 0.0),
    vec2<f32>(1.0, 0.0),
    vec2<f32>(0.0, 1.0),
    vec2<f32>(0.0, 1.0),
    vec2<f32>(1.0, 0.0),
    vec2<f32>(1.0, 1.0),
);

struct Instance {
    @location(0) position: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv_min: vec2<f32>,
    @location(3) uv_max: vec2<f32>,
    @location(4) color: vec4<f32>,
    @location(5) kind: u32,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) kind: u32,
}

@vertex
fn vertex_main(@builtin(vertex_index) index: u32, instance: Instance) -> VertexOutput {
    let corner = CORNERS[index];
    let pixel = instance.position + corner * instance.size;

    // Pixel space (origin top left, y down) to clip space (origin centre, y up).
    let clip = vec2<f32>(
        pixel.x / globals.screen.x * 2.0 - 1.0,
        1.0 - pixel.y / globals.screen.y * 2.0,
    );

    var out: VertexOutput;
    out.clip_position = vec4<f32>(clip, 0.0, 1.0);
    out.uv = mix(instance.uv_min, instance.uv_max, corner);
    out.color = instance.color;
    out.kind = instance.kind;
    return out;
}

@fragment
fn fragment_main(in: VertexOutput) -> @location(0) vec4<f32> {
    if in.kind == KIND_SOLID {
        return in.color;
    }

    let texel = textureSample(atlas_texture, atlas_sampler, in.uv);
    if in.kind == KIND_COLOR {
        return texel;
    }

    // Coverage lives in alpha; the color is ours.
    return vec4<f32>(in.color.rgb, in.color.a * texel.a);
}
