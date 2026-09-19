//! Layer effects (Photoshop's layer styles): drop and inner shadows, outer and inner glows, bevel and
//! emboss, stroke and color overlay, kept as a record on the layer and rendered from the layer's alpha
//! in document pixels. `render` returns what goes under the layer and what goes over it, so the layer's
//! own pixels (and a stroke being painted on them) draw between the two.

use serde::{Deserialize, Serialize};

fn one() -> f64 { 1.0 }
fn t() -> bool { true }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shadow {
    #[serde(default = "t")] pub enabled: bool,
    pub color: [f64; 3],
    pub opacity: f64,
    /// Light angle in degrees, counterclockwise from the right as Photoshop measures it.
    pub angle: f64,
    pub distance: f64,
    /// Blur in pixels.
    pub size: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Glow {
    #[serde(default = "t")] pub enabled: bool,
    pub color: [f64; 3],
    pub opacity: f64,
    pub size: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bevel {
    #[serde(default = "t")] pub enabled: bool,
    /// 0 inner bevel, 1 outer bevel, 2 emboss.
    pub style: u32,
    /// Percent, 100 is Photoshop's default.
    pub depth: f64,
    pub size: f64,
    pub angle: f64,
    pub altitude: f64,
    pub highlight_opacity: f64,
    pub shadow_opacity: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    #[serde(default = "t")] pub enabled: bool,
    pub size: f64,
    /// 0 outside, 1 inside, 2 center.
    pub position: u32,
    pub color: [f64; 3],
    pub opacity: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Overlay {
    #[serde(default = "t")] pub enabled: bool,
    pub color: [f64; 3],
    #[serde(default = "one")] pub opacity: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Effects {
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "dropShadow")] pub drop_shadow: Option<Shadow>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "innerShadow")] pub inner_shadow: Option<Shadow>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "outerGlow")] pub outer_glow: Option<Glow>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "innerGlow")] pub inner_glow: Option<Glow>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub bevel: Option<Bevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub stroke: Option<Stroke>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "colorOverlay")] pub color_overlay: Option<Overlay>,
}

impl Shadow {
    pub fn drop_default() -> Shadow { Shadow { enabled: true, color: [0.0; 3], opacity: 0.75, angle: 120.0, distance: 5.0, size: 5.0 } }
    pub fn inner_default() -> Shadow { Shadow { enabled: true, color: [0.0; 3], opacity: 0.75, angle: 120.0, distance: 5.0, size: 5.0 } }
}
impl Glow {
    pub fn outer_default() -> Glow { Glow { enabled: true, color: [1.0, 1.0, 0.75], opacity: 0.75, size: 10.0 } }
    pub fn inner_default() -> Glow { Glow { enabled: true, color: [1.0, 1.0, 0.75], opacity: 0.75, size: 10.0 } }
}
impl Default for Bevel {
    fn default() -> Bevel { Bevel { enabled: true, style: 0, depth: 100.0, size: 5.0, angle: 120.0, altitude: 30.0, highlight_opacity: 0.75, shadow_opacity: 0.75 } }
}
impl Default for Stroke {
    fn default() -> Stroke { Stroke { enabled: true, size: 3.0, position: 0, color: [1.0, 0.0, 0.0], opacity: 1.0 } }
}
impl Default for Overlay {
    fn default() -> Overlay { Overlay { enabled: true, color: [1.0, 0.0, 0.0], opacity: 1.0 } }
}

/// What the effects draw, in premultiplied ARGB32 rows the size of the alpha given.
pub struct Rendered {
    pub below: Option<Vec<u8>>,
    pub above: Option<Vec<u8>>,
}

fn clamp01(v: f64) -> f64 { v.clamp(0.0, 1.0) }

impl Effects {
    pub fn to_record(&self) -> serde_json::Value { serde_json::to_value(self).unwrap_or(serde_json::Value::Null) }
    pub fn from_record(value: &serde_json::Value) -> Option<Effects> { serde_json::from_value(value.clone()).ok() }

