//! PNG in and out. Layer images decode to premultiplied ARGB32, masks to A8 after the same checks the
//! macOS app makes (8 bits per channel, one frame, masks strictly 8-bit grayscale with no alpha).

use crate::format::ProjectError;
use crate::raster::{a8_from_data, argb_from_packed, with_bytes};
use cairo::{Format, ImageSurface};
use png::{BitDepth, ColorType, Transformations};
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

/// What the file's header says, checked before any pixels are decoded.
pub struct PngHeader {
    pub width: u32,
    pub height: u32,
    pub bit_depth: BitDepth,
    pub color_type: ColorType,
    pub frames: u32,
}

pub enum Decoded {
    Image(ImageSurface),
    Mask(ImageSurface),
}

/// Decodes one PNG. `check` sees the header first and can reject the file (for size budgets) before the
/// pixels are read. `mask` asks for a mask (8-bit gray, no alpha, else the project is invalid).
pub fn decode(path: &Path, mask: bool, check: impl FnOnce(&PngHeader) -> Result<(), ProjectError>) -> Result<Decoded, ProjectError> {
    let file = File::open(path).map_err(|_| ProjectError::MissingImage)?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    decoder.set_transformations(Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(|_| ProjectError::MissingImage)?;
    let info = reader.info();
    let header = PngHeader {
        width: info.width,
        height: info.height,
        bit_depth: info.bit_depth,
        color_type: info.color_type,
        frames: info.animation_control.map(|a| a.num_frames).unwrap_or(1),
    };
    if header.frames != 1 || header.bit_depth == BitDepth::Sixteen { return Err(ProjectError::MissingImage); }
    check(&header)?;
    if mask && (header.color_type != ColorType::Grayscale || header.bit_depth != BitDepth::Eight) {
        return Err(ProjectError::Invalid);
    }
    let size = reader.output_buffer_size().ok_or(ProjectError::MissingImage)?;
    let mut buf = vec![0u8; size];
    let out = reader.next_frame(&mut buf).map_err(|_| ProjectError::MissingImage)?;
    let (w, h) = (header.width as usize, header.height as usize);
    let line = out.line_size;
    let build = |e: cairo::Error| { let _ = e; ProjectError::TooLarge };
    if mask {
        let stride = Format::A8.stride_for_width(header.width).map_err(build)? as usize;
        let mut data = vec![0u8; stride * h];
        for y in 0..h { data[y * stride..y * stride + w].copy_from_slice(&buf[y * line..y * line + w]); }
        return a8_from_data(w as i32, h as i32, data, stride as i32).map(Decoded::Mask).map_err(|_| ProjectError::TooLarge);
    }
    let mut packed = vec![0u8; w * h * 4];
    for y in 0..h {
        let src = &buf[y * line..];
        for x in 0..w {
            let (r, g, b, a) = match out.color_type {
                ColorType::Rgba => (src[x * 4], src[x * 4 + 1], src[x * 4 + 2], src[x * 4 + 3]),
                ColorType::Rgb => (src[x * 3], src[x * 3 + 1], src[x * 3 + 2], 255),
                ColorType::GrayscaleAlpha => (src[x * 2], src[x * 2], src[x * 2], src[x * 2 + 1]),
                ColorType::Grayscale => (src[x], src[x], src[x], 255),
                ColorType::Indexed => return Err(ProjectError::MissingImage),
            };
            let p = &mut packed[(y * w + x) * 4..(y * w + x + 1) * 4];
            p[0] = premultiply(b, a);
            p[1] = premultiply(g, a);
            p[2] = premultiply(r, a);
            p[3] = a;
        }
    }
    argb_from_packed(w as i32, h as i32, packed).map(Decoded::Image).map_err(|_| ProjectError::TooLarge)
}

fn premultiply(c: u8, a: u8) -> u8 { ((c as u32 * a as u32 + 127) / 255) as u8 }
fn unpremultiply(c: u8, a: u8) -> u8 {
    if a == 0 { 0 } else { ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8 }
}

/// Writes an A8 mask as an 8-bit grayscale PNG with no alpha, as the format requires.
pub fn encode_gray(surface: &ImageSurface, path: &Path) -> anyhow::Result<()> {
    let (w, h) = (surface.width() as usize, surface.height() as usize);
    let gray = with_bytes(surface, |data, stride| { let mut out = vec![0u8; w * h]; for y in 0..h { out[y * w..(y + 1) * w].copy_from_slice(&data[y * stride..y * stride + w]); } out })?;
    let file = File::create(path)?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), w as u32, h as u32);
    encoder.set_color(ColorType::Grayscale);
    encoder.set_depth(BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&gray)?;
    Ok(())
}

/// A premultiplied ARGB32 surface as straight-alpha RGBA bytes, packed.
pub fn straight_rgba(surface: &ImageSurface) -> anyhow::Result<(Vec<u8>, usize, usize)> {
    let (w, h) = (surface.width() as usize, surface.height() as usize);
    let rgba = with_bytes(surface, |data, stride| {
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let p = &data[y * stride + x * 4..y * stride + x * 4 + 4];
                let o = &mut rgba[(y * w + x) * 4..(y * w + x + 1) * 4];
                o[0] = unpremultiply(p[2], p[3]);
                o[1] = unpremultiply(p[1], p[3]);
                o[2] = unpremultiply(p[0], p[3]);
                o[3] = p[3];
            }
        }
        rgba
    })?;
    Ok((rgba, w, h))
}

/// A surface from straight-alpha RGBA bytes.
pub fn from_straight_rgba(rgba: &[u8], w: usize, h: usize) -> anyhow::Result<ImageSurface> {
    let mut packed = vec![0u8; w * h * 4];
    for i in 0..w * h {
        let (r, g, b, a) = (rgba[i * 4], rgba[i * 4 + 1], rgba[i * 4 + 2], rgba[i * 4 + 3]);
        packed[i * 4] = premultiply(b, a); packed[i * 4 + 1] = premultiply(g, a); packed[i * 4 + 2] = premultiply(r, a); packed[i * 4 + 3] = a;
    }
    argb_from_packed(w as i32, h as i32, packed)
}

/// PNG bytes of a surface, in memory (for the clipboard).
pub fn png_bytes(surface: &ImageSurface) -> anyhow::Result<Vec<u8>> {
    let (rgba, w, h) = straight_rgba(surface)?;
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, w as u32, h as u32);
        encoder.set_color(ColorType::Rgba);
        encoder.set_depth(BitDepth::Eight);
        encoder.write_header()?.write_image_data(&rgba)?;
    }
    Ok(out)
}

/// Writes a premultiplied ARGB32 surface as a straight-alpha RGBA PNG with its resolution in pixels per inch.
pub fn encode(surface: &ImageSurface, path: &Path, dpi: f64) -> anyhow::Result<()> {
    let (rgba, w, h) = straight_rgba(surface)?;
    let file = File::create(path)?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), w as u32, h as u32);
    encoder.set_color(ColorType::Rgba);
    encoder.set_depth(BitDepth::Eight);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let ppm = (dpi / 0.0254).round() as u32;
    encoder.set_pixel_dims(Some(png::PixelDimensions { xppu: ppm, yppu: ppm, unit: png::Unit::Meter }));
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&rgba)?;
    Ok(())
}
