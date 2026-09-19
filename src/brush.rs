//! A brush stroke, as `BrushStroke` lays it: dabs of a round tip along a smoothed path, accumulated as
//! coverage in 256-pixel tiles of the layer's grid and capped by the stroke's opacity, composed onto the
//! layer's pixels (paint, erase, a cloned or blurred sample, or a healing wash) and written into a live
//! preview surface the canvas draws. The grid is the layer's own pixel grid extended to cover the canvas, so
//! painting past the layer's edge grows it when the stroke commits.

use crate::blur::LazyBlur;
use crate::ffi;
use crate::format::Transform;
use crate::raster::with_bytes_raw_mut;
use crate::render::pixel_to_document;
use crate::selection::Selection;
use anyhow::{Result, bail};
use cairo::{ImageSurface, Matrix};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

#[allow(unused_imports)]
use std::marker::PhantomData;

pub const TILE: usize = 256;
/// Halvings stay aligned with the layer's own when the grid's origin is a multiple of this.
pub const GRID_ALIGN: i32 = 64;

#[derive(Clone, Debug, PartialEq)]
pub struct BrushSettings {
    /// In document pixels, 1 to 2000.
    pub diameter: f64,
    pub hardness: f64,
    pub color: [f64; 3],
    /// Caps the whole stroke, as in Photoshop: overlapping dabs never exceed it.
    pub opacity: f64,
    /// Dab spacing as a fraction of the diameter; None picks the dense spacing that makes round tips read as
    /// one continuous mark (Photoshop's default of 25 percent shows as a string of dabs for a hard tip).
    pub spacing: Option<f64>,
    /// The tip's rotation in degrees and its roundness (1 round, 0.05 a sliver), as Photoshop's Brush Tip Shape.
    pub angle: f64,
    pub roundness: f64,
    /// A sampled tip from a Photoshop brush file, scaled to the diameter, in place of the round tip.
    pub preset: Option<std::rc::Rc<crate::abr::Preset>>,
    /// Angle jitter, 0 to 1 of a full turn: each dab turns by a random share of it.
    pub angle_jitter: f64,
}

impl Default for BrushSettings {
    fn default() -> Self { BrushSettings { diameter: 40.0, hardness: 1.0, color: [0.0; 3], opacity: 1.0, spacing: None, angle: 0.0, roundness: 1.0, preset: None, angle_jitter: 0.0 } }
}

impl BrushSettings {
    /// A tip other than the plain round one: a preset, a squashed or turned round tip.
    pub fn shaped(&self) -> bool { self.preset.is_some() || self.roundness < 0.999 || self.angle % 360.0 != 0.0 || self.angle_jitter > 0.0 }
}

/// A document-size image a stroke copies from, premultiplied 4 bytes per pixel.
pub struct Sample {
    pub pixels: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

impl Sample {
    pub fn pixel(&self, x: f64, y: f64) -> [u8; 4] {
        if x < 0.0 || y < 0.0 || x >= self.width as f64 || y >= self.height as f64 { return [0; 4]; }
        let i = (y as usize * self.width + x as usize) * 4;
        [self.pixels[i], self.pixels[i + 1], self.pixels[i + 2], self.pixels[i + 3]]
    }
}

/// What a stroke lays down.
#[derive(Clone)]
pub enum Kind {
    Paint,
    Erase,
    /// Shows a dark wash while painting; `heal` rebuilds the area from its surroundings at the end.
    Heal { mode: i32 },
    /// Clone Stamp: the sample shifted by `offset` document pixels, painted through the tip.
    /// `replaces` copies the sample's alpha too (a warp's result), rather than drawing over what is there.
    Clone { sample: Rc<Sample>, offset: (f64, f64), replaces: bool },
    /// Blur: the layer softened, painted in place through the tip.
    Blur { sample: Rc<LazyBlur> },
}

impl Kind {
    pub fn name(&self) -> &'static str {
        match self { Kind::Paint => "Brush Stroke", Kind::Erase => "Erase", Kind::Heal { .. } => "Spot Healing", Kind::Clone { .. } => "Clone Stamp", Kind::Blur { .. } => "Blur" }
    }
}

/// The soft tip's fall-off across the region between the hardness radius and the rim: a normalized
/// Gaussian that reaches zero at the rim.
pub fn falloff(u: f64) -> f64 {
    let k = 2.5;
    ((-k * u * u).exp() - (-k).exp()) / (1.0 - (-k).exp()).max(0.0)
}

/// The widest tip kept in memory, as `BrushStroke.gridTipLimit`.
pub const GRID_TIP_LIMIT: f64 = 3000.0;

