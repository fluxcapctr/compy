//! Whole-layer filters and adjustments on a layer's pixels: each takes the image, its settings and, with a
//! selection, the selection's coverage on the layer grid, and returns a new image. The pixel work is the C
//! core's; this module handles byte order, premultiplication and blending the result back through the
//! selection (`PixelFilter.run`, `LevelsFilter`, `ContentFill`, and the healing part of `BrushStroke`).

use crate::ffi;
use crate::raster::{argb_from_packed, with_bytes};
use anyhow::{Result, bail};
use cairo::ImageSurface;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind { AddNoise, Grain, LensCorrection, GradientMap, Levels, Curves, ColorBalance, ContentAwareFill, SpotHeal, Exposure, HueSaturation, GaussianBlur, MotionBlur, Invert, RemoveBackground, Fade, UnsharpMask, SmartSharpen, BrightnessContrast, Vibrance, BlackWhite, PhotoFilter, Threshold, Posterize, ShadowsHighlights, SelectiveColor, ChannelMixer, HighPass, RadialBlur }

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::AddNoise => "Add Noise", Kind::Grain => "Grain", Kind::LensCorrection => "Lens Correction",
            Kind::GradientMap => "Gradient Map", Kind::Levels => "Levels", Kind::ContentAwareFill => "Content-Aware Fill",
            Kind::SpotHeal => "Heal Selection", Kind::Exposure => "Exposure", Kind::HueSaturation => "Hue/Saturation",
            Kind::GaussianBlur => "Gaussian Blur", Kind::MotionBlur => "Motion Blur", Kind::Invert => "Invert", Kind::RemoveBackground => "Remove Background",
            Kind::Curves => "Curves", Kind::ColorBalance => "Color Balance", Kind::Fade => "Fade",
            Kind::UnsharpMask => "Unsharp Mask", Kind::SmartSharpen => "Smart Sharpen", Kind::BrightnessContrast => "Brightness/Contrast", Kind::Vibrance => "Vibrance",
            Kind::BlackWhite => "Black & White", Kind::PhotoFilter => "Photo Filter", Kind::Threshold => "Threshold", Kind::Posterize => "Posterize",
            Kind::ShadowsHighlights => "Shadows/Highlights", Kind::SelectiveColor => "Selective Color", Kind::ChannelMixer => "Channel Mixer", Kind::HighPass => "High Pass", Kind::RadialBlur => "Radial Blur",
        }
    }
    /// Filters that work on the selection itself rather than the layer's colors, and need one.
    pub fn needs_selection(self) -> bool { matches!(self, Kind::ContentAwareFill | Kind::SpotHeal) }
    /// The room a blur needs around the layer to spread into: about three standard deviations, or half a
    /// streak (`FilterEdit.blurMargin`).
    pub fn margin(self, settings: &Settings) -> usize {
        match self { Kind::GaussianBlur => (settings.radius * 3.0 + 2.0).ceil() as usize, Kind::MotionBlur => (settings.distance / 2.0 + 2.0).ceil() as usize, Kind::UnsharpMask | Kind::SmartSharpen => (settings.sharpen.radius * 3.0 + 2.0).ceil() as usize, Kind::HighPass => (settings.high_pass * 3.0 + 2.0).ceil() as usize, Kind::RadialBlur => 8, _ => 0 }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Add Noise strength as Photoshop's percentage, 0.1 to 400.
    pub amount: f64,
    pub gaussian: bool,
    pub monochromatic: bool,
    /// Lens Correction's Remove Distortion, -100 to 100.
    pub distortion: f64,
    pub grain: Grain,
    pub gradient: GradientMap,
    pub levels: Levels,
    /// Spot healing: 0 Content-Aware, 1 Create Texture, 2 Proximity Match.
    pub heal_mode: i32,
    pub seed: u32,
    pub exposure: Exposure,
    pub hue_saturation: HueSaturation,
    pub curves: Curves,
    pub balance: ColorBalance,
    /// Fade: how much of the last filter's result stays, 0 to 1.
    pub fade: f64,
    /// Gaussian Blur radius in layer pixels (the blur's standard deviation), 0.1 to 250.
    pub radius: f64,
    /// Motion Blur direction in degrees, counterclockwise from horizontal as in Photoshop, -90 to 90.
    pub angle: f64,
    /// Motion Blur streak length in layer pixels, 1 to 2000.
    pub distance: f64,
    pub matte: crate::matte::MatteSettings,
    pub sharpen: Sharpen,
    pub brightness: BrightnessContrast,
    pub vibrance: Vibrance,
    pub black_white: BlackWhite,
    pub photo_filter: PhotoFilter,
    pub threshold: Threshold,
    pub posterize: Posterize,
    pub shadows_highlights: ShadowsHighlights,
    pub selective: SelectiveColor,
    pub mixer: ChannelMixer,
    /// High Pass radius in pixels.
    pub high_pass: f64,
    pub radial: RadialBlur,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { curves: Curves::default(), balance: ColorBalance::default(), fade: 1.0, amount: 10.0, gaussian: false, monochromatic: false, distortion: 0.0, grain: Grain::default(), gradient: GradientMap::default(), levels: Levels::default(), heal_mode: 0, seed: 0, exposure: Exposure::default(), hue_saturation: HueSaturation::default(), radius: 1.0, angle: 0.0, distance: 10.0, matte: Default::default(), sharpen: Sharpen::default(), brightness: BrightnessContrast::default(), vibrance: Vibrance::default(), black_white: BlackWhite::default(), photo_filter: PhotoFilter::default(), threshold: Threshold::default(), posterize: Posterize::default(), shadows_highlights: ShadowsHighlights::default(), selective: SelectiveColor::default(), mixer: ChannelMixer::default(), high_pass: 10.0, radial: RadialBlur::default() }
    }
}

impl Settings {
    pub fn normalized(&self) -> Settings {
        let mut s = self.clone();
        s.amount = clamp(s.amount, 0.1, 400.0, 10.0);
        s.distortion = clamp(s.distortion, -100.0, 100.0, 0.0);
        s.radius = clamp(s.radius, 0.1, 250.0, 1.0);
        s.angle = clamp(s.angle, -90.0, 90.0, 0.0);
        s.distance = clamp(s.distance, 1.0, 2000.0, 10.0);
        s.grain = s.grain.normalized();
        s.levels = s.levels.normalized();
        s.matte = s.matte.normalized();
        s.fade = clamp(s.fade, 0.0, 1.0, 1.0);
        s.balance = s.balance.normalized();
        s.sharpen = s.sharpen.normalized();
        s.brightness = s.brightness.normalized();
        s.vibrance = s.vibrance.normalized();
        s.black_white = s.black_white.normalized();
        s.photo_filter = s.photo_filter.normalized();
        s.threshold = s.threshold.normalized();
        s.posterize = s.posterize.normalized();
        s.shadows_highlights = s.shadows_highlights.normalized();
        s.selective = s.selective.normalized();
        s.mixer = s.mixer.normalized();
        s.high_pass = clamp(s.high_pass, 0.1, 250.0, 10.0);
        s.radial = s.radial.normalized();
        s
    }
}

