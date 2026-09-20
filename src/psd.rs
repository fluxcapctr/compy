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
    /// A type layer's text and setting, from its TySh block; the pixels in the file stay until it is edited.
    text: Option<crate::text::TextStyle>,
    /// Layer effects, from the lfx2 block.
    effects: Option<crate::effects::Effects>,
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

/// A layer or mask rectangle's rows and columns, refusing inverted, oversized or absurdly placed ones.
fn rect_dims(top: i32, left: i32, bottom: i32, right: i32) -> Result<(usize, usize)> {
    const REACH: u32 = 1 << 24;
    if [top, left, bottom, right].iter().any(|v| v.unsigned_abs() > REACH) { bail!("a layer rectangle is out of range"); }
    let rows = bottom.checked_sub(top).filter(|v| (0..=30_000).contains(v)).ok_or_else(|| anyhow::anyhow!("a layer rectangle is inverted or taller than 30,000 pixels"))?;
    let cols = right.checked_sub(left).filter(|v| (0..=30_000).contains(v)).ok_or_else(|| anyhow::anyhow!("a layer rectangle is inverted or wider than 30,000 pixels"))?;
    Ok((rows as usize, cols as usize))
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
    if !(1..=56).contains(&channels) { bail!("The file declares {channels} channels; 1 to 56 are valid."); }
    let height = r.u32()? as i32;
    let width = r.u32()? as i32;
    let depth = r.u16()?;
    let mode = r.u16()?;
    if !(1..=30_000).contains(&width) || !(1..=30_000).contains(&height) { bail!("The canvas is {width} x {height}; sides run from 1 to 30,000 pixels."); }
    match mode { 1 | 3 => {} 0 => bail!("Bitmap mode files are not supported."), 2 => bail!("Indexed color files are not supported; convert to RGB first."), 4 => bail!("CMYK files are not supported; convert to RGB first."), 7 | 8 => bail!("Multichannel and Duotone files are not supported."), 9 => bail!("Lab files are not supported; convert to RGB first."), other => bail!("Color mode {other} is not supported.") }
    if ![8, 16, 32].contains(&depth) { bail!("{depth}-bit files are not supported."); }
    if mode == 3 && channels < 3 { bail!("An RGB file needs at least three channels; this one declares {channels}."); }
    if width as i64 * height as i64 > 100_000_000 { bail!("The canvas is {width} x {height}, past the 100-megapixel budget."); }
    let sample_bytes = (depth as usize / 8).max(1);
    // Everything decoded, in samples, counted before any buffer is made.
    let mut samples_budget: usize = 400_000_000;
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
        r.pos = start;
        r.skip(size + (size & 1))?;
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
            let mut records = Vec::with_capacity(count.min(1024));
            for _ in 0..count {
                let (top, left, bottom, right) = (r.i32()?, r.i32()?, r.i32()?, r.i32()?);
                rect_dims(top, left, bottom, right)?;
                let channel_count = r.u16()? as usize;
                if channel_count > 56 { bail!("a layer declares {channel_count} channels"); }
                let mut channel_specs = Vec::with_capacity(channel_count);
                for _ in 0..channel_count {
                    let spec = (r.i16()?, r.u32()? as usize);
                    if spec.1 > data.len() { bail!("a channel claims more data than the file holds"); }
                    channel_specs.push(spec);
                }
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
                    rect_dims(mt, ml, mb, mr)?;
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
                let mut text = None;
                let mut effects = None;
                while r.pos + 12 <= extra_end {
                    let sig = r.bytes(4)?;
                    if sig != b"8BIM" && sig != b"8B64" { break; }
                    let akey: [u8; 4] = r.bytes(4)?.try_into().unwrap();
                    let alen = r.u32()? as usize;
                    let astart = r.pos;
                    match &akey {
                        b"luni" => {
                            let n = r.u32()? as usize;
                            if alen < 4 || n > (alen - 4) / 2 { bail!("a layer name runs past its block"); }
                            let mut units = Vec::with_capacity(n);
                            for _ in 0..n { units.push(r.u16()?); }
                            let unicode = String::from_utf16_lossy(&units);
                            if !unicode.is_empty() { name = unicode.trim_end_matches('\0').to_string(); }
                        }
                        b"lsct" => { section = r.u32()?; }
                        b"TySh" => { match read_tysh(&r.data[astart..(astart + alen).min(r.data.len())], resolution) { Ok(t) => text = Some(t), Err(e) => warnings.push(format!("type layer \"{name}\" opened as pixels: {e:#}")) } }
                        b"lfx2" => { match read_lfx2(&r.data[astart..(astart + alen).min(r.data.len())]) { Ok(e) => effects = e, Err(e) => warnings.push(format!("layer style on \"{name}\" was not read: {e:#}")) } }
                        b"brit" | b"blnc" | b"phfl" | b"vibA" | b"mixr" | b"thrs" | b"post" | b"nvrt" | b"selc" | b"clrL" | b"blwh" | b"SoCo" | b"GdFl" | b"PtFl" => { unsupported_adjustment = true; }
                        other => { if let Some((_, kind)) = ADJUSTMENT_KEYS.iter().find(|(k, _)| *k == other) { adjustment = Some(*kind); } }
                    }
                    r.pos = astart;
                    r.skip(alen + (alen & 1))?;
                }
                r.pos = extra_end;
                records.push((top, left, bottom, right, channel_specs, blend, opacity, clipping, flags, name, section, mask, adjustment, unsupported_adjustment, text, effects));
            }
            // Channel image data follows, in the same order.
            for (top, left, bottom, right, channel_specs, blend, opacity, clipping, flags, name, section, mask, adjustment, unsupported_adjustment, text, effects) in records {
                let mut channels = Vec::new();
                for (id, len) in channel_specs {
                    let start = r.pos;
                    let (rows, cols) = if id == -2 {
                        match mask { Some((mt, ml, mb, mr, _, _)) => rect_dims(mt, ml, mb, mr)?, None => (0, 0) }
                    } else { rect_dims(top, left, bottom, right)? };
                    if len > data.len() - start { bail!("layer \"{name}\" channel {id} runs past the end of the file"); }
                    if len < 2 || rows == 0 || cols == 0 { r.skip(len)?; continue; }
                    let compression = r.u16()?;
                    // Enough declared bytes for what the channel claims to hold, and a share of the budget.
                    let needed = match compression { 0 => rows * cols * sample_bytes, 1 => rows * 2, _ => 0 } + 2;
                    if len < needed { bail!("layer \"{name}\" channel {id} is truncated"); }
                    samples_budget = samples_budget.checked_sub(rows * cols).ok_or_else(|| anyhow::anyhow!("The file's layers exceed the decoding budget."))?;
                    let samples = read_channel(&mut r, rows, cols, depth, compression).with_context(|| format!("layer \"{name}\" channel {id}"))?;
                    channels.push((id, samples));
                    r.pos = start + len;
                }
                raw_layers.push(RawLayer { rect: (top, left, bottom, right), channels, blend, opacity, clipping, hidden: flags & 2 != 0, name, section, mask, adjustment, unsupported_adjustment, text, effects });
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
        let bytes = sample_bytes;
        let needed = match compression { 0 => rows * cols * bytes * channels, 1 => rows * channels * 2, _ => 0 };
        if data.len() - r.pos < needed { bail!("The merged image is truncated."); }
        samples_budget.checked_sub(rows * cols * channels).ok_or_else(|| anyhow::anyhow!("The merged image exceeds the decoding budget."))?;
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
                if let Some(style) = &raw.text {
                    // Editable again: the file's own pixels show until the text is changed, then the app's
                    // rendering takes over (the font may be substituted when it is not installed).
                    layer.text = Some(style.to_record());
                    if !crate::text::families().iter().any(|f| f.eq_ignore_ascii_case(&style.family)) { warnings.push(format!("type layer \"{}\" uses the font {}, which is not installed; editing it will substitute another", raw.name, style.family)); }
                }
            }
            if let Some(effects) = &raw.effects { layer.effects = Some(effects.to_record()); }
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
        blend_mode: if blend == BlendMode::Normal { None } else { Some(blend) }, mask_file: None, mask_enabled: None, mask_source_id: None, adjustment: None, mask_placement: None, mask_linked: None, shape: None, text: None, effects: None,
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
                if layer.text.is_some() { warnings.push(format!("type layer \"{}\" was written as pixels; Photoshop shows it but cannot edit its text", layer.name)); }
                if layer.effects.as_ref().and_then(crate::effects::Effects::from_record).is_some_and(|e| e.blend_if().is_some()) { warnings.push(format!("Blend If on \"{}\" was left out; Photoshop keeps that in blending ranges, which are not written yet", layer.name)); }
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
        // Layer effects go out as Photoshop's own lfx2 descriptor.
        if let Some(effects) = layer.and_then(|l| l.effects.as_ref()).and_then(crate::effects::Effects::from_record).filter(|e| e.is_active()) {
            let mut body = Vec::new();
            body.extend_from_slice(&0u32.to_be_bytes());
            body.extend_from_slice(&16u32.to_be_bytes());
            body.extend_from_slice(&crate::psd_desc::write(&effects_descriptor(&effects)));
            while body.len() % 4 != 0 { body.push(0); }
            extra.bytes(b"8BIM"); extra.bytes(b"lfx2"); extra.u32(body.len() as u32); extra.bytes(&body);
        }
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

// MARK: Layer effects and type, in Photoshop's descriptors

use crate::effects::{Bevel, Effects, Glow, Overlay, Shadow, Stroke};
use crate::psd_desc::{Descriptor, Item, rgb};

fn prc(v: f64) -> Item { Item::Unit("#Prc".into(), v) }
fn pxl(v: f64) -> Item { Item::Unit("#Pxl".into(), v) }
fn ang(v: f64) -> Item { Item::Unit("#Ang".into(), v) }
fn blend_item(key: &str) -> Item { Item::Enum("BlnM".into(), key.into()) }

/// The lfx2 descriptor for a layer's effects, as Photoshop lays it out.
pub fn effects_descriptor(e: &Effects) -> Descriptor {
    let mut d = Descriptor::new("null");
    d.push("Scl ", prc(100.0)).push("masterFXSwitch", Item::Bool(true));
    let shadow = |s: &Shadow, drop: bool| {
        let mut o = Descriptor::new(if drop { "DrSh" } else { "IrSh" });
        o.push("enab", Item::Bool(s.enabled)).push("present", Item::Bool(true)).push("showInDialog", Item::Bool(true)).push("Md  ", blend_item("Mltp")).push("Clr ", rgb(s.color)).push("Opct", prc(s.opacity * 100.0)).push("uglg", Item::Bool(false)).push("lagl", ang(s.angle)).push("Dstn", pxl(s.distance)).push("Ckmt", pxl(0.0)).push("blur", pxl(s.size)).push("Nose", prc(0.0)).push("AntA", Item::Bool(false));
        if drop { o.push("layerConceals", Item::Bool(true)); }
        Item::Desc(o)
    };
    let glow = |g: &Glow, outer: bool| {
        let mut o = Descriptor::new(if outer { "OrGl" } else { "IrGl" });
        o.push("enab", Item::Bool(g.enabled)).push("present", Item::Bool(true)).push("showInDialog", Item::Bool(true)).push("Md  ", blend_item("Scrn")).push("Clr ", rgb(g.color)).push("Opct", prc(g.opacity * 100.0)).push("GlwT", Item::Enum("BETE".into(), "SfBL".into())).push("Ckmt", pxl(0.0)).push("blur", pxl(g.size)).push("Nose", prc(0.0)).push("ShdN", prc(0.0)).push("AntA", Item::Bool(false)).push("Inpr", prc(50.0));
        if !outer { o.push("glwS", Item::Enum("IGSr".into(), "SrcE".into())); }
        Item::Desc(o)
    };
    if let Some(s) = &e.drop_shadow { d.push("DrSh", shadow(s, true)); }
    if let Some(s) = &e.inner_shadow { d.push("IrSh", shadow(s, false)); }
    if let Some(g) = &e.outer_glow { d.push("OrGl", glow(g, true)); }
    if let Some(g) = &e.inner_glow { d.push("IrGl", glow(g, false)); }
    if let Some(b) = &e.bevel {
        let mut o = Descriptor::new("ebbl");
        o.push("enab", Item::Bool(b.enabled)).push("present", Item::Bool(true)).push("showInDialog", Item::Bool(true)).push("hglM", blend_item("Scrn")).push("hglC", rgb([1.0; 3])).push("hglO", prc(b.highlight_opacity * 100.0)).push("sdwM", blend_item("Mltp")).push("sdwC", rgb([0.0; 3])).push("sdwO", prc(b.shadow_opacity * 100.0)).push("bvlT", Item::Enum("bvlT".into(), "SfBL".into())).push("bvlS", Item::Enum("BESl".into(), match b.style { 1 => "OtrB", 2 => "Embs", _ => "InrB" }.into())).push("uglg", Item::Bool(false)).push("lagl", ang(b.angle)).push("Lald", ang(b.altitude)).push("srgR", prc(b.depth)).push("blur", pxl(b.size)).push("bvlD", Item::Enum("BESs".into(), "In  ".into())).push("Sftn", pxl(0.0)).push("useShape", Item::Bool(false)).push("useTexture", Item::Bool(false));
        d.push("ebbl", Item::Desc(o));
    }
    if let Some(o) = &e.color_overlay {
        let mut f = Descriptor::new("SoFi");
        f.push("enab", Item::Bool(o.enabled)).push("present", Item::Bool(true)).push("showInDialog", Item::Bool(true)).push("Md  ", blend_item("Nrml")).push("Opct", prc(o.opacity * 100.0)).push("Clr ", rgb(o.color));
        d.push("SoFi", Item::Desc(f));
    }
    if let Some(s) = &e.stroke {
        let mut f = Descriptor::new("FrFX");
        f.push("enab", Item::Bool(s.enabled)).push("present", Item::Bool(true)).push("showInDialog", Item::Bool(true)).push("Styl", Item::Enum("FStl".into(), match s.position { 1 => "InsF", 2 => "CtrF", _ => "OutF" }.into())).push("PntT", Item::Enum("FrFl".into(), "SClr".into())).push("Md  ", blend_item("Nrml")).push("Opct", prc(s.opacity * 100.0)).push("Sz  ", pxl(s.size)).push("Clr ", rgb(s.color));
        d.push("FrFX", Item::Desc(f));
    }
    d
}

/// The effects an lfx2 descriptor describes; None when it carries nothing this app draws.
pub fn effects_from_descriptor(d: &Descriptor) -> Option<Effects> {
    let master = d.boolean("masterFXSwitch").unwrap_or(true);
    let scale = d.number("Scl ").unwrap_or(100.0) / 100.0;
    let on = |o: &Descriptor| master && o.boolean("enab").unwrap_or(true);
    let mut e = Effects::default();
    let shadow = |o: &Descriptor| Shadow { enabled: on(o), color: o.color("Clr ").unwrap_or([0.0; 3]), opacity: o.number("Opct").unwrap_or(75.0) / 100.0, angle: o.number("lagl").unwrap_or(120.0), distance: o.number("Dstn").unwrap_or(5.0) * scale, size: o.number("blur").unwrap_or(5.0) * scale };
    let glow = |o: &Descriptor, outer: bool| Glow { enabled: on(o), color: o.color("Clr ").unwrap_or(if outer { [1.0, 1.0, 0.75] } else { [1.0, 1.0, 0.75] }), opacity: o.number("Opct").unwrap_or(75.0) / 100.0, size: o.number("blur").unwrap_or(5.0) * scale };
    if let Some(o) = d.desc("DrSh") { e.drop_shadow = Some(shadow(o)); }
    if let Some(o) = d.desc("IrSh") { e.inner_shadow = Some(shadow(o)); }
    if let Some(o) = d.desc("OrGl") { e.outer_glow = Some(glow(o, true)); }
    if let Some(o) = d.desc("IrGl") { e.inner_glow = Some(glow(o, false)); }
    if let Some(o) = d.desc("ebbl") {
        let style = match o.enum_value("bvlS") { Some("OtrB") => 1, Some("Embs") | Some("PlEb") => 2, _ => 0 };
        e.bevel = Some(Bevel { enabled: on(o), style, depth: o.number("srgR").unwrap_or(100.0), size: o.number("blur").unwrap_or(5.0) * scale, angle: o.number("lagl").unwrap_or(120.0), altitude: o.number("Lald").unwrap_or(30.0), highlight_opacity: o.number("hglO").unwrap_or(75.0) / 100.0, shadow_opacity: o.number("sdwO").unwrap_or(75.0) / 100.0 });
    }
    if let Some(o) = d.desc("SoFi") { e.color_overlay = Some(Overlay { enabled: on(o), color: o.color("Clr ").unwrap_or([1.0, 0.0, 0.0]), opacity: o.number("Opct").unwrap_or(100.0) / 100.0 }); }
    if let Some(o) = d.desc("FrFX") {
        let position = match o.enum_value("Styl") { Some("InsF") => 1, Some("CtrF") => 2, _ => 0 };
        e.stroke = Some(Stroke { enabled: on(o), size: o.number("Sz  ").unwrap_or(3.0) * scale, position, color: o.color("Clr ").unwrap_or([1.0, 0.0, 0.0]), opacity: o.number("Opct").unwrap_or(100.0) / 100.0 });
    }
    if e == Effects::default() { None } else { Some(e) }
}

/// The lfx2 block: version, descriptor version, descriptor.
fn read_lfx2(block: &[u8]) -> Result<Option<Effects>> {
    if block.len() < 8 { bail!("the block is too short"); }
    let descriptor_version = u32::from_be_bytes(block[4..8].try_into().unwrap());
    if descriptor_version != 16 { bail!("descriptor version {descriptor_version}"); }
    let (d, _) = crate::psd_desc::parse(&block[8..])?;
    Ok(effects_from_descriptor(&d))
}

/// The TySh block: the type's transform, its descriptor (text and engine data), and the warp.
fn read_tysh(block: &[u8], resolution: f64) -> Result<crate::text::TextStyle> {
    if block.len() < 2 + 48 + 2 + 4 { bail!("the block is too short"); }
    let version = u16::from_be_bytes([block[0], block[1]]);
    if version != 1 { bail!("type tool version {version}"); }
    let f = |i: usize| f64::from_be_bytes(block[2 + i * 8..10 + i * 8].try_into().unwrap());
    let (xx, yy) = (f(0), f(3));
    let text_version = u16::from_be_bytes([block[50], block[51]]);
    if text_version != 50 { bail!("text version {text_version}"); }
    let (d, _) = crate::psd_desc::parse(&block[56..])?;
    let text = d.text("Txt ").unwrap_or("").replace('\r', "\n");
    let engine = d.data("EngineData").unwrap_or(&[]);
    let mut style = crate::text::TextStyle { text, ..crate::text::TextStyle::default() };
    // Points at the document's resolution, through the type's own scale.
    let scale = if yy.abs() > 1e-6 { yy.abs() } else if xx.abs() > 1e-6 { xx.abs() } else { 1.0 };
    let to_px = scale * resolution / 72.0;
    if let Some(size) = engine_number(engine, b"/FontSize") { style.size = (size * to_px).clamp(1.0, 2000.0); }
    if let Some(values) = engine_values(engine, b"/FillColor") { if values.len() >= 4 { style.color = [values[1].clamp(0.0, 1.0), values[2].clamp(0.0, 1.0), values[3].clamp(0.0, 1.0)]; } }
    let font_index = engine_number(engine, b"/Font ").unwrap_or(0.0).max(0.0) as usize;
    if let Some(name) = engine_font_names(engine).get(font_index) {
        let lower = name.to_lowercase();
        style.bold = lower.contains("bold") || lower.contains("black") || lower.contains("heavy") || engine_bool(engine, b"/FauxBold");
        style.italic = lower.contains("italic") || lower.contains("oblique") || engine_bool(engine, b"/FauxItalic");
        // "Helvetica-BoldOblique" and "HelveticaNeue-Light": the family is what comes before the dash, split on capitals.
        let family = name.split('-').next().unwrap_or(name).to_string();
        let mut spaced = String::new();
        for (i, c) in family.chars().enumerate() { if i > 0 && c.is_uppercase() && !spaced.ends_with(' ') { spaced.push(' '); } spaced.push(c); }
        style.family = if crate::text::families().iter().any(|f| f.eq_ignore_ascii_case(&family)) { family } else { spaced };
    }
    if !engine_bool(engine, b"/AutoLeading") { if let Some(leading) = engine_number(engine, b"/Leading") { if let Some(size) = engine_number(engine, b"/FontSize") { if size > 0.0 { style.leading = (leading / size).clamp(0.5, 5.0); } } } }
    if let Some(tracking) = engine_number(engine, b"/Tracking") { style.tracking = tracking / 1000.0 * style.size; }
    style.align = match engine_number(engine, b"/Justification").unwrap_or(0.0) as i32 { 1 => 2, 2 => 1, _ => 0 };
    if engine_number(engine, b"/ShapeType").unwrap_or(0.0) as i32 == 1 { if let Some(b) = engine_values(engine, b"/BoxBounds") { if b.len() >= 4 { let w = (b[2] - b[0]).abs() * to_px; if w >= 1.0 { style.width = Some(w.round()); } } } }
    Ok(style)
}

/// The number after `key` in engine data ("/FontSize 48.0").
fn engine_number(data: &[u8], key: &[u8]) -> Option<f64> {
    let at = find(data, key, 0)?;
    let rest = &data[at + key.len()..];
    let text: String = rest.iter().skip_while(|b| **b == b' ').take_while(|b| b.is_ascii_digit() || **b == b'.' || **b == b'-').map(|b| *b as char).collect();
    text.parse().ok()
}

fn engine_bool(data: &[u8], key: &[u8]) -> bool {
    find(data, key, 0).is_some_and(|at| data[at + key.len()..].iter().skip_while(|b| **b == b' ').take(4).map(|b| *b as char).collect::<String>() == "true")
}

/// The numbers in the first "[ ... ]" after `key`.
fn engine_values(data: &[u8], key: &[u8]) -> Option<Vec<f64>> {
    let at = find(data, key, 0)?;
    let open = find(data, b"[", at)?;
    let close = find(data, b"]", open)?;
    Some(String::from_utf8_lossy(&data[open + 1..close]).split_whitespace().filter_map(|t| t.parse().ok()).collect())
}

/// The font names in the FontSet, in order.
fn engine_font_names(data: &[u8]) -> Vec<String> {
    let Some(start) = find(data, b"/FontSet", 0) else { return Vec::new() };
    let mut names = Vec::new();
    let mut at = start;
    while let Some(n) = find(data, b"/Name (", at) {
        let open = n + b"/Name (".len();
        // Up to the closing parenthesis that is not escaped.
        let mut i = open;
        while i < data.len() && !(data[i] == b')' && data[i - 1] != b'\\') { i += 1; }
        names.push(engine_string(&data[open..i.min(data.len())]));
        at = i;
        if names.len() > 256 { break; }
    }
    names
}

/// A string in engine data: UTF-16 with a byte order mark, escapes undone; or plain bytes.
fn engine_string(raw: &[u8]) -> String {
    let mut bytes = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() { if raw[i] == b'\\' && i + 1 < raw.len() { bytes.push(raw[i + 1]); i += 2; } else { bytes.push(raw[i]); i += 1; } }
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let units: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units).trim_end_matches('\0').to_string()
    } else { String::from_utf8_lossy(&bytes).to_string() }
}

