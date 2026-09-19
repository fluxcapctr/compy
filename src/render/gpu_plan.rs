//! Turns the renderer's state into a GPU plan that composites the way `draw` does. Returns None for
//! anything the GPU path does not express yet (clips to a layer that is not the sibling below, Grain and
//! Hue/Saturation adjustment layers, surfaces past the GPU's texture size), and the CPU draws that frame.

use super::{Renderer, Source};
use crate::format::{BlendMode, Layer, Transform, visible_layers};
use crate::gpu::{AdjustDraw, Affine, EffectsDraw, Item, LayerDraw, MaskDraw, Placed, Plan, StackChild, SurfaceKey};
use anyhow::Result;
use cairo::{ImageSurface, Matrix};
use uuid::Uuid;

fn blend_id(mode: BlendMode) -> u32 {
    match mode {
        BlendMode::Normal => 0, BlendMode::Multiply => 1, BlendMode::Screen => 2, BlendMode::Overlay => 3, BlendMode::Darken => 4, BlendMode::Lighten => 5,
        BlendMode::Difference => 6, BlendMode::ColorDodge => 7, BlendMode::ColorBurn => 8, BlendMode::Hue => 9, BlendMode::Saturation => 10, BlendMode::Color => 11, BlendMode::Luminosity => 12,
    }
}

fn slot(source: Source) -> u8 { match source { Source::Image => 0, Source::Mask => 1, Source::Preview => 2, Source::MaskPreview => 3 } }

impl Renderer {
    /// A plan for the viewport whose device pixel centers map to document points by `device_to_document`,
    /// `width` x `height` device pixels; `max_dimension` is the GPU's texture limit.
    pub fn gpu_plan(&mut self, device_to_document: Matrix, width: u32, height: u32, max_dimension: u32) -> Result<Option<Plan>> {
        self.live.reset();
        let visible = visible_layers(&self.layers);
        self.prepare_stacks(&visible);
        let too_big = |s: &ImageSurface| s.width() as u32 > max_dimension || s.height() as u32 > max_dimension;
        for id in &visible {
            for store in [&self.images, &self.masks, &self.previews, &self.mask_previews] { if store.get(id).is_some_and(too_big) { return Ok(None); } }
        }
        let mut items = Vec::new();
        for id in visible {
            if self.live.stacked.contains(&id) { continue; }
            let layer = self.layer(id).clone();
            if layer.adjustment.is_some() {
                if self.source(id).is_some() { continue; }
                let folders = self.folder_mask_draws(id, &device_to_document);
                match self.adjust_draw(&layer, folders, &device_to_document) { Some(a) => items.push(Item::Adjust(a)), None => return Ok(None) }
                continue;
            }
            if let Some(children) = self.live.stacks.get(&id).cloned() {
                let Some(base) = self.layer_draw(&layer, Vec::new(), &device_to_document)? else { return Ok(None) };
                let mut drawn = Vec::new();
                for child in children {
                    let cl = self.layer(child).clone();
                    if cl.adjustment.is_some() {
                        match self.adjust_draw(&cl, Vec::new(), &device_to_document) { Some(a) => drawn.push(StackChild::Adjust(a)), None => return Ok(None) }
                    } else if let Some(l) = self.layer_draw(&cl, Vec::new(), &device_to_document)? { drawn.push(StackChild::Layer(l)); }
                }
                let folders = self.folder_mask_draws(id, &device_to_document);
                items.push(Item::Stack { base, children: drawn, folders, blend: blend_id(layer.blend_mode()) });
                continue;
            }
            // A clip to something other than the sibling below runs through live coverage on the CPU.
            if self.source(id).is_some() { return Ok(None); }
            let folders = self.folder_mask_draws(id, &device_to_document);
            if let Some(l) = self.layer_draw(&layer, folders, &device_to_document)? { items.push(Item::Layer(l)); }
        }
        self.preview_dirty.clear();
        self.mask_preview_dirty.clear();
        Ok(Some(Plan { items, width, height }))
    }

    fn placed(&self, id: Uuid, source: Source, surface: &ImageSurface, grid: &Transform, device_to_document: &Matrix, nearest: bool, dirty: Option<(i32, i32, i32, i32)>) -> Result<Placed> {
        let to_layer = crate::selection::document_to_layer(grid, surface.width(), surface.height())?;
        let to_texel = Matrix::multiply(device_to_document, &to_layer);
        Ok(Placed { key: SurfaceKey { id, slot: slot(source), ptr: surface.to_raw_none() as usize }, surface: surface.clone(), to_texel: Affine::from_cairo(&to_texel), nearest, dirty })
    }

    fn layer_draw(&mut self, layer: &Layer, folders: Vec<MaskDraw>, device_to_document: &Matrix) -> Result<Option<LayerDraw>> {
        let id = layer.id;
        let kind = if self.previews.contains_key(&id) { Source::Preview } else { Source::Image };
        let Some(image) = self.store(kind).get(&id).cloned() else { return Ok(None) };
        let t = self.preview_transforms.get(&id).copied().unwrap_or(layer.transform);
        let nearest = t.sampling == crate::format::Sampling::Nearest;
        let dirty = if kind == Source::Preview { self.preview_dirty.get(&id).copied() } else { None };
        let placed = self.placed(id, kind, &image, &t, device_to_document, nearest, dirty)?;
        let mask = self.own_mask_draw(layer, &t, device_to_document, nearest)?;
        let effects = match self.styled(id, layer)? {
            Some(s) => {
                let place = |surface: &Option<ImageSurface>, slot: u8| -> Result<Option<Placed>> {
                    let Some(surface) = surface else { return Ok(None) };
                    let mut to_texel = *device_to_document;
                    to_texel = Matrix::multiply(&to_texel, &Matrix::new(1.0, 0.0, 0.0, 1.0, -s.x, -s.y));
                    Ok(Some(Placed { key: SurfaceKey { id, slot, ptr: surface.to_raw_none() as usize }, surface: surface.clone(), to_texel: Affine::from_cairo(&to_texel), nearest: false, dirty: None }))
                };
                Some(EffectsDraw { below: place(&s.below, 4)?, inside: place(&s.inside, 5)?, above: place(&s.above, 6)? })
            }
            None => None,
        };
        Ok(Some(LayerDraw { image: placed, mask, folders, opacity: layer.opacity() as f32, blend: blend_id(layer.blend_mode()), effects }))
    }

