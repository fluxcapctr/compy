//! Gradients with any number of color and opacity stops, the shapes Photoshop's Gradient tool draws them
//! in, and the presets the options bar offers. A gradient is kept as JSON in the project (Gradient Map
//! layers carry one) and edited in the gradient editor.

use serde::{Deserialize, Serialize};

/// A color at a position from 0 (the start) to 1 (the end).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stop { pub position: f64, pub color: [f64; 3] }

/// An opacity at a position; kept apart from the colors, as Photoshop keeps them.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AlphaStop { pub position: f64, pub alpha: f64 }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Gradient {
    pub stops: Vec<Stop>,
    #[serde(default = "opaque")] pub alphas: Vec<AlphaStop>,
}

fn opaque() -> Vec<AlphaStop> { vec![AlphaStop { position: 0.0, alpha: 1.0 }, AlphaStop { position: 1.0, alpha: 1.0 }] }

/// How the gradient is laid over the drag line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Shape { Linear, Radial, Angle, Reflected, Diamond }

impl Shape {
    pub const ALL: [Shape; 5] = [Shape::Linear, Shape::Radial, Shape::Angle, Shape::Reflected, Shape::Diamond];
    pub fn name(self) -> &'static str { match self { Shape::Linear => "Linear", Shape::Radial => "Radial", Shape::Angle => "Angle", Shape::Reflected => "Reflected", Shape::Diamond => "Diamond" } }
    pub fn from_name(name: &str) -> Option<Shape> { Shape::ALL.into_iter().find(|s| s.name().eq_ignore_ascii_case(name.trim())) }
}

impl Default for Gradient { fn default() -> Self { Gradient::two([0.0; 3], [1.0; 3]) } }

impl Gradient {
    pub fn two(a: [f64; 3], b: [f64; 3]) -> Gradient { Gradient { stops: vec![Stop { position: 0.0, color: a }, Stop { position: 1.0, color: b }], alphas: opaque() } }
    /// One color fading out.
    pub fn to_transparent(color: [f64; 3]) -> Gradient { Gradient { stops: vec![Stop { position: 0.0, color }, Stop { position: 1.0, color }], alphas: vec![AlphaStop { position: 0.0, alpha: 1.0 }, AlphaStop { position: 1.0, alpha: 0.0 }] } }

    /// Stops in order, positions clamped, at least two of each; what every drawing path relies on.
    pub fn normalized(&self) -> Gradient {
        let mut stops: Vec<Stop> = self.stops.iter().map(|s| Stop { position: s.position.clamp(0.0, 1.0), color: s.color.map(|c| if c.is_finite() { c.clamp(0.0, 1.0) } else { 0.0 }) }).collect();
        if stops.is_empty() { stops = Gradient::default().stops; }
        if stops.len() == 1 { let s = stops[0]; stops = vec![Stop { position: 0.0, ..s }, Stop { position: 1.0, ..s }]; }
        stops.sort_by(|a, b| a.position.partial_cmp(&b.position).unwrap_or(std::cmp::Ordering::Equal));
        let mut alphas: Vec<AlphaStop> = self.alphas.iter().map(|a| AlphaStop { position: a.position.clamp(0.0, 1.0), alpha: if a.alpha.is_finite() { a.alpha.clamp(0.0, 1.0) } else { 1.0 } }).collect();
        if alphas.is_empty() { alphas = opaque(); }
        if alphas.len() == 1 { let a = alphas[0]; alphas = vec![AlphaStop { position: 0.0, ..a }, AlphaStop { position: 1.0, ..a }]; }
        alphas.sort_by(|a, b| a.position.partial_cmp(&b.position).unwrap_or(std::cmp::Ordering::Equal));
        Gradient { stops, alphas }
    }

    pub fn reversed(&self) -> Gradient {
        let n = self.normalized();
        Gradient { stops: n.stops.iter().rev().map(|s| Stop { position: 1.0 - s.position, color: s.color }).collect(), alphas: n.alphas.iter().rev().map(|a| AlphaStop { position: 1.0 - a.position, alpha: a.alpha }).collect() }
    }

    /// Every color turned to its gray, for painting a mask.
    pub fn grayed(&self) -> Gradient {
        let mut g = self.normalized();
        for s in &mut g.stops { let v = 0.299 * s.color[0] + 0.587 * s.color[1] + 0.114 * s.color[2]; s.color = [v, v, v]; }
        g
    }

