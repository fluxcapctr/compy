//! Photoshop brush files (`.abr`): the sampled tips inside them, as gray coverage bitmaps with their spacing.
//! Versions 1 and 2 (Photoshop 6 and earlier) list brushes plainly; version 6 (Photoshop 7 on) keeps them in
//! an `8BIM samp` section, each with a PackBits-packed bitmap. Computed (round) brushes and the dynamics in
//! the `desc` section are not read: the app's own round tip and controls stand in.

use anyhow::{Context as _, Result, bail};
use std::path::Path;
use std::rc::Rc;

/// One sampled brush tip: coverage 0 to 255 over `width` x `height`, and the spacing it was saved with
/// (percent of the diameter).
#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub name: String,
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
    pub spacing: f64,
    /// How much each dab turns at random, 0 to 1 of a full turn: what keeps a textured tip from stamping the
    /// same mark along a stroke. Not stored in the file; the bundled set sets it.
    pub jitter: f64,
    /// The set it came from (the file's name), which the picker groups by.
    pub set: String,
    /// Further frames of the same size (a GIMP hose's cells), cycled at random per dab.
    pub frames: Vec<Vec<u8>>,
}

impl Preset {
    /// The longest side, which the brush size scales.
    pub fn diameter(&self) -> f64 { self.width.max(self.height) as f64 }
}

struct Reader<'a> { data: &'a [u8], pos: usize }
impl<'a> Reader<'a> {
    fn need(&self, n: usize) -> Result<()> { if self.pos + n > self.data.len() { bail!("the file ends early"); } Ok(()) }
    fn u8(&mut self) -> Result<u8> { self.need(1)?; let v = self.data[self.pos]; self.pos += 1; Ok(v) }
    fn u16(&mut self) -> Result<u16> { self.need(2)?; let v = u16::from_be_bytes([self.data[self.pos], self.data[self.pos + 1]]); self.pos += 2; Ok(v) }
    fn i16(&mut self) -> Result<i16> { Ok(self.u16()? as i16) }
    fn u32(&mut self) -> Result<u32> { self.need(4)?; let v = u32::from_be_bytes(self.data[self.pos..self.pos + 4].try_into().unwrap()); self.pos += 4; Ok(v) }
    fn i32(&mut self) -> Result<i32> { Ok(self.u32()? as i32) }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> { self.need(n)?; let s = &self.data[self.pos..self.pos + n]; self.pos += n; Ok(s) }
    fn skip(&mut self, n: usize) -> Result<()> { self.need(n)?; self.pos += n; Ok(()) }
}

/// Loaded tips that are about as wide as they are tall turn at random per dab, so a texture does not stamp
/// the same mark along a stroke; a clearly elongated tip (a flat, a rake) keeps its direction.
pub fn default_jitter(width: usize, height: usize) -> f64 {
    let ratio = width as f64 / height.max(1) as f64;
    if (0.7..=1.43).contains(&ratio) { 1.0 } else { 0.0 }
}

/// Loads every sampled brush in the file.
pub fn load(path: &Path) -> Result<Vec<Rc<Preset>>> {
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Brush".into());
    parse(&data, &stem)
}