/// The tip for `settings` at `grid_diameter`: a sampled preset, or the round tip, either one squashed by
/// the roundness and turned by the angle when they are not the defaults.
pub fn shaped_tip(grid_diameter: f64, settings: &BrushSettings) -> (usize, Vec<u8>) {
    if !settings.shaped() { return tip(grid_diameter, settings.hardness); }
    let (bw, bh, base) = match &settings.preset {
        Some(p) => (p.width, p.height, p.pixels.clone()),
        None => { let (s, px) = tip(grid_diameter.max(4.0), settings.hardness); (s, s, px) }
    };
    // The longest side lands on the diameter; the box holds it at any angle.
    let scale = grid_diameter / bw.max(bh) as f64;
    let (sw, sh) = (bw as f64 * scale, bh as f64 * scale * settings.roundness.clamp(0.05, 1.0));
    let size = (sw.hypot(sh).ceil() as usize).max(1);
    let source = match crate::raster::a8_from_data(bw as i32, bh as i32, { let stride = cairo::Format::A8.stride_for_width(bw as u32).unwrap_or(bw as i32) as usize; let mut d = vec![0u8; stride * bh]; for y in 0..bh { d[y * stride..y * stride + bw].copy_from_slice(&base[y * bw..(y + 1) * bw]); } d }, cairo::Format::A8.stride_for_width(bw as u32).unwrap_or(bw as i32)) {
        Ok(s) => s, Err(_) => return tip(grid_diameter, settings.hardness),
    };
    let Ok(out) = crate::raster::a8_filled(size as i32, size as i32, 0) else { return tip(grid_diameter, settings.hardness) };
    if let Ok(cr) = cairo::Context::new(&out) {
        cr.translate(size as f64 / 2.0, size as f64 / 2.0);
        cr.rotate(settings.angle.to_radians());
        cr.scale(sw / bw as f64, sh / bh as f64);
        cr.translate(-(bw as f64) / 2.0, -(bh as f64) / 2.0);
        let _ = cr.set_source_surface(&source, 0.0, 0.0);
        cr.source().set_filter(if scale < 1.0 { cairo::Filter::Good } else { cairo::Filter::Bilinear });
        let _ = cr.paint();
    }
    let pixels = crate::raster::with_bytes(&out, |d, stride| (0..size).flat_map(|y| d[y * stride..y * stride + size].to_vec()).collect::<Vec<u8>>()).unwrap_or_else(|_| vec![0; size * size]);
    (size, pixels)
}

/// The tip as gray coverage in grid pixels, its box `size` wide.
pub fn tip(grid_diameter: f64, hardness: f64) -> (usize, Vec<u8>) {
    let size = (grid_diameter.ceil() as usize).max(1);
    let radius = grid_diameter / 2.0;
    let center = size as f64 / 2.0;
    let mut pixels = vec![0u8; size * size];
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f64 + 0.5 - center).powi(2) + (y as f64 + 0.5 - center).powi(2)).sqrt();
            let coverage = if hardness >= 1.0 {
                (radius - d + 0.5).clamp(0.0, 1.0)
            } else if d >= radius {
                0.0
            } else {
                let inner = radius * hardness;
                if d <= inner { 1.0 } else { falloff((d - inner) / (radius - inner)) }
            };
            pixels[y * size + x] = (coverage * 255.0).round() as u8;
        }
    }
    (size, pixels)
}

struct Tile {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    /// The grid's pixels before the stroke, packed 4 bytes per pixel.
    base: Vec<u8>,
    /// Coverage so far, one byte per pixel.
    coverage: Vec<u8>,
    /// Selection coverage on this tile, when there is a selection.
    selection: Option<Vec<u8>>,
    /// Pixels touched since the last compose, in tile coordinates (x0, y0, x1, y1).
    dirty: Option<(usize, usize, usize, usize)>,
}

pub struct Stroke {
    pub settings: BrushSettings,
    pub kind: Kind,
    /// The paint grid.
    pub width: usize,
    pub height: usize,
    /// Where the layer's own pixels sit in the grid.
    pub source: (usize, usize, usize, usize),
    has_image: bool,
    pub to_document: Matrix,
    from_document: Matrix,
    /// The transform placing the whole grid on the document.
    pub transform: Transform,
    canvas: (f64, f64),
    /// The live pixels the canvas draws: the grid, premultiplied BGRA.
    pub preview: ImageSurface,
    selection: Option<Selection>,
    tip_size: usize,
    tip: std::rc::Rc<Vec<u8>>,
    spacing: f64,
    /// Turned copies of the tip for angle jitter, and how many dabs have landed (which picks one).
    variants: Vec<(usize, std::rc::Rc<Vec<u8>>)>,
    dabs: u64,
    tiles: HashMap<usize, Tile>,
    columns: usize,
    samples: Vec<(f64, f64)>,
    previous: Option<(f64, f64)>,
    distance_to_next: f64,
    tail_backup: HashMap<usize, Option<Vec<u8>>>,
    /// Grid pixels changed by the last `append` or `flush`: what the canvas needs to refresh.
    pub changed: Option<(i32, i32, i32, i32)>,
    /// Set while replaying a known path: no provisional tails, and composing waits for the end.
    replaying: bool,
    pending: HashSet<usize>,
}