    /// The color and alpha at `t` (0 to 1), interpolated between the stops on either side.
    pub fn at(&self, t: f64) -> [f64; 4] {
        let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
        let n = self.normalized();
        let color = lerp_stops(&n.stops.iter().map(|s| (s.position, s.color)).collect::<Vec<_>>(), t);
        let alpha = lerp_stops(&n.alphas.iter().map(|a| (a.position, [a.alpha; 3])).collect::<Vec<_>>(), t)[0];
        [color[0], color[1], color[2], alpha]
    }

    /// The positions where either a color or an opacity stop sits, so a Cairo gradient can be built from
    /// straight segments between them.
    pub fn knots(&self) -> Vec<f64> {
        let n = self.normalized();
        let mut k: Vec<f64> = n.stops.iter().map(|s| s.position).chain(n.alphas.iter().map(|a| a.position)).chain([0.0, 1.0]).collect();
        k.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        k.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        k
    }

    /// Adds the stops to a Cairo gradient over `t` from 0 to 1 (colors given straight; Cairo premultiplies).
    /// Two stops at one position make a hard edge: both sides go in, in order, as Cairo allows.
    pub fn fill_pattern(&self, pattern: &cairo::Gradient, from: f64, to: f64) {
        const EPS: f64 = 1e-6;
        for k in self.knots() {
            let (before, after) = (self.at((k - EPS).max(0.0)), self.at((k + EPS).min(1.0)));
            let offset = from + (to - from) * k;
            if k > 0.0 && k < 1.0 && before.iter().zip(&after).any(|(a, b)| (a - b).abs() > 1e-4) {
                pattern.add_color_stop_rgba(offset, before[0], before[1], before[2], before[3]);
            }
            pattern.add_color_stop_rgba(offset, after[0], after[1], after[2], after[3]);
        }
    }

    /// 256 entries of straight RGB bytes, for a Gradient Map.
    pub fn table(&self) -> [u8; 768] {
        let mut table = [0u8; 768];
        for i in 0..256 { let c = self.at(i as f64 / 255.0); for k in 0..3 { table[i * 3 + k] = (c[k] * 255.0).round().clamp(0.0, 255.0) as u8; } }
        table
    }

    pub fn to_json(&self) -> serde_json::Value { serde_json::to_value(self.normalized()).unwrap_or(serde_json::Value::Null) }
    pub fn from_json(value: &serde_json::Value) -> Option<Gradient> { serde_json::from_value::<Gradient>(value.clone()).ok().map(|g| g.normalized()) }

    /// The built-in presets, by name. Foreground and background presets are made from the palette by the caller.
    pub fn presets() -> Vec<(&'static str, Gradient)> {
        let c = |r: f64, g: f64, b: f64| [r, g, b];
        vec![
            ("Black to White", Gradient::two([0.0; 3], [1.0; 3])),
            ("Spectrum", Gradient { stops: vec![Stop { position: 0.0, color: c(1.0, 0.0, 0.0) }, Stop { position: 0.17, color: c(1.0, 1.0, 0.0) }, Stop { position: 0.33, color: c(0.0, 1.0, 0.0) }, Stop { position: 0.5, color: c(0.0, 1.0, 1.0) }, Stop { position: 0.67, color: c(0.0, 0.0, 1.0) }, Stop { position: 0.83, color: c(1.0, 0.0, 1.0) }, Stop { position: 1.0, color: c(1.0, 0.0, 0.0) }], alphas: opaque() }),
            ("Sunset", Gradient { stops: vec![Stop { position: 0.0, color: c(0.10, 0.05, 0.30) }, Stop { position: 0.45, color: c(0.85, 0.25, 0.35) }, Stop { position: 0.8, color: c(1.0, 0.65, 0.25) }, Stop { position: 1.0, color: c(1.0, 0.92, 0.70) }], alphas: opaque() }),
            ("Sky", Gradient { stops: vec![Stop { position: 0.0, color: c(0.12, 0.35, 0.80) }, Stop { position: 1.0, color: c(0.80, 0.92, 1.0) }], alphas: opaque() }),
            ("Chrome", Gradient { stops: vec![Stop { position: 0.0, color: c(0.95, 0.95, 0.97) }, Stop { position: 0.48, color: c(0.55, 0.58, 0.65) }, Stop { position: 0.52, color: c(0.25, 0.28, 0.35) }, Stop { position: 1.0, color: c(0.85, 0.88, 0.92) }], alphas: opaque() }),
            ("Copper", Gradient { stops: vec![Stop { position: 0.0, color: c(0.45, 0.20, 0.10) }, Stop { position: 0.5, color: c(0.95, 0.65, 0.45) }, Stop { position: 1.0, color: c(0.35, 0.15, 0.08) }], alphas: opaque() }),
            ("Transparent Stripes", Gradient { stops: vec![Stop { position: 0.0, color: [0.0; 3] }, Stop { position: 1.0, color: [0.0; 3] }], alphas: vec![AlphaStop { position: 0.0, alpha: 1.0 }, AlphaStop { position: 0.25, alpha: 0.0 }, AlphaStop { position: 0.5, alpha: 1.0 }, AlphaStop { position: 0.75, alpha: 0.0 }, AlphaStop { position: 1.0, alpha: 1.0 }] }),
        ]
    }
}