pub fn parse(data: &[u8], stem: &str) -> Result<Vec<Rc<Preset>>> {
    let mut r = Reader { data, pos: 0 };
    let set = stem.to_string();
    // Everything decoded from one file, in samples, counted before each tip's buffer is made.
    let mut budget: usize = 400_000_000;
    let version = r.u16()?;
    let mut presets = Vec::new();
    match version {
        1 | 2 => {
            let count = r.u16()? as usize;
            for index in 0..count {
                let kind = r.u16()?;
                let size = r.u32()? as usize;
                let start = r.pos;
                if size > data.len() - start { bail!("a brush runs past the end of the file"); }
                if kind == 2 {
                    r.skip(4)?;
                    let spacing = r.i16()? as f64;
                    let mut name = String::new();
                    if version == 2 {
                        let chars = r.u32()? as usize;
                        if chars > 4096 { bail!("a brush name is too long"); }
                        let mut units = Vec::with_capacity(chars);
                        for _ in 0..chars { units.push(r.u16()?); }
                        name = String::from_utf16_lossy(&units).trim_end_matches('\0').trim().to_string();
                    }
                    r.skip(1)?;
                    r.skip(8)?;
                    let (top, left, bottom, right) = (r.i32()?, r.i32()?, r.i32()?, r.i32()?);
                    let depth = r.i16()?;
                    let compression = r.u8()?;
                    if let Some(p) = read_tip(&mut r, top, left, bottom, right, depth, compression, start + size, &mut budget)? {
                        if name.is_empty() { name = format!("{stem} {}", index + 1); }
                        presets.push(Rc::new(Preset { name, width: p.0, height: p.1, pixels: p.2, spacing: if (1.0..=1000.0).contains(&spacing) { spacing } else { 25.0 }, jitter: default_jitter(p.0, p.1), set: set.clone(), frames: Vec::new() }));
                    }
                }
                r.pos = start + size;
            }
        }
        // Photoshop 7 through CC write 6.x; later CC versions write 7.x and 10.x with the same sections.
        6 | 7 | 10 => {
            let subversion = r.u16()?;
            if subversion != 1 && subversion != 2 { bail!("Photoshop brush file version {version}.{subversion} is not supported."); }
            // Skip 8BIM sections until the samples.
            loop {
                if r.bytes(4)? != b"8BIM" { bail!("the file is not laid out as expected"); }
                let tag = r.bytes(4)?;
                if tag == b"samp" { break; }
                let size = r.u32()? as usize;
                r.skip(size)?;
            }
            let section = r.u32()? as usize;
            let end = r.pos.checked_add(section).filter(|e| *e <= data.len()).context("the samples section runs past the end of the file")?;
            let mut index = 0;
            while r.pos < end {
                let size = r.u32()? as usize;
                let padded = size.div_ceil(4) * 4;
                let start = r.pos;
                if padded > data.len() - start { bail!("a brush runs past the end of the file"); }
                // The 37-byte key, then coordinates and unknowns whose size depends on the subversion.
                r.skip(37)?;
                r.skip(if subversion == 1 { 10 } else { 264 })?;
                let (top, left, bottom, right) = (r.i32()?, r.i32()?, r.i32()?, r.i32()?);
                let depth = r.i16()?;
                let compression = r.u8()?;
                index += 1;
                if let Some(p) = read_tip(&mut r, top, left, bottom, right, depth, compression, start + padded, &mut budget)? {
                    presets.push(Rc::new(Preset { name: format!("{stem} {index}"), width: p.0, height: p.1, pixels: p.2, spacing: 25.0, jitter: default_jitter(p.0, p.1), set: set.clone(), frames: Vec::new() }));
                }
                r.pos = start + padded;
            }
        }
        other => bail!("Photoshop brush file version {other} is not supported."),
    }
    if presets.is_empty() { bail!("No sampled brushes were found in the file (computed round brushes are not loaded; the app's round tip covers those)."); }
    Ok(presets)
}

/// One tip's bitmap: raw, or PackBits rows behind a table of row lengths. 8-bit only; 16-bit tips are read
/// at their high byte.
/// `end` is where the brush's record stops (no read passes it); `budget` is what the file may still decode.
fn read_tip(r: &mut Reader, top: i32, left: i32, bottom: i32, right: i32, depth: i16, compression: u8, end: usize, budget: &mut usize) -> Result<Option<(usize, usize, Vec<u8>)>> { read_plane(r, top, left, bottom, right, depth, compression, end, budget, true) }

