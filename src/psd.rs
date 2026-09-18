//! Photoshop's PSD format, read and written: 8-bit RGB layered files with folders, masks, opacity, blend
//! modes and clipping. Layers export rasterized at their document placement (PSD has no non-destructive
//! transforms), adjustment layers are carried as identity adjustments of the same kind on import and left
//! out on export (the merged image still shows their effect). 16-bit files read at 8 bits; CMYK, Lab,
//! indexed and PSB files are refused with a message.

use crate::format::{BlendMode, Layer, Manifest, Point, Project, Sampling, Size, Transform, upper};
use crate::png_io::from_straight_rgba;
use crate::raster::{a8_from_data, with_bytes};
use anyhow::{Context as _, Result, bail};
use cairo::{Format, ImageSurface};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use uuid::Uuid;

const BLEND_KEYS: [(BlendMode, &[u8; 4]); 13] = [
    (BlendMode::Normal, b"norm"), (BlendMode::Multiply, b"mul "), (BlendMode::Screen, b"scrn"), (BlendMode::Overlay, b"over"),
    (BlendMode::Darken, b"dark"), (BlendMode::Lighten, b"lite"), (BlendMode::Difference, b"diff"), (BlendMode::ColorDodge, b"div "),
    (BlendMode::ColorBurn, b"idiv"), (BlendMode::Hue, b"hue "), (BlendMode::Saturation, b"sat "), (BlendMode::Color, b"colr"), (BlendMode::Luminosity, b"lum "),
];

/// Photoshop's adjustment layer keys that map onto this app's kinds.
const ADJUSTMENT_KEYS: [(&[u8; 4], &str); 5] = [(b"levl", "Levels"), (b"curv", "Curves"), (b"hue2", "Hue/Saturation"), (b"expA", "Exposure"), (b"grdm", "Gradient Map")];

// MARK: Reading

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

struct RawLayer {
    rect: (i32, i32, i32, i32),
    channels: Vec<(i16, Vec<u8>)>,
    blend: BlendMode,
    opacity: u8,
    clipping: bool,
    hidden: bool,
    name: String,
    section: u32,
    mask: Option<(i32, i32, i32, i32, u8, u8)>,
    adjustment: Option<&'static str>,
    unsupported_adjustment: bool,
}

/// Unpacks PackBits runs into exactly `out.len()` bytes.
fn unpack_bits(mut input: &[u8], out: &mut [u8]) -> Result<()> {
    let mut o = 0;
    while o < out.len() {
        let Some((&n, rest)) = input.split_first() else { bail!("run-length data ends early") };
        input = rest;
        let n = n as i8;
        if n >= 0 {
            let count = n as usize + 1;
            if input.len() < count || o + count > out.len() { bail!("bad run-length literal"); }
            out[o..o + count].copy_from_slice(&input[..count]);
            input = &input[count..];
            o += count;
        } else if n != -128 {
            let count = (-(n as i32)) as usize + 1;
            let Some((&v, rest)) = input.split_first() else { bail!("bad run-length repeat") };
            input = rest;
            let count = count.min(out.len() - o);
            out[o..o + count].fill(v);
            o += count;
        }
    }
    Ok(())
}

/// Packs a row with PackBits: repeats of three or more become runs, the rest literals.
fn pack_bits(row: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < row.len() {
        let mut run = 1;
        while i + run < row.len() && row[i + run] == row[i] && run < 128 { run += 1; }
        if run >= 3 {
            out.push((1 - run as i32) as i8 as u8);
            out.push(row[i]);
            i += run;
            continue;
        }
        let start = i;
        let mut literal = 0;
        while i < row.len() && literal < 128 {
            let mut r = 1;
            while i + r < row.len() && row[i + r] == row[i] && r < 128 { r += 1; }
            if r >= 3 { break; }
            i += 1;
            literal += 1;
        }
        out.push((literal - 1) as u8);
        out.extend_from_slice(&row[start..start + literal]);
    }
}