/// Shadows/Highlights: the dark parts lifted and the bright parts brought down, each by an amount in
/// percent, judged by the brightness of the surroundings within `radius` so local contrast survives.
#[derive(Clone, Debug, PartialEq)]
pub struct ShadowsHighlights { pub shadows: f64, pub highlights: f64, pub radius: f64 }
impl Default for ShadowsHighlights { fn default() -> Self { ShadowsHighlights { shadows: 35.0, highlights: 0.0, radius: 30.0 } } }
impl ShadowsHighlights {
    pub fn normalized(&self) -> ShadowsHighlights { ShadowsHighlights { shadows: clamp(self.shadows, 0.0, 100.0, 35.0), highlights: clamp(self.highlights, 0.0, 100.0, 0.0), radius: clamp(self.radius, 1.0, 500.0, 30.0) } }
    pub fn is_identity(&self) -> bool { let n = self.normalized(); n.shadows == 0.0 && n.highlights == 0.0 }
    pub fn apply(&self, pixels: &mut [u8], w: usize, h: usize) {
        let n = self.normalized();
        if n.shadows == 0.0 && n.highlights == 0.0 { return; }
        // The neighbourhood brightness: luminosity blurred by the radius.
        let mut luma = vec![0u8; w * h];
        for (i, p) in pixels.chunks_exact(4).enumerate() { let a = p[3] as f64; luma[i] = if a <= 0.0 { 0 } else { ((0.114 * p[0] as f64 + 0.587 * p[1] as f64 + 0.299 * p[2] as f64) / a * 255.0).round().clamp(0.0, 255.0) as u8 }; }
        crate::blur::gaussian(&mut luma, w, h, 1, n.radius / 2.0);
        for (i, p) in pixels.chunks_exact_mut(4).enumerate() {
            let a = p[3] as f64;
            if a <= 0.0 { continue; }
            let lb = luma[i] as f64 / 255.0;
            // A gamma lift where the surroundings are dark, a gamma drop where they are bright.
            let lift = 1.0 / (1.0 + n.shadows / 100.0 * (1.0 - lb).powi(2) * 2.0);
            let drop = 1.0 + n.highlights / 100.0 * lb.powi(2) * 2.0;
            for k in 0..3 {
                let v = (p[k] as f64 / a).clamp(0.0, 1.0);
                let out = v.powf(lift).powf(drop);
                p[k] = (out * a).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

pub const SELECTIVE_RANGES: [&str; 9] = ["Reds", "Yellows", "Greens", "Cyans", "Blues", "Magentas", "Whites", "Neutrals", "Blacks"];

/// Selective Color: cyan, magenta, yellow and black nudged (percent, -100 to 100) within one color
/// family at a time; relative scales what is there, absolute adds a flat amount.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectiveColor { pub range: String, pub adjustments: Vec<(String, [f64; 4])>, pub relative: bool }
impl Default for SelectiveColor { fn default() -> Self { SelectiveColor { range: "Reds".into(), adjustments: Vec::new(), relative: true } } }
impl SelectiveColor {
    pub fn normalized(&self) -> SelectiveColor {
        SelectiveColor { range: if SELECTIVE_RANGES.contains(&self.range.as_str()) { self.range.clone() } else { "Reds".into() }, adjustments: self.adjustments.iter().filter(|(r, _)| SELECTIVE_RANGES.contains(&r.as_str())).map(|(r, a)| (r.clone(), a.map(|v| clamp(v, -100.0, 100.0, 0.0)))).collect(), relative: self.relative }
    }
    pub fn is_identity(&self) -> bool { self.adjustments.iter().all(|(_, a)| *a == [0.0; 4]) }
    pub fn adjustment(&self, range: &str) -> [f64; 4] { self.adjustments.iter().find(|(r, _)| r == range).map(|(_, a)| *a).unwrap_or([0.0; 4]) }
    pub fn set_adjustment(&mut self, range: &str, value: [f64; 4]) { match self.adjustments.iter_mut().find(|(r, _)| r == range) { Some(e) => e.1 = value, None => self.adjustments.push((range.to_string(), value)) } }
    /// How much of each family a straight color holds, 0 to 1, in the order of `SELECTIVE_RANGES`.
    pub fn membership(rgb: [f64; 3]) -> [f64; 9] {
        let (r, g, b) = (rgb[0], rgb[1], rgb[2]);
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let z = |v: f64| v.max(0.0);
        [z(r - g.max(b)), z(r.min(g) - b), z(g - r.max(b)), z(g.min(b) - r), z(b - r.max(g)), z(r.min(b) - g), z(2.0 * min - 1.0), z(1.0 - (max + min - 1.0).abs() - (max - min)), z(1.0 - 2.0 * max)]
    }
    pub fn pixel(&self, rgb: [f64; 3]) -> [f64; 3] {
        let n = self.normalized();
        let member = Self::membership(rgb);
        let mut out = rgb;
        for (i, range) in SELECTIVE_RANGES.iter().enumerate() {
            let a = n.adjustment(range);
            if a == [0.0; 4] || member[i] <= 0.0 { continue; }
            let m = member[i];
            for (c, ink) in [0usize, 1, 2].into_iter().zip([a[0], a[1], a[2]]) {
                // More cyan takes red away (and so on); the black slider takes from every channel.
                let amount = (ink + a[3]) / 100.0 * m;
                let scale = if n.relative { out[c] } else { 1.0 };
                out[c] = (out[c] - amount * scale).clamp(0.0, 1.0);
            }
        }
        out
    }
}

/// Channel Mixer: each output channel as percentages of the input channels plus a constant.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelMixer { pub red: [f64; 4], pub green: [f64; 4], pub blue: [f64; 4], pub monochrome: bool }
impl Default for ChannelMixer { fn default() -> Self { ChannelMixer { red: [100.0, 0.0, 0.0, 0.0], green: [0.0, 100.0, 0.0, 0.0], blue: [0.0, 0.0, 100.0, 0.0], monochrome: false } } }
impl ChannelMixer {
    pub fn normalized(&self) -> ChannelMixer {
        let row = |r: [f64; 4]| [clamp(r[0], -200.0, 200.0, 0.0), clamp(r[1], -200.0, 200.0, 0.0), clamp(r[2], -200.0, 200.0, 0.0), clamp(r[3], -200.0, 200.0, 0.0)];
        ChannelMixer { red: row(self.red), green: row(self.green), blue: row(self.blue), monochrome: self.monochrome }
    }
    pub fn is_identity(&self) -> bool { self.normalized() == ChannelMixer::default() }
    pub fn pixel(&self, rgb: [f64; 3]) -> [f64; 3] {
        let n = self.normalized();
        let mix = |row: [f64; 4]| ((row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2] + row[3]) / 100.0).clamp(0.0, 1.0);
        if n.monochrome { let g = mix(n.red); [g, g, g] } else { [mix(n.red), mix(n.green), mix(n.blue)] }
    }
}

/// Radial Blur: spin around a center by `amount` degrees, or zoom toward it by `amount` percent;
/// the center is a fraction of the layer's width and height.
#[derive(Clone, Debug, PartialEq)]
pub struct RadialBlur { pub amount: f64, pub zoom: bool, pub center: (f64, f64) }
impl Default for RadialBlur { fn default() -> Self { RadialBlur { amount: 10.0, zoom: false, center: (0.5, 0.5) } } }
impl RadialBlur {
    pub fn normalized(&self) -> RadialBlur { RadialBlur { amount: clamp(self.amount, 1.0, 100.0, 10.0), zoom: self.zoom, center: (clamp(self.center.0, 0.0, 1.0, 0.5), clamp(self.center.1, 0.0, 1.0, 0.5)) } }
    pub fn apply(&self, pixels: &mut [u8], w: usize, h: usize) {
        let n = self.normalized();
        let source = pixels.to_vec();
        let (cx, cy) = (n.center.0 * w as f64, n.center.1 * h as f64);
        let steps = 24usize;
        let fetch = |x: f64, y: f64| -> [f64; 4] {
            let (xi, yi) = (x.round() as isize, y.round() as isize);
            if xi < 0 || yi < 0 || xi >= w as isize || yi >= h as isize { return [0.0; 4]; }
            let i = (yi as usize * w + xi as usize) * 4;
            [source[i] as f64, source[i + 1] as f64, source[i + 2] as f64, source[i + 3] as f64]
        };
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (x as f64 + 0.5 - cx, y as f64 + 0.5 - cy);
                let mut sum = [0.0f64; 4];
                for k in 0..steps {
                    let t = k as f64 / (steps - 1) as f64 - 0.5;
                    let (sx, sy) = if n.zoom {
                        let f = 1.0 - t * n.amount / 100.0;
                        (cx + px * f, cy + py * f)
                    } else {
                        let a = t * n.amount.to_radians();
                        let (sa, ca) = a.sin_cos();
                        (cx + px * ca - py * sa, cy + px * sa + py * ca)
                    };
                    let v = fetch(sx - 0.5, sy - 0.5);
                    for c in 0..4 { sum[c] += v[c]; }
                }
                let o = (y * w + x) * 4;
                for c in 0..4 { pixels[o + c] = (sum[c] / steps as f64).round().clamp(0.0, 255.0) as u8; }
            }
        }
    }
}

/// High Pass: what is left when the blur is taken away, around middle gray; the base for frequency
/// separation and for sharpening on an Overlay layer.
pub fn high_pass(pixels: &mut [u8], w: usize, h: usize, radius: f64) {
    let mut blurred = pixels.to_vec();
    crate::blur::gaussian(&mut blurred, w, h, 4, radius);
    for (p, b) in pixels.chunks_exact_mut(4).zip(blurred.chunks_exact(4)) {
        let a = p[3] as f64;
        if a <= 0.0 { continue; }
        for k in 0..3 { p[k] = (p[k] as f64 - b[k] as f64 + a / 2.0).round().clamp(0.0, a) as u8; }
    }
}

/// Unsharp Mask and Smart Sharpen: the layer minus its blur, scaled by `amount` percent, added back
/// where the difference passes `threshold` (Unsharp Mask) or on luminosity alone with small
/// differences held back by `noise` (Smart Sharpen).
#[derive(Clone, Debug, PartialEq)]
pub struct Sharpen { pub amount: f64, pub radius: f64, pub threshold: f64, pub noise: f64 }
impl Default for Sharpen { fn default() -> Self { Sharpen { amount: 100.0, radius: 1.0, threshold: 0.0, noise: 10.0 } } }
impl Sharpen {
    pub fn normalized(&self) -> Sharpen { Sharpen { amount: clamp(self.amount, 1.0, 500.0, 100.0), radius: clamp(self.radius, 0.1, 250.0, 1.0), threshold: clamp(self.threshold, 0.0, 255.0, 0.0), noise: clamp(self.noise, 0.0, 100.0, 10.0) } }
}

/// Brightness (-150 to 150) and Contrast (-50 to 100), as the Brightness/Contrast adjustment.
#[derive(Clone, Debug, PartialEq)]
pub struct BrightnessContrast { pub brightness: f64, pub contrast: f64 }
impl Default for BrightnessContrast { fn default() -> Self { BrightnessContrast { brightness: 0.0, contrast: 0.0 } } }
impl BrightnessContrast {
    pub fn normalized(&self) -> BrightnessContrast { BrightnessContrast { brightness: clamp(self.brightness, -150.0, 150.0, 0.0), contrast: clamp(self.contrast, -50.0, 100.0, 0.0) } }
    pub fn is_identity(&self) -> bool { let n = self.normalized(); n.brightness == 0.0 && n.contrast == 0.0 }
    /// One table for every channel: brightness lifts the midtones (a gamma-like curve that keeps black and
    /// white in place), contrast steepens or flattens around the middle gray.
    pub fn table(&self) -> [f32; 256] {
        let n = self.normalized();
        let factor = if n.contrast >= 0.0 { 1.0 + n.contrast / 100.0 * 1.5 } else { 1.0 + n.contrast / 100.0 };
        let gamma = 2f64.powf(-n.brightness / 150.0 * 1.6);
        let mut t = [0f32; 256];
        for (i, v) in t.iter_mut().enumerate() {
            let x = (i as f64 / 255.0).powf(gamma);
            *v = ((x - 0.5) * factor + 0.5).clamp(0.0, 1.0) as f32;
        }
        t
    }
}

/// Vibrance (boosts the least saturated colors most, skin least) and Saturation (everything alike).
#[derive(Clone, Debug, PartialEq)]
pub struct Vibrance { pub vibrance: f64, pub saturation: f64 }
impl Default for Vibrance { fn default() -> Self { Vibrance { vibrance: 0.0, saturation: 0.0 } } }
impl Vibrance {
    pub fn normalized(&self) -> Vibrance { Vibrance { vibrance: clamp(self.vibrance, -100.0, 100.0, 0.0), saturation: clamp(self.saturation, -100.0, 100.0, 0.0) } }
    pub fn is_identity(&self) -> bool { let n = self.normalized(); n.vibrance == 0.0 && n.saturation == 0.0 }
    pub fn pixel(&self, rgb: [f64; 3]) -> [f64; 3] {
        let n = self.normalized();
        let (max, min) = (rgb[0].max(rgb[1]).max(rgb[2]), rgb[0].min(rgb[1]).min(rgb[2]));
        let luma = 0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2];
        let sat = max - min;
        // Skin (orange hues) is protected: red dominant with green between red and blue.
        let skin = if rgb[0] > rgb[1] && rgb[1] > rgb[2] { 1.0 - ((rgb[0] - rgb[2]).min(1.0)) * 0.5 } else { 1.0 };
        let boost = n.vibrance / 100.0 * (1.0 - sat).max(0.0) * skin + n.saturation / 100.0;
        let f = (1.0 + boost).max(0.0);
        [luma + (rgb[0] - luma) * f, luma + (rgb[1] - luma) * f, luma + (rgb[2] - luma) * f].map(|v| v.clamp(0.0, 1.0))
    }
}

/// Black & White: how bright each hue family comes out, in percent (Photoshop's defaults).
#[derive(Clone, Debug, PartialEq)]
pub struct BlackWhite { pub reds: f64, pub yellows: f64, pub greens: f64, pub cyans: f64, pub blues: f64, pub magentas: f64 }
impl Default for BlackWhite { fn default() -> Self { BlackWhite { reds: 40.0, yellows: 60.0, greens: 40.0, cyans: 60.0, blues: 20.0, magentas: 80.0 } } }
impl BlackWhite {
    pub fn normalized(&self) -> BlackWhite {
        let c = |v: f64, d: f64| clamp(v, -200.0, 300.0, d);
        BlackWhite { reds: c(self.reds, 40.0), yellows: c(self.yellows, 60.0), greens: c(self.greens, 40.0), cyans: c(self.cyans, 60.0), blues: c(self.blues, 20.0), magentas: c(self.magentas, 80.0) }
    }
    /// The gray for a straight color: the darkest channel, plus the middle one's rise weighted by the mixed
    /// hue it makes with the brightest, plus the brightest channel's rise weighted by its own hue.
    pub fn gray(&self, rgb: [f64; 3]) -> f64 {
        let n = self.normalized();
        let (r, g, b) = (rgb[0], rgb[1], rgb[2]);
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let mid = r + g + b - max - min;
        let (primary, secondary) = if r >= g && r >= b { (n.reds, if g >= b { n.yellows } else { n.magentas }) }
            else if g >= r && g >= b { (n.greens, if r >= b { n.yellows } else { n.cyans }) }
            else { (n.blues, if g >= r { n.cyans } else { n.magentas }) };
        (min + (mid - min) * secondary / 100.0 + (max - mid) * primary / 100.0).clamp(0.0, 1.0)
    }
}