    /// Whether anything draws.
    pub fn is_active(&self) -> bool {
        self.drop_shadow.as_ref().is_some_and(|e| e.enabled) || self.inner_shadow.as_ref().is_some_and(|e| e.enabled)
            || self.outer_glow.as_ref().is_some_and(|e| e.enabled) || self.inner_glow.as_ref().is_some_and(|e| e.enabled)
            || self.bevel.as_ref().is_some_and(|e| e.enabled) || self.stroke.as_ref().is_some_and(|e| e.enabled)
            || self.color_overlay.as_ref().is_some_and(|e| e.enabled)
    }

    /// Effects that draw outside the layer's bounds, and how far (document pixels).
    pub fn reach(&self) -> i32 {
        let mut r: f64 = 0.0;
        if let Some(s) = self.drop_shadow.as_ref().filter(|e| e.enabled) { r = r.max(s.distance.abs() + s.size * 3.0); }
        if let Some(g) = self.outer_glow.as_ref().filter(|e| e.enabled) { r = r.max(g.size * 3.0); }
        if let Some(b) = self.bevel.as_ref().filter(|e| e.enabled && e.style != 0) { r = r.max(b.size * 2.0); }
        if let Some(s) = self.stroke.as_ref().filter(|e| e.enabled && e.position != 1) { r = r.max(s.size + 1.0); }
        r.ceil().clamp(0.0, 4000.0) as i32 + 1
    }

