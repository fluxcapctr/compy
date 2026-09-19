//! The brushes that ship with the app, made here rather than drawn by hand: textured, noisy and shaped tips
//! in the spirit of Photoshop's default set, written out as a version 2 `.abr` so the same loader reads them.

use crate::abr::Preset;
use std::rc::Rc;

const SIDE: usize = 256;

/// A small deterministic hash noise, 0 to 1, for textures that look the same on every machine.
fn hash(x: i64, y: i64, seed: u64) -> f64 {
    let mut h = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ seed.wrapping_mul(0x1656_67B1_9E37_79F9);
    h ^= h >> 29; h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9); h ^= h >> 32;
    (h & 0xFFFF_FFFF) as f64 / 4_294_967_295.0
}

/// Value noise: smooth between lattice points `cell` apart.
fn value_noise(x: f64, y: f64, cell: f64, seed: u64) -> f64 {
    let (gx, gy) = (x / cell, y / cell);
    let (x0, y0) = (gx.floor(), gy.floor());
    let (fx, fy) = (gx - x0, gy - y0);
    let s = |t: f64| t * t * (3.0 - 2.0 * t);
    let (sx, sy) = (s(fx), s(fy));
    let (x0, y0) = (x0 as i64, y0 as i64);
    let a = hash(x0, y0, seed); let b = hash(x0 + 1, y0, seed); let c = hash(x0, y0 + 1, seed); let d = hash(x0 + 1, y0 + 1, seed);
    let top = a + (b - a) * sx;
    let bottom = c + (d - c) * sx;
    top + (bottom - top) * sy
}

/// Fractal noise, a few octaves, 0 to 1.
fn fbm(x: f64, y: f64, cell: f64, seed: u64, octaves: u32) -> f64 {
    let (mut sum, mut amp, mut total, mut c) = (0.0, 1.0, 0.0, cell);
    for o in 0..octaves { sum += value_noise(x, y, c, seed + o as u64 * 17) * amp; total += amp; amp *= 0.5; c /= 2.0; }
    sum / total
}

fn make(name: &str, spacing: f64, f: impl Fn(f64, f64) -> f64) -> Rc<Preset> { make_jittered(name, spacing, 1.0, f) }

/// `jitter` 0 keeps a directional tip (a flat, a rake) pointing the same way.
fn make_jittered(name: &str, spacing: f64, jitter: f64, f: impl Fn(f64, f64) -> f64) -> Rc<Preset> {
    let mut pixels = vec![0u8; SIDE * SIDE];
    for y in 0..SIDE { for x in 0..SIDE {
        // Coordinates from -1 to 1 across the tip.
        let (u, v) = ((x as f64 + 0.5) / SIDE as f64 * 2.0 - 1.0, (y as f64 + 0.5) / SIDE as f64 * 2.0 - 1.0);
        pixels[y * SIDE + x] = (f(u, v).clamp(0.0, 1.0) * 255.0).round() as u8;
    } }
    Rc::new(Preset { name: name.to_string(), width: SIDE, height: SIDE, pixels, spacing, jitter, set: "Compositor Basics".into(), frames: Vec::new() })
}

fn px(u: f64) -> f64 { (u + 1.0) / 2.0 * SIDE as f64 }
fn disc(u: f64, v: f64, r: f64, soft: f64) -> f64 { let d = u.hypot(v); ((r - d) / soft.max(0.002)).clamp(0.0, 1.0) }