/// Reads one channel's image data (`compression` already consumed) of `rows` x `cols` samples of `depth` bits,
/// returning 8-bit samples.
fn read_channel(r: &mut Reader, rows: usize, cols: usize, depth: u16, compression: u16) -> Result<Vec<u8>> {
    let bytes = (depth as usize / 8).max(1);
    let mut raw = vec![0u8; rows * cols * bytes];
    match compression {
        0 => raw.copy_from_slice(r.bytes(rows * cols * bytes)?),
        1 => {
            let mut counts = Vec::with_capacity(rows);
            for _ in 0..rows { counts.push(r.u16()? as usize); }
            for (y, &count) in counts.iter().enumerate() {
                let packed = r.bytes(count)?;
                unpack_bits(packed, &mut raw[y * cols * bytes..(y + 1) * cols * bytes])?;
            }
        }
        other => bail!("compression {other} (zip) is not supported yet"),
    }
    Ok(match depth {
        8 => raw,
        16 => raw.chunks_exact(2).map(|p| p[0]).collect(),
        32 => raw.chunks_exact(4).map(|p| (f32::from_be_bytes([p[0], p[1], p[2], p[3]]).clamp(0.0, 1.0) * 255.0).round() as u8).collect(),
        other => bail!("{other}-bit files are not supported"),
    })
}

fn pascal_string(r: &mut Reader, pad: usize) -> Result<String> {
    let len = r.u8()? as usize;
    let text = String::from_utf8_lossy(r.bytes(len)?).to_string();
    let total = 1 + len;
    let padded = total.div_ceil(pad) * pad;
    r.skip(padded - total)?;
    Ok(text)
}