/// Photo Filter: a colored filter over the lens, at a density, keeping the brightness or not.
#[derive(Clone, Debug, PartialEq)]
pub struct PhotoFilter { pub color: [f64; 3], pub density: f64, pub preserve_luminosity: bool }
impl Default for PhotoFilter { fn default() -> Self { PhotoFilter { color: [0.925, 0.541, 0.0], density: 25.0, preserve_luminosity: true } } }
impl PhotoFilter {
    pub fn normalized(&self) -> PhotoFilter { PhotoFilter { color: self.color.map(|c| clamp(c, 0.0, 1.0, 0.5)), density: clamp(self.density, 1.0, 100.0, 25.0), preserve_luminosity: self.preserve_luminosity } }
    pub fn pixel(&self, rgb: [f64; 3]) -> [f64; 3] {
        let n = self.normalized();
        let d = n.density / 100.0;
        let mut out = [rgb[0] * (1.0 - d) + rgb[0] * n.color[0] * d, rgb[1] * (1.0 - d) + rgb[1] * n.color[1] * d, rgb[2] * (1.0 - d) + rgb[2] * n.color[2] * d];
        if n.preserve_luminosity {
            let before = 0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2];
            let after = 0.299 * out[0] + 0.587 * out[1] + 0.114 * out[2];
            if after > 1e-6 { let k = before / after; out = out.map(|v| v * k); }
        }
        out.map(|v| v.clamp(0.0, 1.0))
    }
}

/// Threshold: white at and above `level`, black below, by luminosity.
#[derive(Clone, Debug, PartialEq)]
pub struct Threshold { pub level: f64 }
impl Default for Threshold { fn default() -> Self { Threshold { level: 128.0 } } }
impl Threshold { pub fn normalized(&self) -> Threshold { Threshold { level: clamp(self.level, 1.0, 255.0, 128.0).round() } } }

/// Posterize: each channel snapped to `levels` values.
#[derive(Clone, Debug, PartialEq)]
pub struct Posterize { pub levels: f64 }
impl Default for Posterize { fn default() -> Self { Posterize { levels: 4.0 } } }
impl Posterize {
    pub fn normalized(&self) -> Posterize { Posterize { levels: clamp(self.levels, 2.0, 255.0, 4.0).round() } }
    pub fn table(&self) -> [f32; 256] {
        let q = self.normalized().levels;
        let mut t = [0f32; 256];
        for (i, v) in t.iter_mut().enumerate() { *v = ((i as f64 * q / 256.0).floor() / (q - 1.0)).clamp(0.0, 1.0) as f32; }
        t
    }
}

/// Runs `f` on every pixel's straight color (0 to 1), keeping its alpha; premultiplied BGRA in and out.
pub fn map_straight(pixels: &mut [u8], f: impl Fn([f64; 3]) -> [f64; 3]) {
    for p in pixels.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a == 0 { continue; }
        let straight = [((p[2] as u32 * 255 + a / 2) / a).min(255) as f64 / 255.0, ((p[1] as u32 * 255 + a / 2) / a).min(255) as f64 / 255.0, ((p[0] as u32 * 255 + a / 2) / a).min(255) as f64 / 255.0];
        let out = f(straight).map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u32);
        p[2] = ((out[0] * a + 127) / 255) as u8;
        p[1] = ((out[1] * a + 127) / 255) as u8;
        p[0] = ((out[2] * a + 127) / 255) as u8;
    }
}

/// Unsharp Mask on premultiplied pixels: the difference from a blur, scaled, added back where it passes
/// the threshold. `luminosity_only` (Smart Sharpen) sharpens brightness and leaves color alone, with the
/// smallest differences held back by `noise` percent so grain does not sharpen with the edges.
pub fn sharpen(pixels: &mut [u8], w: usize, h: usize, s: &Sharpen, luminosity_only: bool) {
    let s = s.normalized();
    let mut blurred = pixels.to_vec();
    crate::blur::gaussian(&mut blurred, w, h, 4, s.radius);
    let amount = s.amount / 100.0;
    for (p, b) in pixels.chunks_exact_mut(4).zip(blurred.chunks_exact(4)) {
        if p[3] == 0 { continue; }
        if luminosity_only {
            let lp = 0.114 * p[0] as f64 + 0.587 * p[1] as f64 + 0.299 * p[2] as f64;
            let lb = 0.114 * b[0] as f64 + 0.587 * b[1] as f64 + 0.299 * b[2] as f64;
            let diff = lp - lb;
            // Reduce Noise: differences below a few levels fade out rather than switch off.
            let gate = (s.noise / 100.0) * 12.0;
            let keep = if gate <= 0.0 { 1.0 } else { (diff.abs() / gate).min(1.0) };
            let add = diff * amount * keep;
            for k in 0..3 { p[k] = (p[k] as f64 + add).round().clamp(0.0, p[3] as f64) as u8; }
        } else {
            for k in 0..3 {
                let diff = p[k] as f64 - b[k] as f64;
                if diff.abs() < s.threshold { continue; }
                p[k] = (p[k] as f64 + diff * amount).round().clamp(0.0, p[3] as f64) as u8;
            }
        }
    }
}

fn clamp(v: f64, low: f64, high: f64, fallback: f64) -> f64 { if v.is_finite() { v.clamp(low, high) } else { fallback } }

#[derive(Clone, Debug, PartialEq)]
pub struct Grain { pub amount: f64, pub size: f64, pub roughness: f64 }
impl Default for Grain { fn default() -> Self { Grain { amount: 25.0, size: 1.5, roughness: 50.0 } } }
impl Grain {
    pub fn normalized(&self) -> Grain { Grain { amount: clamp(self.amount, 0.0, 100.0, 25.0), size: clamp(self.size, 0.5, 20.0, 1.5), roughness: clamp(self.roughness, 0.0, 100.0, 50.0) } }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GradientMap { pub shadows: [f64; 3], pub highlights: [f64; 3], pub reversed: bool, /// More than two colors, from the gradient editor; empty means shadows to highlights.
    pub stops: Vec<crate::gradient::Stop> }
impl Default for GradientMap { fn default() -> Self { GradientMap { shadows: [0.0; 3], highlights: [1.0; 3], reversed: false, stops: Vec::new() } } }
impl GradientMap {
    /// The gradient the map runs through, darkest first.
    pub fn gradient(&self) -> crate::gradient::Gradient {
        let g = if self.stops.len() >= 2 { crate::gradient::Gradient { stops: self.stops.clone(), alphas: Vec::new() }.normalized() } else { crate::gradient::Gradient::two(self.shadows, self.highlights) };
        if self.reversed { g.reversed() } else { g }
    }
    /// 256 x 3 straight sRGB bytes, darkest first.
    pub fn table(&self) -> [u8; 768] { self.gradient().table() }
}

/// One channel's levels: input black and white points, gamma, output range (`LevelRange`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range { pub black: f64, pub gamma: f64, pub white: f64, pub output_black: f64, pub output_white: f64 }
impl Default for Range { fn default() -> Self { Range { black: 0.0, gamma: 1.0, white: 255.0, output_black: 0.0, output_white: 255.0 } } }
impl Range {
    pub fn normalized(&self) -> Range {
        let mut r = *self;
        r.black = clamp(r.black, 0.0, 254.0, 0.0);
        r.white = clamp(r.white, r.black + 1.0, 255.0, 255.0);
        r.gamma = clamp(r.gamma, 0.1, 9.99, 1.0);
        r.output_black = clamp(r.output_black, 0.0, 255.0, 0.0);
        r.output_white = clamp(r.output_white, 0.0, 255.0, 255.0);
        r
    }
    pub fn apply(&self, value: f64) -> f64 {
        let s = self.normalized();
        let input = ((value * 255.0 - s.black) / (s.white - s.black)).clamp(0.0, 1.0);
        (s.output_black + input.powf(1.0 / s.gamma) * (s.output_white - s.output_black)) / 255.0
    }
}

/// Levels for RGB together (index 0) and each channel (1 red, 2 green, 3 blue), as `LevelsSettings`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Levels { pub ranges: [Range; 4] }
impl Levels {
    pub fn normalized(&self) -> Levels { Levels { ranges: self.ranges.map(|r| r.normalized()) } }
    pub fn is_identity(&self) -> bool { self.ranges.iter().all(|r| r.normalized() == Range::default()) }
    /// Individual channels, followed by the composite RGB adjustment.
    pub fn apply(&self, value: f64, channel: usize) -> f64 { self.ranges[0].apply(self.ranges[channel].apply(value)) }
    /// Auto Tone: each channel stretched to its own black and white point (`LevelsAuto.color`).
    pub fn auto_tone(histogram: &[[f64; 256]; 4]) -> Levels {
        let mut result = Levels::default();
        for c in 1..4 {
            if let Some((low, high)) = endpoints(&histogram[c]) { result.ranges[c] = Range { black: low, white: high, ..Range::default() }; }
        }
        result
    }

    /// Auto Color: Auto Tone with each channel's gamma set so its mean lands on middle gray
    /// (`LevelsAuto.neutral`).
    pub fn auto_color(histogram: &[[f64; 256]; 4]) -> Levels {
        let mut result = Levels::auto_tone(histogram);
        for c in 1..4 {
            let bins = &histogram[c];
            let total: f64 = bins.iter().sum();
            if total <= 0.0 { continue; }
            let range = result.ranges[c];
            let mean = bins.iter().enumerate().map(|(i, b)| range.apply(i as f64 / 255.0) * b).sum::<f64>() / total;
            if mean > 0.0 && mean < 1.0 { result.ranges[c].gamma = (mean.ln() / 0.5f64.ln()).clamp(0.1, 9.99); }
        }
        result
    }