impl Stroke {
    /// Starts a stroke on a layer of `layer_size` pixels placed by `transform`. `preview_grid` is asked for
    /// the live surface once the grid (its origin in layer pixels, its size, and its transform) is known.
    pub fn new(has_image: bool, layer_size: (i32, i32), transform: &Transform, canvas: (f64, f64), settings: &BrushSettings, kind: Kind,
               selection: Option<Selection>, grow: bool, preview_grid: impl FnOnce(i32, i32, i32, i32, Transform) -> Result<ImageSurface>) -> Result<Stroke> {
        let (iw, ih) = layer_size;
        if !(1.0..=2000.0).contains(&settings.diameter) || !(0.0..=1.0).contains(&settings.hardness) || !(0.01..=1.0).contains(&settings.opacity) { bail!("brush settings out of range"); }
        let original = pixel_to_document(transform, iw, ih);
        let inverse = original.try_invert()?;
        // The grid: the layer's pixels plus whatever of the canvas lies beyond them, aligned so halvings match.
        let corners = [(0.0, 0.0), (canvas.0, 0.0), (0.0, canvas.1), (canvas.0, canvas.1)].map(|(x, y)| inverse.transform_point(x, y));
        let mut x0 = 0f64.min(corners.iter().map(|c| c.0).fold(f64::MAX, f64::min)).floor();
        let mut y0 = 0f64.min(corners.iter().map(|c| c.1).fold(f64::MAX, f64::min)).floor();
        let mut x1 = (iw as f64).max(corners.iter().map(|c| c.0).fold(f64::MIN, f64::max)).ceil();
        let mut y1 = (ih as f64).max(corners.iter().map(|c| c.1).fold(f64::MIN, f64::max)).ceil();
        x0 = (x0 / GRID_ALIGN as f64).floor() * GRID_ALIGN as f64;
        y0 = (y0 / GRID_ALIGN as f64).floor() * GRID_ALIGN as f64;
        if !grow || (x1 - x0) * (y1 - y0) > 100_000_000.0 || x1 - x0 > 30_000.0 || y1 - y0 > 30_000.0 {
            // A layer shown tiny on a big canvas would need an enormous grid; paint within its own pixels then.
            (x0, y0, x1, y1) = (0.0, 0.0, iw as f64, ih as f64);
        }
        let (width, height) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let source = ((-x0) as usize, (-y0) as usize, iw as usize, ih as usize);
        let mut to_document = original;
        to_document.translate(x0, y0);
        let from_document = to_document.try_invert()?;
        let mut expanded = *transform;
        expanded.size = crate::format::Size(width as f64 * transform.size.0 / iw as f64, height as f64 * transform.size.1 / ih as f64);
        let (cx, cy) = to_document.transform_point(width as f64 / 2.0, height as f64 / 2.0);
        expanded.origin = crate::format::Point(cx - expanded.size.0 / 2.0, cy - expanded.size.1 / 2.0);
        let preview = preview_grid(x0 as i32, y0 as i32, width as i32, height as i32, expanded)?;
        let scale = (to_document.xx() * to_document.xx() + to_document.yx() * to_document.yx()).sqrt();
        let grid_diameter = (settings.diameter / scale).max(1.0);
        // The reference keeps grid tips to 3,000 pixels and falls back to a stamp; here the stroke is refused.
        if !grid_diameter.is_finite() || grid_diameter > GRID_TIP_LIMIT {
            bail!("The brush would be {} pixels wide on this layer's own pixels; the limit is {}. Use a smaller brush, or resample the layer with Image Size.", grid_diameter.round(), GRID_TIP_LIMIT);
        }
        let (tip_size, tip) = shaped_tip(grid_diameter, settings);
        let tip = std::rc::Rc::new(tip);
        // A preset's frames, each turned by the jitter, made once and picked per dab.
        let frames = settings.preset.as_ref().map_or(0, |p| p.frames.len());
        let variants: Vec<(usize, std::rc::Rc<Vec<u8>>)> = if settings.angle_jitter > 0.0 || frames > 0 {
            let turns = if settings.angle_jitter > 0.0 { 12usize.div_ceil(frames + 1).max(1) } else { 1 };
            let mut out = Vec::new();
            for frame in 0..=frames {
                for i in 0..turns {
                    let mut turned = settings.clone();
                    if let (Some(p), true) = (settings.preset.as_ref(), frame > 0) {
                        let mut alt = (**p).clone();
                        alt.pixels = p.frames[frame - 1].clone();
                        turned.preset = Some(std::rc::Rc::new(alt));
                    }
                    if turns > 1 { turned.angle += (i as f64 / turns as f64 - 0.5) * 360.0 * settings.angle_jitter.clamp(0.0, 1.0); }
                    let (s, px) = shaped_tip(grid_diameter, &turned);
                    out.push((s, std::rc::Rc::new(px)));
                }
            }
            out
        } else { Vec::new() };
        let spacing = match settings.spacing {
            Some(fraction) => (settings.diameter * fraction.clamp(0.01, 10.0)).max(0.25),
            None => (settings.diameter * if settings.hardness >= 1.0 && !settings.shaped() { 0.015 } else { 0.025 }).max(0.25),
        };
        Ok(Stroke {
            settings: settings.clone(), kind, width, height, source, has_image, to_document, from_document, transform: expanded, canvas, preview,
            selection, tip_size, tip, variants, dabs: 0, spacing, tiles: HashMap::new(), columns: width.div_ceil(TILE),
            samples: Vec::new(), previous: None, distance_to_next: 0.0, tail_backup: HashMap::new(), changed: None, replaying: false, pending: HashSet::new(),
        })
    }