/// Reads a PSD into a project with its warnings (things the file had that were not carried over).
pub fn read(path: &Path) -> Result<(Project, Vec<String>)> {
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut r = Reader { data: &data, pos: 0 };
    if r.bytes(4)? != b"8BPS" { bail!("This is not a Photoshop file."); }
    let version = r.u16()?;
    if version == 2 { bail!("Large Document Format (PSB) files are not supported yet."); }
    if version != 1 { bail!("Unknown PSD version {version}."); }
    r.skip(6)?;
    let channels = r.u16()? as usize;
    let height = r.u32()? as i32;
    let width = r.u32()? as i32;
    let depth = r.u16()?;
    let mode = r.u16()?;
    if !(1..=30_000).contains(&width) || !(1..=30_000).contains(&height) { bail!("The canvas is {width} x {height}; sides run from 1 to 30,000 pixels."); }
    match mode { 1 | 3 => {} 0 => bail!("Bitmap mode files are not supported."), 2 => bail!("Indexed color files are not supported; convert to RGB first."), 4 => bail!("CMYK files are not supported; convert to RGB first."), 7 | 8 => bail!("Multichannel and Duotone files are not supported."), 9 => bail!("Lab files are not supported; convert to RGB first."), other => bail!("Color mode {other} is not supported.") }
    if ![8, 16, 32].contains(&depth) { bail!("{depth}-bit files are not supported."); }
    let mut warnings = Vec::new();
    if depth != 8 { warnings.push(format!("The file is {depth}-bit; it was opened at 8 bits per channel.")); }
    let color_mode_len = r.u32()? as usize;
    r.skip(color_mode_len)?;
    // Image resources: only the resolution matters here.
    let resources_len = r.u32()? as usize;
    let resources_end = r.pos + resources_len;
    let mut resolution = 72.0;
    while r.pos + 12 <= resources_end {
        if r.bytes(4)? != b"8BIM" { break; }
        let id = r.u16()?;
        let _name = pascal_string(&mut r, 2)?;
        let size = r.u32()? as usize;
        let start = r.pos;
        if id == 0x03ED && size >= 16 {
            let fixed = r.u32()?;
            let ppi = fixed as f64 / 65536.0;
            if ppi.is_finite() && (1.0..=9600.0).contains(&ppi) { resolution = ppi; }
        }
        r.pos = start + size + (size & 1);
    }
    r.pos = resources_end;
    // Layer and mask information.
    let layer_mask_len = r.u32()? as usize;
    let layer_mask_end = r.pos + layer_mask_len;
    let mut raw_layers: Vec<RawLayer> = Vec::new();
    if layer_mask_len > 0 {
        let layer_info_len = r.u32()? as usize;
        if layer_info_len > 0 {
            let count = r.i16()?.unsigned_abs() as usize;
            let mut records = Vec::with_capacity(count);
            for _ in 0..count {
                let (top, left, bottom, right) = (r.i32()?, r.i32()?, r.i32()?, r.i32()?);
                let channel_count = r.u16()? as usize;
                let mut channel_specs = Vec::with_capacity(channel_count);
                for _ in 0..channel_count { channel_specs.push((r.i16()?, r.u32()? as usize)); }
                if r.bytes(4)? != b"8BIM" { bail!("a layer record is damaged"); }
                let key: [u8; 4] = r.bytes(4)?.try_into().unwrap();
                // ImageMagick writes "norm" byte-reversed; treat it as Normal.
                let blend = BLEND_KEYS.iter().find(|(_, k)| **k == key).map(|(m, _)| *m).or(if &key == b"mron" { Some(BlendMode::Normal) } else { None }).unwrap_or_else(|| { warnings.push(format!("blend mode {:?} became Normal", String::from_utf8_lossy(&key))); BlendMode::Normal });
                let opacity = r.u8()?;
                let clipping = r.u8()? == 1;
                let flags = r.u8()?;
                r.skip(1)?;
                let extra_len = r.u32()? as usize;
                let extra_end = r.pos + extra_len;
                let mask_len = r.u32()? as usize;
                let mut mask = None;
                if mask_len >= 20 {
                    let mask_start = r.pos;
                    let (mt, ml, mb, mr) = (r.i32()?, r.i32()?, r.i32()?, r.i32()?);
                    let default = r.u8()?;
                    let mflags = r.u8()?;
                    mask = Some((mt, ml, mb, mr, default, mflags));
                    r.pos = mask_start + mask_len;
                } else { r.skip(mask_len)?; }
                let ranges_len = r.u32()? as usize;
                r.skip(ranges_len)?;
                let mut name = pascal_string(&mut r, 4)?;
                let mut section = 0u32;
                let mut adjustment = None;
                let mut unsupported_adjustment = false;
                while r.pos + 12 <= extra_end {
                    let sig = r.bytes(4)?;
                    if sig != b"8BIM" && sig != b"8B64" { break; }
                    let akey: [u8; 4] = r.bytes(4)?.try_into().unwrap();
                    let alen = r.u32()? as usize;
                    let astart = r.pos;
                    match &akey {
                        b"luni" => {
                            let n = r.u32()? as usize;
                            let units: Vec<u16> = (0..n).filter_map(|_| r.u16().ok()).collect();
                            let unicode = String::from_utf16_lossy(&units);
                            if !unicode.is_empty() { name = unicode.trim_end_matches('\0').to_string(); }
                        }
                        b"lsct" => { section = r.u32()?; }
                        b"brit" | b"blnc" | b"phfl" | b"vibA" | b"mixr" | b"thrs" | b"post" | b"nvrt" | b"selc" | b"clrL" | b"blwh" | b"SoCo" | b"GdFl" | b"PtFl" => { unsupported_adjustment = true; }
                        other => { if let Some((_, kind)) = ADJUSTMENT_KEYS.iter().find(|(k, _)| *k == other) { adjustment = Some(*kind); } }
                    }
                    r.pos = astart + alen + (alen & 1);
                }
                r.pos = extra_end;
                records.push((top, left, bottom, right, channel_specs, blend, opacity, clipping, flags, name, section, mask, adjustment, unsupported_adjustment));
            }
            // Channel image data follows, in the same order.
            for (top, left, bottom, right, channel_specs, blend, opacity, clipping, flags, name, section, mask, adjustment, unsupported_adjustment) in records {
                let mut channels = Vec::new();
                for (id, len) in channel_specs {
                    let start = r.pos;
                    let (rows, cols) = if id == -2 {
                        match mask { Some((mt, ml, mb, mr, _, _)) => ((mb - mt).max(0) as usize, (mr - ml).max(0) as usize), None => (0, 0) }
                    } else { ((bottom - top).max(0) as usize, (right - left).max(0) as usize) };
                    if len < 2 || rows == 0 || cols == 0 { r.pos = start + len; continue; }
                    let compression = r.u16()?;
                    let samples = read_channel(&mut r, rows, cols, depth, compression).with_context(|| format!("layer \"{name}\" channel {id}"))?;
                    channels.push((id, samples));
                    r.pos = start + len;
                }
                raw_layers.push(RawLayer { rect: (top, left, bottom, right), channels, blend, opacity, clipping, hidden: flags & 2 != 0, name, section, mask, adjustment, unsupported_adjustment });
            }
        }
    }
    r.pos = layer_mask_end;
    // The merged image, used when the file has no layers.
    let mut layers: Vec<Layer> = Vec::new();
    let mut images: HashMap<Uuid, ImageSurface> = HashMap::new();
    let mut masks: HashMap<Uuid, ImageSurface> = HashMap::new();
    let mut pixels_used: i64 = 0;
    if raw_layers.is_empty() {
        let compression = r.u16()?;
        let (rows, cols) = (height as usize, width as usize);
        let bytes = (depth as usize / 8).max(1);
        let mut planes: Vec<Vec<u8>> = Vec::new();
        match compression {
            0 => { for _ in 0..channels { planes.push(read_channel(&mut r, rows, cols, depth, 0)?); } }
            1 => {
                let mut counts = Vec::with_capacity(rows * channels);
                for _ in 0..rows * channels { counts.push(r.u16()? as usize); }
                for c in 0..channels {
                    let mut raw = vec![0u8; rows * cols * bytes];
                    for y in 0..rows { let packed = r.bytes(counts[c * rows + y])?; unpack_bits(packed, &mut raw[y * cols * bytes..(y + 1) * cols * bytes])?; }
                    planes.push(match depth { 8 => raw, 16 => raw.chunks_exact(2).map(|p| p[0]).collect(), _ => raw.chunks_exact(4).map(|p| (f32::from_be_bytes([p[0], p[1], p[2], p[3]]).clamp(0.0, 1.0) * 255.0).round() as u8).collect() });
                }
            }
            other => bail!("compression {other} is not supported yet"),
        }
        let (rgb, alpha) = if mode == 1 { (vec![&planes[0], &planes[0], &planes[0]], planes.get(1)) } else { (vec![&planes[0], &planes[1], &planes[2]], planes.get(3)) };
        let mut rgba = vec![0u8; rows * cols * 4];
        for i in 0..rows * cols { rgba[i * 4] = rgb[0][i]; rgba[i * 4 + 1] = rgb[1][i]; rgba[i * 4 + 2] = rgb[2][i]; rgba[i * 4 + 3] = alpha.map_or(255, |a| a[i]); }
        let id = Uuid::new_v4();
        layers.push(record(id, "Background", (0.0, 0.0, width as f64, height as f64), None, false, 255, BlendMode::Normal));
        layers[0].image_file = Some(format!("{}.png", upper(id)));
        images.insert(id, from_straight_rgba(&rgba, cols, rows)?);
    } else {
        // Bottom-up records; a folder's divider comes first, its own record last.
        let mut depth = 0usize;
        let mut order: Vec<Layer> = Vec::new();
        let mut base_by_parent: HashMap<Option<Uuid>, Option<Uuid>> = HashMap::new();
        let mut pending_parent: Vec<Vec<Uuid>> = vec![Vec::new()];
        for raw in raw_layers {
            match raw.section {
                3 => { depth += 1; if depth > 64 { bail!("folders nest too deeply"); } pending_parent.push(Vec::new()); continue; }
                1 | 2 => {
                    if depth == 0 { warnings.push(format!("folder \"{}\" closes nothing and was skipped", raw.name)); continue; }
                    depth -= 1;
                    let children = pending_parent.pop().unwrap_or_default();
                    let id = Uuid::new_v4();
                    let mut folder = record(id, &raw.name, (0.0, 0.0, width as f64, height as f64), None, raw.hidden, 255, BlendMode::Normal);
                    folder.is_group = Some(true);
                    if let Some(m) = raw.mask { if let Some(mask) = build_mask(&raw, m, (0, 0, height, width), &mut warnings)? { masks.insert(id, mask); folder.mask_file = Some(format!("{}.mask.png", upper(id))); if m.5 & 2 != 0 { folder.mask_enabled = Some(false); } } }
                    for child in children { if let Some(l) = order.iter_mut().find(|l| l.id == child) { l.parent_id = Some(id); } }
                    if let Some(p) = pending_parent.last_mut() { p.push(id); }
                    order.push(folder);
                    continue;
                }
                _ => {}
            }
            let (top, left, bottom, right) = raw.rect;
            let (w, h) = ((right - left).max(0), (bottom - top).max(0));
            let id = Uuid::new_v4();
            let is_adjustment = raw.adjustment.is_some();
            if raw.unsupported_adjustment && !is_adjustment { warnings.push(format!("adjustment layer \"{}\" is a kind this app does not have; it was skipped", raw.name)); continue; }
            let placement = if is_adjustment { (0.0, 0.0, width as f64, height as f64) } else { (left as f64, top as f64, w as f64, h as f64) };
            let mut layer = record(id, &raw.name, placement, None, raw.hidden, raw.opacity, raw.blend);
            if is_adjustment {
                let kind = raw.adjustment.unwrap();
                layer.adjustment = crate::filters::Adjustment::from_kind(kind).map(|a| a.to_record());
                warnings.push(format!("adjustment layer \"{}\" ({kind}) was opened with default settings; Photoshop's values are not read yet", raw.name));
            } else {
                if w == 0 || h == 0 { warnings.push(format!("layer \"{}\" has no pixels and was skipped", raw.name)); continue; }
                pixels_used += w as i64 * h as i64;
                if pixels_used > 100_000_000 { bail!("The file's layers exceed the 100-megapixel budget."); }
                let n = (w * h) as usize;
                let plane = |id: i16| raw.channels.iter().find(|(c, _)| *c == id).map(|(_, d)| d.as_slice());
                let (r_, g_, b_, a_) = (plane(0), plane(1), plane(2), plane(-1));
                let mut rgba = vec![0u8; n * 4];
                for i in 0..n {
                    let (cr, cg, cb) = if mode == 1 { let v = r_.map_or(0, |p| p[i]); (v, v, v) } else { (r_.map_or(0, |p| p[i]), g_.map_or(0, |p| p[i]), b_.map_or(0, |p| p[i])) };
                    rgba[i * 4] = cr; rgba[i * 4 + 1] = cg; rgba[i * 4 + 2] = cb; rgba[i * 4 + 3] = a_.map_or(255, |p| p[i]);
                }
                images.insert(id, from_straight_rgba(&rgba, w as usize, h as usize)?);
                layer.image_file = Some(format!("{}.png", upper(id)));
            }
            if let Some(m) = raw.mask {
                let over = if is_adjustment { (0, 0, height, width) } else { raw.rect };
                if let Some(mask) = build_mask(&raw, m, over, &mut warnings)? { masks.insert(id, mask); layer.mask_file = Some(format!("{}.mask.png", upper(id))); if m.5 & 2 != 0 { layer.mask_enabled = Some(false); } }
            }
            // Clipping: to the nearest unclipped pixel layer below in the same group.
            let parent_key = pending_parent.len() - 1;
            if raw.clipping {
                let base = base_by_parent.get(&Some(Uuid::from_u128(parent_key as u128))).copied().flatten();
                match base { Some(b) => layer.mask_source_id = Some(b), None => warnings.push(format!("layer \"{}\" is clipped to nothing that was opened", raw.name)) }
            } else if !is_adjustment {
                base_by_parent.insert(Some(Uuid::from_u128(parent_key as u128)), Some(id));
            }
            if let Some(p) = pending_parent.last_mut() { p.push(id); }
            order.push(layer);
        }
        layers = order;
    }
    let manifest = Manifest { format: crate::format::FORMAT.into(), version: crate::format::SAVE_VERSION, color_space: "sRGB".into(), resolution: Some(resolution), document_id: Uuid::new_v4(), width: width as i64, height: height as i64, active_layer_id: layers.last().map(|l| l.id), layers };
    // Run the file's rules over what was built, so a PSD can never make an invalid project.
    let manifest = Manifest::parse(&serde_json::to_vec(&manifest)?)?;
    Ok((Project { path: std::path::PathBuf::new(), manifest, images, masks }, warnings))
}

