//! Composites a project the way `ImageExporter.render` and the canvas do: visible layers bottom to top, each
//! placed by its transform and drawn through its own mask, the masks of every folder around it, and any
//! clipping mask, with its opacity and blend mode. Cairo's compositing operators follow the same PDF blend
//! definitions as Photoshop, which is also why the reference app's `SeparableBlend` workaround has no
//! counterpart here.
//!
//! A `Renderer` owns the document (layers, pixels, masks) and its caches, and draws into any Cairo context:
//! `render_flat` for export, `draw` for a canvas whose context carries the view's pan, zoom and clip. Every
//! offscreen buffer is sized to the context's clip in device pixels, so a zoomed-in view of a huge document
//! costs a window's worth of pixels, not a document's worth.

mod live;

use crate::format::{BlendMode, Layer, Project, Sampling, Transform, entries_ordered, visible_layers};
use crate::raster::{halve, level_for, new_argb, with_bytes};
use anyhow::{Context as _, Result};
use cairo::{Antialias, Context, Extend, Filter, ImageSurface, Matrix, Operator, SurfacePattern};
use std::collections::HashMap;
use uuid::Uuid;

pub struct Rendered {
    pub image: ImageSurface,
    pub resolution: f64,
    /// Things the file asked for that this build does not draw yet.
    pub warnings: Vec<String>,
}

/// Renders the whole canvas at one pixel per document pixel.
pub fn render(project: Project) -> Result<Rendered> {
    let mut renderer = Renderer::new(project)?;
    let image = renderer.render_flat()?;
    Ok(Rendered { image, resolution: renderer.resolution, warnings: renderer.take_warnings() })
}

pub struct Renderer {
    width: i32,
    height: i32,
    resolution: f64,
    layers: Vec<Layer>,
    index: HashMap<Uuid, usize>,
    images: HashMap<Uuid, ImageSurface>,
    masks: HashMap<Uuid, ImageSurface>,
    /// Pixels shown in place of a layer's own: a filter dialog's preview, or a stroke's live grid.
    previews: HashMap<Uuid, ImageSurface>,
    /// Where a preview grid sits on the document when it is not the layer's own grid.
    preview_transforms: HashMap<Uuid, Transform>,
    /// Mask pixels shown in place of a layer's own while a stroke paints its mask.
    mask_previews: HashMap<Uuid, ImageSurface>,
    /// Halved copies by (layer, source, level).
    halved: HashMap<(Uuid, Source, usize), ImageSurface>,
    /// Masks moved apart from their layers, resampled into the layer's own pixel grid, by (layer, preview).
    placed: HashMap<(Uuid, bool), ImageSurface>,
    thumbnails: HashMap<(Uuid, i32), ImageSurface>,
    live: live::LiveMasks,
    warnings: Vec<String>,
}

/// Which of a layer's pixel buffers a halved copy came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Source { Image, Mask, Preview, MaskPreview }

/// The layers, pixels and masks at one moment, shared by reference: what undo history holds.
#[derive(Clone)]
pub struct State {
    pub layers: Vec<Layer>,
    pub images: HashMap<Uuid, ImageSurface>,
    pub masks: HashMap<Uuid, ImageSurface>,
    pub width: i32,
    pub height: i32,
    pub resolution: f64,
}

impl PartialEq for State {
    fn eq(&self, other: &Self) -> bool {
        fn same(a: &HashMap<Uuid, ImageSurface>, b: &HashMap<Uuid, ImageSurface>) -> bool {
            a.len() == b.len() && a.iter().all(|(id, s)| b.get(id).is_some_and(|o| o.to_raw_none() == s.to_raw_none()))
        }
        self.layers == other.layers && same(&self.images, &other.images) && same(&self.masks, &other.masks)
            && self.width == other.width && self.height == other.height && self.resolution == other.resolution
    }
}

impl State {
    /// Bytes held by surfaces in `held` that `current` no longer uses.
    pub fn retained_bytes(held: &[&State], current: &State) -> usize {
        let mut seen: std::collections::HashSet<usize> = current.images.values().chain(current.masks.values()).map(|s| s.to_raw_none() as usize).collect();
        let mut bytes = 0;
        for state in held {
            for surface in state.images.values().chain(state.masks.values()) {
                if seen.insert(surface.to_raw_none() as usize) { bytes += surface.stride() as usize * surface.height() as usize; }
            }
        }
        bytes
    }
}

/// A folder's mask, clipping every layer inside the folder.
pub(crate) struct FolderMask {
    mask: ImageSurface,
    transform: Transform,
}

/// The device pixels a context's clip covers: what an offscreen copy of it needs to hold.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Region {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Region {
    fn of(cr: &Context) -> Result<Option<Region>> {
        let (x1, y1, x2, y2) = cr.clip_extents()?;
        if !(x2 > x1 && y2 > y1) { return Ok(None); }
        let corners = [(x1, y1), (x2, y1), (x1, y2), (x2, y2)].map(|(x, y)| cr.user_to_device(x, y));
        let (mut left, mut top, mut right, mut bottom) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for (x, y) in corners { left = left.min(x); top = top.min(y); right = right.max(x); bottom = bottom.max(y); }
        let (x, y) = (left.floor(), top.floor());
        let (width, height) = ((right.ceil() - x) as i32, (bottom.ceil() - y) as i32);
        if width <= 0 || height <= 0 || width as i64 * height as i64 > 100_000_000 { return Ok(None); }
        Ok(Some(Region { x: x as i32, y: y as i32, width, height }))
    }

    /// A transparent surface covering this region, and a context on it that maps user space exactly as `cr` does.
    fn offscreen(&self, cr: &Context) -> Result<(ImageSurface, Context)> {
        let surface = new_argb(self.width, self.height)?;
        let inner = Context::new(&surface)?;
        inner.set_matrix(Matrix::multiply(&cr.matrix(), &Matrix::new(1.0, 0.0, 0.0, 1.0, -self.x as f64, -self.y as f64)));
        Ok((surface, inner))
    }
}