/// Every bundled tip, in the order the picker shows them.
pub fn presets() -> Vec<Rc<Preset>> {
    vec![
        make("Chalk", 28.0, |u, v| { let n = fbm(px(u), px(v), 5.0, 1, 3); let edge = disc(u, v, 0.92 + 0.08 * (fbm(px(u), px(v), 40.0, 2, 2) - 0.5), 0.08); edge * ((n - 0.47) * 4.0).clamp(0.0, 1.0) }),
        make("Charcoal", 22.0, |u, v| { let (a, b) = (u * 0.75, v * 1.15); let n = fbm(px(u), px(v), 4.0, 3, 3); let edge = disc(a, b, 0.9 + 0.1 * (fbm(px(u), px(v), 30.0, 4, 2) - 0.5), 0.12); edge * ((n - 0.44) * 3.5).clamp(0.0, 1.0) }),
        make_jittered("Dry Brush", 12.0, 0.0, |u, v| { if u.abs() > 0.95 { return 0.0; } let lane = ((v + 1.0) * 14.0).floor() as i64; let strength = hash(lane, 0, 5); if strength < 0.45 { return 0.0; } let ends = ((1.0 - u.abs()) / 0.25).clamp(0.0, 1.0); let wobble = fbm(px(u), px(v), 9.0, 6, 2); ends * strength * ((wobble - 0.4) * 3.0).clamp(0.0, 1.0) * disc(0.0, v, 0.9, 0.2) }),
        make("Sponge", 30.0, |u, v| { let n = fbm(px(u), px(v), 22.0, 7, 3); let n2 = fbm(px(u), px(v), 7.0, 8, 2); disc(u, v, 0.95, 0.15) * ((n - 0.5) * 5.0 + (n2 - 0.5) * 0.6).clamp(0.0, 1.0) }),
        make("Spatter", 45.0, |u, v| { let mut best: f64 = 0.0; for i in 0..70 { let (cx, cy) = (hash(i, 1, 9) * 2.0 - 1.0, hash(i, 2, 9) * 2.0 - 1.0); if cx.hypot(cy) > 0.9 { continue; } let r = 0.02 + hash(i, 3, 9).powi(3) * 0.16; best = best.max(disc(u - cx, v - cy, r, 0.01)); } best }),
        make("Stipple", 30.0, |u, v| { let (cx, cy) = (((u + 1.0) * 10.0).floor(), ((v + 1.0) * 10.0).floor()); let (jx, jy) = (hash(cx as i64, cy as i64, 11) - 0.5, hash(cx as i64, cy as i64, 12) - 0.5); let (dx, dy) = ((u + 1.0) * 10.0 - cx - 0.5 - jx * 0.6, (v + 1.0) * 10.0 - cy - 0.5 - jy * 0.6); let r = 0.16 + hash(cx as i64, cy as i64, 13) * 0.14; disc(dx, dy, r, 0.04) * disc(u, v, 0.92, 0.3) }),
        make("Grain", 25.0, |u, v| { let n = hash((px(u)) as i64, (px(v)) as i64, 21); disc(u, v, 0.95, 0.35) * ((n - 0.55) * 2.5).clamp(0.0, 1.0) }),
        make("Soft Grain", 15.0, |u, v| { let n = fbm(px(u), px(v), 3.0, 22, 2); disc(u, v, 0.95, 0.9) * (0.5 + 0.5 * n) }),
        make("Watercolor", 20.0, |u, v| { let d = u.hypot(v); let wobble = 0.86 + 0.1 * (fbm(px(u), px(v), 48.0, 31, 2) - 0.5); let inside = ((wobble - d) / 0.05).clamp(0.0, 1.0); let rim = ((d - (wobble - 0.16)) / 0.16).clamp(0.0, 1.0); let mottle = 0.55 + 0.45 * fbm(px(u), px(v), 30.0, 32, 3); inside * (mottle * 0.6 + rim * 0.5).clamp(0.0, 1.0) }),
        make("Splat", 50.0, |u, v| { let angle = v.atan2(u); let spikes = 0.62 + 0.24 * ((angle * 7.0).sin() * 0.5 + 0.5) * fbm(angle * 40.0 + 100.0, 0.0, 6.0, 41, 2) + 0.12 * fbm(px(u), px(v), 20.0, 42, 2); disc(u, v, spikes, 0.03) }),
        make_jittered("Flat", 6.0, 0.0, |u, v| { if v.abs() > 0.18 { return 0.0; } let ends = ((0.96 - u.abs()) / 0.06).clamp(0.0, 1.0); let side = ((0.18 - v.abs()) / 0.03).clamp(0.0, 1.0); ends * side * (0.85 + 0.15 * fbm(px(u), px(v), 5.0, 51, 2)) }),
        make_jittered("Angled Flat", 6.0, 0.0, |u, v| { let (c, s) = ((45.0f64).to_radians().cos(), (45.0f64).to_radians().sin()); let (a, b) = (u * c + v * s, -u * s + v * c); if b.abs() > 0.14 { return 0.0; } ((0.95 - a.abs()) / 0.05).clamp(0.0, 1.0) * ((0.14 - b.abs()) / 0.03).clamp(0.0, 1.0) }),
        make_jittered("Rake", 4.0, 0.0, |u, v| { let lanes = 6.0; let t = (v + 0.8) / 1.6 * lanes; if v.abs() > 0.8 { return 0.0; } let within = (t - t.floor() - 0.5).abs(); let bristle = ((0.18 - within) / 0.06).clamp(0.0, 1.0); bristle * ((0.9 - u.abs()) / 0.2).clamp(0.0, 1.0) * (0.7 + 0.3 * fbm(px(u), px(v), 4.0, 61, 2)) }),
        make("Scatter Dots", 80.0, |u, v| { let mut best: f64 = 0.0; for i in 0..14 { let (cx, cy) = (hash(i, 7, 71) * 1.7 - 0.85, hash(i, 8, 71) * 1.7 - 0.85); best = best.max(disc(u - cx, v - cy, 0.07 + hash(i, 9, 71) * 0.08, 0.02)); } best }),
        make("Soft Splotch", 25.0, |u, v| { let d = u.hypot(v); let shape = 0.9 + 0.12 * (fbm(px(u), px(v), 64.0, 81, 2) - 0.5); ((shape - d) / 0.5).clamp(0.0, 1.0).powf(1.4) }),
        make_jittered("Cross Hatch", 10.0, 0.0, |u, v| { let a = ((u + v) * 22.0).sin(); let b = ((u - v) * 22.0).sin(); let lines = ((a.abs() - 0.75) * 6.0).clamp(0.0, 1.0).max(((b.abs() - 0.75) * 6.0).clamp(0.0, 1.0)); lines * disc(u, v, 0.9, 0.2) * (0.6 + 0.4 * fbm(px(u), px(v), 8.0, 91, 2)) }),
    ]
}

