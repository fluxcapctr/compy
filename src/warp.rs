//! Smudge and Liquify, as `WarpStroke` does them: the active layer as the canvas shows it, at document size,
//! changed dab by dab while the canvas shows the working copy in place of the layer. When the stroke ends the
//! result is painted into the layer's own pixels along the stroke's path.

use crate::brush::BrushSettings;
use crate::raster::with_bytes_raw_mut;
use anyhow::Result;
use cairo::ImageSurface;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarpMode { Liquify, Smudge }

impl WarpMode {
    pub fn name(self) -> &'static str { match self { WarpMode::Liquify => "Liquify", WarpMode::Smudge => "Smudge" } }
}

pub struct Warp {
    pub mode: WarpMode,
    pub diameter: f64,
    hardness: f64,
    strength: f64,
    width: usize,
    height: usize,
    /// The working copy, premultiplied BGRA at document size; the canvas draws it while the stroke runs.
    pub surface: ImageSurface,
    /// Every dab's center, for painting the result into the layer.
    pub points: Vec<(f64, f64)>,
    last: Option<(f64, f64)>,
    /// Smudge: the color the brush carries, a (2r + 1) squared BGRA square.
    carried: Vec<f32>,
    scratch: Vec<f32>,
    /// Document pixels changed by the last `append` (x0, y0, x1, y1).
    pub changed: Option<(i32, i32, i32, i32)>,
}

impl Warp {
    /// `surface` is the layer drawn plain at document size; the warp takes it over.
    pub fn new(surface: ImageSurface, mode: WarpMode, settings: &BrushSettings) -> Warp {
        Warp {
            mode, diameter: settings.diameter.max(2.0), hardness: settings.hardness.clamp(0.0, 0.98), strength: settings.opacity.clamp(0.01, 1.0),
            width: surface.width() as usize, height: surface.height() as usize, surface, points: Vec::new(), last: None, carried: Vec::new(), scratch: Vec::new(), changed: None,
        }
    }

    fn radius(&self) -> i64 { (self.diameter / 2.0).ceil() as i64 }

    /// How much a dab moves pixels at a distance `u` (0 center, 1 rim) from its center.
    fn weight(&self, u: f32) -> f32 {
        if u >= 1.0 { return 0.0; }
        let h = self.hardness as f32;
        if u <= h { return 1.0; }
        let t = (1.0 - u) / (1.0 - h);
        t * t * (3.0 - 2.0 * t)
    }

    /// Continues the stroke to `point` (document pixels), dabbing along the way.
    pub fn append(&mut self, point: (f64, f64)) -> Result<()> {
        self.changed = None;
        let Some(from) = self.last else {
            self.last = Some(point);
            if self.mode == WarpMode::Smudge { self.pick_up(point)?; }
            return Ok(());
        };
        let distance = (point.0 - from.0).hypot(point.1 - from.1);
        let spacing = (self.diameter * if self.mode == WarpMode::Smudge { 0.08 } else { 0.025 }).max(1.0);
        if distance < spacing { return Ok(()); }
        let steps = (distance / spacing).ceil() as usize;
        let mut previous = from;
        let (w, h) = (self.width, self.height);
        let r = self.radius();
        let mut bounds: Option<(i64, i64, i64, i64)> = None;
        let surface = self.surface.clone();
        with_bytes_raw_mut(&surface, |pixels, stride| {
            for step in 1..=steps {
                let t = step as f64 / steps as f64;
                let next = (from.0 + (point.0 - from.0) * t, from.1 + (point.1 - from.1) * t);
                if self.mode == WarpMode::Smudge { self.smudge(pixels, stride, next); } else { self.push(pixels, stride, previous, next); }
                self.points.push(next);
                let (cx, cy) = (next.0.round() as i64, next.1.round() as i64);
                let reach = r + ((next.0 - previous.0).abs().max((next.1 - previous.1).abs())).ceil() as i64 + 3;
                let rect = ((cx - reach).max(0), (cy - reach).max(0), (cx + reach + 1).min(w as i64), (cy + reach + 1).min(h as i64));
                bounds = Some(match bounds { Some(b) => (b.0.min(rect.0), b.1.min(rect.1), b.2.max(rect.2), b.3.max(rect.3)), None => rect });
                previous = next;
            }
        })?;
        self.last = Some(point);
        self.changed = bounds.filter(|b| b.2 > b.0 && b.3 > b.1).map(|b| (b.0 as i32, b.1 as i32, b.2 as i32, b.3 as i32));
        Ok(())
    }