impl Renderer {
    pub fn new(project: Project) -> Result<Self> {
        let manifest = project.manifest;
        let index = manifest.layers.iter().enumerate().map(|(i, l)| (l.id, i)).collect();
        Ok(Renderer {
            width: manifest.width as i32,
            height: manifest.height as i32,
            resolution: manifest.resolution(),
            layers: manifest.layers,
            index,
            images: project.images,
            masks: project.masks,
            previews: HashMap::new(),
            preview_transforms: HashMap::new(),
            mask_previews: HashMap::new(),
            halved: HashMap::new(),
            placed: HashMap::new(),
            thumbnails: HashMap::new(),
            live: live::LiveMasks::default(),
            warnings: Vec::new(),
        })
    }

    pub fn width(&self) -> i32 { self.width }
    pub fn height(&self) -> i32 { self.height }
    pub fn resolution(&self) -> f64 { self.resolution }
    pub fn layers(&self) -> &[Layer] { &self.layers }
    pub fn layer(&self, id: Uuid) -> &Layer { &self.layers[self.index[&id]] }
    pub fn has_image(&self, id: Uuid) -> bool { self.images.contains_key(&id) }
    pub fn image_size(&self, id: Uuid) -> Option<(i32, i32)> { self.images.get(&id).map(|s| (s.width(), s.height())) }
    pub fn take_warnings(&mut self) -> Vec<String> { std::mem::take(&mut self.warnings) }
    pub(crate) fn source(&self, id: Uuid) -> Option<Uuid> { self.layer(id).mask_source_id }
    pub(crate) fn parent(&self, id: Uuid) -> Option<Uuid> { self.layer(id).parent_id }

    pub fn image(&self, id: Uuid) -> Option<&ImageSurface> { self.images.get(&id) }
    pub fn images(&self) -> &HashMap<Uuid, ImageSurface> { &self.images }
    pub fn masks(&self) -> &HashMap<Uuid, ImageSurface> { &self.masks }
    pub fn mask(&self, id: Uuid) -> Option<&ImageSurface> { self.masks.get(&id) }

    /// Replaces a layer's pixels. The old surface stays valid for anyone (undo history) still holding it.
    pub fn set_image(&mut self, id: Uuid, surface: ImageSurface) {
        self.images.insert(id, surface);
        self.invalidate(id);
    }

    /// Shows `surface` instead of the layer's pixels until cleared; the same size as they are, ideally.
    pub fn set_preview(&mut self, id: Uuid, surface: Option<ImageSurface>) {
        match surface { Some(s) => { self.previews.insert(id, s); } None => { self.previews.remove(&id); } }
        self.preview_transforms.remove(&id);
        self.halved.retain(|(l, source, _), _| !(*l == id && *source == Source::Preview));
        self.placed.remove(&(id, true));
    }

    /// Shows `surface` in place of the layer's pixels, placed on the document by `transform` rather than the
    /// layer's own (a warp's document-size working copy, or a blur that grew the layer).
    pub fn set_preview_placed(&mut self, id: Uuid, surface: ImageSurface, transform: Transform) {
        self.set_preview(id, Some(surface));
        self.preview_transforms.insert(id, transform);
    }

    /// The layer's mask resampled onto a `width` x `height` grid placed by `grid`, the mask's edge tone
    /// beyond it (`LayerMask.clipImage` over a grown layer). None without a mask.
    pub fn mask_on_grid(&mut self, id: Uuid, grid: &Transform, width: i32, height: i32) -> Result<Option<ImageSurface>> {
        if !self.masks.contains_key(&id) { return Ok(None); }
        let placement = self.layer(id).mask_placement.unwrap_or(self.layer(id).transform);
        Ok(Some(self.place_mask(id, &placement, grid, width, height)?))
    }

    /// Starts a live preview grid for a stroke: `width` x `height` pixels placed by `transform`, holding
    /// the layer's pixels at (`ox`, `oy`). Its reduced copies are assembled from the layer's own cached
    /// halvings (the grid's origin is aligned so they line up), so even a huge layer starts at once.
    pub fn begin_preview(&mut self, id: Uuid, ox: i32, oy: i32, width: i32, height: i32, transform: Transform) -> Result<ImageSurface> {
        self.set_preview(id, None);
        let surface = new_argb(width, height)?;
        if let Some(image) = self.images.get(&id) {
            let cr = Context::new(&surface)?;
            cr.set_source_surface(image, ox as f64, oy as f64)?;
            cr.source().set_filter(Filter::Nearest);
            cr.set_operator(Operator::Source);
            cr.rectangle(ox as f64, oy as f64, image.width() as f64, image.height() as f64);
            cr.fill()?;
        }
        let (mut w, mut h) = (width, height);
        for level in 1..=crate::raster::MAX_LEVEL {
            w = (w + 1) / 2;
            h = (h + 1) / 2;
            let Some(reduced) = self.halved.get(&(id, Source::Image, level)).cloned() else { break };
            let copy = new_argb(w, h)?;
            {
                let cr = Context::new(&copy)?;
                cr.set_source_surface(&reduced, (ox >> level) as f64, (oy >> level) as f64)?;
                cr.source().set_filter(Filter::Nearest);
                cr.set_operator(Operator::Source);
                cr.rectangle((ox >> level) as f64, (oy >> level) as f64, reduced.width() as f64, reduced.height() as f64);
                cr.fill()?;
            }
            self.halved.insert((id, Source::Preview, level), copy);
        }
        self.previews.insert(id, surface.clone());
        self.preview_transforms.insert(id, transform);
        Ok(surface)
    }

    /// The preview grid's pixels changed inside `rect` (x0, y0, x1, y1): brings its reduced copies up to date.
    pub fn preview_changed(&mut self, id: Uuid, rect: (i32, i32, i32, i32)) -> Result<()> {
        let Some(mut source) = self.previews.get(&id).cloned() else { return Ok(()) };
        let mut region = rect;
        for level in 1..=crate::raster::MAX_LEVEL {
            let Some(destination) = self.halved.get(&(id, Source::Preview, level)).cloned() else { break };
            let (x0, x1) = crate::raster::halved_span(region.0, region.2);
            let (y0, y1) = crate::raster::halved_span(region.1, region.3);
            region = (x0, y0, x1, y1);
            crate::raster::halve_into(&source, &destination, region)?;
            source = destination;
        }
        Ok(())
    }