/// A tip or a pattern channel: `shrink` averages a wide plane down (tips only; patterns keep their pixels).
fn read_plane(r: &mut Reader, top: i32, left: i32, bottom: i32, right: i32, depth: i16, compression: u8, end: usize, budget: &mut usize, shrink: bool) -> Result<Option<(usize, usize, Vec<u8>)>> {
    let (w, h) = (right.checked_sub(left), bottom.checked_sub(top));
    let (Some(w), Some(h)) = (w, h) else { bail!("a brush rectangle is inverted") };
    if w <= 0 || h <= 0 || w > 16_384 || h > 16_384 || w as i64 * h as i64 > 50_000_000 { return Ok(None); }
    let (w, h) = (w as usize, h as usize);
    let bytes = match depth { 8 => 1, 16 => 2, _ => return Ok(None) };
    if compression > 1 { bail!("a brush uses an unknown compression ({compression})"); }
    let end = end.min(r.data.len());
    let room = end.saturating_sub(r.pos);
    // A raw tip needs its whole size; a packed one at least a row-length table and a byte per row.
    if (compression == 0 && room < w * h * bytes) || (compression == 1 && room < h * 3) { bail!("a brush's pixels run past its record"); }
    *budget = budget.checked_sub(w * h).ok_or_else(|| anyhow::anyhow!("The file's brushes exceed the 400-megapixel decoding budget."))?;
    let mut raw = vec![0u8; w * h * bytes];
    if compression == 0 {
        raw.copy_from_slice(r.bytes(w * h * bytes)?);
    } else {
        let mut lengths = Vec::with_capacity(h);
        for _ in 0..h { lengths.push(r.i16()?.max(0) as usize); }
        for (y, &len) in lengths.iter().enumerate() {
            if r.pos + len > end { bail!("a brush's rows run past its record"); }
            let packed = r.bytes(len)?;
            let row = &mut raw[y * w * bytes..(y + 1) * w * bytes];
            let mut i = 0;
            let mut o = 0;
            while i < packed.len() && o < row.len() {
                let n = packed[i] as i8;
                i += 1;
                if n >= 0 {
                    let count = n as usize + 1;
                    if count > row.len() - o || count > packed.len() - i { bail!("a brush row is packed wrongly"); }
                    row[o..o + count].copy_from_slice(&packed[i..i + count]);
                    i += count;
                    o += count;
                } else if n != -128 {
                    if i >= packed.len() { bail!("a brush row is packed wrongly"); }
                    let count = (-(n as i32)) as usize + 1;
                    if count > row.len() - o { bail!("a brush row is packed wrongly"); }
                    row[o..o + count].fill(packed[i]);
                    i += 1;
                    o += count;
                }
            }
            // Every row decodes to exactly its width; a short row is a damaged file, not a blank tip.
            if o != row.len() { bail!("a brush row is short"); }
        }
    }
    let pixels: Vec<u8> = if bytes == 1 { raw } else { raw.chunks_exact(2).map(|p| p[0]).collect() };
    Ok(Some(if shrink { shrink_tip(w, h, pixels) } else { (w, h, pixels) }))
}

/// The largest tip kept: the brush never paints wider than 2000 pixels, so a 5000-pixel tip only costs
/// memory. Larger ones are averaged down by a whole factor.
const TIP_LIMIT: usize = 2048;

fn shrink_tip(w: usize, h: usize, pixels: Vec<u8>) -> (usize, usize, Vec<u8>) {
    let factor = w.max(h).div_ceil(TIP_LIMIT);
    if factor <= 1 { return (w, h, pixels); }
    let (nw, nh) = (w.div_ceil(factor), h.div_ceil(factor));
    let mut out = vec![0u8; nw * nh];
    for y in 0..nh {
        for x in 0..nw {
            let (mut sum, mut n) = (0u32, 0u32);
            for sy in y * factor..((y + 1) * factor).min(h) { for sx in x * factor..((x + 1) * factor).min(w) { sum += pixels[sy * w + sx] as u32; n += 1; } }
            out[y * nw + x] = if n == 0 { 0 } else { (sum / n) as u8 };
        }
    }
    (nw, nh, out)
}

/// A pattern carried in a brush file's `patt` section: straight RGBA pixels.
pub struct Pattern { pub name: String, pub width: usize, pub height: usize, pub rgba: Vec<u8> }