    /// Adds a pointer sample (document pixels). Dabs follow a smooth curve through the samples; the newest
    /// piece is drawn as a provisional straight tail and replaced when the next sample arrives.
    pub fn append(&mut self, point: (f64, f64)) -> Result<()> {
        if !point.0.is_finite() || !point.1.is_finite() { return Ok(()); }
        if self.samples.last() == Some(&point) { return Ok(()); }
        let mut changed = self.remove_tail();
        self.samples.push(point);
        if self.samples.len() > 4 { self.samples.remove(0); }
        let n = self.samples.len();
        if n == 1 {
            self.walk(point, &mut changed);
        } else if n >= 3 {
            let (before, start, end, after) = (self.samples[n.saturating_sub(4)], self.samples[n - 3], self.samples[n - 2], self.samples[n - 1]);
            self.curve(start, end, before, after, &mut changed);
        }
        if n >= 2 && !self.replaying { let from = self.samples[n - 2]; self.draw_tail(from, point, &mut changed); }
        if self.replaying { self.pending.extend(changed); return Ok(()); }
        self.publish(&changed)
    }

    /// Lays the whole stroke through `points` at once, as a finished path: no provisional tails, every tile
    /// composed once at the end. What a warp uses to paint its result back into the layer.
    pub fn replay(&mut self, points: &[(f64, f64)]) -> Result<()> {
        self.replaying = true;
        for &point in points { self.append(point)?; }
        let mut changed = self.remove_tail();
        let n = self.samples.len();
        if n >= 2 {
            let (before, start, end) = (self.samples[n.saturating_sub(3)], self.samples[n - 2], self.samples[n - 1]);
            self.curve(start, end, before, end, &mut changed);
            self.samples = vec![end];
        }
        changed.extend(std::mem::take(&mut self.pending));
        self.replaying = false;
        self.publish(&changed)
    }

    /// Replaces the provisional tail with the stroke's final curve piece. Safe to repeat.
    pub fn flush(&mut self) -> Result<()> {
        let mut changed = self.remove_tail();
        let n = self.samples.len();
        if n >= 2 {
            let (before, start, end) = (self.samples[n.saturating_sub(3)], self.samples[n - 2], self.samples[n - 1]);
            self.curve(start, end, before, end, &mut changed);
            self.samples = vec![end];
        }
        self.publish(&changed)
    }

    fn draw_tail(&mut self, start: (f64, f64), end: (f64, f64), changed: &mut HashSet<usize>) {
        for key in self.keys_reached(start, end) {
            let backup = self.tiles.get(&key).map(|t| t.coverage.clone());
            self.tail_backup.insert(key, backup);
        }
        let saved = (self.previous, self.distance_to_next, self.dabs);
        self.walk(end, changed);
        (self.previous, self.distance_to_next, self.dabs) = saved;
    }

    fn remove_tail(&mut self) -> HashSet<usize> {
        let mut restored = HashSet::new();
        for (key, backup) in std::mem::take(&mut self.tail_backup) {
            let Some(tile) = self.tiles.get_mut(&key) else { continue };
            match backup {
                Some(coverage) => tile.coverage = coverage,
                None => tile.coverage.fill(0),
            }
            tile.dirty = Some((0, 0, tile.w, tile.h));
            restored.insert(key);
        }
        restored
    }