    /// Makes a stroke's preview grid the layer's pixels outright, keeping the reduced copies built during the
    /// stroke, so committing a stroke on a huge layer costs nothing beyond bookkeeping.
    pub fn adopt_preview(&mut self, id: Uuid) -> bool {
        let Some(surface) = self.previews.remove(&id) else { return false };
        self.preview_transforms.remove(&id);
        self.halved.retain(|(l, source, _), _| !(*l == id && *source == Source::Image));
        let moved: Vec<(usize, ImageSurface)> = self.halved.iter().filter(|((l, source, _), _)| *l == id && *source == Source::Preview).map(|((_, _, k), s)| (*k, s.clone())).collect();
        self.halved.retain(|(l, source, _), _| !(*l == id && *source == Source::Preview));
        for (k, s) in moved { self.halved.insert((id, Source::Image, k), s); }
        self.images.insert(id, surface);
        self.thumbnails.retain(|(l, _), _| *l != id);
        self.placed.retain(|(l, _), _| *l != id);
        true
    }

    /// Makes the part (x0, y0, x1, y1) of a stroke's preview grid the layer's pixels, cropping the reduced copies
    /// along with it; exact when the origin is a multiple of every halving in use and everything outside the
    /// crop is transparent, which is how a stroke's committed bounds are chosen.
    pub fn adopt_preview_cropped(&mut self, id: Uuid, x0: i32, y0: i32, x1: i32, y1: i32) -> Result<bool> {
        let Some(preview) = self.previews.get(&id).cloned() else { return Ok(false) };
        if x0 % 64 != 0 || y0 % 64 != 0 { return Ok(false); }
        let crop = |source: &ImageSurface, sx: i32, sy: i32, w: i32, h: i32| -> Result<ImageSurface> {
            let out = new_argb(w, h)?;
            let cr = Context::new(&out)?;
            cr.set_source_surface(source, -(sx as f64), -(sy as f64))?;
            cr.source().set_filter(Filter::Nearest);
            cr.set_operator(Operator::Source);
            cr.paint()?;
            Ok(out)
        };
        let image = crop(&preview, x0, y0, x1 - x0, y1 - y0)?;
        let mut levels = Vec::new();
        let (mut w, mut h) = (x1 - x0, y1 - y0);
        for k in 1..=crate::raster::MAX_LEVEL {
            w = (w + 1) / 2;
            h = (h + 1) / 2;
            let Some(level) = self.halved.get(&(id, Source::Preview, k)) else { break };
            if w > level.width() - (x0 >> k) || h > level.height() - (y0 >> k) { break; }
            levels.push((k, crop(level, x0 >> k, y0 >> k, w, h)?));
        }
        self.set_preview(id, None);
        self.images.insert(id, image);
        self.invalidate(id);
        for (k, s) in levels { self.halved.insert((id, Source::Image, k), s); }
        Ok(true)
    }

    /// Starts a live preview of a layer's mask for a stroke: an A8 grid of `width` x `height` holding the mask
    /// stretched to it (a uniform 1 x 1 mask covers it whole), with reduced copies from the mask's own where
    /// the grid is the mask's own size.
    pub fn begin_mask_preview(&mut self, id: Uuid, width: i32, height: i32) -> Result<ImageSurface> {
        self.end_mask_preview(id);
        let surface = crate::raster::a8_filled(width, height, 255)?;
        if let Some(mask) = self.masks.get(&id) {
            let cr = Context::new(&surface)?;
            let pattern = pattern_over(mask, (0.0, 0.0, width as f64, height as f64), Filter::Nearest);
            cr.set_source(&pattern)?;
            cr.set_operator(Operator::Source);
            cr.paint()?;
            if mask.width() == width && mask.height() == height {
                let levels: Vec<(usize, ImageSurface)> = self.halved.iter().filter(|((l, source, _), _)| *l == id && *source == Source::Mask).map(|((_, _, k), s)| (*k, s.clone())).collect();
                for (k, level) in levels {
                    let copy = ImageSurface::create(cairo::Format::A8, level.width(), level.height())?;
                    let cr = Context::new(&copy)?;
                    cr.set_source_surface(&level, 0.0, 0.0)?;
                    cr.set_operator(Operator::Source);
                    cr.paint()?;
                    drop(cr);
                    self.halved.insert((id, Source::MaskPreview, k), copy);
                }
            }
        }
        self.mask_previews.insert(id, surface.clone());
        self.placed.retain(|(l, _), _| *l != id);
        Ok(surface)
    }

    /// The mask preview's pixels changed inside `rect` (x0, y0, x1, y1).
    pub fn mask_preview_changed(&mut self, id: Uuid, rect: (i32, i32, i32, i32)) -> Result<()> {
        let Some(mut source) = self.mask_previews.get(&id).cloned() else { return Ok(()) };
        self.placed.retain(|(l, _), _| *l != id);
        let mut region = rect;
        for level in 1..=crate::raster::MAX_LEVEL {
            let Some(destination) = self.halved.get(&(id, Source::MaskPreview, level)).cloned() else { break };
            let (x0, x1) = crate::raster::halved_span(region.0, region.2);
            let (y0, y1) = crate::raster::halved_span(region.1, region.3);
            region = (x0, y0, x1, y1);
            crate::raster::halve_into(&source, &destination, region)?;
            source = destination;
        }
        Ok(())
    }

    pub fn mask_preview(&self, id: Uuid) -> Option<ImageSurface> { self.mask_previews.get(&id).cloned() }

    pub fn end_mask_preview(&mut self, id: Uuid) {
        self.mask_previews.remove(&id);
        self.halved.retain(|(l, source, _), _| !(*l == id && *source == Source::MaskPreview));
        self.placed.retain(|(l, _), _| *l != id);
    }

    /// Makes the mask preview the layer's mask, reduced copies and all.
    pub fn adopt_mask_preview(&mut self, id: Uuid) -> bool {
        let Some(surface) = self.mask_previews.remove(&id) else { return false };
        self.halved.retain(|(l, source, _), _| !(*l == id && *source == Source::Mask));
        let moved: Vec<(usize, ImageSurface)> = self.halved.iter().filter(|((l, source, _), _)| *l == id && *source == Source::MaskPreview).map(|((_, _, k), s)| (*k, s.clone())).collect();
        self.halved.retain(|(l, source, _), _| !(*l == id && *source == Source::MaskPreview));
        for (k, s) in moved { self.halved.insert((id, Source::Mask, k), s); }
        self.masks.insert(id, surface);
        self.ensure_mask_record(id);
        self.placed.retain(|(l, _), _| *l != id);
        true
    }