    fn pick_up(&mut self, center: (f64, f64)) -> Result<()> {
        let r = self.radius();
        let side = (2 * r + 1) as usize;
        self.carried = vec![0f32; side * side * 4];
        let (cx, cy) = (center.0.round() as i64, center.1.round() as i64);
        let (w, h) = (self.width as i64, self.height as i64);
        let surface = self.surface.clone();
        crate::raster::with_bytes(&surface, |pixels, stride| {
            for dy in -r..=r {
                let y = cy + dy;
                if y < 0 || y >= h { continue; }
                for dx in -r..=r {
                    let x = cx + dx;
                    if x < 0 || x >= w { continue; }
                    let p = y as usize * stride + x as usize * 4;
                    let c = ((dy + r) as usize * side + (dx + r) as usize) * 4;
                    for k in 0..4 { self.carried[c + k] = pixels[p + k] as f32; }
                }
            }
        })
    }

    fn smudge(&mut self, pixels: &mut [u8], stride: usize, center: (f64, f64)) {
        let r = self.radius();
        let side = (2 * r + 1) as usize;
        if self.carried.len() < side * side * 4 { self.carried.resize(side * side * 4, 0.0); }
        let (cx, cy) = (center.0.round() as i64, center.1.round() as i64);
        let (w, h) = (self.width as i64, self.height as i64);
        let keep = self.strength as f32;
        let inv_r = 1.0 / (self.diameter as f32 / 2.0);
        for dy in -r..=r {
            let y = cy + dy;
            if y < 0 || y >= h { continue; }
            for dx in -r..=r {
                let x = cx + dx;
                if x < 0 || x >= w { continue; }
                let wgt = self.weight(((dx * dx + dy * dy) as f32).sqrt() * inv_r);
                if wgt <= 0.0 { continue; }
                let p = y as usize * stride + x as usize * 4;
                let c = ((dy + r) as usize * side + (dx + r) as usize) * 4;
                for k in 0..4 {
                    let under = pixels[p + k] as f32;
                    let painted = under + (self.carried[c + k] - under) * wgt;
                    pixels[p + k] = painted.round().clamp(0.0, 255.0) as u8;
                    // The brush picks up some of what it just left, more the weaker the smudge.
                    self.carried[c + k] = painted + (self.carried[c + k] - painted) * keep;
                }
            }
        }
        crate::ffi::clamp_premultiplied(pixels, 0);
    }

    /// Forward warp: pixels under the brush move with it, most at its center, fading to none at its rim.
    fn push(&mut self, pixels: &mut [u8], stride: usize, a: (f64, f64), b: (f64, f64)) {
        let r = self.radius();
        let mv = (((b.0 - a.0) * self.strength) as f32, ((b.1 - a.1) * self.strength) as f32);
        let margin = mv.0.abs().max(mv.1.abs()).ceil() as i64 + 2;
        let (cx, cy) = (b.0.round() as i64, b.1.round() as i64);
        let (w, h) = (self.width as i64, self.height as i64);
        let (x0, x1) = ((cx - r - margin).max(0), (cx + r + margin).min(w - 1));
        let (y0, y1) = ((cy - r - margin).max(0), (cy + r + margin).min(h - 1));
        if x0 > x1 || y0 > y1 { return; }
        let (cw, ch) = ((x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize);
        if self.scratch.len() < cw * ch * 4 { self.scratch.resize(cw * ch * 4, 0.0); }
        // A copy of the area as it was before this dab, which the dab samples from.
        for y in 0..ch { for x in 0..cw {
            let p = (y + y0 as usize) * stride + (x + x0 as usize) * 4;
            let s = (y * cw + x) * 4;
            for k in 0..4 { self.scratch[s + k] = pixels[p + k] as f32; }
        } }
        let inv_r = 1.0 / (self.diameter as f32 / 2.0);
        for dy in -r..=r {
            let y = cy + dy;
            if y < y0 || y > y1 { continue; }
            for dx in -r..=r {
                let x = cx + dx;
                if x < x0 || x > x1 { continue; }
                let wgt = self.weight(((dx * dx + dy * dy) as f32).sqrt() * inv_r);
                if wgt <= 0.0 { continue; }
                // Bilinear sample of the old pixels, from behind the brush's travel.
                let sx = ((x - x0) as f32 - mv.0 * wgt).clamp(0.0, (cw - 1) as f32);
                let sy = ((y - y0) as f32 - mv.1 * wgt).clamp(0.0, (ch - 1) as f32);
                let ix = (sx as usize).min(cw.saturating_sub(2));
                let iy = (sy as usize).min(ch.saturating_sub(2));
                let (fx, fy) = (sx - ix as f32, sy - iy as f32);
                let p = y as usize * stride + x as usize * 4;
                let s00 = (iy * cw + ix) * 4;
                let s10 = if cw > 1 { s00 + 4 } else { s00 };
                let s01 = if ch > 1 { s00 + cw * 4 } else { s00 };
                let s11 = if cw > 1 { s01 + 4 } else { s01 };
                for k in 0..4 {
                    let top = self.scratch[s00 + k] + (self.scratch[s10 + k] - self.scratch[s00 + k]) * fx;
                    let bottom = self.scratch[s01 + k] + (self.scratch[s11 + k] - self.scratch[s01 + k]) * fx;
                    pixels[p + k] = (top + (bottom - top) * fy).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
}
