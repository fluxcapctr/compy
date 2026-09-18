//! Builds `.comp` packages in a temp directory with pixels chosen so the flattened result is known exactly.

#![allow(dead_code)]

use compositor::format::{self, Project};
use compositor::raster::with_bytes;
use compositor::render;
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use uuid::Uuid;

pub struct Fixture {
    pub dir: PathBuf,
    pub width: u32,
    pub height: u32,
    pub version: i64,
    pub layers: Vec<Value>,
    pub extra: Map<String, Value>,
}

pub struct Spec {
    pub name: &'static str,
    pub visible: bool,
    pub origin: (f64, f64),
    pub size: (f64, f64),
    pub rotation: f64,
    pub flip: (bool, bool),
    pub sampling: &'static str,
    pub pixels: Option<(u32, u32, Vec<[u8; 4]>)>,
    pub parent: Option<Uuid>,
    pub group: bool,
    pub opacity: Option<f64>,
    pub blend: Option<&'static str>,
    pub mask: Option<(u32, u32, Vec<u8>)>,
    pub mask_enabled: Option<bool>,
    pub mask_source: Option<Uuid>,
    pub mask_placement: Option<((f64, f64), (f64, f64))>,
    pub adjustment: Option<Value>,
}

impl Default for Spec {
    fn default() -> Self {
        Spec {
            name: "Layer", visible: true, origin: (0.0, 0.0), size: (1.0, 1.0), rotation: 0.0, flip: (false, false),
            sampling: "Nearest", pixels: None, parent: None, group: false, opacity: None, blend: None, mask: None,
            mask_enabled: None, mask_source: None, mask_placement: None, adjustment: None,
        }
    }
}

/// A `w` x `h` image of one color.
pub fn solid(w: u32, h: u32, color: [u8; 4]) -> (u32, u32, Vec<[u8; 4]>) { (w, h, vec![color; (w * h) as usize]) }
/// A `w` x `h` image where column `x < split` is `left` and the rest `right`.
pub fn halves(w: u32, h: u32, split: u32, left: [u8; 4], right: [u8; 4]) -> (u32, u32, Vec<[u8; 4]>) {
    (w, h, (0..w * h).map(|i| if i % w < split { left } else { right }).collect())
}

pub const RED: [u8; 4] = [255, 0, 0, 255];
pub const BLUE: [u8; 4] = [0, 0, 255, 255];
pub const WHITE: [u8; 4] = [255, 255, 255, 255];
pub const BLACK: [u8; 4] = [0, 0, 0, 255];
pub const CLEAR: [u8; 4] = [0, 0, 0, 0];

impl Fixture {
    pub fn new(name: &str, width: u32, height: u32) -> Fixture {
        let dir = std::env::temp_dir().join(format!("compositor-test-{}-{name}.comp", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("images")).unwrap();
        Fixture { dir, width, height, version: 7, layers: Vec::new(), extra: Map::new() }
    }

    pub fn add(&mut self, spec: Spec) -> Uuid {
        let id = Uuid::new_v4();
        let upper = id.hyphenated().to_string().to_uppercase();
        let mut layer = json!({
            "id": upper,
            "name": spec.name,
            "isVisible": spec.visible,
            "transform": {
                "origin": [spec.origin.0, spec.origin.1],
                "size": [spec.size.0, spec.size.1],
                "rotation": spec.rotation,
                "flipX": spec.flip.0,
                "flipY": spec.flip.1,
                "sampling": spec.sampling,
            },
        });
        let object = layer.as_object_mut().unwrap();
        if let Some((w, h, pixels)) = &spec.pixels {
            let file = format!("{upper}.png");
            write_png(&self.dir.join("images").join(&file), *w, *h, png::ColorType::Rgba, &pixels.concat());
            object.insert("imageFile".into(), json!(file));
        }
        if let Some((w, h, gray)) = &spec.mask {
            let file = format!("{upper}.mask.png");
            write_png(&self.dir.join("images").join(&file), *w, *h, png::ColorType::Grayscale, gray);
            object.insert("maskFile".into(), json!(file));
        }
        if let Some(parent) = spec.parent { object.insert("parentID".into(), json!(parent.hyphenated().to_string().to_uppercase())); }
        if spec.group { object.insert("isGroup".into(), json!(true)); }
        if let Some(o) = spec.opacity { object.insert("opacity".into(), json!(o)); }
        if let Some(b) = spec.blend { object.insert("blendMode".into(), json!(b)); }
        if let Some(e) = spec.mask_enabled { object.insert("maskEnabled".into(), json!(e)); }
        if let Some(s) = spec.mask_source { object.insert("maskSourceID".into(), json!(s.hyphenated().to_string().to_uppercase())); }
        if let Some((origin, size)) = spec.mask_placement {
            object.insert("maskPlacement".into(), json!({"origin": [origin.0, origin.1], "size": [size.0, size.1], "rotation": 0, "flipX": false, "flipY": false, "sampling": "High quality"}));
        }
        if let Some(a) = spec.adjustment { object.insert("adjustment".into(), a); }
        self.layers.push(layer);
        id
    }

    pub fn manifest(&self) -> Value {
        let mut m = json!({
            "format": "com.compositor.project",
            "version": self.version,
            "colorSpace": "sRGB",
            "documentID": Uuid::new_v4().hyphenated().to_string().to_uppercase(),
            "width": self.width,
            "height": self.height,
            "layers": self.layers,
        });
        for (k, v) in &self.extra { m.as_object_mut().unwrap().insert(k.clone(), v.clone()); }
        m
    }

    pub fn write(&self) -> PathBuf {
        std::fs::write(self.dir.join("manifest.json"), serde_json::to_vec_pretty(&self.manifest()).unwrap()).unwrap();
        self.dir.clone()
    }

    pub fn load(&self) -> Result<Project, format::ProjectError> { format::load(&self.write()) }

    /// Renders and returns straight-alpha RGBA pixels, row-major, plus any warnings.
    pub fn render(&self) -> (Vec<[u8; 4]>, Vec<String>) {
        let project = self.load().expect("fixture loads");
        let mut rendered = render::render(project).expect("fixture renders");
        let (w, h) = (rendered.image.width() as usize, rendered.image.height() as usize);
        let pixels = with_bytes(&mut rendered.image, |data, stride| {
            (0..w * h).map(|i| {
                let p = &data[(i / w) * stride + (i % w) * 4..][..4];
                let a = p[3] as u32;
                let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a / 2) / a).min(255) as u8 };
                [un(p[2]), un(p[1]), un(p[0]), p[3]]
            }).collect()
        }).unwrap();
        (pixels, rendered.warnings)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.dir); }
}

pub fn write_png(path: &std::path::Path, w: u32, h: u32, color: png::ColorType, data: &[u8]) {
    let file = std::fs::File::create(path).unwrap();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    encoder.set_color(color);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().unwrap().write_image_data(data).unwrap();
}

/// Every channel within `tolerance`.
pub fn close(actual: [u8; 4], expected: [u8; 4], tolerance: u8) -> bool {
    actual.iter().zip(expected).all(|(a, e)| (*a as i32 - e as i32).abs() <= tolerance as i32)
}

#[track_caller]
pub fn assert_pixel(pixels: &[[u8; 4]], width: u32, x: u32, y: u32, expected: [u8; 4], tolerance: u8) {
    let actual = pixels[(y * width + x) as usize];
    assert!(close(actual, expected, tolerance), "pixel ({x}, {y}) is {actual:?}, expected {expected:?} ±{tolerance}");
}