    fn ensure_mask_record(&mut self, id: Uuid) {
        let i = self.index[&id];
        if self.layers[i].mask_file.is_none() { self.layers[i].mask_file = Some(format!("{}.mask.png", crate::format::upper(id))); }
    }

    pub fn set_mask_enabled(&mut self, id: Uuid, enabled: bool) { let i = self.index[&id]; if self.layers[i].mask_file.is_some() { self.layers[i].mask_enabled = Some(enabled); } }
    pub fn set_mask_placement(&mut self, id: Uuid, placement: Option<Transform>) { let i = self.index[&id]; self.layers[i].mask_placement = placement; self.placed.retain(|(l, _), _| *l != id); }
    pub fn set_mask_linked(&mut self, id: Uuid, linked: bool) { let i = self.index[&id]; self.layers[i].mask_linked = Some(linked); }
    pub fn set_mask_source(&mut self, id: Uuid, source: Option<Uuid>) { let i = self.index[&id]; self.layers[i].mask_source_id = source; }

    pub fn set_layer_transform(&mut self, id: Uuid, transform: Transform) {
        let i = self.index[&id];
        self.layers[i].transform = transform;
        self.placed.retain(|(l, _), _| *l != id);
    }

    /// Replaces a layer's mask pixels (its enabled flag and placement stay as they are), or removes the mask.
    pub fn set_mask(&mut self, id: Uuid, surface: Option<ImageSurface>) {
        match surface {
            Some(s) => { self.masks.insert(id, s); self.ensure_mask_record(id); }
            None => {
                self.masks.remove(&id);
                let i = self.index[&id];
                let layer = &mut self.layers[i];
                layer.mask_file = None; layer.mask_enabled = None; layer.mask_placement = None; layer.mask_linked = None;
            }
        }
        self.invalidate(id);
    }

    pub fn snapshot(&self) -> State { State { layers: self.layers.clone(), images: self.images.clone(), masks: self.masks.clone(), width: self.width, height: self.height, resolution: self.resolution } }

    /// Puts a snapshot back, dropping cached copies of whatever changed.
    pub fn restore(&mut self, state: &State) {
        let ids: std::collections::HashSet<Uuid> = self.images.keys().chain(state.images.keys()).chain(self.masks.keys()).chain(state.masks.keys()).copied().collect();
        for id in ids {
            let same = |a: Option<&ImageSurface>, b: Option<&ImageSurface>| match (a, b) { (Some(x), Some(y)) => x.to_raw_none() == y.to_raw_none(), (None, None) => true, _ => false };
            if !same(self.images.get(&id), state.images.get(&id)) || !same(self.masks.get(&id), state.masks.get(&id)) { self.invalidate(id); }
        }
        self.layers = state.layers.clone();
        self.images = state.images.clone();
        self.masks = state.masks.clone();
        self.width = state.width;
        self.height = state.height;
        self.resolution = state.resolution;
        self.reindex();
    }

    fn reindex(&mut self) { self.index = self.layers.iter().enumerate().map(|(i, l)| (l.id, i)).collect(); }

    pub fn set_size(&mut self, width: i32, height: i32) { self.width = width; self.height = height; }
    pub fn set_resolution(&mut self, resolution: f64) { self.resolution = resolution; }

    /// Adds a layer record (and its pixels and mask) at `index` in the array, bottom to top among its siblings.
    pub fn insert_layer(&mut self, index: usize, layer: Layer, image: Option<ImageSurface>, mask: Option<ImageSurface>) {
        let id = layer.id;
        let index = index.min(self.layers.len());
        self.layers.insert(index, layer);
        if let Some(i) = image { self.images.insert(id, i); }
        if let Some(m) = mask { self.masks.insert(id, m); }
        self.reindex();
    }

    /// Removes a layer, its pixels, its mask, and the clipping links pointing at it.
    pub fn remove_layer(&mut self, id: Uuid) {
        self.layers.retain(|l| l.id != id);
        for layer in &mut self.layers { if layer.mask_source_id == Some(id) { layer.mask_source_id = None; } }
        self.images.remove(&id);
        self.masks.remove(&id);
        self.invalidate(id);
        self.reindex();
    }

    /// Swaps two records in the array (which orders siblings bottom to top).
    pub fn swap_layers(&mut self, a: usize, b: usize) {
        if a < self.layers.len() && b < self.layers.len() { self.layers.swap(a, b); self.reindex(); }
    }

    pub fn layer_index(&self, id: Uuid) -> Option<usize> { self.index.get(&id).copied() }
    pub fn set_layer_name(&mut self, id: Uuid, name: String) { let i = self.index[&id]; self.layers[i].name = name; }
    pub fn set_layer_parent(&mut self, id: Uuid, parent: Option<Uuid>) { let i = self.index[&id]; self.layers[i].parent_id = parent; }
    pub fn set_adjustment(&mut self, id: Uuid, adjustment: Option<crate::format::Adjustment>) { let i = self.index[&id]; self.layers[i].adjustment = adjustment; }
    pub fn set_sampling(&mut self, id: Uuid, sampling: Sampling) { let i = self.index[&id]; self.layers[i].transform.sampling = sampling; }

    /// The layer's mask (or full coverage without one) placed by its transform, drawn as coverage into an A8
    /// context: what Image Size resamples a mask through.
    pub fn draw_mask_plain(&mut self, id: Uuid, cr: &Context) -> Result<()> {
        let t = self.layer(id).mask_placement.unwrap_or(self.layer(id).transform);
        let Some(mask) = self.masks.get(&id).cloned() else { return Ok(()) };
        let (w, h) = (t.size.0, t.size.1);
        let device = device_scale(cr);
        let (source, level, ws, hs) = self.reduced(id, Source::Mask, w * device, t.sampling)?;
        let filter = interpolation(t.sampling, w * device / mask.width() as f64 * (1usize << level) as f64);
        cr.save()?;
        place(cr, &t);
        let rect = (-w / 2.0, -h / 2.0, w * ws, h * hs);
        let pattern = pattern_over(&source, rect, filter);
        cr.set_source(&pattern)?;
        cr.rectangle(rect.0, rect.1, rect.2, rect.3);
        cr.clip();
        cr.set_operator(Operator::Source);
        cr.paint()?;
        cr.restore()?;
        Ok(())
    }

    fn invalidate(&mut self, id: Uuid) {
        self.halved.retain(|(l, _, _), _| *l != id);
        self.thumbnails.retain(|(l, _), _| *l != id);
        self.placed.retain(|(l, _), _| *l != id);
    }