fn record(id: Uuid, name: &str, placement: (f64, f64, f64, f64), parent: Option<Uuid>, hidden: bool, opacity: u8, blend: BlendMode) -> Layer {
    let name = if name.trim().is_empty() { "Layer".to_string() } else { name.chars().take(4000).collect() };
    Layer {
        id, name, is_visible: !hidden,
        transform: Transform { origin: Point(placement.0, placement.1), size: Size(placement.2.max(1.0), placement.3.max(1.0)), rotation: 0.0, flip_x: false, flip_y: false, sampling: Sampling::High },
        image_file: None, parent_id: parent, is_group: None, opacity: if opacity == 255 { None } else { Some(opacity as f64 / 255.0) },
        blend_mode: if blend == BlendMode::Normal { None } else { Some(blend) }, mask_file: None, mask_enabled: None, mask_source_id: None, adjustment: None, mask_placement: None, mask_linked: None, shape: None,
    }
}

/// The user mask channel placed over the layer's rectangle (top, left, bottom, right), the mask's default
/// color beyond its own rectangle, as an A8 of the layer's size.
fn build_mask(raw: &RawLayer, m: (i32, i32, i32, i32, u8, u8), over: (i32, i32, i32, i32), warnings: &mut Vec<String>) -> Result<Option<ImageSurface>> {
    let (mt, ml, mb, mr, default, _flags) = m;
    let (w, h) = ((over.3 - over.1).max(1), (over.2 - over.0).max(1));
    let Some((_, samples)) = raw.channels.iter().find(|(c, _)| *c == -2) else { return Ok(None) };
    if w > 30_000 || h > 30_000 { warnings.push(format!("mask on \"{}\" is too large and was skipped", raw.name)); return Ok(None); }
    let stride = Format::A8.stride_for_width(w as u32)? as usize;
    let mut data = vec![default; stride * h as usize];
    let (mw, mh) = ((mr - ml).max(0), (mb - mt).max(0));
    for y in 0..mh {
        let ty = mt + y - over.0;
        if ty < 0 || ty >= h { continue; }
        for x in 0..mw {
            let tx = ml + x - over.1;
            if tx < 0 || tx >= w { continue; }
            data[ty as usize * stride + tx as usize] = samples[(y * mw + x) as usize];
        }
    }
    Ok(Some(a8_from_data(w, h, data, stride as i32)?))
}