    /// Auto Contrast: a shared black and white point clipping 0.1% at each end of every channel.
    pub fn auto_contrast(histogram: &[[f64; 256]; 4]) -> Levels {
        let mut result = Levels::default();
        let limits: Vec<(f64, f64)> = histogram[1..].iter().filter_map(endpoints).collect();
        if let (Some(low), Some(high)) = (limits.iter().map(|l| l.0).reduce(f64::min), limits.iter().map(|l| l.1).reduce(f64::max)) {
            if low < high { result.ranges[0] = Range { black: low, white: high, ..Range::default() }; }
        }
        result
    }
}

/// A histogram's black and white points, clipping 0.1% of the pixels at each end.
fn endpoints(bins: &[f64; 256]) -> Option<(f64, f64)> {
    let total: f64 = bins.iter().sum();
    if total <= 0.0 { return None; }
    let (mut sum, mut low, mut high) = (0.0, 0usize, 255usize);
    for (i, b) in bins.iter().enumerate() { sum += b; if sum > total * 0.001 { low = i; break; } }
    sum = 0.0;
    for (i, b) in bins.iter().enumerate().rev() { sum += b; if sum > total * 0.001 { high = i; break; } }
    (low < high).then_some((low as f64, high as f64))
}

/// Color Balance: cyan/red, magenta/green and yellow/blue shifts (-100 to 100) for the shadows, midtones
/// and highlights, weighted by each pixel's tone, optionally keeping its luminosity.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorBalance { pub shadows: [f64; 3], pub midtones: [f64; 3], pub highlights: [f64; 3], pub preserve_luminosity: bool }
impl Default for ColorBalance { fn default() -> Self { ColorBalance { shadows: [0.0; 3], midtones: [0.0; 3], highlights: [0.0; 3], preserve_luminosity: true } } }
impl ColorBalance {
    pub fn normalized(&self) -> ColorBalance {
        let c = |v: [f64; 3]| v.map(|x| clamp(x, -100.0, 100.0, 0.0));
        ColorBalance { shadows: c(self.shadows), midtones: c(self.midtones), highlights: c(self.highlights), preserve_luminosity: self.preserve_luminosity }
    }
    pub fn is_identity(&self) -> bool { self.shadows == [0.0; 3] && self.midtones == [0.0; 3] && self.highlights == [0.0; 3] }
    /// One channel's shift for a value from 0 to 1: the three ranges' sliders weighted by how much of
    /// a shadow, midtone or highlight the value is (a full slider moves a midtone by a quarter).
    fn shift(&self, channel: usize, v: f64) -> f64 {
        let shadow = (1.0 - v * 2.0).clamp(0.0, 1.0);
        let highlight = (v * 2.0 - 1.0).clamp(0.0, 1.0);
        let midtone = 1.0 - (v * 2.0 - 1.0).abs();
        (self.shadows[channel] * shadow + self.midtones[channel] * midtone + self.highlights[channel] * highlight) / 100.0 * 0.25
    }
    /// Applies to packed BGRA premultiplied pixels.
    pub fn apply(&self, pixels: &mut [u8]) {
        if self.is_identity() { return; }
        unpremultiply_partial(pixels);
        for px in pixels.chunks_exact_mut(4) {
            if px[3] == 0 { continue; }
            let (b, g, r) = (px[0] as f64 / 255.0, px[1] as f64 / 255.0, px[2] as f64 / 255.0);
            let mut out = [r + self.shift(0, r), g + self.shift(1, g), b + self.shift(2, b)].map(|c| c.clamp(0.0, 1.0));
            if self.preserve_luminosity {
                // The luma the shift took away goes back into the channels that still have room; a
                // channel at a bound passes its share to the others.
                let weights = [0.299, 0.587, 0.114];
                let luma = |c: [f64; 3]| c[0] * weights[0] + c[1] * weights[1] + c[2] * weights[2];
                let target = luma([r, g, b]);
                for _ in 0..4 {
                    let missing = target - luma(out);
                    if missing.abs() < 1e-4 { break; }
                    let free: f64 = (0..3).filter(|i| if missing > 0.0 { out[*i] < 1.0 } else { out[*i] > 0.0 }).map(|i| weights[i]).sum();
                    if free <= 0.0 { break; }
                    let step = missing / free;
                    for i in 0..3 { if (missing > 0.0 && out[i] < 1.0) || (missing < 0.0 && out[i] > 0.0) { out[i] = (out[i] + step).clamp(0.0, 1.0); } }
                }
            }
            px[2] = (out[0] * 255.0).round() as u8; px[1] = (out[1] * 255.0).round() as u8; px[0] = (out[2] * 255.0).round() as u8;
        }
        premultiply_partial(pixels);
    }
}

/// A packed copy of a surface's pixels (stride = width * 4).
fn packed(source: &ImageSurface) -> Result<(Vec<u8>, usize, usize)> {
    let (w, h) = (source.width() as usize, source.height() as usize);
    let mut out = vec![0u8; w * h * 4];
    with_bytes(source, |data, stride| {
        for y in 0..h { out[y * w * 4..(y + 1) * w * 4].copy_from_slice(&data[y * stride..y * stride + w * 4]); }
    })?;
    Ok((out, w, h))
}

/// A packed (width x height) copy of an A8 coverage surface.
pub fn packed_gray(coverage: &ImageSurface) -> Result<Vec<u8>> {
    let (w, h) = (coverage.width() as usize, coverage.height() as usize);
    let mut out = vec![0u8; w * h];
    with_bytes(coverage, |data, stride| {
        for y in 0..h { out[y * w..(y + 1) * w].copy_from_slice(&data[y * stride..y * stride + w]); }
    })?;
    Ok(out)
}

/// Runs `kind` on `source` and returns the result as a new image. `coverage` is the selection on the
/// layer's grid: colour filters blend their result through it, the fill and heal work inside it.
pub fn run(kind: Kind, source: &ImageSurface, settings: &Settings, coverage: Option<&ImageSurface>) -> Result<ImageSurface> {
    let settings = settings.normalized();
    let (mut pixels, w, h) = packed(source)?;
    let stride = w * 4;
    let coverage = coverage.map(packed_gray).transpose()?;
    if kind.needs_selection() && coverage.is_none() { bail!("{} needs a selection", kind.name()); }
    match kind {
        Kind::AddNoise => ffi::noise(&mut pixels, w, h, stride, settings.amount as f32, settings.gaussian, settings.monochromatic, settings.seed),
        Kind::Grain => {
            if settings.grain.amount > 0.0 {
                ffi::swap_red_blue(&mut pixels, stride, w, h);
                ffi::grain(&mut pixels, w, h, stride, settings.grain.amount, settings.grain.size, settings.grain.roughness, settings.seed, (0.0, 0.0), 1.0);
                ffi::swap_red_blue(&mut pixels, stride, w, h);
            }
        }
        Kind::LensCorrection => {
            let source_pixels = pixels.clone();
            ffi::lens(&source_pixels, &mut pixels, w, h, stride, settings.distortion / 100.0 * LENS_STRENGTH);
        }
        Kind::GradientMap => {
            ffi::swap_red_blue(&mut pixels, stride, w, h);
            ffi::gradient_map(&mut pixels, w, h, stride, &settings.gradient.table());
            ffi::swap_red_blue(&mut pixels, stride, w, h);
        }
        Kind::Levels => {
            if !settings.levels.is_identity() {
                // Tables in the buffer's own order, blue first; the reference builds them red first for RGBA.
                let mut tables = [0f32; 768];
                for (slot, channel) in [3usize, 2, 1].into_iter().enumerate() {
                    for v in 0..256 { tables[slot * 256 + v] = settings.levels.apply(v as f64 / 255.0, channel) as f32; }
                }
                // Soft edges are adjusted like the colour they are: unpremultiplied around the lookup.
                unpremultiply_partial(&mut pixels);
                ffi::levels(&mut pixels, w * h, &tables);
                premultiply_partial(&mut pixels);
            }
        }
        Kind::ContentAwareFill => {
            let mask = coverage.as_ref().unwrap();
            if !ffi::fill(&mut pixels, stride, mask, w, w, h)? {
                bail!("Not enough unselected, opaque image pixels to synthesize a fill. Use a smaller selection with some surrounding image.");
            }
            return argb_from_packed(w as i32, h as i32, pixels);
        }
        Kind::SpotHeal => {
            let mask = coverage.as_ref().unwrap();
            heal_region(&mut pixels, mask, w, h, settings.heal_mode, settings.seed)?;
            return argb_from_packed(w as i32, h as i32, pixels);
        }
        // Premultiplied: each color becomes alpha minus color, so transparency is kept (`PixelInvert`).
        Kind::Invert => { for px in pixels.chunks_exact_mut(4) { let a = px[3]; px[0] = a - px[0]; px[1] = a - px[1]; px[2] = a - px[2]; } }
        Kind::RemoveBackground => bail!("Remove Background runs through the document, not as a pixel filter."),
        Kind::GaussianBlur => crate::blur::gaussian(&mut pixels, w, h, 4, settings.radius),
        Kind::MotionBlur => motion_blur(&mut pixels, w, h, settings.angle, settings.distance),
        Kind::Exposure => Adjustment::Exposure(settings.exposure.clone()).apply(&mut pixels, w, h, (0.0, 0.0), 1.0),
        Kind::Curves => Adjustment::Curves(settings.curves.clone()).apply(&mut pixels, w, h, (0.0, 0.0), 1.0),
        Kind::ColorBalance => settings.balance.apply(&mut pixels),
        Kind::Fade => bail!("Fade runs through the document, not as a pixel filter."),
        Kind::UnsharpMask => sharpen(&mut pixels, w, h, &settings.sharpen, false),
        Kind::SmartSharpen => sharpen(&mut pixels, w, h, &settings.sharpen, true),
        Kind::BrightnessContrast => Adjustment::BrightnessContrast(settings.brightness.clone()).apply(&mut pixels, w, h, (0.0, 0.0), 1.0),
        Kind::Vibrance => Adjustment::Vibrance(settings.vibrance.clone()).apply(&mut pixels, w, h, (0.0, 0.0), 1.0),
        Kind::BlackWhite => Adjustment::BlackWhite(settings.black_white.clone()).apply(&mut pixels, w, h, (0.0, 0.0), 1.0),
        Kind::PhotoFilter => Adjustment::PhotoFilter(settings.photo_filter.clone()).apply(&mut pixels, w, h, (0.0, 0.0), 1.0),
        Kind::Threshold => Adjustment::Threshold(settings.threshold.clone()).apply(&mut pixels, w, h, (0.0, 0.0), 1.0),
        Kind::Posterize => Adjustment::Posterize(settings.posterize.clone()).apply(&mut pixels, w, h, (0.0, 0.0), 1.0),
        Kind::ShadowsHighlights => settings.shadows_highlights.apply(&mut pixels, w, h),
        Kind::SelectiveColor => Adjustment::SelectiveColor(settings.selective.clone()).apply(&mut pixels, w, h, (0.0, 0.0), 1.0),
        Kind::ChannelMixer => Adjustment::ChannelMixer(settings.mixer.clone()).apply(&mut pixels, w, h, (0.0, 0.0), 1.0),
        Kind::HighPass => high_pass(&mut pixels, w, h, settings.high_pass),
        Kind::RadialBlur => settings.radial.apply(&mut pixels, w, h),
        Kind::HueSaturation => Adjustment::HueSaturation(settings.hue_saturation.clone()).apply(&mut pixels, w, h, (0.0, 0.0), 1.0),
    }
    if let Some(mask) = &coverage {
        // coverage x adjusted + (1 - coverage) x original, per byte; both sides are premultiplied.
        with_bytes(source, |original, ostride| {
            for y in 0..h {
                for x in 0..w {
                    let c = mask[y * w + x] as u32;
                    if c == 255 { continue; }
                    for k in 0..4 {
                        let i = y * stride + x * 4 + k;
                        let o = original[y * ostride + x * 4 + k] as u32;
                        pixels[i] = ((pixels[i] as u32 * c + o * (255 - c) + 127) / 255) as u8;
                    }
                }
            }
        })?;
    }
    argb_from_packed(w as i32, h as i32, pixels)
}

/// Motion Blur: each pixel becomes the average of the pixels along a streak of `distance` through it at
/// `angle` (counterclockwise from horizontal, as Photoshop measures it), sampled evenly, transparent beyond
/// the image. Rows are split across threads.
pub fn motion_blur(pixels: &mut [u8], w: usize, h: usize, angle: f64, distance: f64) {
    if distance < 1.0 || w == 0 || h == 0 { return; }
    let samples = (distance.ceil() as usize).clamp(2, 96);
    let (dx, dy) = (angle.to_radians().cos(), -angle.to_radians().sin());
    let source = pixels.to_vec();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16);
    let chunk_rows = (h / threads).max(1);
    std::thread::scope(|scope| {
        for (chunk, out) in pixels.chunks_mut(chunk_rows * w * 4).enumerate() {
            let y0 = chunk * chunk_rows;
            let source = &source;
            scope.spawn(move || {
                for (i, orow) in out.chunks_mut(w * 4).enumerate() {
                    let y = y0 + i;
                    for x in 0..w {
                        let mut acc = [0u32; 4];
                        for s in 0..samples {
                            let t = (s as f64 + 0.5) / samples as f64 - 0.5;
                            let (sx, sy) = ((x as f64 + 0.5 + dx * distance * t).floor(), (y as f64 + 0.5 + dy * distance * t).floor());
                            if sx < 0.0 || sy < 0.0 || sx >= w as f64 || sy >= h as f64 { continue; }
                            let p = (sy as usize * w + sx as usize) * 4;
                            for k in 0..4 { acc[k] += source[p + k] as u32; }
                        }
                        for k in 0..4 { orow[x * 4 + k] = ((acc[k] + samples as u32 / 2) / samples as u32) as u8; }
                    }
                }
            });
        }
    });
}