    /// The layer's own pixels placed by its transform, nothing else applied (what the wand samples when
    /// reading one layer).
    pub fn draw_layer_plain(&mut self, id: Uuid, cr: &Context) -> Result<()> {
        if !self.images.contains_key(&id) { return Ok(()); }
        let t = self.layer(id).transform;
        let iw = self.images[&id].width() as f64;
        let (w, h) = (t.size.0, t.size.1);
        let device = device_scale(cr);
        let (source, level, ws, hs) = self.reduced(id, Source::Image, w * device, t.sampling)?;
        let filter = interpolation(t.sampling, w * device / iw * (1usize << level) as f64);
        cr.save()?;
        place(cr, &t);
        cr.set_antialias(if t.sampling == Sampling::Nearest { Antialias::None } else { Antialias::Default });
        let rect = (-w / 2.0, -h / 2.0, w * ws, h * hs);
        let pattern = pattern_over(&source, rect, filter);
        cr.set_source(&pattern)?;
        cr.rectangle(rect.0, rect.1, rect.2, rect.3);
        cr.clip();
        cr.paint()?;
        cr.restore()?;
        Ok(())
    }

    pub fn set_visible(&mut self, id: Uuid, visible: bool) { let i = self.index[&id]; self.layers[i].is_visible = visible; }
    pub fn set_opacity(&mut self, id: Uuid, opacity: f64) { let i = self.index[&id]; self.layers[i].opacity = Some(opacity.clamp(0.0, 1.0)); }
    pub fn set_blend_mode(&mut self, id: Uuid, mode: BlendMode) { let i = self.index[&id]; self.layers[i].blend_mode = Some(mode); }

    /// The whole document at one pixel per document pixel.
    pub fn render_flat(&mut self) -> Result<ImageSurface> {
        let canvas = new_argb(self.width, self.height)?;
        {
            let cr = Context::new(&canvas)?;
            cr.rectangle(0.0, 0.0, self.width as f64, self.height as f64);
            cr.clip();
            self.draw(&cr)?;
        }
        Ok(canvas)
    }

    /// Draws the visible layers into `cr`, whose user space is document pixels and whose clip bounds the work.
    /// With an adjustment layer in the stack the composite is built on an offscreen copy of the clip region
    /// first (an adjustment reads back what lies beneath it), then painted, as `AdjustmentSurface` does.
    pub fn draw(&mut self, cr: &Context) -> Result<()> {
        self.live.reset();
        let visible = visible_layers(&self.layers);
        self.prepare_stacks(&visible);
        let adjusts = visible.iter().any(|id| self.layer(*id).adjustment.is_some());
        let readable = ImageSurface::try_from(cr.target()).is_ok();
        if adjusts && !readable {
            let Some(region) = Region::of(cr)? else { return Ok(()) };
            let (surface, inner) = region.offscreen(cr)?;
            inner.rectangle(region.x as f64, region.y as f64, region.width as f64, region.height as f64);
            for id in visible {
                let folders = self.folder_masks(id);
                self.draw_composite(id, &inner, &folders)?;
            }
            drop(inner);
            cr.save()?;
            cr.identity_matrix();
            cr.set_source_surface(&surface, region.x as f64, region.y as f64)?;
            cr.paint()?;
            cr.restore()?;
            return Ok(());
        }
        for id in visible {
            let folders = self.folder_masks(id);
            self.draw_composite(id, cr, &folders)?;
        }
        Ok(())
    }

    /// An adjustment layer: everything drawn so far in the context's clip region, adjusted, put back through
    /// the folders' masks and the layer's own mask, at its opacity and blend mode (`LiveMaskRenderer.adjust`).
    pub(crate) fn adjust(&mut self, id: Uuid, cr: &Context, folders: &[FolderMask]) -> Result<()> {
        let layer = self.layer(id).clone();
        let Some(record) = &layer.adjustment else { return Ok(()) };
        let Some(adjustment) = crate::filters::Adjustment::from_record(record) else { return Ok(()) };
        let Ok(target) = ImageSurface::try_from(cr.target()) else { return Ok(()) };
        let Some(region) = Region::of(cr)? else { return Ok(()) };
        cr.target().flush();
        let (w, h) = (region.width as usize, region.height as usize);
        let (tw, th) = (target.width() as i32, target.height() as i32);
        if region.x < 0 || region.y < 0 || region.x + region.width > tw || region.y + region.height > th { return Ok(()); }
        let mut original = vec![0u8; w * h * 4];
        with_bytes(&target, |data, stride| {
            for r in 0..h { original[r * w * 4..(r + 1) * w * 4].copy_from_slice(&data[(region.y as usize + r) * stride + region.x as usize * 4..(region.y as usize + r) * stride + (region.x as usize + w) * 4]); }
        })?;
        let mut adjusted = original.clone();
        // Where this region sits on the document, so Grain's pattern stays put.
        let (ox, oy) = cr.device_to_user(region.x as f64, region.y as f64)?;
        let units = 1.0 / device_scale(cr).max(1e-9);
        adjustment.apply(&mut adjusted, w, h, (ox, oy), units);
        let mode = layer.blend_mode();
        if mode != BlendMode::Normal {
            // Blend colors at full coverage, then restore the original alpha, so soft edges don't thicken.
            let mut alpha = vec![0u8; w * h];
            crate::ffi::extract_alpha(&original, w * 4, &mut alpha, w, w, h);
            let mut base = original.clone();
            crate::ffi::unpremultiply_opaque(&mut base, w * 4, w, h);
            crate::ffi::unpremultiply_opaque(&mut adjusted, w * 4, w, h);
            let base_surface = crate::raster::argb_from_packed(w as i32, h as i32, base)?;
            let top = crate::raster::argb_from_packed(w as i32, h as i32, adjusted)?;
            {
                let bcr = Context::new(&base_surface)?;
                bcr.set_source_surface(&top, 0.0, 0.0)?;
                bcr.set_operator(operator(mode));
                bcr.paint()?;
            }
            adjusted = with_bytes(&base_surface, |data, stride| { let mut out = vec![0u8; w * h * 4]; for r in 0..h { out[r * w * 4..(r + 1) * w * 4].copy_from_slice(&data[r * stride..r * stride + w * 4]); } out })?;
            crate::ffi::restore_alpha(&mut adjusted, w * 4, &alpha, w, w, h);
        }
        let opacity = layer.opacity();
        if opacity < 1.0 {
            for (a, o) in adjusted.iter_mut().zip(&original) { *a = (*a as f64 * opacity + *o as f64 * (1.0 - opacity)).round() as u8; }
        }
        let result = crate::raster::argb_from_packed(w as i32, h as i32, adjusted)?;
        // Coverage: the folders' masks times the layer's own, as an A8 over the region.
        let coverage = crate::raster::a8_filled(region.width, region.height, 255)?;
        {
            let ccr = Context::new(&coverage)?;
            ccr.set_matrix(Matrix::multiply(&cr.matrix(), &Matrix::new(1.0, 0.0, 0.0, 1.0, -region.x as f64, -region.y as f64)));
            let mut masks: Vec<(ImageSurface, Transform)> = folders.iter().map(|f| (f.mask.clone(), f.transform)).collect();
            if layer.mask_enabled() { if let Some(m) = self.masks.get(&id) { masks.push((m.clone(), layer.transform)); } }
            for (mask, t) in masks {
                ccr.save()?;
                place(&ccr, &t);
                let rect = (-t.size.0 / 2.0, -t.size.1 / 2.0, t.size.0, t.size.1);
                let pattern = pattern_over(&mask, rect, quality(t.sampling));
                ccr.set_source(&pattern)?;
                ccr.set_operator(Operator::In);
                ccr.paint()?;
                ccr.restore()?;
            }
        }
        cr.save()?;
        cr.identity_matrix();
        cr.set_source_surface(&result, region.x as f64, region.y as f64)?;
        cr.set_operator(Operator::Source);
        cr.mask_surface(&coverage, region.x as f64, region.y as f64)?;
        cr.restore()?;
        Ok(())
    }

