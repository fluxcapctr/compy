//! The `.comp` project package: `manifest.json` plus `images/<layer UUID>.png` and `.mask.png` assets.
//! `reference/docs/project-format.md` specifies versions 1 through 6; `ProjectStore.swift` in the reference
//! app adds version 7 (adjustment layers, mask placement, shape layers) and is the source of every rule here.

pub mod validate;
pub use validate::{live_mask_graph, upper};

use crate::png_io::{Decoded, decode};
use cairo::ImageSurface;
use serde::{Deserialize, Serialize, Serializer};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const FORMAT: &str = "com.compositor.project";
pub const VERSIONS: std::ops::RangeInclusive<i64> = 1..=7;
pub const MAX_SIDE: i64 = 30_000;
pub const MAX_PIXELS: i64 = 100_000_000;
pub const MAX_LAYERS: usize = 10_000;
pub const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectError {
    Invalid,
    Version(i64),
    MissingImage,
    TooLarge,
}

impl fmt::Display for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid => write!(f, "This is not a valid Compositor project, or its metadata is damaged."),
            Self::Version(v) => write!(f, "This project uses format version {v}. This app supports versions 1–7."),
            Self::MissingImage => write!(f, "An image inside the project is missing or damaged."),
            Self::TooLarge => write!(f, "This project exceeds the supported canvas, layer, file-size, or 100-megapixel image limit."),
        }
    }
}
impl std::error::Error for ProjectError {}