/// Remove Distortion at 100 moves the image's corners by this share of their distance from the center.
pub const LENS_STRENGTH: f64 = 0.35;

fn unpremultiply_partial(pixels: &mut [u8]) {
    for p in pixels.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a == 0 || a == 255 { continue; }
        for k in 0..3 { p[k] = ((p[k] as u32 * 255 + a / 2) / a).min(255) as u8; }
    }
}

fn premultiply_partial(pixels: &mut [u8]) {
    for p in pixels.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a == 0 || a == 255 { continue; }
        for k in 0..3 { p[k] = ((p[k] as u32 * a + 127) / 255) as u8; }
    }
}

/// Spot healing over the covered pixels, run on a region around them with room for the patch search (about
/// three spot-widths, as `BrushStroke.heal` allows), and copied back.
fn heal_region(pixels: &mut [u8], coverage: &[u8], w: usize, h: usize, mode: i32, seed: u32) -> Result<()> {
    let Some((left, top, right, bottom)) = ffi::gray_bounds(coverage, w, h, w) else { return Ok(()) };
    let reach = (((right - left).max(bottom - top) + 32) as f64 * 3.2).ceil() as usize;
    let (x0, y0) = (left.saturating_sub(reach), top.saturating_sub(reach));
    let (x1, y1) = ((right + reach).min(w), (bottom + reach).min(h));
    let (rw, rh) = (x1 - x0, y1 - y0);
    let mut region = vec![0u8; rw * rh * 4];
    let mut mask = vec![0u8; rw * rh];
    for y in 0..rh {
        region[y * rw * 4..(y + 1) * rw * 4].copy_from_slice(&pixels[((y0 + y) * w + x0) * 4..((y0 + y) * w + x1) * 4]);
        mask[y * rw..(y + 1) * rw].copy_from_slice(&coverage[(y0 + y) * w + x0..(y0 + y) * w + x1]);
    }
    ffi::heal(&mut region, &mask, rw, rh, rw * 4, 1.0, mode, seed)?;
    for y in 0..rh {
        pixels[((y0 + y) * w + x0) * 4..((y0 + y) * w + x1) * 4].copy_from_slice(&region[y * rw * 4..(y + 1) * rw * 4]);
    }
    Ok(())
}

/// Histograms of a layer's pixels (inside `coverage` when given): [mean of channels, red, green, blue] x 256.
pub fn histogram(source: &ImageSurface, coverage: Option<&ImageSurface>) -> Result<[[f64; 256]; 4]> {
    let (pixels, w, h) = packed(source)?;
    let coverage = coverage.map(packed_gray).transpose()?;
    let bins = ffi::histogram(&pixels, coverage.as_deref(), w * h);
    let mut out = [[0f64; 256]; 4];
    // The buffer is blue first: the C's per-channel bins come back blue, green, red.
    for (slot, channel) in [0usize, 3, 2, 1].into_iter().enumerate() {
        out[channel].copy_from_slice(&bins[slot * 256..(slot + 1) * 256]);
    }
    Ok(out)
}

// MARK: Exposure, Hue/Saturation, Curves, and adjustment layers

/// Photoshop's Exposure: stops scale linear light, an offset shifts it, gamma bends the result.
#[derive(Clone, Debug, PartialEq)]
pub struct Exposure { pub exposure: f64, pub offset: f64, pub gamma: f64 }
impl Default for Exposure { fn default() -> Self { Exposure { exposure: 0.0, offset: 0.0, gamma: 1.0 } } }
impl Exposure {
    pub fn normalized(&self) -> Exposure { Exposure { exposure: clamp(self.exposure, -20.0, 20.0, 0.0), offset: clamp(self.offset, -0.5, 0.5, 0.0), gamma: clamp(self.gamma, 0.01, 9.99, 1.0) } }
    pub fn is_identity(&self) -> bool { self.normalized() == Exposure::default() }
    /// Each channel's output (0 to 1) for each input byte, decoded to linear light and encoded back.
    pub fn table(&self) -> [f32; 256] {
        let s = self.normalized();
        let scale = 2f64.powf(s.exposure);
        let mut t = [0f32; 256];
        for (i, v) in t.iter_mut().enumerate() {
            let encoded = i as f64 / 255.0;
            let mut linear = if encoded <= 0.04045 { encoded / 12.92 } else { ((encoded + 0.055) / 1.055).powf(2.4) };
            linear = (linear * scale + s.offset).max(0.0).powf(1.0 / s.gamma);
            let output = if linear <= 0.0031308 { linear * 12.92 } else { 1.055 * linear.powf(1.0 / 2.4) - 0.055 };
            *v = output.clamp(0.0, 1.0) as f32;
        }
        t
    }
}

/// A hue band in degrees, wrapping at 360: full strength between `range_start` and `range_end`, fading to
/// nothing at `falloff_start` and `falloff_end` (`HueBand`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HueBand { pub falloff_start: f64, pub range_start: f64, pub range_end: f64, pub falloff_end: f64 }
impl HueBand {
    fn forward(from: f64, to: f64) -> f64 { let d = (to - from) % 360.0; if d < 0.0 { d + 360.0 } else { d } }
    pub fn weight(&self, hue: f64) -> f64 {
        let span = Self::forward(self.falloff_start, self.falloff_end);
        if span <= 0.0 { return 1.0; }
        let position = Self::forward(self.falloff_start, hue);
        if position > span { return 0.0; }
        let ramp_in = Self::forward(self.falloff_start, self.range_start);
        let plateau_end = Self::forward(self.falloff_start, self.range_end);
        if position < ramp_in { return if ramp_in > 0.0 { position / ramp_in } else { 1.0 }; }
        if position <= plateau_end { return 1.0; }
        let ramp_out = span - plateau_end;
        if ramp_out > 0.0 { (span - position) / ramp_out } else { 1.0 }
    }
}

pub const COLOR_RANGES: [&str; 7] = ["Master", "Reds", "Yellows", "Greens", "Cyans", "Blues", "Magentas"];
pub fn default_band(range: &str) -> HueBand {
    let b = |a, b, c, d| HueBand { falloff_start: a, range_start: b, range_end: c, falloff_end: d };
    match range {
        "Reds" => b(315.0, 345.0, 15.0, 45.0), "Yellows" => b(15.0, 45.0, 75.0, 105.0), "Greens" => b(75.0, 105.0, 135.0, 165.0),
        "Cyans" => b(135.0, 165.0, 195.0, 225.0), "Blues" => b(195.0, 225.0, 255.0, 285.0), "Magentas" => b(255.0, 285.0, 315.0, 345.0),
        _ => b(0.0, 0.0, 360.0, 360.0),
    }
}