    /// The enabled masks of every folder containing `id`, nearest folder first.
    fn folder_masks(&self, id: Uuid) -> Vec<FolderMask> {
        let mut result = Vec::new();
        let mut folder = self.parent(id);
        let mut depth = 0;
        while let (Some(current), true) = (folder, depth < 64) {
            let layer = self.layer(current);
            if layer.mask_enabled() {
                if let Some(mask) = self.masks.get(&current) {
                    result.push(FolderMask { mask: mask.clone(), transform: layer.transform });
                }
            }
            folder = layer.parent_id;
            depth += 1;
        }
        result
    }

    /// Draws one layer: the image placed by its transform through its own mask, then through every alpha
    /// mask in `folders` and `clip` (a device-space coverage), composited with its opacity and blend mode.
    /// `LayerRenderer.draw` plus the clips `FolderMaskClip` and `LiveMaskRenderer.draw` put around it.
    pub(crate) fn draw_own(&mut self, id: Uuid, cr: &Context, clip: Option<(&ImageSurface, Region)>, folders: &[FolderMask]) -> Result<()> {
        let kind = if self.previews.contains_key(&id) { Source::Preview } else { Source::Image };
        let Some(image) = self.store(kind).get(&id).cloned() else { return Ok(()) };
        let layer = self.layer(id).clone();
        let t = self.preview_transforms.get(&id).copied().unwrap_or(layer.transform);
        let iw = image.width() as f64;
        let (w, h) = (t.size.0, t.size.1);
        let mask = self.mask_for(&layer, &t, image.width(), image.height(), kind == Source::Preview)?;
        drop(image);
        // Large reductions draw from sharp halvings; Cairo then only does the last 2x or less.
        let device = device_scale(cr);
        let (source, level, ws, hs) = self.reduced(id, kind, w * device, t.sampling)?;
        let filter = interpolation(t.sampling, w * device / iw * (1usize << level) as f64);
        let clip_mask = match mask {
            Some(MaskSource::Own) => { let source = self.mask_source(id); Some(self.reduced(id, source, w * device, t.sampling)?) }
            Some(MaskSource::Placed(surface)) => Some((surface, 0, 1.0, 1.0)),
            None => None,
        };
        cr.save()?;
        cr.push_group();
        {
            cr.save()?;
            place(cr, &t);
            cr.set_antialias(if t.sampling == Sampling::Nearest { Antialias::None } else { Antialias::Default });
            let rect = (-w / 2.0, -h / 2.0, w * ws, h * hs);
            let pattern = pattern_over(&source, rect, filter);
            cr.set_source(&pattern)?;
            cr.rectangle(rect.0, rect.1, rect.2, rect.3);
            cr.clip();
            match clip_mask {
                Some((mask, _, mws, mhs)) => {
                    let mrect = (-w / 2.0, -h / 2.0, w * mws, h * mhs);
                    let mpattern = pattern_over(&mask, mrect, filter);
                    cr.rectangle(mrect.0, mrect.1, mrect.2, mrect.3);
                    cr.clip();
                    cr.mask(&mpattern)?;
                }
                None => cr.paint()?,
            }
            cr.restore()?;
        }
        cr.pop_group_to_source()?;
        self.through_masks(cr, folders, clip)?;
        cr.set_operator(operator(layer.blend_mode()));
        cr.paint_with_alpha(layer.opacity())?;
        cr.restore()?;
        Ok(())
    }

    /// Multiplies the current source's alpha by each folder mask (placed on the document by its folder's
    /// transform) and by `clip`, a coverage over a device-space region. The source is locked to the user
    /// space it was set in, so changing the matrix here leaves it where it is.
    pub(crate) fn through_masks(&self, cr: &Context, folders: &[FolderMask], clip: Option<(&ImageSurface, Region)>) -> Result<()> {
        for folder in folders {
            cr.push_group();
            cr.save()?;
            let t = &folder.transform;
            place(cr, t);
            let rect = (-t.size.0 / 2.0, -t.size.1 / 2.0, t.size.0, t.size.1);
            let pattern = pattern_over(&folder.mask, rect, quality(t.sampling));
            cr.rectangle(rect.0, rect.1, rect.2, rect.3);
            cr.clip();
            cr.mask(&pattern)?;
            cr.restore()?;
            cr.pop_group_to_source()?;
        }
        if let Some((coverage, region)) = clip {
            let matrix = cr.matrix();
            cr.identity_matrix();
            cr.push_group();
            cr.mask_surface(coverage, region.x as f64, region.y as f64)?;
            cr.pop_group_to_source()?;
            cr.set_matrix(matrix);
        }
        Ok(())
    }