    /// Tile keys a straight run from `start` to `end` can touch.
    fn keys_reached(&self, start: (f64, f64), end: (f64, f64)) -> Vec<usize> {
        let reach = self.settings.diameter / 2.0 + 2.0;
        let (bx0, by0) = (start.0.min(end.0) - reach, start.1.min(end.1) - reach);
        let (bx1, by1) = (start.0.max(end.0) + reach, start.1.max(end.1) + reach);
        let corners = [(bx0, by0), (bx1, by0), (bx0, by1), (bx1, by1)].map(|(x, y)| self.from_document.transform_point(x, y));
        let gx0 = corners.iter().map(|c| c.0).fold(f64::MAX, f64::min).floor().max(0.0) as usize;
        let gy0 = corners.iter().map(|c| c.1).fold(f64::MAX, f64::min).floor().max(0.0) as usize;
        let gx1 = (corners.iter().map(|c| c.0).fold(f64::MIN, f64::max).ceil().max(0.0) as usize).min(self.width);
        let gy1 = (corners.iter().map(|c| c.1).fold(f64::MIN, f64::max).ceil().max(0.0) as usize).min(self.height);
        let mut keys = Vec::new();
        if gx1 <= gx0 || gy1 <= gy0 { return keys; }
        for ty in gy0 / TILE..=(gy1 - 1) / TILE { for tx in gx0 / TILE..=(gx1 - 1) / TILE { keys.push(ty * self.columns + tx); } }
        keys
    }

    /// Centripetal Catmull-Rom between `start` and `end`: through every sample without the loops or overshoot
    /// uniform splines make at uneven mouse speeds.
    fn curve(&mut self, start: (f64, f64), end: (f64, f64), before: (f64, f64), after: (f64, f64), changed: &mut HashSet<usize>) {
        fn knot(t: f64, a: (f64, f64), b: (f64, f64)) -> f64 { t + ((b.0 - a.0).hypot(b.1 - a.1)).sqrt().max(0.0001) }
        fn mix(a: (f64, f64), b: (f64, f64), ta: f64, tb: f64, t: f64) -> (f64, f64) {
            let (wa, wb) = ((tb - t) / (tb - ta), (t - ta) / (tb - ta));
            (a.0 * wa + b.0 * wb, a.1 * wa + b.1 * wb)
        }
        let t0 = 0.0;
        let t1 = knot(t0, before, start);
        let t2 = knot(t1, start, end);
        let t3 = knot(t2, end, after);
        let pieces = (((end.0 - start.0).hypot(end.1 - start.1) / 2.0).ceil() as usize).max(1);
        for index in 1..=pieces {
            let t = t1 + (t2 - t1) * index as f64 / pieces as f64;
            let (a1, a2, a3) = (mix(before, start, t0, t1, t), mix(start, end, t1, t2, t), mix(end, after, t2, t3, t));
            let (b1, b2) = (mix(a1, a2, t0, t2, t), mix(a2, a3, t1, t3, t));
            let point = if index == pieces { end } else { mix(b1, b2, t1, t2, t) };
            self.walk(point, changed);
        }
    }

    /// Lays evenly spaced dabs along a straight run from the previous dab position.
    fn walk(&mut self, point: (f64, f64), changed: &mut HashSet<usize>) {
        match self.previous {
            Some(previous) => {
                let (dx, dy) = (point.0 - previous.0, point.1 - previous.1);
                let length = dx.hypot(dy);
                if length > 0.0 {
                    let mut distance = self.distance_to_next;
                    while distance <= length {
                        self.dab((previous.0 + dx * distance / length, previous.1 + dy * distance / length), changed);
                        distance += self.spacing;
                    }
                    self.distance_to_next = distance - length;
                }
            }
            None => {
                self.dab(point, changed);
                self.distance_to_next = self.spacing;
            }
        }
        self.previous = Some(point);
    }

    /// One tip, snapped to whole grid pixels, accumulated into the coverage of every tile it touches.
    fn dab(&mut self, point: (f64, f64), changed: &mut HashSet<usize>) {
        if point.0 < -self.settings.diameter || point.1 < -self.settings.diameter || point.0 > self.canvas.0 + self.settings.diameter || point.1 > self.canvas.1 + self.settings.diameter { return; }
        let (cx, cy) = self.from_document.transform_point(point.0, point.1);
        // The tip, or one of its turned copies chosen by a hash of the dab count (the same on replay).
        let pick = if self.variants.is_empty() { None } else { let h = self.dabs.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40; Some((h % self.variants.len() as u64) as usize) };
        self.dabs += 1;
        let (tip_size, tip): (usize, std::rc::Rc<Vec<u8>>) = match pick { Some(i) => (self.variants[i].0, self.variants[i].1.clone()), None => (self.tip_size, self.tip.clone()) };
        let size = tip_size as f64;
        let (bx, by) = ((cx - size / 2.0).round() as i64, (cy - size / 2.0).round() as i64);
        let (gx0, gy0) = (bx.max(0) as usize, by.max(0) as usize);
        let (gx1, gy1) = (((bx + size as i64).max(0) as usize).min(self.width), ((by + size as i64).max(0) as usize).min(self.height));
        if gx1 <= gx0 || gy1 <= gy0 { return; }
        // Round tips accumulate softly (dabs screen over each other); a sampled or shaped tip keeps to the
        // strongest dab under each pixel, so its holes survive along the stroke as a dry medium's do.
        let hard = self.settings.hardness >= 1.0 || self.settings.shaped();
        for ty in gy0 / TILE..=(gy1 - 1) / TILE {
            for tx in gx0 / TILE..=(gx1 - 1) / TILE {
                let key = ty * self.columns + tx;
                self.allocate(key, tx, ty);
                let tile = self.tiles.get_mut(&key).unwrap();
                let (lx0, ly0) = (gx0.max(tile.x) - tile.x, gy0.max(tile.y) - tile.y);
                let (lx1, ly1) = (gx1.min(tile.x + tile.w) - tile.x, gy1.min(tile.y + tile.h) - tile.y);
                if lx1 <= lx0 || ly1 <= ly0 { continue; }
                for ly in ly0..ly1 {
                    let ty_ = (tile.y + ly) as i64 - by;
                    for lx in lx0..lx1 {
                        let tx_ = (tile.x + lx) as i64 - bx;
                        let t = tip[ty_ as usize * tip_size + tx_ as usize] as u32;
                        if t == 0 { continue; }
                        let c = &mut tile.coverage[ly * tile.w + lx];
                        let current = *c as u32;
                        *c = if hard { current.max(t) as u8 } else { (current + t - (current * t + 127) / 255) as u8 };
                    }
                }
                tile.dirty = Some(match tile.dirty { Some(d) => (d.0.min(lx0), d.1.min(ly0), d.2.max(lx1), d.3.max(ly1)), None => (lx0, ly0, lx1, ly1) });
                changed.insert(key);
            }
        }
    }