/// Hue/Saturation as Photoshop's Cmd+U: per-range hue shift, saturation and lightness, weighted by how
/// strongly each range's band claims a pixel's hue, or Colorize (`HueSaturationSettings`).
/// A Swift `[ColorRange: T]` dictionary: encoded as a flat array of alternating keys and values (the enum is
/// not `CodingKeyRepresentable`), though a JSON object is accepted too.
fn keyed_entries(value: Option<&serde_json::Value>) -> Vec<(String, serde_json::Value)> {
    match value {
        Some(serde_json::Value::Array(items)) => items.chunks_exact(2).filter_map(|pair| pair[0].as_str().map(|k| (k.to_string(), pair[1].clone()))).collect(),
        Some(serde_json::Value::Object(map)) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        _ => Vec::new(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HueSaturation {
    /// Which range the sliders edit (and Colorize reads).
    pub range: String,
    pub colorize: bool,
    /// Applies the selected range to everything outside its band instead.
    pub invert_range: bool,
    /// (hue, saturation, lightness) per range name.
    pub adjustments: Vec<(String, [f64; 3])>,
    pub bands: Vec<(String, HueBand)>,
}
impl Default for HueSaturation { fn default() -> Self { HueSaturation { range: "Master".into(), colorize: false, invert_range: false, adjustments: Vec::new(), bands: Vec::new() } } }
impl HueSaturation {
    pub fn adjustment(&self, range: &str) -> [f64; 3] { self.adjustments.iter().find(|(r, _)| r == range).map(|(_, a)| *a).unwrap_or([0.0; 3]) }
    pub fn set_adjustment(&mut self, range: &str, value: [f64; 3]) {
        match self.adjustments.iter_mut().find(|(r, _)| r == range) { Some(entry) => entry.1 = value, None => self.adjustments.push((range.to_string(), value)) }
    }
    pub fn band(&self, range: &str) -> HueBand { self.bands.iter().find(|(r, _)| r == range).map(|(_, b)| *b).unwrap_or_else(|| default_band(range)) }
    pub fn is_identity(&self) -> bool { !self.colorize && self.adjustments.iter().all(|(_, a)| *a == [0.0; 3]) }
    fn weight(&self, range: &str, hue: f64) -> f64 {
        if range == "Master" { return 1.0; }
        let weight = self.band(range).weight(hue);
        if self.invert_range && range == self.range { 1.0 - weight } else { weight }
    }
    /// How much every range shifts a given hue, sampled once per degree.
    fn response(&self) -> Vec<[f64; 3]> {
        (0..=360).map(|degree| {
            let mut r = [0.0; 3];
            for (range, a) in &self.adjustments {
                if *a == [0.0; 3] { continue; }
                let w = self.weight(range, degree as f64);
                if w <= 0.0 { continue; }
                for k in 0..3 { r[k] += a[k] * w; }
            }
            r
        }).collect()
    }
    fn adjust(&self, rgb: [f64; 3], response: &[[f64; 3]]) -> [f64; 3] {
        let (mut hue, mut saturation, mut lightness) = to_hsl(rgb);
        let amount;
        if self.colorize {
            let a = self.adjustment(&self.range);
            hue = a[0] % 360.0;
            saturation = (a[1] / 100.0).clamp(0.0, 1.0);
            amount = a[2] / 100.0;
        } else {
            let sampled = response[(hue.round() as usize).min(360)];
            amount = sampled[2] / 100.0;
            hue = (hue + sampled[0]) % 360.0;
            if hue < 0.0 { hue += 360.0; }
            saturation = (saturation * (1.0 + sampled[1] / 100.0)).clamp(0.0, 1.0);
        }
        let amount = amount.clamp(-1.0, 1.0);
        lightness = if amount >= 0.0 { lightness + (1.0 - lightness) * amount } else { lightness * (1.0 + amount) };
        to_rgb(hue, saturation, lightness.clamp(0.0, 1.0))
    }
    /// A 3D lookup table of `dim` steps per channel (as the reference's `CIColorCube`), indexed
    /// `((b * dim + g) * dim + r) * 3`.
    pub fn cube(&self, dim: usize) -> Vec<u8> {
        let response = self.response();
        let mut out = vec![0u8; dim * dim * dim * 3];
        let step = (dim - 1) as f64;
        for b in 0..dim { for g in 0..dim { for r in 0..dim {
            let c = self.adjust([r as f64 / step, g as f64 / step, b as f64 / step], &response);
            let i = ((b * dim + g) * dim + r) * 3;
            for k in 0..3 { out[i + k] = (c[k] * 255.0).round().clamp(0.0, 255.0) as u8; }
        } } }
        out
    }
}

fn to_hsl(rgb: [f64; 3]) -> (f64, f64, f64) {
    let [r, g, b] = rgb;
    let (high, low) = (r.max(g).max(b), r.min(g).min(b));
    let lightness = (high + low) / 2.0;
    let delta = high - low;
    if delta <= 0.0 { return (0.0, 0.0, lightness); }
    let saturation = delta / (1.0 - (2.0 * lightness - 1.0).abs());
    let mut hue = if high == r { (g - b) / delta } else if high == g { (b - r) / delta + 2.0 } else { (r - g) / delta + 4.0 };
    hue *= 60.0;
    if hue < 0.0 { hue += 360.0; }
    (hue, saturation.min(1.0), lightness)
}

fn to_rgb(hue: f64, saturation: f64, lightness: f64) -> [f64; 3] {
    if saturation <= 0.0 { return [lightness; 3]; }
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue / 60.0;
    let second = chroma * (1.0 - ((sector % 2.0) - 1.0).abs());
    let base = lightness - chroma / 2.0;
    let (r, g, b) = match sector as i32 { 0 => (chroma, second, 0.0), 1 => (second, chroma, 0.0), 2 => (0.0, chroma, second), 3 => (0.0, second, chroma), 4 => (second, 0.0, chroma), _ => (chroma, 0.0, second) };
    [(r + base).clamp(0.0, 1.0), (g + base).clamp(0.0, 1.0), (b + base).clamp(0.0, 1.0)]
}

/// Curves: per-channel points through which a shape-preserving cubic runs (`CurvesSettings`).
#[derive(Clone, Debug, PartialEq)]
pub struct Curves { pub channels: [Vec<(f64, f64)>; 4] }
impl Default for Curves { fn default() -> Self { Curves { channels: std::array::from_fn(|_| vec![(0.0, 0.0), (255.0, 255.0)]) } } }
impl Curves {
    pub fn is_identity(&self) -> bool { self.channels.iter().all(|c| *c == vec![(0.0, 0.0), (255.0, 255.0)]) }
    pub fn value(&self, x: f64, channel: usize) -> f64 {
        let p = &self.channels[channel];
        if p.len() < 2 { return x; }
        let i = p.iter().rposition(|q| q.0 <= x).unwrap_or(0).min(p.len() - 2);
        let d: Vec<f64> = p.windows(2).map(|w| (w[1].1 - w[0].1) / (w[1].0 - w[0].0)).collect();
        let slope = |j: usize| -> f64 {
            if j == 0 { return d[0]; }
            if j == p.len() - 1 { return *d.last().unwrap(); }
            if d[j - 1] * d[j] <= 0.0 { return 0.0; }
            2.0 / (1.0 / d[j - 1] + 1.0 / d[j])
        };
        let h = p[i + 1].0 - p[i].0;
        let t = ((x - p[i].0) / h).clamp(0.0, 1.0);
        let y = (2.0 * t * t * t - 3.0 * t * t + 1.0) * p[i].1 + (t * t * t - 2.0 * t * t + t) * h * slope(i)
            + (-2.0 * t * t * t + 3.0 * t * t) * p[i + 1].1 + (t * t * t - t * t) * h * slope(i + 1);
        y.clamp(0.0, 255.0)
    }
}

/// Applies a per-channel table (RGB order, 3 x 256, values 0 to 1) to packed BGRA premultiplied pixels the
/// way the reference applies `levels_apply`: straight colors around the lookup for soft edges.
fn apply_tables(pixels: &mut [u8], count: usize, rgb: &[[f32; 256]; 3]) {
    let mut tables = [0f32; 768];
    for (slot, channel) in [2usize, 1, 0].into_iter().enumerate() { tables[slot * 256..(slot + 1) * 256].copy_from_slice(&rgb[channel]); }
    unpremultiply_partial(pixels);
    ffi::levels(pixels, count, &tables);
    premultiply_partial(pixels);
}

/// One adjustment layer's settings, read from the file's JSON and written back the same way.
#[derive(Clone, Debug, PartialEq)]
pub enum Adjustment {
    Levels(Levels),
    Curves(Curves),
    Exposure(Exposure),
    GradientMap(GradientMap),
    Grain { grain: Grain, seed: u32 },
    HueSaturation(HueSaturation),
    BrightnessContrast(BrightnessContrast),
    Vibrance(Vibrance),
    BlackWhite(BlackWhite),
    PhotoFilter(PhotoFilter),
    Threshold(Threshold),
    Posterize(Posterize),
    ShadowsHighlights(ShadowsHighlights),
    SelectiveColor(SelectiveColor),
    ChannelMixer(ChannelMixer),
}

impl Adjustment {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Adjustment::Levels(_) => "Levels", Adjustment::Curves(_) => "Curves", Adjustment::Exposure(_) => "Exposure", Adjustment::GradientMap(_) => "Gradient Map", Adjustment::Grain { .. } => "Grain", Adjustment::HueSaturation(_) => "Hue/Saturation",
            Adjustment::BrightnessContrast(_) => "Brightness/Contrast", Adjustment::Vibrance(_) => "Vibrance", Adjustment::BlackWhite(_) => "Black & White", Adjustment::PhotoFilter(_) => "Photo Filter", Adjustment::Threshold(_) => "Threshold", Adjustment::Posterize(_) => "Posterize",
            Adjustment::ShadowsHighlights(_) => "Shadows/Highlights", Adjustment::SelectiveColor(_) => "Selective Color", Adjustment::ChannelMixer(_) => "Channel Mixer",
        }
    }

    pub fn from_kind(kind: &str) -> Option<Adjustment> {
        Some(match kind {
            "Levels" => Adjustment::Levels(Levels::default()), "Curves" => Adjustment::Curves(Curves::default()), "Exposure" => Adjustment::Exposure(Exposure::default()),
            "Gradient Map" => Adjustment::GradientMap(GradientMap::default()), "Grain" => Adjustment::Grain { grain: Grain::default(), seed: 0 },
            "Hue/Saturation" => Adjustment::HueSaturation(HueSaturation::default()),
            "Brightness/Contrast" => Adjustment::BrightnessContrast(BrightnessContrast::default()), "Vibrance" => Adjustment::Vibrance(Vibrance::default()),
            "Black & White" => Adjustment::BlackWhite(BlackWhite::default()), "Photo Filter" => Adjustment::PhotoFilter(PhotoFilter::default()),
            "Threshold" => Adjustment::Threshold(Threshold::default()), "Posterize" => Adjustment::Posterize(Posterize::default()),
            "Shadows/Highlights" => Adjustment::ShadowsHighlights(ShadowsHighlights::default()), "Selective Color" => Adjustment::SelectiveColor(SelectiveColor::default()), "Channel Mixer" => Adjustment::ChannelMixer(ChannelMixer::default()), _ => return None,
        })
    }

    /// From the file's record (`LayerAdjustment`'s fields).
    pub fn from_record(record: &crate::format::Adjustment) -> Option<Adjustment> {
        use serde_json::Value;
        let s = &record.settings;
        let num = |v: Option<&Value>, d: f64| v.and_then(Value::as_f64).unwrap_or(d);
        let color = |v: Option<&Value>, d: [f64; 3]| v.map(|c| [num(c.get("red"), d[0]), num(c.get("green"), d[1]), num(c.get("blue"), d[2])]).unwrap_or(d);
        Some(match record.kind.as_str() {
            "Levels" => {
                let mut levels = Levels::default();
                if let Some(ranges) = s.get("levels").and_then(|l| l.get("ranges")).and_then(Value::as_array) {
                    for (i, r) in ranges.iter().take(4).enumerate() {
                        levels.ranges[i] = Range { black: num(r.get("black"), 0.0), gamma: num(r.get("gamma"), 1.0), white: num(r.get("white"), 255.0), output_black: num(r.get("outputBlack"), 0.0), output_white: num(r.get("outputWhite"), 255.0) };
                    }
                }
                Adjustment::Levels(levels.normalized())
            }
            "Curves" => {
                let mut curves = Curves::default();
                if let Some(channels) = s.get("curves").and_then(|c| c.get("channels")).and_then(Value::as_array) {
                    for (i, ch) in channels.iter().take(4).enumerate() {
                        if let Some(points) = ch.as_array() {
                            let pts: Vec<(f64, f64)> = points.iter().map(|p| (num(p.get("x"), 0.0), num(p.get("y"), 0.0))).collect();
                            if pts.len() >= 2 { curves.channels[i] = pts; }
                        }
                    }
                }
                Adjustment::Curves(curves)
            }
            "Exposure" => {
                let e = s.get("exposureSettings");
                Adjustment::Exposure(Exposure { exposure: num(e.and_then(|e| e.get("exposure")), 0.0), offset: num(e.and_then(|e| e.get("offset")), 0.0), gamma: num(e.and_then(|e| e.get("gamma")), 1.0) }.normalized())
            }
            "Gradient Map" => {
                let g = s.get("gradientMapSettings");
                let stops = g.and_then(|g| g.get("stops")).and_then(Value::as_array).map(|a| a.iter().filter_map(|st| Some(crate::gradient::Stop { position: st.get("position")?.as_f64()?, color: color(st.get("color"), [0.0; 3]) })).collect()).unwrap_or_default();
                Adjustment::GradientMap(GradientMap { shadows: color(g.and_then(|g| g.get("shadows")), [0.0; 3]), highlights: color(g.and_then(|g| g.get("highlights")), [1.0; 3]), reversed: g.and_then(|g| g.get("reversed")).and_then(Value::as_bool).unwrap_or(false), stops })
            }
            "Grain" => {
                let g = s.get("grainSettings");
                Adjustment::Grain { grain: Grain { amount: num(g.and_then(|g| g.get("amount")), 25.0), size: num(g.and_then(|g| g.get("size")), 1.5), roughness: num(g.and_then(|g| g.get("roughness")), 50.0) }.normalized(), seed: g.and_then(|g| g.get("seed")).and_then(Value::as_u64).unwrap_or(0) as u32 }
            }
            "Hue/Saturation" => {
                let mut hs = HueSaturation { colorize: s.get("colorize").and_then(Value::as_bool).unwrap_or(false), ..HueSaturation::default() };
                match s.get("hsvSettings") {
                    Some(v) => {
                        hs.range = v.get("range").and_then(Value::as_str).unwrap_or("Master").to_string();
                        hs.colorize = v.get("colorize").and_then(Value::as_bool).unwrap_or(hs.colorize);
                        hs.invert_range = v.get("invertRange").and_then(Value::as_bool).unwrap_or(false);
                        for (range, a) in keyed_entries(v.get("adjustments")) { hs.adjustments.push((range, [num(a.get("hue"), 0.0), num(a.get("saturation"), 0.0), num(a.get("lightness"), 0.0)])); }
                        for (range, b) in keyed_entries(v.get("bands")) { hs.bands.push((range, HueBand { falloff_start: num(b.get("falloffStart"), 0.0), range_start: num(b.get("rangeStart"), 0.0), range_end: num(b.get("rangeEnd"), 360.0), falloff_end: num(b.get("falloffEnd"), 360.0) })); }
                    }
                    None => hs.adjustments.push(("Master".into(), [num(s.get("hue"), 0.0), num(s.get("saturation"), 0.0), num(s.get("lightness"), 0.0)])),
                }
                Adjustment::HueSaturation(hs)
            }
            "Brightness/Contrast" => { let v = s.get("brightnessContrast"); Adjustment::BrightnessContrast(BrightnessContrast { brightness: num(v.and_then(|v| v.get("brightness")), 0.0), contrast: num(v.and_then(|v| v.get("contrast")), 0.0) }.normalized()) }
            "Vibrance" => { let v = s.get("vibranceSettings"); Adjustment::Vibrance(Vibrance { vibrance: num(v.and_then(|v| v.get("vibrance")), 0.0), saturation: num(v.and_then(|v| v.get("saturation")), 0.0) }.normalized()) }
            "Black & White" => {
                let v = s.get("blackWhite");
                let d = BlackWhite::default();
                Adjustment::BlackWhite(BlackWhite { reds: num(v.and_then(|v| v.get("reds")), d.reds), yellows: num(v.and_then(|v| v.get("yellows")), d.yellows), greens: num(v.and_then(|v| v.get("greens")), d.greens), cyans: num(v.and_then(|v| v.get("cyans")), d.cyans), blues: num(v.and_then(|v| v.get("blues")), d.blues), magentas: num(v.and_then(|v| v.get("magentas")), d.magentas) }.normalized())
            }
            "Photo Filter" => {
                let v = s.get("photoFilter");
                let d = PhotoFilter::default();
                Adjustment::PhotoFilter(PhotoFilter { color: color(v.and_then(|v| v.get("color")), d.color), density: num(v.and_then(|v| v.get("density")), d.density), preserve_luminosity: v.and_then(|v| v.get("preserveLuminosity")).and_then(Value::as_bool).unwrap_or(true) }.normalized())
            }
            "Threshold" => Adjustment::Threshold(Threshold { level: num(s.get("threshold").and_then(|v| v.get("level")), 128.0) }.normalized()),
            "Posterize" => Adjustment::Posterize(Posterize { levels: num(s.get("posterize").and_then(|v| v.get("levels")), 4.0) }.normalized()),
            "Shadows/Highlights" => { let v = s.get("shadowsHighlights"); let d = ShadowsHighlights::default(); Adjustment::ShadowsHighlights(ShadowsHighlights { shadows: num(v.and_then(|v| v.get("shadows")), d.shadows), highlights: num(v.and_then(|v| v.get("highlights")), d.highlights), radius: num(v.and_then(|v| v.get("radius")), d.radius) }.normalized()) }
            "Selective Color" => {
                let v = s.get("selectiveColor");
                let mut sc = SelectiveColor { range: v.and_then(|v| v.get("range")).and_then(Value::as_str).unwrap_or("Reds").to_string(), adjustments: Vec::new(), relative: v.and_then(|v| v.get("relative")).and_then(Value::as_bool).unwrap_or(true) };
                for (range, a) in keyed_entries(v.and_then(|v| v.get("adjustments"))) { sc.adjustments.push((range, [num(a.get("cyan"), 0.0), num(a.get("magenta"), 0.0), num(a.get("yellow"), 0.0), num(a.get("black"), 0.0)])); }
                Adjustment::SelectiveColor(sc.normalized())
            }
            "Channel Mixer" => {
                let v = s.get("channelMixer");
                let row = |k: &str, d: [f64; 4]| v.and_then(|v| v.get(k)).and_then(Value::as_array).map(|a| [num(a.first(), d[0]), num(a.get(1), d[1]), num(a.get(2), d[2]), num(a.get(3), d[3])]).unwrap_or(d);
                let d = ChannelMixer::default();
                Adjustment::ChannelMixer(ChannelMixer { red: row("red", d.red), green: row("green", d.green), blue: row("blue", d.blue), monochrome: v.and_then(|v| v.get("monochrome")).and_then(Value::as_bool).unwrap_or(false) }.normalized())
            }
            _ => return None,
        })
    }

    /// Every setting the record carries is within the reference's `LayerAdjustment.isValid` bounds. Missing
    /// fields pass (they take defaults); present ones must be in range.
    pub fn record_is_valid(record: &crate::format::Adjustment) -> bool {
        use serde_json::Value;
        let s = &record.settings;
        let finite = |v: Option<&Value>| v.is_none_or(|v| v.as_f64().is_some_and(f64::is_finite));
        let within = |v: Option<&Value>, lo: f64, hi: f64| v.is_none_or(|v| v.as_f64().is_some_and(|n| n.is_finite() && (lo..=hi).contains(&n)));
        let boolean = |v: Option<&Value>| v.is_none_or(Value::is_boolean);
        // A present container has to be the right kind of value; only an absent one takes the defaults.
        let object = |v: Option<&Value>| v.is_none_or(Value::is_object);
        let mut ok = within(s.get("hue"), -360.0, 360.0) && within(s.get("saturation"), -100.0, 100.0) && within(s.get("lightness"), -100.0, 100.0) && boolean(s.get("colorize"));
        ok &= object(s.get("hsvSettings")) && object(s.get("levels")) && object(s.get("curves")) && object(s.get("exposureSettings")) && object(s.get("gradientMapSettings")) && object(s.get("grainSettings"));
        ok &= object(s.get("brightnessContrast")) && object(s.get("vibranceSettings")) && object(s.get("blackWhite")) && object(s.get("photoFilter")) && object(s.get("threshold")) && object(s.get("posterize"));
        ok &= object(s.get("shadowsHighlights")) && object(s.get("selectiveColor")) && object(s.get("channelMixer"));
        if !ok { return false; }
        if let Some(v) = s.get("shadowsHighlights") { ok &= within(v.get("shadows"), 0.0, 100.0) && within(v.get("highlights"), 0.0, 100.0) && within(v.get("radius"), 1.0, 500.0); }
        if let Some(v) = s.get("selectiveColor") {
            ok &= boolean(v.get("relative")) && v.get("range").is_none_or(|r| r.as_str().is_some_and(|r| SELECTIVE_RANGES.contains(&r)));
            ok &= v.get("adjustments").is_none_or(|d| d.is_array() || d.is_object());
            ok &= keyed_entries(v.get("adjustments")).into_iter().all(|(r, a)| SELECTIVE_RANGES.contains(&r.as_str()) && a.is_object() && ["cyan", "magenta", "yellow", "black"].iter().all(|k| within(a.get(*k), -100.0, 100.0)));
        }
        if let Some(v) = s.get("channelMixer") { ok &= boolean(v.get("monochrome")) && ["red", "green", "blue"].iter().all(|k| v.get(*k).is_none_or(|row| row.as_array().is_some_and(|a| a.len() == 4 && a.iter().all(|x| x.as_f64().is_some_and(|n| n.is_finite() && (-200.0..=200.0).contains(&n)))))); }
        if let Some(v) = s.get("brightnessContrast") { ok &= within(v.get("brightness"), -150.0, 150.0) && within(v.get("contrast"), -50.0, 100.0); }
        if let Some(v) = s.get("vibranceSettings") { ok &= within(v.get("vibrance"), -100.0, 100.0) && within(v.get("saturation"), -100.0, 100.0); }
        if let Some(v) = s.get("blackWhite") { ok &= ["reds", "yellows", "greens", "cyans", "blues", "magentas"].iter().all(|k| within(v.get(*k), -200.0, 300.0)); }
        if let Some(v) = s.get("photoFilter") { ok &= within(v.get("density"), 1.0, 100.0) && boolean(v.get("preserveLuminosity")); if let Some(c) = v.get("color") { ok &= c.is_object() && ["red", "green", "blue"].iter().all(|k| within(c.get(*k), 0.0, 1.0)); } }
        if let Some(v) = s.get("threshold") { ok &= within(v.get("level"), 1.0, 255.0); }
        if let Some(v) = s.get("posterize") { ok &= within(v.get("levels"), 2.0, 255.0); }
        if let Some(v) = s.get("hsvSettings") {
            ok &= v.get("range").is_none_or(|r| r.as_str().is_some_and(|r| COLOR_RANGES.contains(&r) || r == "Master")) && boolean(v.get("colorize")) && boolean(v.get("invertRange"));
            for key in ["adjustments", "bands"] { ok &= v.get(key).is_none_or(|d| d.is_array() || d.is_object()); }
            ok &= keyed_entries(v.get("adjustments")).into_iter().all(|(_, a)| a.is_object() && within(a.get("hue"), -360.0, 360.0) && within(a.get("saturation"), -100.0, 100.0) && within(a.get("lightness"), -100.0, 100.0));
            ok &= keyed_entries(v.get("bands")).into_iter().all(|(_, b)| b.is_object() && ["falloffStart", "rangeStart", "rangeEnd", "falloffEnd"].iter().all(|k| finite(b.get(*k))));
        }
        if let Some(ranges) = s.get("levels").and_then(|l| l.get("ranges")) {
            let Some(ranges) = ranges.as_array() else { return false };
            ok &= ranges.len() == 4 && ranges.iter().all(|r| {
                if !r.is_object() { return false; }
                let n = |k: &str, d: f64| r.get(k).and_then(Value::as_f64).unwrap_or(d);
                let numeric = ["black", "gamma", "white", "outputBlack", "outputWhite"].iter().all(|k| finite(r.get(*k)));
                let range = Range { black: n("black", 0.0), gamma: n("gamma", 1.0), white: n("white", 255.0), output_black: n("outputBlack", 0.0), output_white: n("outputWhite", 255.0) };
                numeric && range == range.normalized()
            });
        }
        if let Some(channels) = s.get("curves").and_then(|c| c.get("channels")) {
            let Some(channels) = channels.as_array() else { return false };
            ok &= channels.len() == 4 && channels.iter().all(|ch| {
                let Some(points) = ch.as_array() else { return false };
                if !points.iter().all(Value::is_object) { return false; }
                let pts: Vec<(f64, f64)> = points.iter().map(|p| (p.get("x").and_then(Value::as_f64).unwrap_or(f64::NAN), p.get("y").and_then(Value::as_f64).unwrap_or(f64::NAN))).collect();
                (2..=32).contains(&pts.len()) && pts[0].0 == 0.0 && pts[pts.len() - 1].0 == 255.0
                    && pts.iter().all(|p| p.0.is_finite() && p.1.is_finite() && (0.0..=255.0).contains(&p.0) && (0.0..=255.0).contains(&p.1))
                    && pts.windows(2).all(|w| w[0].0 < w[1].0)
            });
        }
        if let Some(e) = s.get("exposureSettings") { ok &= within(e.get("exposure"), -20.0, 20.0) && within(e.get("offset"), -0.5, 0.5) && within(e.get("gamma"), 0.01, 9.99); }
        if let Some(g) = s.get("gradientMapSettings") {
            ok &= boolean(g.get("reversed"));
            for key in ["shadows", "highlights"] { if let Some(c) = g.get(key) { ok &= c.is_object() && ["red", "green", "blue"].iter().all(|k| within(c.get(*k), 0.0, 1.0)); } }
            if let Some(stops) = g.get("stops") { ok &= stops.as_array().is_some_and(|a| a.iter().all(|st| st.is_object() && within(st.get("position"), 0.0, 1.0) && st.get("color").is_some_and(|c| c.is_object() && ["red", "green", "blue"].iter().all(|k| within(c.get(*k), 0.0, 1.0))))); }
        }
        if let Some(g) = s.get("grainSettings") { ok &= within(g.get("amount"), 0.0, 100.0) && within(g.get("size"), 0.5, 20.0) && within(g.get("roughness"), 0.0, 100.0) && g.get("seed").is_none_or(|v| v.as_u64().is_some()); }
        ok
    }

    /// The file record: the kind and every setting, in the reference app's field names.
    pub fn to_record(&self) -> crate::format::Adjustment {
        use serde_json::{Map, Value, json};
        let mut settings = Map::new();
        // Every record carries the legacy top-level fields the Swift always writes.
        settings.insert("hue".into(), json!(0.0)); settings.insert("saturation".into(), json!(0.0)); settings.insert("lightness".into(), json!(0.0)); settings.insert("colorize".into(), json!(false));
        settings.insert("levels".into(), json!({"channel": "RGB", "ranges": Levels::default().ranges.iter().map(|r| json!({"black": r.black, "gamma": r.gamma, "white": r.white, "outputBlack": r.output_black, "outputWhite": r.output_white})).collect::<Vec<_>>()}));
        settings.insert("curves".into(), json!({"channel": "RGB", "channels": Curves::default().channels.iter().map(|c| c.iter().map(|p| json!({"x": p.0, "y": p.1})).collect::<Vec<_>>()).collect::<Vec<_>>()}));
        match self {
            Adjustment::Levels(l) => { settings.insert("levels".into(), json!({"channel": "RGB", "ranges": l.ranges.iter().map(|r| json!({"black": r.black, "gamma": r.gamma, "white": r.white, "outputBlack": r.output_black, "outputWhite": r.output_white})).collect::<Vec<_>>()})); }
            Adjustment::Curves(c) => { settings.insert("curves".into(), json!({"channel": "RGB", "channels": c.channels.iter().map(|ch| ch.iter().map(|p| json!({"x": p.0, "y": p.1})).collect::<Vec<_>>()).collect::<Vec<_>>()})); }
            Adjustment::Exposure(e) => { settings.insert("exposureSettings".into(), json!({"exposure": e.exposure, "offset": e.offset, "gamma": e.gamma})); }
            Adjustment::GradientMap(g) => {
                let mut v = json!({"shadows": {"red": g.shadows[0], "green": g.shadows[1], "blue": g.shadows[2]}, "highlights": {"red": g.highlights[0], "green": g.highlights[1], "blue": g.highlights[2]}, "reversed": g.reversed});
                if g.stops.len() >= 2 { v["stops"] = json!(g.stops.iter().map(|s| json!({"position": s.position, "color": {"red": s.color[0], "green": s.color[1], "blue": s.color[2]}})).collect::<Vec<_>>()); }
                settings.insert("gradientMapSettings".into(), v);
            }
            Adjustment::Grain { grain, seed } => { settings.insert("grainSettings".into(), json!({"amount": grain.amount, "size": grain.size, "roughness": grain.roughness, "seed": seed})); }
            Adjustment::HueSaturation(h) => {
                let master = h.adjustment("Master");
                settings.insert("hue".into(), json!(master[0])); settings.insert("saturation".into(), json!(master[1])); settings.insert("lightness".into(), json!(master[2])); settings.insert("colorize".into(), json!(h.colorize));
                // Swift encodes a dictionary keyed by the ColorRange enum as a flat array of key, value, key, value.
                let adjustments: Vec<Value> = h.adjustments.iter().flat_map(|(r, a)| [json!(r), json!({"hue": a[0], "saturation": a[1], "lightness": a[2]})]).collect();
                let bands: Vec<Value> = COLOR_RANGES.iter().flat_map(|r| { let b = h.band(r); [json!(r), json!({"falloffStart": b.falloff_start, "rangeStart": b.range_start, "rangeEnd": b.range_end, "falloffEnd": b.falloff_end})] }).collect();
                settings.insert("hsvSettings".into(), json!({"range": h.range, "colorize": h.colorize, "invertRange": h.invert_range, "adjustments": adjustments, "bands": bands}));
            }
            Adjustment::BrightnessContrast(b) => { settings.insert("brightnessContrast".into(), json!({"brightness": b.brightness, "contrast": b.contrast})); }
            Adjustment::Vibrance(v) => { settings.insert("vibranceSettings".into(), json!({"vibrance": v.vibrance, "saturation": v.saturation})); }
            Adjustment::BlackWhite(b) => { settings.insert("blackWhite".into(), json!({"reds": b.reds, "yellows": b.yellows, "greens": b.greens, "cyans": b.cyans, "blues": b.blues, "magentas": b.magentas})); }
            Adjustment::PhotoFilter(p) => { settings.insert("photoFilter".into(), json!({"color": {"red": p.color[0], "green": p.color[1], "blue": p.color[2]}, "density": p.density, "preserveLuminosity": p.preserve_luminosity})); }
            Adjustment::Threshold(t) => { settings.insert("threshold".into(), json!({"level": t.level})); }
            Adjustment::Posterize(p) => { settings.insert("posterize".into(), json!({"levels": p.levels})); }
            Adjustment::ShadowsHighlights(v) => { settings.insert("shadowsHighlights".into(), json!({"shadows": v.shadows, "highlights": v.highlights, "radius": v.radius})); }
            Adjustment::SelectiveColor(sc) => {
                let adjustments: Vec<Value> = sc.adjustments.iter().flat_map(|(r, a)| [json!(r), json!({"cyan": a[0], "magenta": a[1], "yellow": a[2], "black": a[3]})]).collect();
                settings.insert("selectiveColor".into(), json!({"range": sc.range, "relative": sc.relative, "adjustments": adjustments}));
            }
            Adjustment::ChannelMixer(m) => { settings.insert("channelMixer".into(), json!({"red": m.red, "green": m.green, "blue": m.blue, "monochrome": m.monochrome})); }
        }
        crate::format::Adjustment { kind: self.kind_name().to_string(), settings }
    }

    /// Applies the adjustment in place to packed premultiplied BGRA pixels. `origin` and `units_per_pixel`
    /// place the pixels in document space, so Grain's pattern stays fixed however the canvas is drawn.
    pub fn apply(&self, pixels: &mut [u8], w: usize, h: usize, origin: (f64, f64), units_per_pixel: f64) {
        let stride = w * 4;
        match self {
            Adjustment::Levels(levels) => {
                if levels.is_identity() { return; }
                let mut rgb = [[0f32; 256]; 3];
                for (c, table) in rgb.iter_mut().enumerate() { for v in 0..256 { table[v] = levels.apply(v as f64 / 255.0, c + 1) as f32; } }
                apply_tables(pixels, w * h, &rgb);
            }
            Adjustment::Curves(curves) => {
                if curves.is_identity() { return; }
                let mut rgb = [[0f32; 256]; 3];
                for (c, table) in rgb.iter_mut().enumerate() { for v in 0..256 { table[v] = (curves.value(curves.value(v as f64, c + 1), 0) / 255.0) as f32; } }
                apply_tables(pixels, w * h, &rgb);
            }
            Adjustment::Exposure(exposure) => {
                if exposure.is_identity() { return; }
                let t = exposure.table();
                apply_tables(pixels, w * h, &[t, t, t]);
            }
            Adjustment::GradientMap(g) => {
                ffi::swap_red_blue(pixels, stride, w, h);
                ffi::gradient_map(pixels, w, h, stride, &g.table());
                ffi::swap_red_blue(pixels, stride, w, h);
            }
            Adjustment::Grain { grain, seed } => {
                if grain.amount <= 0.0 { return; }
                ffi::swap_red_blue(pixels, stride, w, h);
                ffi::grain(pixels, w, h, stride, grain.amount, grain.size, grain.roughness, *seed, origin, units_per_pixel);
                ffi::swap_red_blue(pixels, stride, w, h);
            }
            Adjustment::BrightnessContrast(b) => { if b.is_identity() { return; } let t = b.table(); apply_tables(pixels, w * h, &[t, t, t]); }
            Adjustment::Vibrance(v) => { if v.is_identity() { return; } map_straight(pixels, |rgb| v.pixel(rgb)); }
            Adjustment::BlackWhite(b) => { map_straight(pixels, |rgb| { let g = b.gray(rgb); [g, g, g] }); }
            Adjustment::PhotoFilter(p) => { map_straight(pixels, |rgb| p.pixel(rgb)); }
            Adjustment::Threshold(t) => { let level = t.normalized().level / 255.0; map_straight(pixels, |rgb| { let l = 0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2]; if l >= level { [1.0; 3] } else { [0.0; 3] } }); }
            Adjustment::Posterize(p) => { let t = p.table(); apply_tables(pixels, w * h, &[t, t, t]); }
            Adjustment::ShadowsHighlights(v) => v.apply(pixels, w, h),
            Adjustment::SelectiveColor(sc) => { if sc.is_identity() { return; } map_straight(pixels, |rgb| sc.pixel(rgb)); }
            Adjustment::ChannelMixer(m) => { if m.is_identity() { return; } map_straight(pixels, |rgb| m.pixel(rgb)); }
            Adjustment::HueSaturation(hs) => {
                if hs.is_identity() { return; }
                let dim = 33usize;
                let cube = hs.cube(dim);
                let step = (dim - 1) as f64;
                for p in pixels.chunks_exact_mut(4) {
                    let a = p[3] as u32;
                    if a == 0 { continue; }
                    let straight = [((p[2] as u32 * 255 + a / 2) / a).min(255) as f64 / 255.0, ((p[1] as u32 * 255 + a / 2) / a).min(255) as f64 / 255.0, ((p[0] as u32 * 255 + a / 2) / a).min(255) as f64 / 255.0];
                    let out = trilinear(&cube, dim, step, straight);
                    p[2] = ((out[0] as u32 * a + 127) / 255) as u8;
                    p[1] = ((out[1] as u32 * a + 127) / 255) as u8;
                    p[0] = ((out[2] as u32 * a + 127) / 255) as u8;
                }
            }
        }
    }
}