    fn store(&self, source: Source) -> &HashMap<Uuid, ImageSurface> {
        match source { Source::Image => &self.images, Source::Mask => &self.masks, Source::Preview => &self.previews, Source::MaskPreview => &self.mask_previews }
    }
    fn store_mut(&mut self, source: Source) -> &mut HashMap<Uuid, ImageSurface> {
        match source { Source::Image => &mut self.images, Source::Mask => &mut self.masks, Source::Preview => &mut self.previews, Source::MaskPreview => &mut self.mask_previews }
    }
    /// The mask pixels to draw a layer through right now: a stroke's preview when one is painting the mask.
    fn mask_source(&self, id: Uuid) -> Source { if self.mask_previews.contains_key(&id) { Source::MaskPreview } else { Source::Mask } }

    /// A layer's pixels (image, mask or preview) reduced for landing `device_width` device pixels wide: the
    /// surface, how many halvings it had, and how far past the layer's bounds it reaches (halvings round up).
    fn reduced(&mut self, id: Uuid, source: Source, device_width: f64, sampling: Sampling) -> Result<(ImageSurface, usize, f64, f64)> {
        let base = &self.store(source)[&id];
        let (bw, bh) = (base.width(), base.height());
        if sampling == Sampling::Nearest { return Ok((base.clone(), 0, 1.0, 1.0)); }
        let wanted = level_for(device_width / bw.max(1) as f64);
        let (surface, level) = self.halved_copy(id, source, wanted)?;
        let ws = ((surface.width() as usize) << level) as f64 / bw.max(1) as f64;
        let hs = ((surface.height() as usize) << level) as f64 / bh.max(1) as f64;
        Ok((surface, level, ws, hs))
    }

    /// Up to `wanted` halvings of a layer's pixels, built on demand and kept.
    pub(crate) fn halved_copy(&mut self, id: Uuid, source: Source, wanted: usize) -> Result<(ImageSurface, usize)> {
        let base = self.store(source)[&id].clone();
        if wanted == 0 || (base.width() <= 1 && base.height() <= 1) { return Ok((base, 0)); }
        drop(base);
        let mut applied = 0;
        for level in 1..=wanted {
            if self.halved.contains_key(&(id, source, level)) { applied = level; continue; }
            let next = {
                let previous = if level == 1 { self.store_mut(source).get_mut(&id).unwrap() } else { self.halved.get_mut(&(id, source, level - 1)).unwrap() };
                if previous.width() <= 1 && previous.height() <= 1 { break; }
                halve(previous).with_context(|| format!("halving layer {id}"))?
            };
            self.halved.insert((id, source, level), next);
            applied = level;
        }
        if applied == 0 { return Ok((self.store(source)[&id].clone(), 0)); }
        Ok((self.halved[&(id, source, applied)].clone(), applied))
    }

    /// A layer's pixels fitted inside `size` x `size` (masks and placement not applied), for the layer
    /// panel; nil for folders and blank layers. Built from the same halvings the canvas draws from.
    pub fn thumbnail(&mut self, id: Uuid, size: i32) -> Result<Option<ImageSurface>> {
        if let Some(existing) = self.thumbnails.get(&(id, size)) { return Ok(Some(existing.clone())); }
        let Some((iw, ih)) = self.image_size(id) else { return Ok(None) };
        let scale = (size as f64 / iw.max(ih) as f64).min(1.0);
        let (w, h) = (((iw as f64 * scale).round() as i32).max(1), ((ih as f64 * scale).round() as i32).max(1));
        let (source, level) = self.halved_copy(id, Source::Image, level_for(scale))?;
        let surface = new_argb(w, h)?;
        {
            let cr = Context::new(&surface)?;
            let full = ((source.width() as usize) << level) as f64 / iw as f64;
            let fullh = ((source.height() as usize) << level) as f64 / ih as f64;
            let pattern = pattern_over(&source, (0.0, 0.0, w as f64 * full, h as f64 * fullh), Filter::Bilinear);
            cr.set_source(&pattern)?;
            cr.paint()?;
        }
        self.thumbnails.insert((id, size), surface.clone());
        Ok(Some(surface))
    }

    /// A layer's mask fitted inside `size` x `size` as A8, for the layer panel; None without a mask.
    pub fn mask_thumbnail(&mut self, id: Uuid, size: i32) -> Result<Option<ImageSurface>> {
        let Some(mask) = self.masks.get(&id).cloned() else { return Ok(None) };
        let (mw, mh) = (mask.width(), mask.height());
        // Fit the mask to the layer's own proportions, since a 1 x 1 mask covers the whole layer.
        let (pw, ph) = self.image_size(id).unwrap_or((mw.max(mh), mw.max(mh)));
        let scale = (size as f64 / pw.max(ph) as f64).min(1.0);
        let (w, h) = (((pw as f64 * scale).round() as i32).max(1), ((ph as f64 * scale).round() as i32).max(1));
        let level = level_for(w as f64 / mw.max(1) as f64);
        let (source, _) = self.halved_copy(id, Source::Mask, level)?;
        let surface = ImageSurface::create(cairo::Format::A8, w, h)?;
        {
            let cr = Context::new(&surface)?;
            let pattern = pattern_over(&source, (0.0, 0.0, w as f64, h as f64), Filter::Bilinear);
            cr.set_source(&pattern)?;
            cr.set_operator(Operator::Source);
            cr.paint()?;
        }
        Ok(Some(surface))
    }

    /// The mask a layer's pixels are drawn through, as `LayerMask.clipImage` gives it: the mask itself while
    /// it covers the grid being drawn (`grid`, `width` x `height`), or resampled into that grid when it sits
    /// elsewhere (moved apart from the layer, or the grid is a stroke's larger one). Nil while disabled or absent.
    fn mask_for(&mut self, layer: &Layer, grid: &Transform, width: i32, height: i32, preview: bool) -> Result<Option<MaskSource>> {
        if !layer.mask_enabled() || !self.masks.contains_key(&layer.id) { return Ok(None); }
        let placement = layer.mask_placement.unwrap_or(layer.transform);
        if placement.same_placement(grid) || width <= 0 || height <= 0 { return Ok(Some(MaskSource::Own)); }
        if let Some(existing) = self.placed.get(&(layer.id, preview)) { return Ok(Some(MaskSource::Placed(existing.clone()))); }
        let placed = self.place_mask(layer.id, &placement, grid, width, height)?;
        self.placed.insert((layer.id, preview), placed.clone());
        Ok(Some(MaskSource::Placed(placed)))
    }