/// The set as a version 2 `.abr` file (names included), the form the loader reads back.
pub fn abr_bytes(presets: &[Rc<Preset>]) -> Vec<u8> {
    let mut out = vec![0, 2];
    out.extend_from_slice(&(presets.len() as u16).to_be_bytes());
    for p in presets {
        let mut body = Vec::new();
        body.extend_from_slice(&[0, 0, 0, 0]);
        body.extend_from_slice(&(p.spacing.round() as i16).to_be_bytes());
        let name: Vec<u16> = p.name.encode_utf16().collect();
        body.extend_from_slice(&(name.len() as u32).to_be_bytes());
        for u in name { body.extend_from_slice(&u.to_be_bytes()); }
        body.push(1);
        for v in [0i16, 0, p.height as i16, p.width as i16] { body.extend_from_slice(&v.to_be_bytes()); }
        for v in [0i32, 0, p.height as i32, p.width as i32] { body.extend_from_slice(&v.to_be_bytes()); }
        body.extend_from_slice(&8i16.to_be_bytes());
        body.push(0);
        body.extend_from_slice(&p.pixels);
        out.extend_from_slice(&2u16.to_be_bytes());
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&body);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_set_round_trips_through_the_loader() {
        let set = presets();
        assert!(set.len() >= 14);
        let bytes = abr_bytes(&set);
        let back = crate::abr::parse(&bytes, "x").unwrap();
        assert_eq!(back.len(), set.len());
        for (a, b) in set.iter().zip(&back) {
            assert_eq!((a.name.as_str(), a.width, a.height, a.spacing), (b.name.as_str(), b.width, b.height, b.spacing));
            assert_eq!(a.pixels, b.pixels);
            let painted = a.pixels.iter().filter(|v| **v > 64).count() as f64 / a.pixels.len() as f64;
            assert!(painted > 0.01 && painted < 0.9, "{}: {:.1}% painted", a.name, painted * 100.0);
        }
    }
}
