//! Draw the application icon, for Windows and for the window itself.
//!
//! Generated rather than drawn by hand, for the same reason the README
//! screenshots are (`docs/screenshots.md`): it takes its colours from
//! [`Theme::dark`], so it cannot drift away from the editor it stands for, and
//! regenerating it is one command rather than an afternoon in an editor
//! nobody has installed.
//!
//! ```text
//! cargo run -p aitch-ui --release --example make_icon -- \
//!     packaging/windows/aitch.ico crates/aitch-ui/assets/icon.rgba
//! ```
//!
//! One or more outputs, each written in the format its extension names: `.ico`
//! for Windows, `.png` for a 256px picture, `.rgba` for the raw block the
//! editor includes at compile time and hands to winit as its window icon.
//!
//! The shapes are rectangles and one rounded corner radius, so there is no
//! font to find and no glyph to shape: an `H` over the two footer rows, which
//! is the one thing about this editor you can see from across a room. Every
//! size is rendered at 4× and boxed down, which is where the smooth edges come
//! from.

use aitch_ui::theme::{Color, Theme};

/// Sizes Windows asks for. 16 and 32 are the ones anyone actually sees; 256
/// is what Explorer's largest view uses.
const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

/// Supersampling factor. Everything is drawn with hard edges at this
/// multiple and averaged down, which anti-aliases the rounded corners and the
/// stems of the H without any of them knowing about it.
const SS: u32 = 4;

/// A rectangle in 0..1 of the icon's side, top-left origin.
struct Rect {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Rect {
    const fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect { x0, y0, x1, y1 }
    }

    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }
}

/// The two stems and the crossbar of the H, then the two footer rows.
const LEFT_STEM: Rect = Rect::new(0.28, 0.17, 0.40, 0.62);
const RIGHT_STEM: Rect = Rect::new(0.60, 0.17, 0.72, 0.62);
const CROSSBAR: Rect = Rect::new(0.28, 0.355, 0.72, 0.435);
/// The second row is shorter than the first, the way the real footer's is
/// when the last entry does not fill the width.
const FOOTER_TOP: Rect = Rect::new(0.22, 0.70, 0.78, 0.775);
const FOOTER_BOTTOM: Rect = Rect::new(0.22, 0.815, 0.63, 0.89);

/// Corner radius of the tile, as a fraction of its side.
const RADIUS: f32 = 0.22;

/// Size written for a `.png`, for anywhere that wants one picture.
const PREVIEW: u32 = 256;

/// Size written for a `.rgba`. The window icon is scaled by the compositor
/// from whatever it is given, and 64 is enough for a taskbar button without
/// putting a large blob in the binary.
const WINDOW: u32 = 64;

fn main() {
    let outputs: Vec<String> = std::env::args().skip(1).collect();
    if outputs.is_empty() {
        eprintln!("usage: make_icon <output.ico|output.png|output.rgba>...");
        std::process::exit(2);
    }

    let theme = Theme::dark();
    // The tile is the colour the footer and status line sit on, so the icon is
    // the editor's chrome rather than an unrelated blue square. The H is the
    // cursor's colour and the footer rows are a key cap's.
    let tile = to_srgb(theme.chrome_background);
    let letter = to_srgb(theme.cursor);
    let rows = to_srgb(theme.key_background);

    for out in &outputs {
        let extension = std::path::Path::new(out)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();

        let bytes = match extension.as_str() {
            "ico" => {
                let entries: Vec<(u32, Vec<u8>)> = SIZES
                    .iter()
                    .map(|&size| {
                        let pixels = render(size, tile, letter, rows);
                        let image = if size < DIB_BELOW {
                            encode_dib(size, &pixels)
                        } else {
                            encode_png(size, &pixels)
                        };
                        (size, image)
                    })
                    .collect();
                encode_ico(&entries)
            }
            // A trailing `-N` names the size, which is what the freedesktop
            // icon directories want: `aitch-48.png` is the 48x48 one. Without
            // a suffix it is the single large picture a README would use.
            "png" => {
                let size = png_size(out);
                encode_png(size, &render(size, tile, letter, rows))
            }
            // The same shape dump_frame writes for a `.raw`: width, height,
            // then the pixels. The binary includes this with `include_bytes!`
            // and hands it to winit, so there is no decoder in the editor and
            // no image crate in its dependency tree.
            "rgba" => {
                let pixels = render(WINDOW, tile, letter, rows);
                let mut raw = Vec::with_capacity(8 + pixels.len());
                raw.extend_from_slice(&WINDOW.to_le_bytes());
                raw.extend_from_slice(&WINDOW.to_le_bytes());
                raw.extend_from_slice(&pixels);
                raw
            }
            _ => {
                eprintln!("{out}: expected .ico, .png or .rgba");
                std::process::exit(2);
            }
        };

        std::fs::write(out, &bytes).unwrap_or_else(|e| panic!("could not write {out}: {e}"));
        eprintln!("wrote {out} ({} bytes)", bytes.len());
    }
}

/// The size a `.png` output asks for, from a trailing `-N` in its name.
///
/// `aitch-48.png` is 48x48. Anything without a suffix is [`PREVIEW`], the one
/// large picture. Sizes are clamped: a typo should not try to render a
/// hundred-thousand-pixel icon, and zero is not a picture.
fn png_size(path: &str) -> u32 {
    std::path::Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.rsplit_once('-'))
        .and_then(|(_, size)| size.parse::<u32>().ok())
        .map_or(PREVIEW, |size| size.clamp(1, 1024))
}