    fn allocate(&mut self, key: usize, tx: usize, ty: usize) {
        if self.tiles.contains_key(&key) { return; }
        let (x, y) = (tx * TILE, ty * TILE);
        let (w, h) = (TILE.min(self.width - x), TILE.min(self.height - y));
        let mut base = vec![0u8; w * h * 4];
        crate::raster::with_bytes(&self.preview, |data, stride| {
            for r in 0..h { base[r * w * 4..(r + 1) * w * 4].copy_from_slice(&data[(y + r) * stride + x * 4..(y + r) * stride + (x + w) * 4]); }
        }).ok();
        let selection = self.selection.as_ref().map(|sel| sel.coverage_on_grid(&self.to_document, x, y, w, h));
        self.tiles.insert(key, Tile { x, y, w, h, base, coverage: vec![0u8; w * h], selection, dirty: None });
    }

    /// Composes every changed tile's dirty part onto its base and writes it into the preview.
    fn publish(&mut self, changed: &HashSet<usize>) -> Result<()> {
        self.changed = None;
        let mut composed: Vec<(usize, usize, usize, usize, Vec<u8>)> = Vec::new();
        for &key in changed {
            let Some(tile) = self.tiles.get_mut(&key) else { continue };
            let Some((x0, y0, x1, y1)) = tile.dirty.take() else { continue };
            let (w, rw, rh) = (tile.w, x1 - x0, y1 - y0);
            let mut out = vec![0u8; rw * rh * 4];
            let opacity = self.settings.opacity;
            let color = self.settings.color;
            for r in 0..rh {
                let ly = y0 + r;
                for c in 0..rw {
                    let lx = x0 + c;
                    let i = ly * w + lx;
                    let mut cov = tile.coverage[i] as f64 / 255.0;
                    if let Some(sel) = &tile.selection { cov *= sel[i] as f64 / 255.0; }
                    let base = &tile.base[i * 4..i * 4 + 4];
                    let o = &mut out[(r * rw + c) * 4..(r * rw + c) * 4 + 4];
                    if cov <= 0.0 { o.copy_from_slice(base); continue; }
                    let a = cov * opacity;
                    let over = |src: [f64; 4], a: f64, o: &mut [u8]| {
                        for k in 0..4 { o[k] = (src[k] * a + base[k] as f64 * (1.0 - a)).round().clamp(0.0, 255.0) as u8; }
                    };
                    match &self.kind {
                        Kind::Paint => over([color[2] * 255.0, color[1] * 255.0, color[0] * 255.0, 255.0], a, o),
                        Kind::Erase => { for k in 0..4 { o[k] = (base[k] as f64 * (1.0 - a)).round() as u8; } }
                        Kind::Heal { .. } => over([0.12 * 255.0, 0.12 * 255.0, 0.12 * 255.0, 255.0], cov * 0.45, o),
                        Kind::Clone { sample, offset, replaces } => {
                            // The grid point's document position, from the matrix directly (a call per pixel is slow).
                            let (gx, gy) = (tile.x as f64 + lx as f64 + 0.5, tile.y as f64 + ly as f64 + 0.5);
                            let m = &self.to_document;
                            let (dx, dy) = (m.xx() * gx + m.xy() * gy + m.x0(), m.yx() * gx + m.yy() * gy + m.y0());
                            let p = sample.pixel(dx + offset.0, dy + offset.1);
                            if *replaces { for k in 0..4 { o[k] = (p[k] as f64 * a + base[k] as f64 * (1.0 - a)).round().clamp(0.0, 255.0) as u8; } }
                            else {
                                // Source-over of the premultiplied sample: a transparent sample leaves the layer alone.
                                let keep = 1.0 - a * p[3] as f64 / 255.0;
                                for k in 0..4 { o[k] = (p[k] as f64 * a + base[k] as f64 * keep).round().clamp(0.0, 255.0) as u8; }
                            }
                        }
                        Kind::Blur { .. } => {
                            // Filled below from the blurred region, one block per tile rather than per pixel.
                            o.copy_from_slice(base);
                        }
                    }
                }
            }
            if let Kind::Blur { sample } = &self.kind {
                // Document pixels under the tile's dirty part, blurred, painted through the coverage.
                let (dx0, dy0) = self.to_document.transform_point((tile.x + x0) as f64, (tile.y + y0) as f64);
                let (dx1, dy1) = self.to_document.transform_point((tile.x + x1) as f64, (tile.y + y1) as f64);
                let (rx, ry) = (dx0.min(dx1).floor().max(0.0) as usize, dy0.min(dy1).floor().max(0.0) as usize);
                let (rx1, ry1) = ((dx0.max(dx1).ceil().max(0.0) as usize).min(sample.width), (dy0.max(dy1).ceil().max(0.0) as usize).min(sample.height));
                if rx1 > rx && ry1 > ry {
                    let block = sample.region(rx, ry, rx1 - rx, ry1 - ry);
                    let bw = rx1 - rx;
                    for r in 0..rh {
                        for c in 0..rw {
                            let i = (y0 + r) * w + x0 + c;
                            let mut cov = tile.coverage[i] as f64 / 255.0;
                            if let Some(sel) = &tile.selection { cov *= sel[i] as f64 / 255.0; }
                            if cov <= 0.0 { continue; }
                            let a = cov * opacity;
                            let (gx, gy) = (tile.x as f64 + (x0 + c) as f64 + 0.5, tile.y as f64 + (y0 + r) as f64 + 0.5);
                            let m = &self.to_document;
                            let (dx, dy) = (m.xx() * gx + m.xy() * gy + m.x0(), m.yx() * gx + m.yy() * gy + m.y0());
                            let (sx, sy) = (dx.floor() as isize - rx as isize, dy.floor() as isize - ry as isize);
                            if sx < 0 || sy < 0 || sx as usize >= bw || sy as usize >= ry1 - ry { continue; }
                            let s = &block[(sy as usize * bw + sx as usize) * 4..][..4];
                            let base = &tile.base[i * 4..i * 4 + 4];
                            let o = &mut out[(r * rw + c) * 4..(r * rw + c) * 4 + 4];
                            for k in 0..4 { o[k] = (s[k] as f64 * a + base[k] as f64 * (1.0 - a)).round().clamp(0.0, 255.0) as u8; }
                        }
                    }
                }
            }
            composed.push((tile.x + x0, tile.y + y0, rw, rh, out));
        }
        if composed.is_empty() { return Ok(()); }
        let mut bounds: Option<(i32, i32, i32, i32)> = None;
        with_bytes_raw_mut(&self.preview, |data, stride| {
            for (x, y, rw, rh, out) in &composed {
                for r in 0..*rh { data[(y + r) * stride + x * 4..(y + r) * stride + (x + rw) * 4].copy_from_slice(&out[r * rw * 4..(r + 1) * rw * 4]); }
                let rect = (*x as i32, *y as i32, (x + rw) as i32, (y + rh) as i32);
                bounds = Some(match bounds { Some(b) => (b.0.min(rect.0), b.1.min(rect.1), b.2.max(rect.2), b.3.max(rect.3)), None => rect });
            }
        })?;
        self.changed = bounds;
        Ok(())
    }