// MARK: Writing

struct Writer { out: Vec<u8> }
impl Writer {
    fn u8(&mut self, v: u8) { self.out.push(v); }
    fn u16(&mut self, v: u16) { self.out.extend_from_slice(&v.to_be_bytes()); }
    fn i16(&mut self, v: i16) { self.u16(v as u16); }
    fn u32(&mut self, v: u32) { self.out.extend_from_slice(&v.to_be_bytes()); }
    fn i32(&mut self, v: i32) { self.u32(v as u32); }
    fn bytes(&mut self, b: &[u8]) { self.out.extend_from_slice(b); }
    fn pascal(&mut self, s: &str, pad: usize) {
        let b: Vec<u8> = s.bytes().take(255).collect();
        self.u8(b.len() as u8);
        self.bytes(&b);
        let total = 1 + b.len();
        for _ in total..total.div_ceil(pad) * pad { self.u8(0); }
    }
}

/// A channel plane packed row by row: the row counts, then the data.
fn rle_channel(plane: &[u8], rows: usize, cols: usize) -> (Vec<u8>, Vec<u8>) {
    let mut counts = Vec::with_capacity(rows * 2);
    let mut data = Vec::with_capacity(plane.len() / 2);
    for y in 0..rows {
        let start = data.len();
        pack_bits(&plane[y * cols..(y + 1) * cols], &mut data);
        counts.extend_from_slice(&((data.len() - start) as u16).to_be_bytes());
    }
    (counts, data)
}

