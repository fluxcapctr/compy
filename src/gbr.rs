//! GIMP brush files: `.gbr` (one tip: gray, or RGBA whose alpha is the tip) and `.gih` (an image hose: a text
//! header, then several `.gbr` tips back to back). GIMP and Krita ship their tips this way; the app turns
//! them into the same presets a Photoshop `.abr` gives.

use crate::abr::Preset;
use anyhow::{Context as _, Result, bail};
use std::path::Path;
use std::rc::Rc;

fn u32_at(d: &[u8], at: usize) -> Result<u32> { d.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]])).context("the file ends early") }

/// One `.gbr` starting at `data[0]`: the preset and how many bytes it took.
fn parse_one(data: &[u8], fallback_name: &str) -> Result<(Preset, usize)> {
    let header = u32_at(data, 0)? as usize;
    let version = u32_at(data, 4)?;
    let (width, height, bytes) = (u32_at(data, 8)? as usize, u32_at(data, 12)? as usize, u32_at(data, 16)? as usize);
    if !(1..=3).contains(&version) || data.get(20..24) != Some(b"GIMP") { bail!("This is not a GIMP brush file."); }
    let spacing = u32_at(data, 24)? as f64;
    if header < 28 || header > data.len() || width == 0 || height == 0 || width > 16_384 || height > 16_384 || width * height > 50_000_000 { bail!("the brush's header is out of range"); }
    if bytes != 1 && bytes != 4 { bail!("{bytes}-byte pixels are not supported"); }
    let name = String::from_utf8_lossy(&data[28..header]).trim_end_matches('\0').trim().to_string();
    let name = if name.is_empty() { fallback_name.to_string() } else { name };
    let end = header + width * height * bytes;
    let pixels_raw = data.get(header..end).context("the brush's pixels end early")?;
    // A gray tip is the coverage itself (255 paints); an RGBA tip's alpha is its coverage.
    let pixels: Vec<u8> = if bytes == 1 { pixels_raw.to_vec() } else { pixels_raw.chunks_exact(4).map(|p| p[3]).collect() };
    Ok((Preset { name, width, height, pixels, spacing: if (1.0..=1000.0).contains(&spacing) { spacing } else { 20.0 }, jitter: crate::abr::default_jitter(width, height), set: fallback_name.to_string(), frames: Vec::new() }, end))
}

pub fn parse(data: &[u8], stem: &str) -> Result<Vec<Rc<Preset>>> {
    if data.get(20..24) == Some(b"GIMP") {
        let (mut p, _) = parse_one(data, stem)?;
        p.set = stem.to_string();
        return Ok(vec![Rc::new(p)]);
    }
    // An image hose: a name line, a parameter line, then the tips.
    let text_end = data.iter().enumerate().filter(|(_, b)| **b == b'\n').nth(1).map(|(i, _)| i + 1).context("This is not a GIMP brush file.")?;
    let header = String::from_utf8_lossy(&data[..text_end]).to_string();
    let mut lines = header.lines();
    let name = lines.next().unwrap_or(stem).trim().to_string();
    let count: usize = lines.next().unwrap_or("").split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0);
    if count == 0 || count > 512 { bail!("This is not a GIMP brush file."); }
    let mut cells = Vec::new();
    let mut pos = text_end;
    for i in 0..count {
        let (mut p, used) = parse_one(&data[pos..], &format!("{name} {}", i + 1))?;
        p.set = stem.to_string();
        cells.push(p);
        pos += used;
    }
    // Cells of one size are one brush that cycles through them; odd sizes stay separate tips.
    let (w, h) = (cells[0].width, cells[0].height);
    if cells.iter().all(|c| c.width == w && c.height == h) {
        let mut first = cells.remove(0);
        first.name = name.clone();
        first.frames = cells.into_iter().map(|c| c.pixels).collect();
        return Ok(vec![Rc::new(first)]);
    }
    Ok(cells.into_iter().enumerate().map(|(i, mut p)| { p.name = format!("{name} {}", i + 1); Rc::new(p) }).collect())
}

pub fn load(path: &Path) -> Result<Vec<Rc<Preset>>> {
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Brush".into());
    parse(&data, &stem)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn gbr(name: &str, w: u32, h: u32, bytes: u32, pixels: &[u8]) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&(28 + name.len() as u32 + 1).to_be_bytes());
        f.extend_from_slice(&2u32.to_be_bytes());
        f.extend_from_slice(&w.to_be_bytes()); f.extend_from_slice(&h.to_be_bytes()); f.extend_from_slice(&bytes.to_be_bytes());
        f.extend_from_slice(b"GIMP");
        f.extend_from_slice(&30u32.to_be_bytes());
        f.extend_from_slice(name.as_bytes()); f.push(0);
        f.extend_from_slice(pixels);
        f
    }
    #[test]
    fn reads_gray_rgba_and_hoses() {
        let gray = parse(&gbr("Gray", 2, 1, 1, &[255, 0]), "x").unwrap();
        assert_eq!((gray[0].name.as_str(), gray[0].pixels.as_slice(), gray[0].spacing), ("Gray", &[255u8, 0][..], 30.0));
        let rgba = parse(&gbr("Color", 1, 1, 4, &[10, 20, 30, 200]), "x").unwrap();
        assert_eq!(rgba[0].pixels, vec![200]);
        let mut hose = b"Hose\n2 ncells:2\n".to_vec();
        hose.extend(gbr("a", 1, 1, 1, &[9])); hose.extend(gbr("b", 1, 1, 1, &[8]));
        let tips = parse(&hose, "x").unwrap();
        assert_eq!(tips.len(), 1, "same-size cells are one cycling brush");
        assert_eq!((tips[0].name.as_str(), tips[0].pixels[0], tips[0].frames.len(), tips[0].frames[0][0]), ("Hose", 9, 1, 8));
        assert!(parse(b"nope", "x").is_err());
    }
}