    /// The layer's own mask through its placement (`mask_for`): on the layer's grid it covers nothing
    /// beyond its rectangle; placed elsewhere, its background tone shows beyond it.
    fn own_mask_draw(&mut self, layer: &Layer, grid: &Transform, device_to_document: &Matrix, nearest: bool) -> Result<Option<MaskDraw>> {
        let id = layer.id;
        if !layer.mask_enabled() || !self.masks.contains_key(&id) { return Ok(None); }
        let kind = self.mask_source(id);
        let placement = if kind == Source::MaskPreview { self.mask_preview_placements.get(&id).copied().or(layer.mask_placement).unwrap_or(layer.transform) } else { layer.mask_placement.unwrap_or(layer.transform) };
        let surface = self.store(kind)[&id].clone();
        let outside = if placement.same_placement(grid) { 0.0 } else { self.mask_background(id)? as f32 / 255.0 };
        let dirty = if kind == Source::MaskPreview { self.mask_preview_dirty.get(&id).copied() } else { None };
        Ok(Some(MaskDraw { placed: self.placed(id, kind, &surface, &placement, device_to_document, nearest, dirty)?, outside }))
    }

    fn folder_mask_draws(&self, id: Uuid, device_to_document: &Matrix) -> Vec<MaskDraw> {
        self.folder_masks(id).into_iter().filter_map(|f| {
            let surface = self.masks.get(&f.id)?.clone();
            let nearest = f.transform.sampling == crate::format::Sampling::Nearest;
            self.placed(f.id, Source::Mask, &surface, &f.transform, device_to_document, nearest, None).ok().map(|placed| MaskDraw { placed, outside: 0.0 })
        }).collect()
    }

    fn adjust_draw(&mut self, layer: &Layer, folders: Vec<MaskDraw>, device_to_document: &Matrix) -> Option<AdjustDraw> {
        let record = layer.adjustment.as_ref()?;
        let adjustment = crate::filters::Adjustment::from_record(record)?;
        let (lut, by_luminance) = match &adjustment {
            crate::filters::Adjustment::Levels(l) => { if l.is_identity() { return Some(self.no_adjust(layer, folders, device_to_document)); } let mut t = vec![0u8; 1024]; for v in 0..256 { for c in 0..3 { t[v * 4 + c] = (l.apply(v as f64 / 255.0, c + 1) * 255.0).round().clamp(0.0, 255.0) as u8; } t[v * 4 + 3] = 255; } (t, false) }
            crate::filters::Adjustment::Curves(cv) => { if cv.is_identity() { return Some(self.no_adjust(layer, folders, device_to_document)); } let mut t = vec![0u8; 1024]; for v in 0..256 { for c in 0..3 { t[v * 4 + c] = cv.value(cv.value(v as f64, c + 1), 0).round().clamp(0.0, 255.0) as u8; } t[v * 4 + 3] = 255; } (t, false) }
            crate::filters::Adjustment::Exposure(e) => { if e.is_identity() { return Some(self.no_adjust(layer, folders, device_to_document)); } let table = e.table(); let mut t = vec![0u8; 1024]; for v in 0..256 { let o = (table[v] * 255.0).round().clamp(0.0, 255.0) as u8; t[v * 4] = o; t[v * 4 + 1] = o; t[v * 4 + 2] = o; t[v * 4 + 3] = 255; } (t, false) }
            crate::filters::Adjustment::GradientMap(g) => { let table = g.table(); let mut t = vec![0u8; 1024]; for v in 0..256 { t[v * 4] = table[v * 3]; t[v * 4 + 1] = table[v * 3 + 1]; t[v * 4 + 2] = table[v * 3 + 2]; t[v * 4 + 3] = 255; } (t, true) }
            _ => return None,
        };
        let mask = self.adjust_mask(layer, device_to_document);
        Some(AdjustDraw { lut, by_luminance, mask, folders, opacity: layer.opacity() as f32, blend: blend_id(layer.blend_mode()) })
    }

    /// An identity adjustment: a lookup that changes nothing, at opacity zero.
    fn no_adjust(&self, layer: &Layer, folders: Vec<MaskDraw>, device_to_document: &Matrix) -> AdjustDraw {
        let mut t = vec![0u8; 1024];
        for v in 0..256 { t[v * 4] = v as u8; t[v * 4 + 1] = v as u8; t[v * 4 + 2] = v as u8; t[v * 4 + 3] = 255; }
        AdjustDraw { lut: t, by_luminance: false, mask: self.adjust_mask(layer, device_to_document), folders, opacity: 0.0, blend: 0 }
    }

    fn adjust_mask(&self, layer: &Layer, device_to_document: &Matrix) -> Option<MaskDraw> {
        if !layer.mask_enabled() { return None; }
        let surface = self.masks.get(&layer.id)?.clone();
        let placement = layer.mask_placement.unwrap_or(layer.transform);
        let nearest = placement.sampling == crate::format::Sampling::Nearest;
        self.placed(layer.id, Source::Mask, &surface, &placement, device_to_document, nearest, None).ok().map(|placed| MaskDraw { placed, outside: 0.0 })
    }
}