fn lerp_stops(stops: &[(f64, [f64; 3])], t: f64) -> [f64; 3] {
    if t <= stops[0].0 { return stops[0].1; }
    for pair in stops.windows(2) {
        let ((p0, c0), (p1, c1)) = (pair[0], pair[1]);
        if t <= p1 {
            if p1 - p0 <= 1e-9 { return c1; }
            let u = (t - p0) / (p1 - p0);
            return [c0[0] + (c1[0] - c0[0]) * u, c0[1] + (c1[1] - c0[1]) * u, c0[2] + (c1[2] - c0[2]) * u];
        }
    }
    stops[stops.len() - 1].1
}

/// The parameter `t` of a point for the shapes Cairo has no pattern for: `angle` sweeps around the start,
/// `diamond` grows a square from it, both reaching 1 at the end's distance.
pub fn shape_t(shape: Shape, start: (f64, f64), end: (f64, f64), p: (f64, f64)) -> f64 {
    let (dx, dy) = (end.0 - start.0, end.1 - start.1);
    let len = dx.hypot(dy);
    // No length: the start color everywhere, whatever the shape.
    if len < 1e-9 { return 0.0; }
    let (px, py) = (p.0 - start.0, p.1 - start.1);
    match shape {
        Shape::Angle => {
            let a = py.atan2(px) - dy.atan2(dx);
            (a.rem_euclid(std::f64::consts::TAU)) / std::f64::consts::TAU
        }
        Shape::Diamond => {
            // Distance in the line's own frame, as a square rather than a circle.
            let (ux, uy) = (dx / len, dy / len);
            let along = (px * ux + py * uy).abs();
            let across = (px * uy - py * ux).abs();
            (along.max(across) / len).min(1.0)
        }
        Shape::Reflected => (((px * dx + py * dy) / (len * len)).abs()).min(1.0),
        Shape::Radial => (px.hypot(py) / len).min(1.0),
        Shape::Linear => ((px * dx + py * dy) / (len * len)).clamp(0.0, 1.0),
    }
}