    /// Spot Healing, once the stroke ends: rebuilds the painted area from nearby texture (`HealPixels.c`)
    /// and writes it into the preview, reading the layer's original pixels rather than the wash.
    pub fn heal(&mut self) -> Result<()> {
        let Kind::Heal { mode } = self.kind else { return Ok(()) };
        let mut painted: Option<(usize, usize, usize, usize)> = None;
        for tile in self.tiles.values() {
            if let Some((l, t, r, b)) = ffi::gray_bounds(&tile.coverage, tile.w, tile.h, tile.w) {
                let rect = (tile.x + l, tile.y + t, tile.x + r, tile.y + b);
                painted = Some(match painted { Some(p) => (p.0.min(rect.0), p.1.min(rect.1), p.2.max(rect.2), p.3.max(rect.3)), None => rect });
            }
        }
        let Some((px0, py0, px1, py1)) = painted else { return Ok(()) };
        let reach = (((px1 - px0).max(py1 - py0) + 32) as f64 * 3.2).ceil() as usize;
        let (x0, y0) = (px0.saturating_sub(reach), py0.saturating_sub(reach));
        let (x1, y1) = ((px1 + reach).min(self.width), (py1 + reach).min(self.height));
        let (w, h) = (x1 - x0, y1 - y0);
        let mut pixels = vec![0u8; w * h * 4];
        let mut coverage = vec![0u8; w * h];
        crate::raster::with_bytes(&self.preview, |data, stride| {
            for r in 0..h { pixels[r * w * 4..(r + 1) * w * 4].copy_from_slice(&data[(y0 + r) * stride + x0 * 4..(y0 + r) * stride + x1 * 4]); }
        })?;
        for tile in self.tiles.values() {
            // Original pixels where the wash was composed, and the coverage clipped by the selection.
            for ly in 0..tile.h {
                let gy = tile.y + ly;
                if gy < y0 || gy >= y1 { continue; }
                for lx in 0..tile.w {
                    let gx = tile.x + lx;
                    if gx < x0 || gx >= x1 { continue; }
                    let i = ly * tile.w + lx;
                    let o = (gy - y0) * w + gx - x0;
                    pixels[o * 4..o * 4 + 4].copy_from_slice(&tile.base[i * 4..i * 4 + 4]);
                    let mut c = tile.coverage[i] as u32;
                    if let Some(sel) = &tile.selection { c = (c * sel[i] as u32 + 127) / 255; }
                    coverage[o] = c as u8;
                }
            }
        }
        ffi::heal(&mut pixels, &coverage, w, h, w * 4, self.settings.opacity as f32, mode, rand_seed())?;
        with_bytes_raw_mut(&self.preview, |data, stride| {
            for r in 0..h { data[(y0 + r) * stride + x0 * 4..(y0 + r) * stride + x1 * 4].copy_from_slice(&pixels[r * w * 4..(r + 1) * w * 4]); }
        })?;
        self.changed = Some((x0 as i32, y0 as i32, x1 as i32, y1 as i32));
        Ok(())
    }