/// Swift's `CGPoint` and `CGSize` encode as two-element arrays.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
pub struct Point(pub f64, pub f64);
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
pub struct Size(pub f64, pub f64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
pub enum Sampling {
    #[serde(rename = "Nearest")]
    Nearest,
    #[serde(rename = "Smooth")]
    Smooth,
    #[default]
    #[serde(rename = "High quality")]
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
pub enum BlendMode {
    #[default]
    Normal, Multiply, Screen, Overlay, Darken, Lighten, Difference,
    #[serde(rename = "Color Dodge")]
    ColorDodge,
    #[serde(rename = "Color Burn")]
    ColorBurn,
    Hue, Saturation, Color, Luminosity,
}

impl BlendMode {
    /// Every mode, in the order the blend menu lists them.
    pub const ALL: [BlendMode; 13] = [
        Self::Normal, Self::Multiply, Self::Screen, Self::Overlay, Self::Darken, Self::Lighten, Self::Difference,
        Self::ColorDodge, Self::ColorBurn, Self::Hue, Self::Saturation, Self::Color, Self::Luminosity,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Normal => "Normal", Self::Multiply => "Multiply", Self::Screen => "Screen",
            Self::Overlay => "Overlay", Self::Darken => "Darken", Self::Lighten => "Lighten",
            Self::Difference => "Difference", Self::ColorDodge => "Color Dodge", Self::ColorBurn => "Color Burn",
            Self::Hue => "Hue", Self::Saturation => "Saturation", Self::Color => "Color", Self::Luminosity => "Luminosity",
        }
    }
}

/// Unrotated bounds in document pixels; rotation is clockwise (y down) around their center.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
pub struct Transform {
    pub origin: Point,
    pub size: Size,
    #[serde(default)]
    pub rotation: f64,
    #[serde(default, rename = "flipX")]
    pub flip_x: bool,
    #[serde(default, rename = "flipY")]
    pub flip_y: bool,
    #[serde(default)]
    pub sampling: Sampling,
}

impl Transform {
    pub fn center(&self) -> Point { Point(self.origin.0 + self.size.0 / 2.0, self.origin.1 + self.size.1 / 2.0) }
    pub fn radians(&self) -> f64 { (self.rotation % 360.0).to_radians() }
    pub fn is_valid(&self) -> bool {
        [self.origin.0, self.origin.1, self.size.0, self.size.1, self.rotation].iter().all(|v| v.is_finite())
            && (1.0..=300_000.0).contains(&self.size.0) && (1.0..=300_000.0).contains(&self.size.1)
            && self.origin.0.abs() <= 1_000_000.0 && self.origin.1.abs() <= 1_000_000.0
    }
    /// The document point a unit-square point (0 to 1, y down) lands on.
    pub fn point(&self, unit: (f64, f64)) -> (f64, f64) {
        let (x, y) = ((unit.0 - 0.5) * self.size.0, (unit.1 - 0.5) * self.size.1);
        let (c, s) = (self.radians().cos(), self.radians().sin());
        let center = self.center();
        (center.0 + x * c - y * s, center.1 + x * s + y * c)
    }

    /// Whether a document point lies inside the (rotated) box.
    pub fn contains(&self, point: (f64, f64)) -> bool {
        let center = self.center();
        let (x, y) = (point.0 - center.0, point.1 - center.1);
        let (c, s) = (self.radians().cos(), self.radians().sin());
        (x * c + y * s).abs() <= self.size.0 / 2.0 && (-x * s + y * c).abs() <= self.size.1 / 2.0
    }

    /// The upright box around the placed layer: (min x, min y, max x, max y).
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        let corners = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].map(|u| self.point(u));
        (corners.iter().map(|c| c.0).fold(f64::MAX, f64::min), corners.iter().map(|c| c.1).fold(f64::MAX, f64::min),
         corners.iter().map(|c| c.0).fold(f64::MIN, f64::max), corners.iter().map(|c| c.1).fold(f64::MIN, f64::max))
    }

    /// Whole pixels and whole degrees: what dragging, scaling and rotating leave behind.
    pub fn rounded(&self) -> Transform {
        let mut r = *self;
        r.origin = Point(self.origin.0.round(), self.origin.1.round());
        r.size = Size(self.size.0.round().max(1.0), self.size.1.round().max(1.0));
        r.rotation = self.rotation.round();
        r
    }

    /// The unit square (0 to 1, y down) mapped where this transform places a layer on the document.
    pub fn unit_to_document(&self) -> cairo::Matrix { crate::render::pixel_to_document(self, 1, 1) }

    /// A transform placing the unit square as `map` does: a rotated, maybe flipped rectangle (shear, which only
    /// uneven scaling of something rotated adds, is dropped). Keeps this transform's sampling. `placing`.
    pub fn placing(&self, map: &cairo::Matrix) -> Transform {
        let sign = if self.flip_x { -1.0 } else { 1.0 };
        let angle = (map.yx() * sign).atan2(map.xx() * sign);
        let along = -map.xy() * angle.sin() + map.yy() * angle.cos();
        let middle = map.transform_point(0.5, 0.5);
        let mut result = *self;
        result.size = Size(map.xx().hypot(map.yx()), along.abs());
        let degrees = angle.to_degrees();
        result.rotation = degrees + ((self.rotation - degrees) / 360.0).round() * 360.0;
        result.flip_y = along < 0.0;
        result.origin = Point(middle.0 - result.size.0 / 2.0, middle.1 - result.size.1 / 2.0);
        result
    }

    /// This placement carried along as a layer moves from `old` to `new` (`following`).
    pub fn following(&self, old: &Transform, new: &Transform) -> Transform {
        if old == new { return *self; }
        if old.size == new.size && old.rotation == new.rotation && old.flip_x == new.flip_x && old.flip_y == new.flip_y {
            let mut moved = *self;
            moved.origin = Point(self.origin.0 + new.origin.0 - old.origin.0, self.origin.1 + new.origin.1 - old.origin.1);
            return moved;
        }
        let Ok(inverse) = old.unit_to_document().try_invert() else { return *self };
        let map = cairo::Matrix::multiply(&cairo::Matrix::multiply(&self.unit_to_document(), &inverse), &new.unit_to_document());
        self.placing(&map)
    }

    /// This placement mirrored across a vertical line at `axis` (or a horizontal one): the picture flips, its
    /// angle turns the other way, and its middle crosses to the other side of the line. `mirrored`.
    pub fn mirrored(&self, horizontally: bool, axis: f64) -> Transform {
        let mut result = *self;
        let center = self.center();
        if horizontally {
            result.flip_x = !self.flip_x;
            result.origin.0 = 2.0 * axis - center.0 - self.size.0 / 2.0;
        } else {
            result.flip_y = !self.flip_y;
            result.origin.1 = 2.0 * axis - center.1 - self.size.1 / 2.0;
        }
        result.rotation = -self.rotation;
        result
    }

    /// The same place on the document, whatever the sampling.
    pub fn same_placement(&self, other: &Transform) -> bool {
        self.origin == other.origin && self.size == other.size && self.rotation == other.rotation
            && self.flip_x == other.flip_x && self.flip_y == other.flip_y
    }
}