    /// Everything the effects draw around `alpha` (`w` x `h`, a coverage from 0 to 255).
    pub fn render(&self, alpha: &[u8], w: usize, h: usize) -> Rendered {
        let n = w * h;
        let mut below: Option<Vec<u8>> = None;
        let mut above: Option<Vec<u8>> = None;
        let a01: Vec<f32> = alpha.iter().map(|a| *a as f32 / 255.0).collect();
        fn layer(buf: &mut Option<Vec<u8>>, n: usize) -> &mut Vec<u8> { buf.get_or_insert_with(|| vec![0u8; n * 4]) }

        if let Some(s) = self.drop_shadow.as_ref().filter(|e| e.enabled) {
            let blurred = blur(alpha, w, h, s.size);
            let (dx, dy) = offset(s.angle, s.distance);
            let shifted = shift(&blurred, w, h, dx, dy);
            let mut cov: Vec<f32> = shifted.iter().map(|v| *v as f32 / 255.0 * s.opacity as f32).collect();
            // Photoshop knocks the layer out of its own shadow.
            for (c, a) in cov.iter_mut().zip(&a01) { *c *= 1.0 - a; }
            paint(layer(&mut below, n), &cov, s.color);
        }
        if let Some(g) = self.outer_glow.as_ref().filter(|e| e.enabled) {
            let blurred = blur(alpha, w, h, g.size);
            let cov: Vec<f32> = blurred.iter().zip(&a01).map(|(v, a)| clamp01((*v as f64 / 255.0 * 1.6).min(1.0) * g.opacity) as f32 * (1.0 - a)).collect();
            paint(layer(&mut below, n), &cov, g.color);
        }
        if let Some(o) = self.color_overlay.as_ref().filter(|e| e.enabled) {
            let cov: Vec<f32> = a01.iter().map(|a| a * o.opacity as f32).collect();
            paint(layer(&mut above, n), &cov, o.color);
        }
        if let Some(g) = self.inner_glow.as_ref().filter(|e| e.enabled) {
            let inverse: Vec<u8> = alpha.iter().map(|a| 255 - a).collect();
            let blurred = blur(&inverse, w, h, g.size);
            let cov: Vec<f32> = blurred.iter().zip(&a01).map(|(v, a)| clamp01((*v as f64 / 255.0 * 1.6).min(1.0) * g.opacity) as f32 * a).collect();
            paint(layer(&mut above, n), &cov, g.color);
        }
        if let Some(s) = self.inner_shadow.as_ref().filter(|e| e.enabled) {
            let inverse: Vec<u8> = alpha.iter().map(|a| 255 - a).collect();
            let blurred = blur(&inverse, w, h, s.size);
            let (dx, dy) = offset(s.angle, s.distance);
            let shifted = shift_fill(&blurred, w, h, dx, dy, 255);
            let cov: Vec<f32> = shifted.iter().zip(&a01).map(|(v, a)| *v as f32 / 255.0 * s.opacity as f32 * a).collect();
            paint(layer(&mut above, n), &cov, s.color);
        }
        if let Some(s) = self.stroke.as_ref().filter(|e| e.enabled && e.size > 0.0) {
            let inside: Vec<bool> = alpha.iter().map(|a| *a >= 128).collect();
            // Distances run between pixel centers; the shape's edge sits half a pixel before the first inside pixel.
            let outside_d = distance(&inside, w, h, true);
            let inside_d = distance(&inside, w, h, false);
            let size = s.size as f32;
            let cov: Vec<f32> = (0..n).map(|i| {
                let a = a01[i];
                let c = match s.position {
                    1 => (size + 1.0 - inside_d[i]).clamp(0.0, 1.0) * a,
                    2 => (size / 2.0 + 1.0 - outside_d[i].min(inside_d[i])).clamp(0.0, 1.0),
                    _ => (size + 1.0 - outside_d[i]).clamp(0.0, 1.0) * (1.0 - a),
                };
                c * s.opacity as f32
            }).collect();
            paint(layer(&mut above, n), &cov, s.color);
        }
        if let Some(b) = self.bevel.as_ref().filter(|e| e.enabled && e.size > 0.0) {
            let height = blur(alpha, w, h, b.size);
            let height: Vec<f32> = height.iter().map(|v| *v as f32 / 255.0).collect();
            let (la, alt) = (b.angle.to_radians(), b.altitude.to_radians().clamp(0.05, 1.5));
            // Light direction in image space (y down), with its altitude above the surface.
            let light = ((la.cos() * alt.cos()) as f32, (-(la.sin()) * alt.cos()) as f32, alt.sin() as f32);
            let scale = (b.size * b.depth / 100.0 * 1.5) as f32;
            let mut highlight = vec![0f32; n];
            let mut shadow = vec![0f32; n];
            for y in 0..h {
                for x in 0..w {
                    let i = y * w + x;
                    let dx = height[y * w + (x + 1).min(w - 1)] - height[y * w + x.saturating_sub(1)];
                    let dy = height[(y + 1).min(h - 1) * w + x] - height[y.saturating_sub(1) * w + x];
                    let (nx, ny, nz) = (-dx * scale / 2.0, -dy * scale / 2.0, 1.0f32);
                    let len = (nx * nx + ny * ny + nz * nz).sqrt();
                    let dot = (nx * light.0 + ny * light.1 + nz * light.2) / len;
                    let delta = dot - light.2;
                    let region = match b.style { 0 => a01[i], 1 => 1.0 - a01[i], _ => 1.0 };
                    if delta > 0.0 { highlight[i] = (delta * 2.0).min(1.0) * region * b.highlight_opacity as f32; }
                    else { shadow[i] = (-delta * 2.0).min(1.0) * region * b.shadow_opacity as f32; }
                }
            }
            paint(layer(&mut above, n), &highlight, [1.0; 3]);
            paint(layer(&mut above, n), &shadow, [0.0; 3]);
        }
        Rendered { below, above }
    }
}

/// A shadow's offset for Photoshop's angle: 120 degrees puts the light upper left, the shadow lower right.
fn offset(angle: f64, distance: f64) -> (f64, f64) {
    let a = angle.to_radians();
    (-a.cos() * distance, a.sin() * distance)
}