struct Placed { rect: (i32, i32, i32, i32), planes: Vec<(i16, Vec<u8>)>, mask: Option<((i32, i32, i32, i32), Vec<u8>)> }

/// Writes the document as a PSD. Returns what was left out.
pub fn write(document: &mut crate::document::Document, path: &Path) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    let (width, height) = (document.width(), document.height());
    let resolution = document.renderer.resolution();
    let layers = document.renderer.layers().to_vec();
    // Records bottom to top: a folder is its divider, its contents, then its own record.
    enum Rec { Divider, Folder(Layer, Option<((i32, i32, i32, i32), Vec<u8>)>), Pixel(Layer, Placed) }
    let mut recs: Vec<Rec> = Vec::new();
    let children: HashMap<Option<Uuid>, Vec<Layer>> = { let mut m: HashMap<Option<Uuid>, Vec<Layer>> = HashMap::new(); for l in &layers { m.entry(l.parent_id).or_default().push(l.clone()); } m };
    fn emit(parent: Option<Uuid>, children: &HashMap<Option<Uuid>, Vec<Layer>>, document: &mut crate::document::Document, recs: &mut Vec<Rec>, warnings: &mut Vec<String>, depth: usize) -> Result<()> {
        if depth > 64 { return Ok(()); }
        for layer in children.get(&parent).cloned().unwrap_or_default() {
            if layer.is_group() {
                recs.push(Rec::Divider);
                emit(Some(layer.id), children, document, recs, warnings, depth + 1)?;
                let mask = rasterize_mask(document, &layer)?;
                recs.push(Rec::Folder(layer, mask));
            } else if layer.adjustment.is_some() {
                warnings.push(format!("adjustment layer \"{}\" was left out; the merged image includes its effect", layer.name));
            } else if let Some(placed) = rasterize(document, &layer)? {
                recs.push(Rec::Pixel(layer, placed));
            } else {
                warnings.push(format!("blank layer \"{}\" was left out", layer.name));
            }
        }
        Ok(())
    }
    emit(None, &children, document, &mut recs, &mut warnings, 0)?;
    for layer in &layers {
        if let Some(source) = layer.mask_source_id {
            // PSD clips to whatever unclipped layer lies directly beneath; anything else can't be expressed.
            let siblings: Vec<&Layer> = layers.iter().filter(|l| l.parent_id == layer.parent_id).collect();
            let index = siblings.iter().position(|l| l.id == layer.id).unwrap_or(0);
            let base_below = siblings[..index].iter().rev().find(|l| l.mask_source_id.is_none()).map(|l| l.id);
            if base_below != Some(source) { warnings.push(format!("layer \"{}\" clips to a layer that is not directly beneath it; Photoshop clips it to the one that is", layer.name)); }
        }
    }
    let mut w = Writer { out: Vec::with_capacity(1 << 20) };
    w.bytes(b"8BPS"); w.u16(1); w.bytes(&[0; 6]); w.u16(4); w.u32(height as u32); w.u32(width as u32); w.u16(8); w.u16(3);
    w.u32(0);
    // Resolution resource.
    let mut res = Writer { out: Vec::new() };
    res.bytes(b"8BIM"); res.u16(0x03ED); res.u8(0); res.u8(0); res.u32(16);
    let fixed = (resolution * 65536.0).round() as u32;
    res.u32(fixed); res.u16(1); res.u16(1); res.u32(fixed); res.u16(1); res.u16(1);
    w.u32(res.out.len() as u32); w.bytes(&res.out);
    // Layer records.
    let mut records = Writer { out: Vec::new() };
    let mut channel_data = Writer { out: Vec::new() };
    records.i16(recs.len() as i16);
    for rec in &recs {
        let (layer, rect, planes, mask, section, name): (Option<&Layer>, (i32, i32, i32, i32), Vec<(i16, &[u8])>, Option<&((i32, i32, i32, i32), Vec<u8>)>, u32, String) = match rec {
            Rec::Divider => (None, (0, 0, 0, 0), vec![(-1, &[][..])], None, 3, "</Layer group>".into()),
            Rec::Folder(l, m) => (Some(l), (0, 0, 0, 0), vec![(-1, &[][..])], m.as_ref(), 1, l.name.clone()),
            Rec::Pixel(l, p) => (Some(l), p.rect, p.planes.iter().map(|(id, d)| (*id, d.as_slice())).collect(), p.mask.as_ref(), 0, l.name.clone()),
        };
        let (top, left, bottom, right) = rect;
        let (rows, cols) = ((bottom - top).max(0) as usize, (right - left).max(0) as usize);
        records.i32(top); records.i32(left); records.i32(bottom); records.i32(right);
        let mut chans: Vec<(i16, Vec<u8>)> = Vec::new();
        for (id, plane) in &planes {
            if rows == 0 || cols == 0 { chans.push((*id, vec![0, 0])); continue; }
            let (counts, data) = rle_channel(plane, rows, cols);
            let mut c = vec![0, 1]; c.extend_from_slice(&counts); c.extend_from_slice(&data);
            chans.push((*id, c));
        }
        if let Some((mrect, mplane)) = mask {
            let (mrows, mcols) = ((mrect.2 - mrect.0).max(0) as usize, (mrect.3 - mrect.1).max(0) as usize);
            if mrows > 0 && mcols > 0 {
                let (counts, data) = rle_channel(mplane, mrows, mcols);
                let mut c = vec![0, 1]; c.extend_from_slice(&counts); c.extend_from_slice(&data);
                chans.push((-2, c));
            }
        }
        records.u16(chans.len() as u16);
        for (id, c) in &chans { records.i16(*id); records.u32(c.len() as u32); }
        records.bytes(b"8BIM");
        let blend = layer.map(|l| l.blend_mode()).unwrap_or(BlendMode::Normal);
        records.bytes(BLEND_KEYS.iter().find(|(m, _)| *m == blend).map(|(_, k)| *k).unwrap_or(b"norm"));
        records.u8(layer.map_or(255, |l| (l.opacity() * 255.0).round() as u8));
        let clipped = layer.is_some_and(|l| l.mask_source_id.is_some());
        records.u8(clipped as u8);
        records.u8(if layer.is_some_and(|l| !l.is_visible) { 2 } else { 0 } | 8);
        records.u8(0);
        let mut extra = Writer { out: Vec::new() };
        match mask {
            Some((mrect, _)) => {
                extra.u32(20);
                extra.i32(mrect.0); extra.i32(mrect.1); extra.i32(mrect.2); extra.i32(mrect.3);
                extra.u8(255);
                extra.u8(if layer.is_some_and(|l| l.mask_file.is_some() && !l.mask_enabled()) { 2 } else { 0 });
                extra.u16(0);
            }
            None => extra.u32(0),
        }
        extra.u32(0);
        extra.pascal(&name, 4);
        // Unicode name, and the section marker for folders.
        let units: Vec<u16> = name.encode_utf16().collect();
        extra.bytes(b"8BIM"); extra.bytes(b"luni"); extra.u32(4 + units.len() as u32 * 2 + if units.len() % 2 == 1 { 2 } else { 0 });
        extra.u32(units.len() as u32); for u in &units { extra.u16(*u); } if units.len() % 2 == 1 { extra.u16(0); }
        if section != 0 { extra.bytes(b"8BIM"); extra.bytes(b"lsct"); extra.u32(12); extra.u32(section); extra.bytes(b"8BIM"); extra.bytes(b"pass"); }
        records.u32(extra.out.len() as u32);
        records.bytes(&extra.out);
        for (_, c) in chans { channel_data.bytes(&c); }
    }
    let mut layer_info = Writer { out: Vec::new() };
    layer_info.bytes(&records.out);
    layer_info.bytes(&channel_data.out);
    if layer_info.out.len() % 2 == 1 { layer_info.u8(0); }
    let mut layer_mask = Writer { out: Vec::new() };
    layer_mask.u32(layer_info.out.len() as u32);
    layer_mask.bytes(&layer_info.out);
    layer_mask.u32(0);
    w.u32(layer_mask.out.len() as u32);
    w.bytes(&layer_mask.out);
    // The merged image, RGBA planes, run-length packed with all row counts first.
    let flat = document.renderer.render_flat()?;
    let (rgba, cw, ch) = crate::png_io::straight_rgba(&flat)?;
    let mut counts_all = Vec::new();
    let mut data_all = Vec::new();
    for c in 0..4 {
        let plane: Vec<u8> = (0..cw * ch).map(|i| rgba[i * 4 + c]).collect();
        let (counts, data) = rle_channel(&plane, ch, cw);
        counts_all.extend_from_slice(&counts);
        data_all.extend_from_slice(&data);
    }
    w.u16(1);
    w.bytes(&counts_all);
    w.bytes(&data_all);
    let mut file = std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    file.write_all(&w.out)?;
    Ok(warnings)
}

