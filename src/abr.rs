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
                    if let Some(p) = read_tip(&mut r, top, left, bottom, right, depth, compression)? {
                        if name.is_empty() { name = format!("{stem} {}", index + 1); }
                        presets.push(Rc::new(Preset { name, width: p.0, height: p.1, pixels: p.2, spacing: if (1.0..=1000.0).contains(&spacing) { spacing } else { 25.0 }, jitter: default_jitter(p.0, p.1), set: set.clone(), frames: Vec::new() }));
                    }
                }
                r.pos = start + size;
            }
        }
        6 => {
            let subversion = r.u16()?;
            if subversion != 1 && subversion != 2 { bail!("Photoshop brush file version 6.{subversion} is not supported."); }
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
                if let Some(p) = read_tip(&mut r, top, left, bottom, right, depth, compression)? {
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
fn read_tip(r: &mut Reader, top: i32, left: i32, bottom: i32, right: i32, depth: i16, compression: u8) -> Result<Option<(usize, usize, Vec<u8>)>> {
    let (w, h) = (right.checked_sub(left), bottom.checked_sub(top));
    let (Some(w), Some(h)) = (w, h) else { bail!("a brush rectangle is inverted") };
    if w <= 0 || h <= 0 || w > 16_384 || h > 16_384 || w as i64 * h as i64 > 50_000_000 { return Ok(None); }
    let (w, h) = (w as usize, h as usize);
    let bytes = match depth { 8 => 1, 16 => 2, _ => return Ok(None) };
    let mut raw = vec![0u8; w * h * bytes];
    if compression == 0 {
        raw.copy_from_slice(r.bytes(w * h * bytes)?);
    } else {
        let mut lengths = Vec::with_capacity(h);
        for _ in 0..h { lengths.push(r.i16()?.max(0) as usize); }
        for (y, &len) in lengths.iter().enumerate() {
            let packed = r.bytes(len)?;
            let row = &mut raw[y * w * bytes..(y + 1) * w * bytes];
            let mut i = 0;
            let mut o = 0;
            while i < packed.len() && o < row.len() {
                let n = packed[i] as i8;
                i += 1;
                if n >= 0 {
                    let count = (n as usize + 1).min(row.len() - o).min(packed.len() - i);
                    row[o..o + count].copy_from_slice(&packed[i..i + count]);
                    i += count;
                    o += count;
                } else if n != -128 {
                    if i >= packed.len() { break; }
                    let count = ((-(n as i32)) as usize + 1).min(row.len() - o);
                    row[o..o + count].fill(packed[i]);
                    i += 1;
                    o += count;
                }
            }
        }
    }
    let pixels: Vec<u8> = if bytes == 1 { raw } else { raw.chunks_exact(2).map(|p| p[0]).collect() };
    Ok(Some((w, h, pixels)))
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
}