fn find(data: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= data.len() { return None; }
    data[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

#[cfg(test)]
mod descriptor_tests {
    use super::*;

    #[test]
    fn effects_round_trip_through_lfx2_and_type_reads_from_tysh() {
        let mut e = Effects::default();
        e.drop_shadow = Some(Shadow { enabled: true, color: [0.1, 0.2, 0.3], opacity: 0.6, angle: 135.0, distance: 7.0, size: 9.0 });
        e.stroke = Some(Stroke { enabled: true, size: 4.0, position: 1, color: [1.0, 1.0, 0.0], opacity: 0.8 });
        e.bevel = Some(Bevel { enabled: true, style: 2, depth: 150.0, size: 6.0, angle: 90.0, altitude: 45.0, highlight_opacity: 0.5, shadow_opacity: 0.4 });
        e.color_overlay = Some(Overlay { enabled: false, color: [0.0, 1.0, 0.0], opacity: 0.3 });
        let bytes = crate::psd_desc::write(&effects_descriptor(&e));
        let mut block = vec![0, 0, 0, 0, 0, 0, 0, 16];
        block.extend_from_slice(&bytes);
        let back = read_lfx2(&block).unwrap().unwrap();
        let s = back.drop_shadow.unwrap();
        assert!((s.color[0] - 0.1).abs() < 1e-9 && (s.opacity - 0.6).abs() < 1e-9 && s.angle == 135.0 && s.distance == 7.0 && s.size == 9.0);
        let st = back.stroke.unwrap();
        assert!(st.position == 1 && st.size == 4.0 && (st.opacity - 0.8).abs() < 1e-9);
        let b = back.bevel.unwrap();
        assert!(b.style == 2 && b.depth == 150.0 && b.altitude == 45.0 && (b.highlight_opacity - 0.5).abs() < 1e-9);
        assert!(!back.color_overlay.unwrap().enabled);
        // A TySh block: the transform, the text descriptor with engine data, and a warp.
        let mut d = Descriptor::new("TxLr");
        d.push("Txt ", Item::Text("Hello\rWorld".into()));
        let mut name = vec![0xFEu8, 0xFF];
        for u in "Helvetica-BoldOblique".encode_utf16() { name.extend_from_slice(&u.to_be_bytes()); }
        let mut engine = b"<< /EngineDict << /StyleRun << /RunArray [ << /StyleSheet << /StyleSheetData << /Font 0 /FontSize 24.0 /AutoLeading false /Leading 36.0 /Tracking 50 /FillColor << /Type 1 /Values [ 1.0 0.5 0.25 0.0 ] >> >> >> >> ] >> /ParagraphRun << /RunArray [ << /ParagraphSheet << /Properties << /Justification 2 >> >> >> ] >> >> /ResourceDict << /FontSet [ << /Name (".to_vec();
        engine.extend_from_slice(&name);
        engine.extend_from_slice(b") /Script 0 >> ] >> /Rendered << /Shapes << /Children [ << /ShapeType 1 /Cookie << /Photoshop << /ShapeType 1 /BoxBounds [ 0.0 0.0 200.0 50.0 ] >> >> >> ] >> >> >>");
        d.push("EngineData", Item::Data(engine));
        let mut block = vec![0u8, 1];
        for v in [2.0f64, 0.0, 0.0, 2.0, 10.0, 20.0] { block.extend_from_slice(&v.to_be_bytes()); }
        block.extend_from_slice(&50u16.to_be_bytes());
        block.extend_from_slice(&16u32.to_be_bytes());
        block.extend_from_slice(&crate::psd_desc::write(&d));
        let style = read_tysh(&block, 144.0).unwrap();
        assert_eq!(style.text, "Hello\nWorld");
        assert_eq!(style.size, 24.0 * 2.0 * 2.0, "points through the transform and the resolution");
        assert!((style.color[0] - 0.5).abs() < 1e-9 && (style.color[1] - 0.25).abs() < 1e-9);
        assert!(style.bold && style.italic);
        assert_eq!(style.family, "Helvetica");
        assert!((style.leading - 1.5).abs() < 1e-9);
        assert!((style.tracking - 0.05 * 96.0).abs() < 1e-9);
        assert_eq!(style.align, 1);
        assert_eq!(style.width, Some(800.0));
    }
}