    pub fn touched_anything(&self) -> bool { !self.tiles.is_empty() }

    /// The grid rectangle worth keeping after the stroke: the layer's own pixels plus everything painted with
    /// any alpha (x0, y0, x1, y1), snapped outward to the halving alignment so the stroke's reduced copies
    /// can be cropped rather than rebuilt. The snap only ever adds transparent pixels.
    pub fn committed_bounds(&self) -> Option<(usize, usize, usize, usize)> {
        let (x0, y0, x1, y1) = self.tight_bounds()?;
        let a = GRID_ALIGN as usize;
        Some((x0 / a * a, y0 / a * a, (x1.div_ceil(a) * a).min(self.width), (y1.div_ceil(a) * a).min(self.height)))
    }

    fn tight_bounds(&self) -> Option<(usize, usize, usize, usize)> {
        let (sx, sy, sw, sh) = self.source;
        let mut bounds = if self.has_image { Some((sx, sy, sx + sw, sy + sh)) } else { None };
        let _ = crate::raster::with_bytes(&self.preview, |data, stride| {
            for tile in self.tiles.values() {
                let mut rows = vec![0u8; tile.w * tile.h * 4];
                for r in 0..tile.h { rows[r * tile.w * 4..(r + 1) * tile.w * 4].copy_from_slice(&data[(tile.y + r) * stride + tile.x * 4..(tile.y + r) * stride + (tile.x + tile.w) * 4]); }
                if let Some((l, t, r, b)) = ffi::alpha_bounds(&rows, tile.w, tile.h, tile.w * 4) {
                    let rect = (tile.x + l, tile.y + t, tile.x + r, tile.y + b);
                    bounds = Some(match bounds { Some(b) => (b.0.min(rect.0), b.1.min(rect.1), b.2.max(rect.2), b.3.max(rect.3)), None => rect });
                }
            }
        });
        bounds
    }

    /// The transform placing a grid rectangle on the document, once it becomes the layer's pixels.
    pub fn transform_for(&self, bounds: (usize, usize, usize, usize)) -> Transform {
        let (x0, y0, x1, y1) = bounds;
        let mut result = self.transform;
        result.size = crate::format::Size((x1 - x0) as f64 * self.transform.size.0 / self.width as f64, (y1 - y0) as f64 * self.transform.size.1 / self.height as f64);
        let (cx, cy) = self.to_document.transform_point((x0 + x1) as f64 / 2.0, (y0 + y1) as f64 / 2.0);
        result.origin = crate::format::Point(cx - result.size.0 / 2.0, cy - result.size.1 / 2.0);
        result
    }
}

fn rand_seed() -> u32 {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    (t as u64 ^ (t >> 64) as u64) as u32 ^ 0x9e37_79b9
}