/// One icon at `size`, as tightly packed RGBA.
fn render(size: u32, tile: [u8; 3], letter: [u8; 3], rows: [u8; 3]) -> Vec<u8> {
    let hi = size * SS;
    let samples = SS * SS;
    let mut out = Vec::with_capacity((size * size * 4) as usize);

    for y in 0..size {
        for x in 0..size {
            // Accumulate the supersamples of this pixel. Everything outside
            // the tile is transparent, so alpha is averaged along with the
            // colour and the rounded corners come out soft.
            //
            // Colour is averaged over the *covered* samples and alpha over all
            // of them, because PNG, the 32bpp DIB and winit all want straight
            // alpha rather than premultiplied. Dividing colour by every sample
            // would darken it in proportion to coverage, and the compositor
            // would then multiply by alpha a second time -- a black fringe all
            // the way around the rounded corners.
            let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
            let mut covered = 0u32;
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = x * SS + sx;
                    let py = y * SS + sy;
                    let u = (px as f32 + 0.5) / hi as f32;
                    let v = (py as f32 + 0.5) / hi as f32;

                    if !in_tile(u, v) {
                        continue;
                    }
                    let colour = if LEFT_STEM.contains(u, v)
                        || RIGHT_STEM.contains(u, v)
                        || CROSSBAR.contains(u, v)
                    {
                        letter
                    } else if FOOTER_TOP.contains(u, v) || FOOTER_BOTTOM.contains(u, v) {
                        rows
                    } else {
                        tile
                    };
                    r += colour[0] as u32;
                    g += colour[1] as u32;
                    b += colour[2] as u32;
                    a += 255;
                    covered += 1;
                }
            }

            // Alpha over every sample, so a pixel half off the corner is half
            // as opaque -- that is the point. Colour over the covered ones
            // only, so it stays the colour it is rather than fading to black.
            let mix = covered.max(1);
            out.push((r / mix) as u8);
            out.push((g / mix) as u8);
            out.push((b / mix) as u8);
            out.push((a / samples) as u8);
        }
    }
    out
}

/// Whether a point is inside the rounded square.
fn in_tile(u: f32, v: f32) -> bool {
    // Distance from the nearest corner's centre, but only in the corner
    // quadrants; everywhere else the square's own edges decide.
    let cx = u.clamp(RADIUS, 1.0 - RADIUS);
    let cy = v.clamp(RADIUS, 1.0 - RADIUS);
    let (dx, dy) = (u - cx, v - cy);
    dx * dx + dy * dy <= RADIUS * RADIUS
}

/// A theme colour back to 8-bit sRGB. [`Color`] holds linear values, because
/// the surface is configured with an sRGB format and a clear value has to be
/// linear; a PNG wants the other end of that conversion.
fn to_srgb(colour: Color) -> [u8; 3] {
    fn channel(linear: f64) -> u8 {
        let c = if linear <= 0.003_130_8 {
            linear * 12.92
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        };
        (c.clamp(0.0, 1.0) * 255.0).round() as u8
    }
    [channel(colour.r), channel(colour.g), channel(colour.b)]
}

fn encode_png(size: u32, pixels: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, size, size);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .expect("a PNG header")
        .write_image_data(pixels)
        .expect("PNG data");
    out
}

/// One icon entry as an uncompressed 32-bit DIB, the original icon format.
///
/// A `BITMAPINFOHEADER` with twice the real height — the header describes the
/// colour image and the 1-bit mask below it as one bitmap — then bottom-up
/// BGRA rows, then the mask. The mask is left all zeros: with an alpha channel
/// present Windows composites on that instead, and a mask of "nothing is
/// transparent" is what every icon with alpha carries.
fn encode_dib(size: u32, pixels: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&(size as i32).to_le_bytes()); // biWidth
    out.extend_from_slice(&((size * 2) as i32).to_le_bytes()); // biHeight
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    out.extend_from_slice(&(size * size * 4).to_le_bytes()); // biSizeImage
    out.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    out.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    // Bottom-up, and BGRA rather than RGBA.
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            out.push(pixels[i + 2]);
            out.push(pixels[i + 1]);
            out.push(pixels[i]);
            out.push(pixels[i + 3]);
        }
    }

    // The AND mask: one bit per pixel, each row padded to four bytes.
    let row = size.div_ceil(8).next_multiple_of(4);
    out.extend(std::iter::repeat_n(0u8, (row * size) as usize));
    out
}

/// Below this, an entry is stored as an uncompressed DIB rather than a PNG.
///
/// Modern Windows reads PNG entries at any size, but GDI+ and anything else
/// built on the old icon APIs read only the DIB ones — `System.Drawing.Icon`
/// asked for 256 and handed back 128 when every entry here was a PNG. The
/// small sizes are where those APIs look and where a DIB is cheap, so they are
/// stored the old way and the large ones stay compressed; 256×256 as a DIB
/// would be 256 KB on its own.
const DIB_BELOW: u32 = 64;

/// Pack the images into an `.ico`.
fn encode_ico(entries: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // 1 = icon, 2 = cursor
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());

    // The directory is fixed width, so every image offset is known before any
    // of them is written.
    let mut offset = 6 + 16 * entries.len() as u32;
    for (size, png) in entries {
        // 256 does not fit in a byte and is written as 0, which is the format
        // saying "the largest size there is".
        let dimension = if *size >= 256 { 0u8 } else { *size as u8 };
        out.push(dimension); // width
        out.push(dimension); // height
        out.push(0); // colours in the palette; 0 for truecolour
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in entries {
        out.extend_from_slice(png);
    }

    out
}