/// An adjustment layer's settings. Only the kind is read for now; the rest is kept as it was written.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Adjustment {
    pub kind: String,
    #[serde(flatten)]
    pub settings: serde_json::Map<String, serde_json::Value>,
}

pub const ADJUSTMENT_KINDS: [&str; 15] = ["Hue/Saturation", "Levels", "Curves", "Exposure", "Gradient Map", "Grain", "Brightness/Contrast", "Vibrance", "Black & White", "Photo Filter", "Threshold", "Posterize", "Shadows/Highlights", "Selective Color", "Channel Mixer"];

/// Swift writes UUIDs in uppercase; so do we, so files round-trip byte for byte where it matters.
fn upper_uuid<S: Serializer>(id: &Uuid, s: S) -> Result<S::Ok, S::Error> { s.serialize_str(&id.hyphenated().to_string().to_uppercase()) }
fn upper_uuid_opt<S: Serializer>(id: &Option<Uuid>, s: S) -> Result<S::Ok, S::Error> {
    match id { Some(id) => upper_uuid(id, s), None => s.serialize_none() }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Layer {
    #[serde(serialize_with = "upper_uuid")]
    pub id: Uuid,
    pub name: String,
    #[serde(rename = "isVisible")]
    pub is_visible: bool,
    pub transform: Transform,
    #[serde(default, rename = "imageFile", skip_serializing_if = "Option::is_none")]
    pub image_file: Option<String>,
    #[serde(default, rename = "parentID", skip_serializing_if = "Option::is_none", serialize_with = "upper_uuid_opt")]
    pub parent_id: Option<Uuid>,
    #[serde(default, rename = "isGroup", skip_serializing_if = "Option::is_none")]
    pub is_group: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    #[serde(default, rename = "blendMode", skip_serializing_if = "Option::is_none")]
    pub blend_mode: Option<BlendMode>,
    #[serde(default, rename = "maskFile", skip_serializing_if = "Option::is_none")]
    pub mask_file: Option<String>,
    #[serde(default, rename = "maskEnabled", skip_serializing_if = "Option::is_none")]
    pub mask_enabled: Option<bool>,
    #[serde(default, rename = "maskSourceID", skip_serializing_if = "Option::is_none", serialize_with = "upper_uuid_opt")]
    pub mask_source_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adjustment: Option<Adjustment>,
    #[serde(default, rename = "maskPlacement", skip_serializing_if = "Option::is_none")]
    pub mask_placement: Option<Transform>,
    #[serde(default, rename = "maskLinked", skip_serializing_if = "Option::is_none")]
    pub mask_linked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<serde_json::Value>,
    /// A type layer's text and style (this app's extension; other readers ignore it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<serde_json::Value>,
    /// Layer effects: shadows, glows, bevel, stroke, overlay (this app's extension).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<serde_json::Value>,
}

impl Layer {
    pub fn is_group(&self) -> bool { self.is_group == Some(true) }
    pub fn opacity(&self) -> f64 { self.opacity.unwrap_or(1.0) }
    pub fn blend_mode(&self) -> BlendMode { self.blend_mode.unwrap_or_default() }
    pub fn mask_enabled(&self) -> bool { self.mask_file.is_some() && self.mask_enabled.unwrap_or(true) }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Manifest {
    #[serde(default = "default_format")]
    pub format: String,
    pub version: i64,
    #[serde(default = "default_color_space", rename = "colorSpace")]
    pub color_space: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<f64>,
    #[serde(rename = "documentID", serialize_with = "upper_uuid")]
    pub document_id: Uuid,
    pub width: i64,
    pub height: i64,
    #[serde(default, rename = "activeLayerID", skip_serializing_if = "Option::is_none", serialize_with = "upper_uuid_opt")]
    pub active_layer_id: Option<Uuid>,
    pub layers: Vec<Layer>,
}

/// The current format version, what saves declare.
pub const SAVE_VERSION: i64 = 7;

/// Writes a `.comp` package: the manifest and every layer's image and mask as PNG, staged beside the
/// destination and swapped into place only once complete, as the reference's coordinated write does.
pub fn save(path: &Path, manifest: &Manifest, images: &HashMap<Uuid, ImageSurface>, masks: &HashMap<Uuid, ImageSurface>) -> anyhow::Result<()> {
    use anyhow::Context as _;
    validate::manifest(manifest)?;
    let json = serde_json::to_vec_pretty(manifest)?;
    if json.len() as u64 > MAX_MANIFEST_BYTES { anyhow::bail!("the manifest is too large"); }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Untitled.comp".into());
    let staging = parent.join(format!(".{name}.saving-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(staging.join("images")).with_context(|| format!("creating {}", staging.display()))?;
    let result = (|| -> anyhow::Result<()> {
        std::fs::write(staging.join("manifest.json"), &json)?;
        for layer in &manifest.layers {
            if let Some(file) = &layer.image_file {
                let image = images.get(&layer.id).with_context(|| format!("layer {} has no pixels", layer.name))?;
                crate::png_io::encode(image, &staging.join("images").join(file), manifest.resolution())?;
            }
            if let Some(file) = &layer.mask_file {
                let mask = masks.get(&layer.id).with_context(|| format!("layer {} has no mask pixels", layer.name))?;
                crate::png_io::encode_gray(mask, &staging.join("images").join(file))?;
            }
        }
        Ok(())
    })();
    if let Err(error) = result { let _ = std::fs::remove_dir_all(&staging); return Err(error); }
    // Swap: the old package steps aside, the new one takes its place, the old one goes.
    let backup = parent.join(format!(".{name}.previous-{}", std::process::id()));
    let existed = path.exists();
    if existed { std::fs::rename(path, &backup).with_context(|| "moving the previous package aside")?; }
    if let Err(error) = std::fs::rename(&staging, path) {
        if existed { let _ = std::fs::rename(&backup, path); }
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error).with_context(|| format!("placing {}", path.display()));
    }
    if existed { let _ = std::fs::remove_dir_all(&backup); }
    Ok(())
}

fn default_format() -> String { FORMAT.to_string() }
fn default_color_space() -> String { "sRGB".to_string() }

impl Manifest {
    /// Older version-1 projects default to 72 pixels per inch.
    pub fn resolution(&self) -> f64 { self.resolution.unwrap_or(72.0) }

    pub fn parse(json: &[u8]) -> Result<Manifest, ProjectError> {
        #[derive(Deserialize)]
        struct Header { format: String, version: i64 }
        let header: Header = serde_json::from_slice(json).map_err(|_| ProjectError::Invalid)?;
        if header.format != FORMAT { return Err(ProjectError::Invalid); }
        if !VERSIONS.contains(&header.version) { return Err(ProjectError::Version(header.version)); }
        let manifest: Manifest = serde_json::from_slice(json).map_err(|_| ProjectError::Invalid)?;
        validate::manifest(&manifest)?;
        Ok(manifest)
    }
}

/// A loaded project: the manifest plus decoded layer images (premultiplied ARGB32) and masks (A8) by layer id.
pub struct Project {
    pub path: PathBuf,
    pub manifest: Manifest,
    pub images: HashMap<Uuid, ImageSurface>,
    pub masks: HashMap<Uuid, ImageSurface>,
}

/// Reads a `.comp` package, rejecting bad metadata, unsafe paths, missing assets and oversized data.
pub fn load(path: &Path) -> Result<Project, ProjectError> {
    if !path.is_dir() { return Err(ProjectError::Invalid); }
    let manifest_path = path.join("manifest.json");
    check_file(&manifest_path, path, MAX_MANIFEST_BYTES)?;
    let json = std::fs::read(&manifest_path).map_err(|_| ProjectError::Invalid)?;
    let manifest = Manifest::parse(&json)?;
    let mut images = HashMap::new();
    let mut masks = HashMap::new();
    let (mut pixels, mut mask_pixels) = (0i64, 0i64);
    for layer in &manifest.layers {
        for is_mask in [false, true] {
            let Some(filename) = (if is_mask { &layer.mask_file } else { &layer.image_file }) else { continue };
            let file = path.join("images").join(filename);
            check_file(&file, path, MAX_ASSET_BYTES)?;
            let used = if is_mask { &mut mask_pixels } else { &mut pixels };
            let decoded = decode(&file, is_mask, |header| check_size(header.width as i64, header.height as i64, used))?;
            match decoded {
                Decoded::Image(surface) => { images.insert(layer.id, surface); }
                Decoded::Mask(surface) => { masks.insert(layer.id, surface); }
            }
        }
    }
    Ok(Project { path: path.to_path_buf(), manifest, images, masks })
}

fn check_size(width: i64, height: i64, used: &mut i64) -> Result<(), ProjectError> {
    if !(1..=MAX_SIDE).contains(&width) || !(1..=MAX_SIDE).contains(&height) || width * height > MAX_PIXELS - *used {
        return Err(ProjectError::TooLarge);
    }
    *used += width * height;
    Ok(())
}

/// A regular, non-symlinked file inside the package, no larger than `maximum` bytes.
fn check_file(file: &Path, package: &Path, maximum: u64) -> Result<(), ProjectError> {
    let root = package.canonicalize().map_err(|_| ProjectError::Invalid)?;
    let resolved = file.canonicalize().map_err(|_| ProjectError::TooLarge)?;
    if !resolved.starts_with(&root) { return Err(ProjectError::Invalid); }
    let link = std::fs::symlink_metadata(file).map_err(|_| ProjectError::TooLarge)?;
    if link.file_type().is_symlink() || !link.is_file() || link.len() > maximum { return Err(ProjectError::TooLarge); }
    Ok(())
}

/// Layers in drawing order, bottom to top, each with its depth and effective visibility (a hidden folder
/// hides everything inside it). Mirrors `LayerHierarchy.entries`.
pub struct Entry<'a> {
    pub layer: &'a Layer,
    pub depth: usize,
    pub visible: bool,
}

pub fn entries(layers: &[Layer]) -> Vec<Entry<'_>> { entries_ordered(layers, false) }

/// `top_first` lists them as the layers panel does: top layer first, each folder above its contents.
pub fn entries_ordered(layers: &[Layer], top_first: bool) -> Vec<Entry<'_>> {
    let mut children: HashMap<Option<Uuid>, Vec<&Layer>> = HashMap::new();
    for layer in layers { children.entry(layer.parent_id).or_default().push(layer); }
    let mut result = Vec::with_capacity(layers.len());
    fn visit<'a>(parent: Option<Uuid>, depth: usize, visible: bool, top_first: bool, children: &HashMap<Option<Uuid>, Vec<&'a Layer>>, out: &mut Vec<Entry<'a>>) {
        if depth > 64 { return; }
        let siblings = children.get(&parent).map(|v| v.as_slice()).unwrap_or(&[]);
        let order: Vec<&'a Layer> = if top_first { siblings.iter().rev().copied().collect() } else { siblings.to_vec() };
        for layer in order {
            let effective = visible && layer.is_visible;
            out.push(Entry { layer, depth, visible: effective });
            if layer.is_group() { visit(Some(layer.id), depth + 1, effective, top_first, children, out); }
        }
    }
    visit(None, 0, true, top_first, &children, &mut result);
    result
}

/// Ids of the visible, non-folder layers in drawing order.
pub fn visible_layers(layers: &[Layer]) -> Vec<Uuid> {
    entries(layers).into_iter().filter(|e| e.visible && !e.layer.is_group()).map(|e| e.layer.id).collect()
}