fn trilinear(cube: &[u8], dim: usize, step: f64, rgb: [f64; 3]) -> [u8; 3] {
    let f = rgb.map(|v| v.clamp(0.0, 1.0) * step);
    let i0 = f.map(|v| (v.floor() as usize).min(dim - 1));
    let i1 = i0.map(|i| (i + 1).min(dim - 1));
    let t = [f[0] - i0[0] as f64, f[1] - i0[1] as f64, f[2] - i0[2] as f64];
    let at = |r: usize, g: usize, b: usize, k: usize| cube[((b * dim + g) * dim + r) * 3 + k] as f64;
    let mut out = [0u8; 3];
    for k in 0..3 {
        let c00 = at(i0[0], i0[1], i0[2], k) * (1.0 - t[0]) + at(i1[0], i0[1], i0[2], k) * t[0];
        let c10 = at(i0[0], i1[1], i0[2], k) * (1.0 - t[0]) + at(i1[0], i1[1], i0[2], k) * t[0];
        let c01 = at(i0[0], i0[1], i1[2], k) * (1.0 - t[0]) + at(i1[0], i0[1], i1[2], k) * t[0];
        let c11 = at(i0[0], i1[1], i1[2], k) * (1.0 - t[0]) + at(i1[0], i1[1], i1[2], k) * t[0];
        let c0 = c00 * (1.0 - t[1]) + c10 * t[1];
        let c1 = c01 * (1.0 - t[1]) + c11 * t[1];
        out[k] = (c0 * (1.0 - t[2]) + c1 * t[2]).round().clamp(0.0, 255.0) as u8;
    }
    out
}