/// A layer's pixels at its document placement: the rectangle and the A, R, G, B planes (straight alpha).
fn rasterize(document: &mut crate::document::Document, layer: &Layer) -> Result<Option<Placed>> {
    if !document.renderer.has_image(layer.id) { return Ok(None); }
    let (x0, y0, x1, y1) = layer.transform.bounds();
    let (left, top, right, bottom) = (x0.floor() as i32, y0.floor() as i32, x1.ceil() as i32, y1.ceil() as i32);
    let (w, h) = ((right - left).max(1), (bottom - top).max(1));
    if w > 30_000 || h > 30_000 { bail!("layer \"{}\" is too large to export", layer.name); }
    let surface = crate::raster::new_argb(w, h)?;
    {
        let cr = cairo::Context::new(&surface)?;
        cr.translate(-(left as f64), -(top as f64));
        document.renderer.draw_layer_plain(layer.id, &cr)?;
    }
    let (rgba, cw, ch) = crate::png_io::straight_rgba(&surface)?;
    let n = cw * ch;
    let planes = vec![
        (-1, (0..n).map(|i| rgba[i * 4 + 3]).collect()),
        (0, (0..n).map(|i| rgba[i * 4]).collect()),
        (1, (0..n).map(|i| rgba[i * 4 + 1]).collect()),
        (2, (0..n).map(|i| rgba[i * 4 + 2]).collect()),
    ];
    let mask = rasterize_mask(document, layer)?;
    Ok(Some(Placed { rect: (top, left, bottom, right), planes, mask }))
}