/// Every pattern in a version 6 or later brush file (or a `.pat` file, which is the same section alone).
pub fn patterns(data: &[u8]) -> Result<Vec<Pattern>> {
    let mut r = Reader { data, pos: 0 };
    // Decoded RGBA kept from one file, all patterns together.
    let mut output_budget: usize = 768 << 20;
    let mut out = Vec::new();
    // A pattern library (.pat) is the same records under an 8BPT header, counted rather than sized.
    if data.starts_with(b"8BPT") {
        r.pos = 4;
        let version = r.u16()?;
        if version != 1 { bail!("Photoshop pattern file version {version} is not supported."); }
        let count = r.u32()? as usize;
        for _ in 0..count.min(4096) {
            if r.pos >= data.len() { break; }
            match read_pattern(&mut r, data.len(), &mut output_budget) {
                Ok(Some(p)) => out.push(p),
                Ok(None) => {}
                Err(e) => { eprintln!("pattern skipped: {e:#}"); break; }
            }
        }
        return Ok(out);
    }
    let version = r.u16()?;
    if !matches!(version, 6 | 7 | 10) { return Ok(Vec::new()); }
    r.u16()?;
    while r.pos + 12 <= data.len() {
        if r.bytes(4)? != b"8BIM" { break; }
        let tag = r.bytes(4)?;
        let size = r.u32()? as usize;
        let start = r.pos;
        if tag == b"patt" {
            let end = start.checked_add(size).filter(|e| *e <= data.len()).context("the patterns section runs past the end of the file")?;
            while r.pos + 4 <= end {
                let len = r.u32()? as usize;
                let entry_start = r.pos;
                let entry_end = entry_start.checked_add(len).filter(|e| *e <= end).context("a pattern runs past its section")?;
                match read_pattern(&mut r, entry_end, &mut output_budget) {
                    Ok(Some(p)) => out.push(p),
                    Ok(None) => {}
                    Err(e) => eprintln!("pattern skipped: {e:#}"),
                }
                r.pos = entry_start + len.div_ceil(4) * 4;
            }
        }
        r.pos = start + size + (size & 1);
    }
    Ok(out)
}