fn blur(alpha: &[u8], w: usize, h: usize, size: f64) -> Vec<u8> {
    let mut out = alpha.to_vec();
    if size > 0.1 { crate::blur::gaussian(&mut out, w, h, 1, size / 2.0); }
    out
}

fn shift(src: &[u8], w: usize, h: usize, dx: f64, dy: f64) -> Vec<u8> { shift_fill(src, w, h, dx, dy, 0) }

/// `src` moved by (`dx`, `dy`) with bilinear sampling; `fill` stands in beyond its edges.
fn shift_fill(src: &[u8], w: usize, h: usize, dx: f64, dy: f64, fill: u8) -> Vec<u8> {
    let mut out = vec![fill; w * h];
    let sample = |x: i64, y: i64| -> f64 { if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 { fill as f64 } else { src[y as usize * w + x as usize] as f64 } };
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = (x as f64 - dx, y as f64 - dy);
            let (x0, y0) = (sx.floor(), sy.floor());
            let (fx, fy) = (sx - x0, sy - y0);
            let (x0, y0) = (x0 as i64, y0 as i64);
            let v = sample(x0, y0) * (1.0 - fx) * (1.0 - fy) + sample(x0 + 1, y0) * fx * (1.0 - fy) + sample(x0, y0 + 1) * (1.0 - fx) * fy + sample(x0 + 1, y0 + 1) * fx * fy;
            out[y * w + x] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// Composites `color` at `coverage` over a premultiplied ARGB32 buffer.
fn paint(buf: &mut [u8], coverage: &[f32], color: [f64; 3]) {
    let (r, g, b) = (color[0] as f32, color[1] as f32, color[2] as f32);
    for (i, c) in coverage.iter().enumerate() {
        if *c <= 0.0005 { continue; }
        let c = c.min(1.0);
        let p = &mut buf[i * 4..i * 4 + 4];
        let keep = 1.0 - c;
        p[0] = (b * c * 255.0 + p[0] as f32 * keep).round().min(255.0) as u8;
        p[1] = (g * c * 255.0 + p[1] as f32 * keep).round().min(255.0) as u8;
        p[2] = (r * c * 255.0 + p[2] as f32 * keep).round().min(255.0) as u8;
        p[3] = (c * 255.0 + p[3] as f32 * keep).round().min(255.0) as u8;
    }
}

/// Euclidean distance from every pixel to the nearest pixel that is (`to_inside`) inside or (else) outside
/// the shape, measured between pixel centers; 0 on pixels that are themselves such pixels.
fn distance(inside: &[bool], w: usize, h: usize, to_inside: bool) -> Vec<f32> {
    const FAR: f32 = 1.0e12;
    let mut f: Vec<f32> = inside.iter().map(|i| if *i == to_inside { 0.0 } else { FAR }).collect();
    let mut column = vec![0f32; h.max(w)];
    let mut d = vec![0f32; h.max(w)];
    let mut v = vec![0usize; h.max(w)];
    let mut z = vec![0f32; h.max(w) + 1];
    for x in 0..w {
        for y in 0..h { column[y] = f[y * w + x]; }
        transform_1d(&column[..h], &mut d[..h], &mut v, &mut z);
        for y in 0..h { f[y * w + x] = d[y]; }
    }
    for y in 0..h {
        column[..w].copy_from_slice(&f[y * w..(y + 1) * w]);
        transform_1d(&column[..w], &mut d[..w], &mut v, &mut z);
        f[y * w..(y + 1) * w].copy_from_slice(&d[..w]);
    }
    f.iter().map(|sq| sq.min(1.0e12).sqrt()).collect()
}

/// One pass of Felzenszwalb and Huttenlocher's squared distance transform.
fn transform_1d(f: &[f32], d: &mut [f32], v: &mut [usize], z: &mut [f32]) {
    let n = f.len();
    if n == 0 { return; }
    let mut k = 0usize;
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..n {
        loop {
            let p = v[k];
            let s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2.0 * (q as f32 - p as f32));
            if s <= z[k] && k > 0 { k -= 1; continue; }
            if s <= z[k] { v[k] = q; z[k + 1] = f32::INFINITY; break; }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = f32::INFINITY;
            break;
        }
    }
    k = 0;
    for q in 0..n {
        while z[k + 1] < q as f32 { k += 1; }
        let p = v[k];
        d[q] = (q as f32 - p as f32).powi(2) + f[p];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(w: usize, h: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> Vec<u8> {
        let mut a = vec![0u8; w * h];
        for y in y0..y1 { for x in x0..x1 { a[y * w + x] = 255; } }
        a
    }

    #[test]
    fn distances_measure_to_the_shape_edge() {
        let a = square(20, 20, 5, 5, 15, 15);
        let inside: Vec<bool> = a.iter().map(|v| *v >= 128).collect();
        let out = distance(&inside, 20, 20, true);
        assert_eq!(out[10 * 20 + 10], 0.0);
        assert_eq!(out[10 * 20 + 2], 3.0, "three pixels left of the square");
        assert!((out[0] - (5.0f32 * 5.0 + 5.0 * 5.0).sqrt()).abs() < 0.01);
        let inn = distance(&inside, 20, 20, false);
        assert_eq!(inn[10 * 20 + 5], 1.0, "the edge pixel is one from the outside");
        assert_eq!(inn[10 * 20 + 9], 5.0);
    }

    #[test]
    fn drop_shadow_falls_lower_right_and_stroke_rings_the_shape() {
        let (w, h) = (40, 40);
        let a = square(w, h, 10, 10, 30, 30);
        let mut e = Effects::default();
        e.drop_shadow = Some(Shadow { size: 0.0, distance: 5.0, ..Shadow::drop_default() });
        let r = e.render(&a, w, h);
        let below = r.below.unwrap();
        assert!(r.above.is_none());
        let at = |x: usize, y: usize| below[(y * w + x) * 4 + 3];
        assert!(at(31, 31) > 150, "shadow lower right: {}", at(31, 31));
        assert_eq!(at(8, 8), 0, "no shadow upper left");
        assert_eq!(at(20, 20), 0, "knocked out under the shape");
        let mut e = Effects::default();
        e.stroke = Some(Stroke { size: 2.0, position: 0, ..Stroke::default() });
        let r = e.render(&a, w, h);
        let above = r.above.unwrap();
        let at = |x: usize, y: usize| above[(y * w + x) * 4 + 3];
        assert_eq!(at(8, 20), 255, "outside stroke two pixels out");
        assert_eq!(at(20, 20), 0, "nothing on the shape itself");
        assert_eq!(at(6, 20), 0, "nothing past the stroke");
        assert!(e.reach() >= 3);
        assert_eq!(Effects::from_record(&e.to_record()), Some(e));
    }

    #[test]
    fn bevel_lights_the_upper_left_edge() {
        let (w, h) = (40, 40);
        let a = square(w, h, 10, 10, 30, 30);
        let mut e = Effects::default();
        e.bevel = Some(Bevel::default());
        let above = e.render(&a, w, h).above.unwrap();
        let px = |x: usize, y: usize| { let p = &above[(y * w + x) * 4..][..4]; (p[2], p[3]) };
        let (top_r, top_a) = px(20, 11);
        let (bottom_r, bottom_a) = px(20, 28);
        // Premultiplied: white has red equal to alpha, black has none.
        assert!(top_a > 30 && top_r + 1 >= top_a, "highlight along the top edge: {top_r} {top_a}");
        assert!(bottom_a > 30 && bottom_r == 0, "shadow along the bottom edge: {bottom_r} {bottom_a}");
        assert_eq!(px(20, 20).1, 0, "flat middle untouched");
    }
}