/// A layer's or folder's mask as coverage over the placement's bounds, with its rectangle.
fn rasterize_mask(document: &mut crate::document::Document, layer: &Layer) -> Result<Option<((i32, i32, i32, i32), Vec<u8>)>> {
    if document.renderer.mask(layer.id).is_none() { return Ok(None); }
    let placement = layer.mask_placement.unwrap_or(layer.transform);
    let (x0, y0, x1, y1) = placement.bounds();
    let (left, top, right, bottom) = (x0.floor() as i32, y0.floor() as i32, x1.ceil() as i32, y1.ceil() as i32);
    let (w, h) = ((right - left).max(1), (bottom - top).max(1));
    let a8 = crate::raster::a8_filled(w, h, 255)?;
    {
        let cr = cairo::Context::new(&a8)?;
        cr.translate(-(left as f64), -(top as f64));
        document.renderer.draw_mask_plain(layer.id, &cr)?;
    }
    let plane = with_bytes(&a8, |data, stride| { let mut out = vec![0u8; (w * h) as usize]; for y in 0..h as usize { out[y * w as usize..(y + 1) * w as usize].copy_from_slice(&data[y * stride..y * stride + w as usize]); } out })?;
    Ok(Some(((top, left, bottom, right), plane)))
}

#[allow(dead_code)]
fn read_all(path: &Path) -> Result<Vec<u8>> { let mut v = Vec::new(); std::fs::File::open(path)?.read_to_end(&mut v)?; Ok(v) }