fn read_pattern(r: &mut Reader, end: usize, output_budget: &mut usize) -> Result<Option<Pattern>> {
    let version = r.u32()?;
    if version != 1 { bail!("pattern version {version}"); }
    let mode = r.u32()?;
    let (h, w) = (r.u16()? as usize, r.u16()? as usize);
    let chars = r.u32()? as usize;
    if chars > 4096 { bail!("a pattern name is too long"); }
    let mut units = Vec::with_capacity(chars);
    for _ in 0..chars { units.push(r.u16()?); }
    let name = String::from_utf16_lossy(&units).trim_end_matches('\0').trim().to_string();
    let id_len = r.u8()? as usize;
    r.skip(id_len)?;
    // The virtual memory array list: version, length, the bounds, then every channel in turn.
    let vm_version = r.u32()?;
    if vm_version != 3 { bail!("pattern data version {vm_version}"); }
    r.u32()?;
    let (top, left, _bottom, _right) = (r.i32()?, r.i32()?, r.i32()?, r.i32()?);
    let channels = r.u32()? as usize;
    if w == 0 || h == 0 || w > 8192 || h > 8192 { return Ok(None); }
    let wanted = match mode { 1 => 1, 3 => 3, other => bail!("pattern color mode {other}") };
    // What this pattern will cost once decoded: its planes and its RGBA.
    let cost = w * h * (wanted + 4);
    *output_budget = output_budget.checked_sub(cost).ok_or_else(|| anyhow::anyhow!("the file's patterns exceed the 768 MB decoding budget; the rest are skipped"))?;
    let mut planes: Vec<Vec<u8>> = Vec::new();
    let mut budget = 200_000_000usize;
    for _ in 0..channels.min(64) {
        if planes.len() >= wanted || r.pos + 4 > end { break; }
        // An unwritten channel is its flag alone; only a written one carries a length and pixels.
        let written = r.u32()?;
        if written == 0 { continue; }
        let len = r.u32()? as usize;
        let channel_start = r.pos;
        if len == 0 { continue; }
        let depth = r.u32()?;
        let (ct, cl, cb, cr) = (r.i32()?, r.i32()?, r.i32()?, r.i32()?);
        let depth16 = r.i16()?;
        let compression = r.u8()?;
        let _ = depth;
        let tip = read_plane(r, ct, cl, cb, cr, if depth16 == 0 { 8 } else { depth16 }, compression, channel_start + len, &mut budget, false)?;
        match tip {
            Some((tw, th, px)) if tw == w && th == h && ct == top && cl == left => planes.push(px),
            Some((tw, th, px)) => {
                // A channel bounded within the pattern: placed at its own offset from the pattern's corner.
                let (dx, dy) = ((cl - left) as i64, (ct - top) as i64);
                let mut full = vec![0u8; w * h];
                for y in 0..th { let ty = y as i64 + dy; if ty < 0 || ty >= h as i64 { continue; } for x in 0..tw { let tx = x as i64 + dx; if tx < 0 || tx >= w as i64 { continue; } full[ty as usize * w + tx as usize] = px[y * tw + x]; } }
                planes.push(full);
            }
            None => bail!("a pattern channel could not be read"),
        }
        r.pos = channel_start + len;
    }
    if planes.len() < wanted { bail!("the pattern has {} of {wanted} channels", planes.len()); }
    let mut rgba = vec![255u8; w * h * 4];
    for i in 0..w * h {
        if wanted == 1 { let v = planes[0][i]; rgba[i * 4] = v; rgba[i * 4 + 1] = v; rgba[i * 4 + 2] = v; }
        else { rgba[i * 4] = planes[0][i]; rgba[i * 4 + 1] = planes[1][i]; rgba[i * 4 + 2] = planes[2][i]; }
    }
    Ok(Some(Pattern { name, width: w, height: h, rgba }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A version 2 file with one 4 x 2 sampled brush, raw pixels.
    fn v2_file() -> Vec<u8> {
        let mut f = vec![0, 2, 0, 1];
        let mut body = Vec::new();
        body.extend_from_slice(&[0, 0, 0, 0]);
        body.extend_from_slice(&30i16.to_be_bytes());
        let name: Vec<u16> = "Dots".encode_utf16().collect();
        body.extend_from_slice(&(name.len() as u32).to_be_bytes());
        for u in name { body.extend_from_slice(&u.to_be_bytes()); }
        body.push(1);
        body.extend_from_slice(&[0; 8]);
        for v in [0i32, 0, 2, 4] { body.extend_from_slice(&v.to_be_bytes()); }
        body.extend_from_slice(&8i16.to_be_bytes());
        body.push(0);
        body.extend_from_slice(&[255, 128, 0, 255, 0, 255, 128, 0]);
        f.extend_from_slice(&2u16.to_be_bytes());
        f.extend_from_slice(&(body.len() as u32).to_be_bytes());
        f.extend_from_slice(&body);
        f
    }

    #[test]
    fn reads_a_version_2_brush() {
        let presets = parse(&v2_file(), "test").unwrap();
        assert_eq!(presets.len(), 1);
        let p = &presets[0];
        assert_eq!((p.name.as_str(), p.width, p.height, p.spacing), ("Dots", 4, 2, 30.0));
        assert_eq!(p.pixels, vec![255, 128, 0, 255, 0, 255, 128, 0]);
        assert!(parse(b"\x00\x09", "x").is_err());
        assert!(parse(&v2_file()[..20], "x").is_err(), "a truncated file is refused");
    }

    #[test]
    fn reads_a_version_6_packed_brush() {
        // 8BIM samp with one brush: key, subversion 1 padding, a 3 x 2 tip packed row by row.
        let mut brush = Vec::new();
        brush.extend_from_slice(&[0u8; 37]);
        brush.extend_from_slice(&[0u8; 10]);
        for v in [0i32, 0, 2, 3] { brush.extend_from_slice(&v.to_be_bytes()); }
        brush.extend_from_slice(&8i16.to_be_bytes());
        brush.push(1);
        // Row lengths, then rows: a repeat of 255 x3; a literal 0, 128, 255.
        brush.extend_from_slice(&2i16.to_be_bytes());
        brush.extend_from_slice(&4i16.to_be_bytes());
        brush.extend_from_slice(&[0xfe, 255]);
        brush.extend_from_slice(&[2, 0, 128, 255]);
        let mut samp = Vec::new();
        samp.extend_from_slice(&(brush.len() as u32).to_be_bytes());
        samp.extend_from_slice(&brush);
        while samp.len() % 4 != 0 { samp.push(0); }
        let mut f = vec![0, 6, 0, 1];
        f.extend_from_slice(b"8BIMpatt");
        f.extend_from_slice(&0u32.to_be_bytes());
        f.extend_from_slice(b"8BIMsamp");
        f.extend_from_slice(&(samp.len() as u32).to_be_bytes());
        f.extend_from_slice(&samp);
        let presets = parse(&f, "kit").unwrap();
        assert_eq!(presets.len(), 1);
        assert_eq!((presets[0].width, presets[0].height), (3, 2));
        assert_eq!(presets[0].pixels, vec![255, 255, 255, 0, 128, 255]);
        assert_eq!(presets[0].name, "kit 1");
    }

    /// One pattern record: gray, `w` x `h`, with an unwritten channel first, then a raw channel bounded
    /// at (`cl`, `ct`) of `cw` x `ch` holding `pixels`; wrapped in an abr file or a pat file.
    fn pattern_record(w: u16, h: u16, cl: i32, ct: i32, cw: i32, ch: i32, pixels: &[u8]) -> Vec<u8> {
        let mut e = Vec::new();
        e.extend_from_slice(&1u32.to_be_bytes());
        e.extend_from_slice(&1u32.to_be_bytes());
        e.extend_from_slice(&h.to_be_bytes());
        e.extend_from_slice(&w.to_be_bytes());
        let name: Vec<u16> = "Tile".encode_utf16().collect();
        e.extend_from_slice(&(name.len() as u32).to_be_bytes());
        for u in name { e.extend_from_slice(&u.to_be_bytes()); }
        e.push(2); e.extend_from_slice(b"id");
        e.extend_from_slice(&3u32.to_be_bytes());
        e.extend_from_slice(&0u32.to_be_bytes());
        for v in [0i32, 0, h as i32, w as i32] { e.extend_from_slice(&v.to_be_bytes()); }
        e.extend_from_slice(&24u32.to_be_bytes());
        // An unwritten channel: the flag alone.
        e.extend_from_slice(&0u32.to_be_bytes());
        let mut c = Vec::new();
        c.extend_from_slice(&8u32.to_be_bytes());
        for v in [ct, cl, ct + ch, cl + cw] { c.extend_from_slice(&v.to_be_bytes()); }
        c.extend_from_slice(&8i16.to_be_bytes());
        c.push(0);
        c.extend_from_slice(pixels);
        e.extend_from_slice(&1u32.to_be_bytes());
        e.extend_from_slice(&(c.len() as u32).to_be_bytes());
        e.extend_from_slice(&c);
        e
    }

    fn abr_with_pattern(record: &[u8]) -> Vec<u8> {
        let mut section = Vec::new();
        section.extend_from_slice(&(record.len() as u32).to_be_bytes());
        section.extend_from_slice(record);
        while section.len() % 4 != 0 { section.push(0); }
        let mut f = vec![0, 6, 0, 2];
        f.extend_from_slice(b"8BIMpatt");
        f.extend_from_slice(&(section.len() as u32).to_be_bytes());
        f.extend_from_slice(&section);
        f
    }

    #[test]
    fn reads_patterns_with_unwritten_offset_and_wide_channels_and_pat_files() {
        // A 4 x 1 gray pattern whose one white pixel sits at x = 2, behind an unwritten channel.
        let f = abr_with_pattern(&pattern_record(4, 1, 2, 0, 1, 1, &[255]));
        let list = patterns(&f).unwrap();
        assert_eq!(list.len(), 1);
        let p = &list[0];
        assert_eq!((p.width, p.height, p.name.as_str()), (4, 1, "Tile"));
        assert_eq!([p.rgba[0], p.rgba[2 * 4]], [0, 255], "the pixel lands at its own offset");
        // A 4096 x 1 white pattern keeps every pixel: patterns are never shrunk like tips.
        let wide = abr_with_pattern(&pattern_record(4096, 1, 0, 0, 4096, 1, &[255u8; 4096]));
        let list = patterns(&wide).unwrap();
        assert_eq!(list[0].width, 4096);
        assert!(list[0].rgba.chunks_exact(4).all(|px| px[0] == 255), "no black past 2048");
        // The same record in a .pat library.
        let mut pat = b"8BPT".to_vec();
        pat.extend_from_slice(&1u16.to_be_bytes());
        pat.extend_from_slice(&1u32.to_be_bytes());
        pat.extend_from_slice(&pattern_record(4, 1, 2, 0, 1, 1, &[255]));
        let list = patterns(&pat).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].rgba[2 * 4], 255);
        // A version 2 brush file has no patterns.
        assert!(patterns(&[0, 2, 0, 0]).unwrap().is_empty());
    }
}