/// The gradient rasterized over `width` x `height` document pixels (premultiplied ARGB), for the shapes
/// Cairo cannot express as a pattern.
pub fn raster(width: i32, height: i32, shape: Shape, start: (f64, f64), end: (f64, f64), gradient: &Gradient) -> anyhow::Result<cairo::ImageSurface> {
    let surface = crate::raster::new_argb(width, height)?;
    // A lookup of 1024 steps keeps the per-pixel work to an index.
    let lut: Vec<[u8; 4]> = (0..1024).map(|i| { let c = gradient.at(i as f64 / 1023.0); let a = c[3]; [(c[2] * a * 255.0).round() as u8, (c[1] * a * 255.0).round() as u8, (c[0] * a * 255.0).round() as u8, (a * 255.0).round() as u8] }).collect();
    crate::raster::with_bytes_raw_mut(&surface, |data, stride| {
        for y in 0..height as usize {
            for x in 0..width as usize {
                let t = shape_t(shape, start, end, (x as f64 + 0.5, y as f64 + 0.5));
                let px = lut[((t * 1023.0).round() as usize).min(1023)];
                data[y * stride + x * 4..y * stride + x * 4 + 4].copy_from_slice(&px);
            }
        }
    })?;
    Ok(surface)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stops_interpolate_reverse_and_round_trip() {
        let g = Gradient { stops: vec![Stop { position: 0.0, color: [1.0, 0.0, 0.0] }, Stop { position: 0.5, color: [0.0, 1.0, 0.0] }, Stop { position: 1.0, color: [0.0, 0.0, 1.0] }], alphas: vec![AlphaStop { position: 0.0, alpha: 1.0 }, AlphaStop { position: 1.0, alpha: 0.0 }] };
        assert_eq!(g.at(0.0), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(g.at(0.5), [0.0, 1.0, 0.0, 0.5]);
        let q = g.at(0.25);
        assert!((q[0] - 0.5).abs() < 1e-9 && (q[1] - 0.5).abs() < 1e-9 && (q[3] - 0.75).abs() < 1e-9);
        assert_eq!(g.reversed().at(0.0), [0.0, 0.0, 1.0, 0.0]);
        assert_eq!(Gradient::from_json(&g.to_json()).unwrap(), g.normalized());
        let messy = Gradient { stops: vec![Stop { position: 1.5, color: [2.0, -1.0, 0.5] }], alphas: vec![] };
        let n = messy.normalized();
        assert_eq!(n.stops.len(), 2);
        assert_eq!(n.stops[1].color, [1.0, 0.0, 0.5]);
        assert_eq!(n.alphas.len(), 2);
        assert_eq!(g.knots(), vec![0.0, 0.5, 1.0]);
        // Two stops at one position: a hard edge, kept on both sides in a Cairo pattern.
        let hard = Gradient { stops: vec![Stop { position: 0.0, color: [0.0; 3] }, Stop { position: 0.5, color: [0.0; 3] }, Stop { position: 0.5, color: [1.0; 3] }, Stop { position: 1.0, color: [1.0; 3] }], alphas: vec![] };
        let surface = crate::raster::new_argb(100, 1).unwrap();
        let cr = cairo::Context::new(&surface).unwrap();
        let p = cairo::LinearGradient::new(0.0, 0.0, 100.0, 0.0);
        hard.fill_pattern(&p, 0.0, 1.0);
        cr.set_source(&p).unwrap();
        cr.paint().unwrap();
        drop(cr);
        let (a, b) = crate::raster::with_bytes(&surface, |d, _| (d[25 * 4], d[75 * 4])).unwrap();
        assert!(a < 8 && b > 247, "black before the edge, white after it: {a} {b}");
        assert_eq!(Shape::from_name("diamond"), Some(Shape::Diamond));
    }

    #[test]
    fn shapes_parameterize_as_expected() {
        let (s, e) = ((10.0, 10.0), (20.0, 10.0));
        assert!((shape_t(Shape::Linear, s, e, (15.0, 10.0)) - 0.5).abs() < 1e-9);
        assert_eq!(shape_t(Shape::Linear, s, e, (0.0, 10.0)), 0.0);
        assert!((shape_t(Shape::Reflected, s, e, (5.0, 10.0)) - 0.5).abs() < 1e-9, "reflected mirrors across the start");
        assert!((shape_t(Shape::Radial, s, e, (10.0, 15.0)) - 0.5).abs() < 1e-9);
        assert!((shape_t(Shape::Diamond, s, e, (10.0, 15.0)) - 0.5).abs() < 1e-9);
        assert!((shape_t(Shape::Diamond, s, e, (15.0, 15.0)) - 0.5).abs() < 1e-9, "a diamond's corner");
        assert!((shape_t(Shape::Angle, s, e, (10.0, 20.0)) - 0.25).abs() < 1e-9, "a quarter turn from the line");
        let g = Gradient::two([0.0; 3], [1.0; 3]);
        let r = raster(4, 1, Shape::Linear, (0.0, 0.5), (4.0, 0.5), &g).unwrap();
        let v = crate::raster::with_bytes(&r, |d, _| [d[0], d[3 * 4]]).unwrap();
        assert!(v[0] < 40 && v[1] > 200, "dark at the start, light at the end: {v:?}");
    }
}