    /// `LayerMask.placed`: a `width` x `height` gray grid placed by `grid`, holding the mask drawn where
    /// `placement` puts it on the document, and the mask's background color everywhere else.
    fn place_mask(&mut self, id: Uuid, placement: &Transform, grid: &Transform, width: i32, height: i32) -> Result<ImageSurface> {
        let kind = self.mask_source(id);
        let (mw, mh) = { let m = &self.store(kind)[&id]; (m.width(), m.height()) };
        let background = self.mask_background(id)?;
        // Drawn from a sharp halving near the size the mask covers in the grid.
        let covered = placement.size.0 / grid.size.0.max(1.0) * width as f64;
        let level = level_for(covered / mw.max(1) as f64);
        let (source, _) = self.halved_copy(id, kind, level)?;
        let surface = crate::raster::a8_filled(width, height, background)?;
        {
            let cr = Context::new(&surface)?;
            let to_document = pixel_to_document(placement, mw, mh);
            let from_document = pixel_to_document(grid, width, height).try_invert()?;
            cr.transform(Matrix::multiply(&to_document, &from_document));
            let rect = (0.0, 0.0, mw as f64, mh as f64);
            cr.rectangle(rect.0, rect.1, rect.2, rect.3);
            cr.clip();
            let pattern = pattern_over(&source, rect, Filter::Best);
            cr.set_source(&pattern)?;
            cr.set_operator(Operator::Source);
            cr.paint()?;
        }
        Ok(surface)
    }

    /// What a mask shows beyond its pixels once placed apart from its layer: white or black, whichever most
    /// of its edge is. The reference reads a 96-pixel thumbnail's border; this averages the same band at full size.
    fn mask_background(&mut self, id: Uuid) -> Result<u8> {
        let mask = self.masks.get_mut(&id).unwrap();
        let (w, h) = (mask.width() as usize, mask.height() as usize);
        let band = w.max(h).div_ceil(96);
        let bright = with_bytes(mask, |data, stride| {
            let (mut total, mut count) = (0u64, 0u64);
            for y in 0..h {
                for x in 0..w {
                    if y < band || y + band >= h || x < band || x + band >= w {
                        total += data[y * stride + x] as u64;
                        count += 1;
                    }
                }
            }
            total * 2 >= count * 255
        })?;
        Ok(if bright { 255 } else { 0 })
    }

    /// Layers as the panel lists them, top to bottom, with depth and effective visibility.
    pub fn rows(&self) -> Vec<(Uuid, usize, bool)> {
        entries_ordered(&self.layers, true).into_iter().map(|e| (e.layer.id, e.depth, e.visible)).collect()
    }
}

enum MaskSource {
    Own,
    Placed(ImageSurface),
}

/// Device pixels per user unit along the context's x axis.
pub(crate) fn device_scale(cr: &Context) -> f64 {
    let m = cr.matrix();
    (m.xx() * m.xx() + m.yx() * m.yx()).sqrt()
}

/// Moves the context to a layer's center, rotated and flipped as its transform says; the layer's unrotated
/// box is then centered on the origin.
fn place(cr: &Context, t: &Transform) {
    let c = t.center();
    cr.translate(c.0, c.1);
    cr.rotate(t.radians());
    cr.scale(if t.flip_x { -1.0 } else { 1.0 }, if t.flip_y { -1.0 } else { 1.0 });
}

/// `BrushRaster.pixelToDocument`: a layer's `width` x `height` pixel grid mapped onto the document.
pub(crate) fn pixel_to_document(t: &Transform, width: i32, height: i32) -> Matrix {
    let mut m = Matrix::identity();
    let c = t.center();
    m.translate(c.0, c.1);
    m.rotate(t.radians());
    m.scale(t.size.0 / width as f64 * if t.flip_x { -1.0 } else { 1.0 },
            t.size.1 / height as f64 * if t.flip_y { -1.0 } else { 1.0 });
    m.translate(-(width as f64) / 2.0, -(height as f64) / 2.0);
    m
}

/// A pattern drawing `surface` stretched over `rect` (x, y, width, height) in the current user space.
fn pattern_over(surface: &ImageSurface, rect: (f64, f64, f64, f64), filter: Filter) -> SurfacePattern {
    let pattern = SurfacePattern::create(surface);
    let sx = surface.width() as f64 / rect.2;
    let sy = surface.height() as f64 / rect.3;
    pattern.set_matrix(Matrix::new(sx, 0.0, 0.0, sy, -rect.0 * sx, -rect.1 * sy));
    pattern.set_filter(filter);
    pattern.set_extend(Extend::Pad);
    pattern
}

/// Cairo's filter for the last resample, `final_factor` output pixels per (reduced) image pixel. Shrinking
/// uses bilinear, since the sharp halvings have already done the heavy reduction; enlarging keeps the
/// layer's own setting. `LayerRenderer.interpolation`.
fn interpolation(sampling: Sampling, final_factor: f64) -> Filter {
    if sampling == Sampling::Nearest { Filter::Nearest } else if final_factor <= 1.0 { Filter::Bilinear } else { quality(sampling) }
}

fn quality(sampling: Sampling) -> Filter {
    match sampling { Sampling::Nearest => Filter::Nearest, Sampling::Smooth => Filter::Bilinear, Sampling::High => Filter::Best }
}

pub(crate) fn operator(mode: BlendMode) -> Operator {
    match mode {
        BlendMode::Normal => Operator::Over,
        BlendMode::Multiply => Operator::Multiply,
        BlendMode::Screen => Operator::Screen,
        BlendMode::Overlay => Operator::Overlay,
        BlendMode::Darken => Operator::Darken,
        BlendMode::Lighten => Operator::Lighten,
        BlendMode::Difference => Operator::Difference,
        BlendMode::ColorDodge => Operator::ColorDodge,
        BlendMode::ColorBurn => Operator::ColorBurn,
        BlendMode::Hue => Operator::HslHue,
        BlendMode::Saturation => Operator::HslSaturation,
        BlendMode::Color => Operator::HslColor,
        BlendMode::Luminosity => Operator::HslLuminosity,
    }
}
