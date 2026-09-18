//! An open project being edited: the renderer's layers and pixels, the selection, and undo history over
//! both. Every edit runs between `begin_edit` and `end_edit`, as `EditorSession` does, so it lands as one
//! undo step named for the menu item.

use crate::filters::{self, Kind, Settings};
use crate::format::{Project, Transform, live_mask_graph};
use crate::history::History;
use crate::raster::{new_argb, with_bytes};
use crate::render::{self, Renderer};
use crate::selection::{Mode, Selection};
use anyhow::{Result, bail};
use cairo::{Context, ImageSurface};
use uuid::Uuid;

pub struct Document {
    pub renderer: Renderer,
    pub selection: Option<Selection>,
    /// The active layer, which edits apply to.
    pub active: Option<Uuid>,
    history: History<State>,
    stroke: Option<crate::brush::Stroke>,
    stroke_mask: bool,
    mask_target: bool,
    pub document_id: Uuid,
    pub path: Option<std::path::PathBuf>,
}

#[derive(Clone)]
pub struct State {
    render: render::State,
    selection: Option<Selection>,
    active: Option<Uuid>,
}

impl PartialEq for State {
    fn eq(&self, other: &Self) -> bool { self.render == other.render && self.selection == other.selection && self.active == other.active }
}

/// The Magic Wand's options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WandSettings {
    /// How far (0 to 255) each channel may differ from the sampled color and still be selected.
    pub tolerance: i32,
    /// Pixels either side of the click averaged into the color to match: 0, 1 (3 by 3) or 2 (5 by 5).
    pub sample_radius: usize,
    /// Only similar pixels connected to the clicked one, rather than every similar pixel.
    pub contiguous: bool,
    /// Read the visible composite rather than just the active layer.
    pub sample_all_layers: bool,
}

impl Default for WandSettings {
    fn default() -> Self { WandSettings { tolerance: 32, sample_radius: 0, contiguous: true, sample_all_layers: false } }
}

impl Document {
    pub fn new(project: Project) -> Result<Document> {
        // A file that names no active layer opens on its topmost image layer.
        let active = project.manifest.active_layer_id.or_else(|| {
            crate::format::entries_ordered(&project.manifest.layers, true).into_iter().find(|e| !e.layer.is_group() && e.layer.adjustment.is_none()).map(|e| e.layer.id)
        });
        let document_id = project.manifest.document_id;
        let path = if project.path.as_os_str().is_empty() { None } else { Some(project.path.clone()) };
        let renderer = Renderer::new(project)?;
        Ok(Document { renderer, selection: None, active, history: History::new(100, 256 * 1024 * 1024), stroke: None, stroke_mask: false, mask_target: false, document_id, path })
    }

    pub fn width(&self) -> i32 { self.renderer.width() }
    pub fn height(&self) -> i32 { self.renderer.height() }

    fn state(&self) -> State { State { render: self.renderer.snapshot(), selection: self.selection.clone(), active: self.active } }
    fn apply(&mut self, state: &State) {
        self.renderer.restore(&state.render);
        self.selection = state.selection.clone();
        self.active = state.active;
    }

    pub fn begin_edit(&mut self, name: &str) { let state = self.state(); self.history.begin(name, state); }
    pub fn end_edit(&mut self) {
        let state = self.state();
        self.history.end(state, |held, current| {
            let renders: Vec<&render::State> = held.iter().map(|s| &s.render).collect();
            let mut bytes = render::State::retained_bytes(&renders, &current.render);
            let current_mask = current.selection.as_ref().map(|s| s.mask.to_raw_none() as usize);
            let mut seen = std::collections::HashSet::new();
            for state in held {
                if let Some(sel) = &state.selection {
                    let ptr = sel.mask.to_raw_none() as usize;
                    if Some(ptr) != current_mask && seen.insert(ptr) { bytes += sel.mask.stride() as usize * sel.mask.height() as usize; }
                }
            }
            bytes
        });
    }
    pub fn can_undo(&self) -> bool { self.history.can_undo() }
    pub fn can_redo(&self) -> bool { self.history.can_redo() }
    pub fn undo_name(&self) -> Option<&str> { self.history.undo_name() }
    pub fn redo_name(&self) -> Option<&str> { self.history.redo_name() }
    pub fn undo(&mut self) -> bool { match self.history.undo() { Some(state) => { self.apply(&state); true } None => false } }
    pub fn redo(&mut self) -> bool { match self.history.redo() { Some(state) => { self.apply(&state); true } None => false } }

    /// Sets the selection as one undo step; nothing happens when it is unchanged.
    pub fn set_selection(&mut self, selection: Option<Selection>, name: &str) {
        if selection == self.selection { return; }
        self.begin_edit(name);
        self.selection = selection;
        self.end_edit();
    }

    pub fn select_all(&mut self) -> Result<()> {
        let all = Selection::all(self.width(), self.height())?;
        self.set_selection(Some(all), "Select All");
        Ok(())
    }

    pub fn deselect(&mut self) { self.set_selection(None, "Deselect"); }

    pub fn invert_selection(&mut self) -> Result<()> {
        let Some(current) = &self.selection else { return Ok(()) };
        let inverted = current.inverted()?;
        self.set_selection(Some(inverted), "Inverse");
        Ok(())
    }

    /// Combines `shape` with the current selection by `mode`, as `applySelection` does: replacing, adding,
    /// or subtracting (which changes nothing without a selection).
    pub fn apply_selection(&mut self, shape: Selection, mode: Mode, name: &str) -> Result<()> {
        let result = match (&self.selection, mode) {
            (_, Mode::Replace) | (None, Mode::Add) => shape,
            (None, Mode::Subtract) => return Ok(()),
            (Some(current), mode) => current.combined(&shape, mode)?,
        };
        self.set_selection(Some(result), name);
        Ok(())
    }

    /// The Magic Wand at document point (x, y): pixels similar to the one there, read from the active layer
    /// or the whole composite, combined with the selection by `mode`. In New mode a click that matches nothing
    /// deselects, as the reference does.
    pub fn wand(&mut self, x: f64, y: f64, settings: &WandSettings, mode: Mode) -> Result<()> {
        let (w, h) = (self.width(), self.height());
        if !(x.is_finite() && y.is_finite() && x >= 0.0 && y >= 0.0 && x < w as f64 && y < h as f64) { return Ok(()); }
        let sample = new_argb(w, h)?;
        {
            let cr = Context::new(&sample)?;
            cr.rectangle(0.0, 0.0, w as f64, h as f64);
            cr.clip();
            if settings.sample_all_layers { self.renderer.draw(&cr)?; }
            else if let Some(id) = self.active.filter(|id| !self.renderer.layer(*id).is_group()) { self.renderer.draw_layer_plain(id, &cr)?; }
        }
        let mut packed = vec![0u8; w as usize * h as usize];
        let count = with_bytes(&sample, |data, stride| {
            crate::ffi::wand(data, w as usize, h as usize, stride, (x as usize, y as usize), settings.sample_radius, settings.tolerance.clamp(0, 255), settings.contiguous, &mut packed)
        })??;
        if count == 0 {
            if mode == Mode::Replace { self.deselect(); }
            return Ok(());
        }
        let shape = Selection::from_packed(&packed, w, h)?;
        self.apply_selection(shape, mode, "Magic Wand")
    }

    /// The active layer's id and pixels, when it is an image layer.
    pub fn active_image(&self) -> Option<(Uuid, ImageSurface)> {
        let id = self.active?;
        let layer = self.renderer.layer(id);
        if layer.is_group() { return None; }
        self.renderer.image(id).map(|s| (id, s.clone()))
    }

    /// The selection's coverage on the active layer's grid: None with no selection (everything), Some(zeros)
    /// for an explicit empty selection (nothing).
    fn active_coverage(&self, id: Uuid, image: &ImageSurface) -> Result<Option<ImageSurface>> {
        let Some(selection) = &self.selection else { return Ok(None) };
        let transform = self.renderer.layer(id).transform;
        Ok(Some(selection.coverage_on_layer(&transform, image.width(), image.height())?))
    }

    fn filtered(&self, kind: Kind, settings: &Settings) -> Result<(Uuid, ImageSurface)> {
        let Some((id, image)) = self.active_image() else { bail!("Select an image layer first.") };
        let coverage = self.active_coverage(id, &image)?;
        if kind.needs_selection() && self.selection.as_ref().is_none_or(|s| s.is_empty()) { bail!("{} needs a selection.", kind.name()); }
        let result = filters::run(kind, &image, settings, coverage.as_ref())?;
        Ok((id, result))
    }

    /// Runs a filter on the active layer as one undo step. Settings that would change nothing (no
    /// distortion, no grain, identity levels) close without a step, as the reference's OK does.
    pub fn apply_filter(&mut self, kind: Kind, settings: &Settings) -> Result<()> {
        let s = settings.normalized();
        let identity = match kind {
            Kind::LensCorrection => s.distortion == 0.0,
            Kind::Grain => s.grain.amount == 0.0,
            Kind::Levels => s.levels.is_identity(),
            Kind::Exposure => s.exposure.is_identity(),
            Kind::HueSaturation => s.hue_saturation.is_identity(),
            _ => false,
        };
        if identity { self.clear_preview(); return Ok(()); }
        let (id, result) = self.filtered(kind, settings)?;
        self.renderer.set_preview(id, None);
        self.begin_edit(kind.name());
        self.renderer.set_image(id, result);
        self.end_edit();
        Ok(())
    }

    /// Shows what the filter would do without committing it.
    pub fn preview_filter(&mut self, kind: Kind, settings: &Settings) -> Result<()> {
        let (id, result) = self.filtered(kind, settings)?;
        self.renderer.set_preview(id, Some(result));
        Ok(())
    }

    pub fn clear_preview(&mut self) {
        if let Some(id) = self.active { self.renderer.set_preview(id, None); }
    }

    /// The active layer's histogram inside the selection, for the Levels dialog.
    pub fn histogram(&self) -> Result<[[f64; 256]; 4]> {
        let Some((id, image)) = self.active_image() else { bail!("Select an image layer first.") };
        let coverage = self.active_coverage(id, &image)?;
        filters::histogram(&image, coverage.as_ref())
    }
}

// MARK: Brush strokes

/// What a brush tool paints.
#[derive(Clone, Debug, PartialEq)]
pub enum StrokeKind {
    Paint,
    Erase,
    Heal { mode: i32 },
    /// The sample is taken when the stroke starts, from the active layer or every visible layer.
    Clone { offset: (f64, f64), all_layers: bool },
    Blur,
}

impl Document {
    pub fn stroke_active(&self) -> bool { self.stroke.is_some() }

    /// A document-size copy of the active layer alone, or of the whole composite.
    fn sample(&mut self, all_layers: bool) -> Result<crate::brush::Sample> {
        let (w, h) = (self.width(), self.height());
        let surface = new_argb(w, h)?;
        {
            let cr = Context::new(&surface)?;
            cr.rectangle(0.0, 0.0, w as f64, h as f64);
            cr.clip();
            if all_layers { self.renderer.draw(&cr)?; }
            else if let Some(id) = self.active { self.renderer.draw_layer_plain(id, &cr)?; }
        }
        let (wu, hu) = (w as usize, h as usize);
        let mut pixels = vec![0u8; wu * hu * 4];
        with_bytes(&surface, |data, stride| {
            for y in 0..hu { pixels[y * wu * 4..(y + 1) * wu * 4].copy_from_slice(&data[y * stride..y * stride + wu * 4]); }
        })?;
        Ok(crate::brush::Sample { pixels, width: wu, height: hu })
    }

    /// Starts a stroke on the active layer at `point` (document pixels). Nothing starts on a folder, an
    /// adjustment layer, or an explicitly empty selection.
    pub fn begin_stroke(&mut self, point: (f64, f64), settings: &crate::brush::BrushSettings, kind: StrokeKind) -> Result<()> {
        if self.stroke.is_some() { bail!("a stroke is already in progress"); }
        let Some(id) = self.active else { bail!("Select a layer first.") };
        let layer = self.renderer.layer(id).clone();
        if layer.is_group() || layer.adjustment.is_some() { bail!("Select an image layer first."); }
        if self.selection.as_ref().is_some_and(|s| s.is_empty()) { bail!("Nothing is selected."); }
        let kind = match kind {
            StrokeKind::Paint => crate::brush::Kind::Paint,
            StrokeKind::Erase => crate::brush::Kind::Erase,
            StrokeKind::Heal { mode } => crate::brush::Kind::Heal { mode },
            StrokeKind::Clone { offset, all_layers } => crate::brush::Kind::Clone { sample: std::rc::Rc::new(self.sample(all_layers)?), offset },
            StrokeKind::Blur => {
                let sigma = (settings.diameter / 10.0).clamp(1.5, 30.0);
                let sample = self.sample(false)?;
                crate::brush::Kind::Blur { sample: std::rc::Rc::new(crate::blur::LazyBlur::new(sample.pixels, sample.width, sample.height, sigma)) }
            }
        };
        let size = self.renderer.image_size(id).unwrap_or((layer.transform.size.0.round().max(1.0) as i32, layer.transform.size.1.round().max(1.0) as i32));
        let canvas = (self.width() as f64, self.height() as f64);
        let renderer = &mut self.renderer;
        let mut stroke = crate::brush::Stroke::new(renderer.image(id).is_some(), size, &layer.transform, canvas, settings, kind, self.selection.clone(), true,
            |x0, y0, w, h, transform| renderer.begin_preview(id, -x0, -y0, w, h, transform))?;
        stroke.append(point)?;
        if let Some(rect) = stroke.changed { self.renderer.preview_changed(id, rect)?; }
        self.stroke = Some(stroke);
        Ok(())
    }

    pub fn continue_stroke(&mut self, point: (f64, f64)) -> Result<()> {
        let Some(id) = self.active else { return Ok(()) };
        let Some(mut stroke) = self.stroke.take() else { return Ok(()) };
        let result = stroke.append(point);
        let sync = match (&result, self.stroke_mask) {
            (Ok(()), true) => self.sync_mask_preview(id, &stroke),
            (Ok(()), false) => stroke.changed.map_or(Ok(()), |rect| self.renderer.preview_changed(id, rect)),
            _ => Ok(()),
        };
        self.stroke = Some(stroke);
        result.and(sync)
    }

    pub fn cancel_stroke(&mut self) {
        self.stroke = None;
        if let Some(id) = self.active { self.renderer.set_preview(id, None); self.renderer.end_mask_preview(id); }
        self.stroke_mask = false;
    }

    /// Ends the stroke: the final curve piece, healing if that is what it was, then the layer's pixels (and
    /// its transform, when painting reached past its edge) replaced as one undo step.
    pub fn finish_stroke(&mut self) -> Result<()> {
        let Some(mut stroke) = self.stroke.take() else { return Ok(()) };
        let Some(id) = self.active else { return Ok(()) };
        if std::mem::take(&mut self.stroke_mask) {
            let result = stroke.flush().and_then(|_| self.sync_mask_preview(id, &stroke));
            if result.is_ok() && stroke.touched_anything() {
                self.begin_edit("Paint Mask");
                self.renderer.adopt_mask_preview(id);
                self.end_edit();
            } else { self.renderer.end_mask_preview(id); }
            return result;
        }
        let result = (|| -> Result<()> {
            stroke.flush()?;
            stroke.heal()?;
            if !stroke.touched_anything() { return Ok(()); }
            let Some((x0, y0, x1, y1)) = stroke.committed_bounds() else { return Ok(()) };
            let (w, h) = ((x1 - x0) as i32, (y1 - y0) as i32);
            // The whole grid stays: the preview itself becomes the layer, halvings and all. Otherwise the
            // renderer crops both; the bounds are aligned so that is exact.
            let whole = (x0, y0, x1, y1) == (0, 0, stroke.width, stroke.height);
            let _ = (w, h);
            let transform = stroke.transform_for((x0, y0, x1, y1));
            let layer = self.renderer.layer(id).clone();
            let (sx, sy, sw, sh) = stroke.source;
            let grew = (x0, y0, x1, y1) != (sx, sy, sx + sw, sy + sh);
            // A mask covering the old grid is carried onto the new one, white where the layer grew.
            let mask = match self.renderer.mask(id) {
                Some(old) if grew && layer.mask_placement.is_none() => {
                    let expanded = crate::raster::a8_filled(w, h, 255)?;
                    let cr = Context::new(&expanded)?;
                    let pattern = cairo::SurfacePattern::create(old);
                    let (sx_, sy_) = (old.width() as f64 / sw as f64, old.height() as f64 / sh as f64);
                    pattern.set_matrix(cairo::Matrix::new(sx_, 0.0, 0.0, sy_, -(sx as f64 - x0 as f64) * sx_, -(sy as f64 - y0 as f64) * sy_));
                    pattern.set_filter(cairo::Filter::Nearest);
                    pattern.set_extend(cairo::Extend::Pad);
                    cr.rectangle(sx as f64 - x0 as f64, sy as f64 - y0 as f64, sw as f64, sh as f64);
                    cr.clip();
                    cr.set_source(&pattern)?;
                    cr.set_operator(cairo::Operator::Source);
                    cr.paint()?;
                    drop(cr);
                    Some(expanded)
                }
                _ => None,
            };
            self.begin_edit(stroke.kind.name());
            let adopted = if whole { self.renderer.adopt_preview(id) } else { self.renderer.adopt_preview_cropped(id, x0 as i32, y0 as i32, x1 as i32, y1 as i32)? };
            if !adopted {
                let image = new_argb(w, h)?;
                let cr = Context::new(&image)?;
                cr.set_source_surface(&stroke.preview, -(x0 as f64), -(y0 as f64))?;
                cr.source().set_filter(cairo::Filter::Nearest);
                cr.set_operator(cairo::Operator::Source);
                cr.paint()?;
                drop(cr);
                self.renderer.set_image(id, image);
            }
            if transform != layer.transform { self.renderer.set_layer_transform(id, transform); }
            if let Some(mask) = mask { self.renderer.set_mask(id, Some(mask)); }
            self.end_edit();
            Ok(())
        })();
        self.renderer.set_preview(id, None);
        result
    }
}

// MARK: Transforms, selections shapes, masks, clipping

impl Document {
    /// The mask's placement once its layer moves from `old` to `new`: carried along when linked, left on the
    /// document when unlinked; nil while it still covers the layer (`LayerMask.placement(movingLayer:to:)`).
    fn carried_mask_placement(&self, id: Uuid, old: &Transform, new: &Transform) -> Option<Transform> {
        let layer = self.renderer.layer(id);
        let mask = self.renderer.mask(id)?;
        if mask.width() <= 1 && mask.height() <= 1 { return None; }
        let linked = layer.mask_linked.unwrap_or(true);
        let moved = if linked { layer.mask_placement.map(|p| p.following(old, new)) } else { Some(layer.mask_placement.unwrap_or(*old)) };
        moved.filter(|m| !m.same_placement(new))
    }

    /// Places a layer, carrying its mask, as one undo step.
    pub fn set_transform(&mut self, id: Uuid, transform: Transform, name: &str) {
        if !transform.is_valid() { return; }
        let old = self.renderer.layer(id).transform;
        if old == transform { return; }
        let placement = self.carried_mask_placement(id, &old, &transform);
        self.begin_edit(name);
        if self.renderer.mask(id).is_some() { self.renderer.set_mask_placement(id, placement); }
        self.renderer.set_layer_transform(id, transform);
        self.end_edit();
    }

    /// The layers a transform of `id` moves: a folder's visible descendants, or the layer itself.
    pub fn transform_members(&self, id: Uuid) -> Vec<Uuid> {
        let layer = self.renderer.layer(id);
        if !layer.is_group() { return vec![id]; }
        let layers = self.renderer.layers();
        let mut result = Vec::new();
        for entry in crate::format::entries(layers) {
            if entry.layer.is_group() || !self.renderer.has_image(entry.layer.id) { continue; }
            let mut parent = entry.layer.parent_id;
            while let Some(p) = parent { if p == id { result.push(entry.layer.id); break; } parent = layers.iter().find(|l| l.id == p).and_then(|l| l.parent_id); }
        }
        result
    }

    /// Moves the active layer (or every layer in the active folder) by whole pixels.
    pub fn nudge(&mut self, dx: f64, dy: f64) {
        let Some(id) = self.active else { return };
        let members = self.transform_members(id);
        if members.is_empty() { return; }
        self.begin_edit("Move Layer");
        for m in members {
            let mut t = self.renderer.layer(m).transform;
            t.origin = crate::format::Point(t.origin.0 + dx, t.origin.1 + dy);
            self.set_transform(m, t, "Move Layer");
        }
        self.end_edit();
    }

    /// Flips the active layer about its own middle (a folder's contents about the box around them).
    pub fn flip_layer(&mut self, horizontally: bool) {
        let Some(id) = self.active else { return };
        let members = self.transform_members(id);
        if members.is_empty() { return; }
        let boxes: Vec<(f64, f64, f64, f64)> = members.iter().map(|m| self.renderer.layer(*m).transform.bounds()).collect();
        let axis = if members.len() == 1 {
            let c = self.renderer.layer(members[0]).transform.center();
            if horizontally { c.0 } else { c.1 }
        } else if horizontally {
            (boxes.iter().map(|b| b.0).fold(f64::MAX, f64::min) + boxes.iter().map(|b| b.2).fold(f64::MIN, f64::max)) / 2.0
        } else {
            (boxes.iter().map(|b| b.1).fold(f64::MAX, f64::min) + boxes.iter().map(|b| b.3).fold(f64::MIN, f64::max)) / 2.0
        };
        self.begin_edit(if horizontally { "Flip Horizontal" } else { "Flip Vertical" });
        for m in members {
            let t = self.renderer.layer(m).transform;
            let flipped = t.mirrored(horizontally, axis);
            self.set_transform(m, flipped, "Flip");
        }
        self.end_edit();
    }

    /// What a moving layer snaps to: the canvas edges and center, and every other visible layer's bounds and
    /// center, in whole pixels.
    pub fn snap_targets(&self, excluding: &[Uuid]) -> (Vec<f64>, Vec<f64>) {
        let (w, h) = (self.width() as f64, self.height() as f64);
        let (mut xs, mut ys) = (vec![0.0, (w / 2.0).round(), w], vec![0.0, (h / 2.0).round(), h]);
        for id in crate::format::visible_layers(self.renderer.layers()) {
            if excluding.contains(&id) || !self.renderer.has_image(id) { continue; }
            let (x0, y0, x1, y1) = self.renderer.layer(id).transform.bounds();
            xs.extend([x0.round(), ((x0 + x1) / 2.0).round(), x1.round()]);
            ys.extend([y0.round(), ((y0 + y1) / 2.0).round(), y1.round()]);
        }
        (xs, ys)
    }

    /// `draft` nudged so the layer it places lines up with a nearby edge or center, and the guides it met.
    pub fn snapped_move(&self, draft: Transform, moving: &[Uuid], tolerance: f64) -> (Transform, Option<f64>, Option<f64>) {
        let (xs, ys) = self.snap_targets(moving);
        let ((dx, dy), gx, gy) = crate::transform::snap_offset(draft.bounds(), &xs, &ys, tolerance);
        let mut snapped = draft;
        snapped.origin = crate::format::Point(draft.origin.0 + dx, draft.origin.1 + dy);
        (snapped, gx, gy)
    }

    /// A rectangle or ellipse marquee, combined with the selection by `mode`.
    pub fn select_box(&mut self, x: f64, y: f64, w: f64, h: f64, ellipse: bool, mode: Mode, antialiased: bool) -> Result<()> {
        if w <= 0.0 || h <= 0.0 { if mode == Mode::Replace { self.deselect(); } return Ok(()); }
        let shape = Selection::from_shape(self.width(), self.height(), antialiased, |cr| {
            if ellipse {
                cr.save()?;
                cr.translate(x + w / 2.0, y + h / 2.0);
                cr.scale(w / 2.0, h / 2.0);
                cr.arc(0.0, 0.0, 1.0, 0.0, std::f64::consts::TAU);
                cr.restore()?;
            } else {
                cr.rectangle(x, y, w, h);
            }
            cr.fill()?;
            Ok(())
        })?;
        self.apply_selection(shape, mode, if ellipse { "Elliptical Marquee" } else { "Rectangular Marquee" })
    }

    /// A lasso outline through `points`, combined with the selection by `mode`. Fewer than three points, or
    /// an outline with no area, deselects in New mode.
    pub fn select_polygon(&mut self, points: &[(f64, f64)], mode: Mode, antialiased: bool, name: &str) -> Result<()> {
        let (x0, x1) = (points.iter().map(|p| p.0).fold(f64::MAX, f64::min), points.iter().map(|p| p.0).fold(f64::MIN, f64::max));
        let (y0, y1) = (points.iter().map(|p| p.1).fold(f64::MAX, f64::min), points.iter().map(|p| p.1).fold(f64::MIN, f64::max));
        if points.len() < 3 || x1 <= x0 || y1 <= y0 { if mode == Mode::Replace { self.deselect(); } return Ok(()); }
        let shape = Selection::from_shape(self.width(), self.height(), antialiased, |cr| {
            cr.move_to(points[0].0, points[0].1);
            for p in &points[1..] { cr.line_to(p.0, p.1); }
            cr.close_path();
            cr.set_fill_rule(cairo::FillRule::Winding);
            cr.fill()?;
            Ok(())
        })?;
        self.apply_selection(shape, mode, name)
    }

    /// Moves the selection outline (never pixels) by whole pixels, as one undo step.
    pub fn move_selection(&mut self, dx: f64, dy: f64) -> Result<()> {
        let Some(current) = &self.selection else { return Ok(()) };
        if current.is_empty() { return Ok(()); }
        let moved = current.translated(dx.round() as i32, dy.round() as i32)?;
        self.set_selection(Some(moved), "Move Selection");
        Ok(())
    }

    /// The layer's own pixels (at least half opaque) as a selection, as Cmd-clicking its thumbnail does.
    pub fn select_layer_pixels(&mut self, id: Uuid, mode: Mode) -> Result<()> {
        let Some(image) = self.renderer.image(id).cloned() else { return Ok(()) };
        let transform = self.renderer.layer(id).transform;
        let (w, h) = (self.width(), self.height());
        let mask = crate::raster::a8_filled(w, h, 0)?;
        {
            let cr = Context::new(&mask)?;
            let matrix = crate::render::pixel_to_document(&transform, image.width(), image.height());
            cr.transform(matrix);
            cr.set_source_surface(&image, 0.0, 0.0)?;
            cr.source().set_filter(cairo::Filter::Nearest);
            cr.rectangle(0.0, 0.0, image.width() as f64, image.height() as f64);
            cr.fill()?;
        }
        // Keep only pixels at least half covered.
        let stride = mask.stride() as usize;
        let data = with_bytes(&mask, |d, _| d.iter().map(|v| if *v >= 128 { 255 } else { 0 }).collect::<Vec<u8>>())?;
        let shape = Selection::from_mask(crate::raster::a8_from_data(w, h, data, stride as i32)?, false)?;
        self.apply_selection(shape, mode, "Load Layer Selection")
    }

    // Masks

    /// Whether strokes and fills go to the active layer's mask rather than its pixels.
    pub fn mask_target(&self) -> bool { self.mask_target }
    pub fn set_mask_target(&mut self, on: bool) {
        self.mask_target = on && self.active.is_some_and(|id| self.renderer.mask(id).is_some());
    }

    /// A mask all white (reveal) or all black (hide), or with a selection that color with the selected area
    /// painted the opposite (a white mask hides the selection). The selection is used up.
    pub fn add_mask(&mut self, revealing: bool) -> Result<()> {
        let Some(id) = self.active else { return Ok(()) };
        if self.renderer.mask(id).is_some() { return Ok(()); }
        let layer = self.renderer.layer(id).clone();
        let (name, mask, used_selection) = match &self.selection {
            Some(selection) if !selection.is_empty() => {
                let (w, h) = self.renderer.image_size(id).unwrap_or((layer.transform.size.0.round().max(1.0) as i32, layer.transform.size.1.round().max(1.0) as i32));
                let coverage = selection.coverage_on_layer(&layer.transform, w, h)?;
                let stride = coverage.stride() as usize;
                let data = with_bytes(&coverage, |d, _| d.iter().map(|c| if revealing { 255 - c } else { *c }).collect::<Vec<u8>>())?;
                ("Add Mask from Selection", crate::raster::a8_from_data(w, h, data, stride as i32)?, true)
            }
            _ => (if revealing { "Add Reveal-All Mask" } else { "Add Hide-All Mask" }, crate::raster::a8_filled(1, 1, if revealing { 255 } else { 0 })?, false),
        };
        self.begin_edit(name);
        self.renderer.set_mask(id, Some(mask));
        if used_selection { self.selection = None; }
        self.mask_target = true;
        self.end_edit();
        Ok(())
    }

    pub fn delete_mask(&mut self) {
        let Some(id) = self.active else { return };
        if self.renderer.mask(id).is_none() { return; }
        self.begin_edit("Delete Layer Mask");
        self.renderer.set_mask(id, None);
        self.mask_target = false;
        self.end_edit();
    }

    pub fn toggle_mask_enabled(&mut self) {
        let Some(id) = self.active else { return };
        if self.renderer.mask(id).is_none() { return; }
        let enabled = self.renderer.layer(id).mask_enabled();
        self.begin_edit(if enabled { "Disable Layer Mask" } else { "Enable Layer Mask" });
        self.renderer.set_mask_enabled(id, !enabled);
        self.end_edit();
    }

    pub fn toggle_mask_linked(&mut self, id: Uuid) {
        if self.renderer.mask(id).is_none() { return; }
        let linked = self.renderer.layer(id).mask_linked.unwrap_or(true);
        self.begin_edit(if linked { "Unlink Layer Mask" } else { "Link Layer Mask" });
        self.renderer.set_mask_linked(id, !linked);
        self.end_edit();
    }

    pub fn invert_mask(&mut self) -> Result<()> {
        let Some(id) = self.active else { return Ok(()) };
        let Some(mask) = self.renderer.mask(id).cloned() else { return Ok(()) };
        let stride = mask.stride() as usize;
        let data = with_bytes(&mask, |d, _| d.iter().map(|v| 255 - v).collect::<Vec<u8>>())?;
        let inverted = crate::raster::a8_from_data(mask.width(), mask.height(), data, stride as i32)?;
        self.begin_edit("Invert Mask");
        self.renderer.set_mask(id, Some(inverted));
        self.end_edit();
        Ok(())
    }

    // Clipping masks

    /// Whether `id` can clip to `source` under the format's rules.
    pub fn can_clip(&self, source: Uuid, target: Uuid) -> bool {
        if source == target { return false; }
        let mut layers = self.renderer.layers().to_vec();
        let Some(t) = layers.iter_mut().find(|l| l.id == target) else { return false };
        if t.is_group() { return false; }
        t.mask_source_id = Some(source);
        let s = layers.iter().find(|l| l.id == source);
        s.is_some_and(|s| !s.is_group() && s.adjustment.is_none()) && live_mask_graph(&layers).is_ok()
    }

    /// Alt-click on a layer: clips it to the sibling below (sharing that sibling's base when it is itself
    /// clipped), or releases it and the clipped layers above it that share its base.
    pub fn toggle_clipping(&mut self, id: Uuid) {
        let layers = self.renderer.layers().to_vec();
        let Some(layer) = layers.iter().find(|l| l.id == id) else { return };
        if layer.is_group() { return; }
        let siblings: Vec<&crate::format::Layer> = layers.iter().filter(|l| l.parent_id == layer.parent_id).collect();
        let Some(index) = siblings.iter().position(|l| l.id == id) else { return };
        if let Some(source) = layer.mask_source_id {
            let releases: Vec<Uuid> = siblings[index..].iter().take_while(|l| l.id == id || l.mask_source_id == Some(source)).map(|l| l.id).collect();
            self.begin_edit("Release Clipping Mask");
            for r in releases { self.renderer.set_mask_source(r, None); }
            self.end_edit();
            return;
        }
        if index == 0 { return; }
        let below = siblings[index - 1];
        if below.is_group() { return; }
        let source = below.mask_source_id.unwrap_or(below.id);
        if !self.can_clip(source, id) { return; }
        self.begin_edit("Create Clipping Mask");
        self.renderer.set_mask_source(id, Some(source));
        self.end_edit();
    }
}

// MARK: Mask painting

impl Document {
    /// Starts a stroke on the active layer's mask. Brush paints `white` or black; every other kind paints black.
    pub fn begin_mask_stroke(&mut self, point: (f64, f64), settings: &crate::brush::BrushSettings, white: bool) -> Result<()> {
        if self.stroke.is_some() { bail!("a stroke is already in progress"); }
        let Some(id) = self.active else { bail!("Select a layer first.") };
        let layer = self.renderer.layer(id).clone();
        let Some(mask) = self.renderer.mask(id).cloned() else { bail!("This layer has no mask.") };
        if !layer.mask_enabled() { bail!("Enable the mask to paint it."); }
        if self.selection.as_ref().is_some_and(|s| s.is_empty()) { bail!("Nothing is selected."); }
        // A mask with its own placement is painted in its own grid; otherwise the grid is the layer's.
        let (grid, transform) = match layer.mask_placement {
            Some(p) => ((mask.width(), mask.height()), p),
            None => (self.renderer.image_size(id).unwrap_or((layer.transform.size.0.round().max(1.0) as i32, layer.transform.size.1.round().max(1.0) as i32)), layer.transform),
        };
        let mut paint = settings.clone();
        paint.color = if white { [1.0; 3] } else { [0.0; 3] };
        let canvas = (self.width() as f64, self.height() as f64);
        let a8 = self.renderer.begin_mask_preview(id, grid.0, grid.1)?;
        let mut stroke = crate::brush::Stroke::new(true, grid, &transform, canvas, &paint, crate::brush::Kind::Paint, self.selection.clone(), false, |_, _, w, h, _| {
            // The stroke's grid is the mask as opaque gray.
            let surface = new_argb(w, h)?;
            let (wu, hu) = (w as usize, h as usize);
            let bytes = with_bytes(&a8, |data, stride| { let mut out = vec![0u8; wu * hu * 4]; for y in 0..hu { for x in 0..wu { let v = data[y * stride + x]; let o = (y * wu + x) * 4; out[o] = v; out[o + 1] = v; out[o + 2] = v; out[o + 3] = 255; } } out })?;
            crate::raster::with_bytes_raw_mut(&surface, |data, stride| { for y in 0..hu { data[y * stride..y * stride + wu * 4].copy_from_slice(&bytes[y * wu * 4..(y + 1) * wu * 4]); } })?;
            Ok(surface)
        })?;
        stroke.append(point)?;
        self.stroke_mask = true;
        self.sync_mask_preview(id, &stroke)?;
        self.stroke = Some(stroke);
        Ok(())
    }

    /// Copies the stroke's changed gray pixels into the renderer's A8 preview and brings its reduced copies up.
    fn sync_mask_preview(&mut self, id: Uuid, stroke: &crate::brush::Stroke) -> Result<()> {
        let Some(rect) = stroke.changed else { return Ok(()) };
        let Some(a8) = self.renderer.mask_preview(id) else { return Ok(()) };
        let (x0, y0, x1, y1) = (rect.0.max(0) as usize, rect.1.max(0) as usize, rect.2.max(0) as usize, rect.3.max(0) as usize);
        let rows = with_bytes(&stroke.preview, |data, stride| {
            let mut out = vec![0u8; (x1 - x0) * (y1 - y0)];
            for y in y0..y1 { for x in x0..x1 { out[(y - y0) * (x1 - x0) + x - x0] = data[y * stride + x * 4]; } }
            out
        })?;
        crate::raster::with_bytes_raw_mut(&a8, |data, stride| { for y in y0..y1 { data[y * stride + x0..y * stride + x1].copy_from_slice(&rows[(y - y0) * (x1 - x0)..(y - y0 + 1) * (x1 - x0)]); } })?;
        self.renderer.mask_preview_changed(id, rect)
    }
}

// MARK: Files, layers, size, adjustments

impl Document {
    /// A blank document of `width` x `height` with one empty layer, as New Canvas makes.
    pub fn blank(width: i32, height: i32, resolution: f64) -> Result<Document> {
        let id = Uuid::new_v4();
        let layer = crate::format::Layer {
            id, name: "Layer 1".into(), is_visible: true,
            transform: Transform { origin: crate::format::Point(0.0, 0.0), size: crate::format::Size(width as f64, height as f64), rotation: 0.0, flip_x: false, flip_y: false, sampling: Default::default() },
            image_file: None, parent_id: None, is_group: None, opacity: None, blend_mode: None, mask_file: None, mask_enabled: None, mask_source_id: None, adjustment: None, mask_placement: None, mask_linked: None, shape: None,
        };
        let manifest = crate::format::Manifest { format: crate::format::FORMAT.into(), version: crate::format::SAVE_VERSION, color_space: "sRGB".into(), resolution: Some(resolution), document_id: Uuid::new_v4(), width: width as i64, height: height as i64, active_layer_id: Some(id), layers: vec![layer] };
        let json = serde_json::to_vec(&manifest)?;
        let manifest = crate::format::Manifest::parse(&json)?;
        Document::new(Project { path: std::path::PathBuf::new(), manifest, images: Default::default(), masks: Default::default() })
    }

    /// The manifest as it stands, for saving.
    pub fn manifest(&self) -> crate::format::Manifest {
        crate::format::Manifest {
            format: crate::format::FORMAT.into(), version: crate::format::SAVE_VERSION, color_space: "sRGB".into(),
            resolution: Some(self.renderer.resolution()), document_id: self.document_id, width: self.width() as i64, height: self.height() as i64,
            active_layer_id: self.active, layers: self.renderer.layers().to_vec(),
        }
    }

    pub fn save(&mut self, path: &std::path::Path) -> Result<()> {
        let manifest = self.manifest();
        crate::format::save(path, &manifest, self.renderer.images(), self.renderer.masks())?;
        self.history.mark_saved();
        Ok(())
    }

    pub fn is_modified(&self) -> bool { self.history.is_modified() }

    fn unique_name(&self, prefix: &str) -> String {
        let names: std::collections::HashSet<&str> = self.renderer.layers().iter().map(|l| l.name.as_str()).collect();
        let mut n = 1;
        while names.contains(format!("{prefix} {n}").as_str()) { n += 1; }
        format!("{prefix} {n}")
    }

    /// Where a new layer goes: just above the active layer, or at the top of the active folder.
    fn insertion(&self) -> (usize, Option<Uuid>) {
        let layers = self.renderer.layers();
        let Some(active) = self.active.and_then(|id| self.renderer.layer_index(id)) else { return (layers.len(), None) };
        let layer = &layers[active];
        if layer.is_group() {
            let folder = layer.id;
            let inside = |id: Uuid| { let mut p = layers.iter().find(|l| l.id == id).and_then(|l| l.parent_id); let mut n = 0; while let Some(c) = p { if c == folder { return true; } p = layers.iter().find(|l| l.id == c).and_then(|l| l.parent_id); n += 1; if n > 64 { break; } } false };
            let top = layers.iter().rposition(|l| inside(l.id)).map(|i| i + 1).unwrap_or(active + 1);
            (top.max(active + 1), Some(folder))
        } else { (active + 1, layer.parent_id) }
    }

    fn blank_record(&self, name: String, parent: Option<Uuid>) -> crate::format::Layer {
        crate::format::Layer {
            id: Uuid::new_v4(), name, is_visible: true,
            transform: Transform { origin: crate::format::Point(0.0, 0.0), size: crate::format::Size(self.width() as f64, self.height() as f64), rotation: 0.0, flip_x: false, flip_y: false, sampling: Default::default() },
            image_file: None, parent_id: parent, is_group: None, opacity: None, blend_mode: None, mask_file: None, mask_enabled: None, mask_source_id: None, adjustment: None, mask_placement: None, mask_linked: None, shape: None,
        }
    }

    pub fn add_blank_layer(&mut self) -> Uuid {
        let (index, parent) = self.insertion();
        let record = self.blank_record(self.unique_name("Layer"), parent);
        let id = record.id;
        self.begin_edit("New Layer");
        self.renderer.insert_layer(index, record, None, None);
        self.active = Some(id);
        self.mask_target = false;
        self.end_edit();
        id
    }

    pub fn add_folder(&mut self) -> Uuid {
        let (index, parent) = self.insertion();
        let mut record = self.blank_record(self.unique_name("Folder"), parent);
        record.is_group = Some(true);
        let id = record.id;
        self.begin_edit("New Folder");
        self.renderer.insert_layer(index, record, None, None);
        self.active = Some(id);
        self.end_edit();
        id
    }

    /// A new adjustment layer of `kind` above the active layer, affecting everything beneath it.
    pub fn add_adjustment(&mut self, kind: &str) -> Option<Uuid> {
        let adjustment = crate::filters::Adjustment::from_kind(kind)?;
        let adjustment = match adjustment { crate::filters::Adjustment::Grain { grain, .. } => crate::filters::Adjustment::Grain { grain, seed: rand_seed() }, other => other };
        let (index, parent) = self.insertion();
        let mut record = self.blank_record(kind.to_string(), parent);
        record.adjustment = Some(adjustment.to_record());
        let id = record.id;
        self.begin_edit(&format!("New {kind} Adjustment"));
        self.renderer.insert_layer(index, record, None, None);
        self.active = Some(id);
        self.end_edit();
        Some(id)
    }

    pub fn adjustment(&self, id: Uuid) -> Option<crate::filters::Adjustment> {
        self.renderer.layer(id).adjustment.as_ref().and_then(crate::filters::Adjustment::from_record)
    }

    /// Changes an adjustment layer's settings; `commit` makes it an undo step, otherwise it is a live preview.
    pub fn set_adjustment(&mut self, id: Uuid, adjustment: &crate::filters::Adjustment, commit: bool) {
        if commit { self.begin_edit(&format!("{} Adjustment", adjustment.kind_name())); }
        self.renderer.set_adjustment(id, Some(adjustment.to_record()));
        if commit { self.end_edit(); }
    }

    /// Deletes the active layer (a folder with its contents); clipping links to it are dropped.
    pub fn delete_layer(&mut self) {
        let Some(id) = self.active else { return };
        let mut ids = vec![id];
        let layers = self.renderer.layers().to_vec();
        let mut i = 0;
        while i < ids.len() { let parent = ids[i]; for l in &layers { if l.parent_id == Some(parent) { ids.push(l.id); } } i += 1; }
        let index = self.renderer.layer_index(id).unwrap_or(0);
        self.begin_edit("Delete Layer");
        for id in ids { self.renderer.remove_layer(id); }
        let remaining = self.renderer.layers();
        self.active = if remaining.is_empty() { None } else { Some(remaining[index.min(remaining.len() - 1)].id) };
        self.mask_target = false;
        self.end_edit();
    }

    /// A copy of the active layer just above it, sharing its pixels until either is painted.
    pub fn duplicate_layer(&mut self) -> Option<Uuid> {
        let id = self.active?;
        let mut record = self.renderer.layer(id).clone();
        if record.is_group() { return None; }
        record.id = Uuid::new_v4();
        record.name = format!("{} copy", record.name);
        record.mask_source_id = None;
        if record.image_file.is_some() { record.image_file = Some(format!("{}.png", crate::format::upper(record.id))); }
        if record.mask_file.is_some() { record.mask_file = Some(format!("{}.mask.png", crate::format::upper(record.id))); }
        let image = self.renderer.image(id).cloned();
        let mask = self.renderer.mask(id).cloned();
        let index = self.renderer.layer_index(id)? + 1;
        let new_id = record.id;
        self.begin_edit("Duplicate Layer");
        self.renderer.insert_layer(index, record, image, mask);
        self.active = Some(new_id);
        self.end_edit();
        Some(new_id)
    }

    /// Moves the active layer one step up (toward the top) or down among its siblings.
    pub fn move_layer(&mut self, up: bool) {
        let Some(id) = self.active else { return };
        let layers = self.renderer.layers().to_vec();
        let Some(index) = layers.iter().position(|l| l.id == id) else { return };
        let parent = layers[index].parent_id;
        let sibling = if up { layers.iter().enumerate().skip(index + 1).find(|(_, l)| l.parent_id == parent).map(|(i, _)| i) }
            else { layers.iter().enumerate().take(index).filter(|(_, l)| l.parent_id == parent).map(|(i, _)| i).next_back() };
        let Some(other) = sibling else { return };
        self.begin_edit(if up { "Move Layer Up" } else { "Move Layer Down" });
        self.renderer.swap_layers(index, other);
        self.end_edit();
    }

    pub fn rename_layer(&mut self, id: Uuid, name: &str) {
        let name = name.trim();
        if name.is_empty() || name.len() > 16_384 || self.renderer.layer(id).name == name { return; }
        self.begin_edit("Rename Layer");
        self.renderer.set_layer_name(id, name.to_string());
        self.end_edit();
    }

    // Export and import

    pub fn export_png(&mut self, path: &std::path::Path) -> Result<()> {
        let image = self.renderer.render_flat()?;
        crate::png_io::encode(&image, path, self.renderer.resolution())
    }

    /// The flattened document over a background color as JPEG bytes at `quality` (0 to 1).
    pub fn jpeg_bytes(&mut self, quality: f64, background: [f64; 3]) -> Result<Vec<u8>> {
        let image = self.renderer.render_flat()?;
        let (rgba, w, h) = crate::png_io::straight_rgba(&image)?;
        let mut rgb = vec![0u8; w * h * 3];
        for i in 0..w * h {
            let a = rgba[i * 4 + 3] as f64 / 255.0;
            for k in 0..3 { rgb[i * 3 + k] = (rgba[i * 4 + k] as f64 * a + background[k] * 255.0 * (1.0 - a)).round().clamp(0.0, 255.0) as u8; }
        }
        let mut out = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, (quality.clamp(0.0, 1.0) * 100.0).round().max(1.0) as u8);
        use image::ImageEncoder;
        encoder.write_image(&rgb, w as u32, h as u32, image::ExtendedColorType::Rgb8)?;
        Ok(out)
    }

    pub fn export_jpeg(&mut self, path: &std::path::Path, quality: f64, background: [f64; 3]) -> Result<()> {
        let bytes = self.jpeg_bytes(quality, background)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }

    /// The visible composite inside the selection (or the whole canvas) as PNG bytes, with the region it
    /// covers: Copy Merged.
    pub fn copy_merged(&mut self) -> Result<Option<(Vec<u8>, (i32, i32, i32, i32))>> {
        let flat = self.renderer.render_flat()?;
        let (x0, y0, x1, y1) = match &self.selection {
            Some(s) => match s.bounds { Some(b) => (b.0 as i32, b.1 as i32, b.2 as i32, b.3 as i32), None => return Ok(None) },
            None => (0, 0, self.width(), self.height()),
        };
        let (w, h) = (x1 - x0, y1 - y0);
        let out = new_argb(w, h)?;
        {
            let cr = Context::new(&out)?;
            cr.set_source_surface(&flat, -(x0 as f64), -(y0 as f64))?;
            match &self.selection {
                Some(s) => { cr.mask_surface(&s.mask, -(x0 as f64), -(y0 as f64))?; }
                None => cr.paint()?,
            }
        }
        Ok(Some((crate::png_io::png_bytes(&out)?, (x0, y0, w, h))))
    }

    /// Imports an image file (PNG, JPEG or TIFF) as a new layer centered on the canvas at its own size.
    pub fn import_image(&mut self, path: &std::path::Path) -> Result<Uuid> {
        use image::ImageDecoder;
        let mut decoder = image::ImageReader::open(path)?.with_guessed_format()?.into_decoder()?;
        let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
        let mut decoded = image::DynamicImage::from_decoder(decoder)?;
        decoded.apply_orientation(orientation);
        let rgba = decoded.to_rgba8();
        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
        if w == 0 || h == 0 || w > 30_000 || h > 30_000 || w * h > 100_000_000 { bail!("This image is larger than the 30,000-pixel side or 100-megapixel limit."); }
        let surface = crate::png_io::from_straight_rgba(rgba.as_raw(), w, h)?;
        let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Image".into());
        let (index, parent) = self.insertion();
        let mut record = self.blank_record(name, parent);
        record.transform.origin = crate::format::Point(((self.width() as f64 - w as f64) / 2.0).round(), ((self.height() as f64 - h as f64) / 2.0).round());
        record.transform.size = crate::format::Size(w as f64, h as f64);
        record.image_file = Some(format!("{}.png", crate::format::upper(record.id)));
        let id = record.id;
        self.begin_edit("Import Image");
        self.renderer.insert_layer(index, record, Some(surface), None);
        self.active = Some(id);
        self.mask_target = false;
        self.end_edit();
        Ok(id)
    }

    // Canvas and image size

    /// Resizes the canvas: layers keep their pixels and shift by the anchor (0 to 8, row-major from the top
    /// left; 4 keeps the center). With `fill`, a "Canvas Extension" layer of that color goes under everything,
    /// transparent where the old canvas was. `offset` overrides the anchor (Crop passes the crop's origin).
    pub fn canvas_size(&mut self, width: i32, height: i32, anchor: usize, fill: Option<[f64; 3]>, offset: Option<(f64, f64)>, name: &str) -> Result<()> {
        if !(1..=30_000).contains(&width) || !(1..=30_000).contains(&height) || anchor > 8 { bail!("Canvas sizes run from 1 to 30,000 pixels per side."); }
        let (ow, oh) = (self.width(), self.height());
        let (dx, dy) = offset.unwrap_or_else(|| ((((width - ow) as f64) * (anchor % 3) as f64 / 2.0).floor(), (((height - oh) as f64) * (anchor / 3) as f64 / 2.0).floor()));
        if width == ow && height == oh && dx == 0.0 && dy == 0.0 { return Ok(()); }
        self.begin_edit(name);
        for layer in self.renderer.layers().to_vec() {
            let mut t = layer.transform;
            t.origin = crate::format::Point(t.origin.0 + dx, t.origin.1 + dy);
            self.renderer.set_layer_transform(layer.id, t);
            if let Some(mut p) = layer.mask_placement { p.origin = crate::format::Point(p.origin.0 + dx, p.origin.1 + dy); self.renderer.set_mask_placement(layer.id, Some(p)); }
        }
        self.renderer.set_size(width, height);
        if let (Some(color), true) = (fill, width > ow || height > oh) {
            let extension = new_argb(width, height)?;
            {
                let cr = Context::new(&extension)?;
                cr.set_source_rgb(color[0], color[1], color[2]);
                cr.paint()?;
                cr.set_operator(cairo::Operator::Clear);
                cr.rectangle(dx, dy, ow as f64, oh as f64);
                cr.fill()?;
            }
            let mut record = self.blank_record("Canvas Extension".into(), None);
            record.image_file = Some(format!("{}.png", crate::format::upper(record.id)));
            self.renderer.insert_layer(0, record, Some(extension), None);
        }
        if let Some(selection) = self.selection.clone() {
            // The selection stays with the pixels it covered, as far as the new canvas keeps them.
            let moved = Selection::from_shape(width, height, selection.antialiased, |cr| { cr.set_source_surface(&selection.mask, dx, dy)?; cr.paint()?; Ok(()) })?;
            self.selection = Some(moved);
        }
        self.end_edit();
        Ok(())
    }

    /// Crops the canvas to the selection's bounds.
    pub fn crop_to_selection(&mut self) -> Result<()> {
        let Some(bounds) = self.selection.as_ref().and_then(|s| s.bounds) else { bail!("Select an area to crop to first.") };
        let (x0, y0, x1, y1) = (bounds.0 as i32, bounds.1 as i32, bounds.2 as i32, bounds.3 as i32);
        self.begin_edit("Crop");
        self.canvas_size(x1 - x0, y1 - y0, 4, None, Some((-(x0 as f64), -(y0 as f64))), "Crop")?;
        self.selection = None;
        self.end_edit();
        Ok(())
    }

    /// Resamples the whole document to `width` x `height`: every layer is rasterized at its new size in place
    /// (baking its rotation), masks likewise, and the resolution changes (`ImageResizer`).
    pub fn image_size(&mut self, width: i32, height: i32, resolution: f64, sampling: crate::format::Sampling) -> Result<()> {
        if !(1..=30_000).contains(&width) || !(1..=30_000).contains(&height) || !(1.0..=9600.0).contains(&resolution) { bail!("Image sizes run from 1 to 30,000 pixels per side, at 1 to 9600 pixels per inch."); }
        let (ow, oh) = (self.width(), self.height());
        self.begin_edit("Image Size");
        if width != ow || height != oh {
            let (sx, sy) = (width as f64 / ow as f64, height as f64 / oh as f64);
            for layer in self.renderer.layers().to_vec() {
                let (bx0, by0, bx1, by1) = layer.transform.bounds();
                let (left, top) = ((bx0 * sx).floor(), (by0 * sy).floor());
                let (w, h) = (((bx1 * sx).ceil() - left).max(1.0) as i32, ((by1 * sy).ceil() - top).max(1.0) as i32);
                let placed = Transform { origin: crate::format::Point(left, top), size: crate::format::Size(w as f64, h as f64), rotation: 0.0, flip_x: false, flip_y: false, sampling };
                if self.renderer.has_image(layer.id) {
                    self.renderer.set_sampling(layer.id, sampling);
                    let surface = new_argb(w, h)?;
                    {
                        let cr = Context::new(&surface)?;
                        cr.translate(-left, -top);
                        cr.scale(sx, sy);
                        self.renderer.draw_layer_plain(layer.id, &cr)?;
                    }
                    self.renderer.set_image(layer.id, surface);
                }
                if let Some(mask) = self.renderer.mask(layer.id).cloned() {
                    if let Some(p) = layer.mask_placement {
                        let mut scale = cairo::Matrix::identity();
                        scale.scale(sx, sy);
                        self.renderer.set_mask_placement(layer.id, Some(p.placing(&cairo::Matrix::multiply(&p.unit_to_document(), &scale))));
                    } else if mask.width() > 1 || mask.height() > 1 {
                        let a8 = crate::raster::a8_filled(w, h, 0)?;
                        {
                            let cr = Context::new(&a8)?;
                            cr.translate(-left, -top);
                            cr.scale(sx, sy);
                            self.renderer.draw_mask_plain(layer.id, &cr)?;
                        }
                        self.renderer.set_mask(layer.id, Some(a8));
                    }
                }
                self.renderer.set_layer_transform(layer.id, placed);
            }
            self.renderer.set_size(width, height);
            if let Some(selection) = self.selection.clone() {
                self.selection = Some(Selection::from_shape(width, height, selection.antialiased, |cr| { cr.scale(sx, sy); cr.set_source_surface(&selection.mask, 0.0, 0.0)?; cr.paint()?; Ok(()) })?);
            }
        }
        self.renderer.set_resolution(resolution);
        self.end_edit();
        Ok(())
    }

    /// Expand (positive) or Contract (negative) the selection by whole pixels.
    pub fn resize_selection(&mut self, amount: i32) -> Result<()> {
        let Some(current) = &self.selection else { return Ok(()) };
        if current.is_empty() || amount == 0 || amount.abs() > 500 { return Ok(()); }
        let resized = current.resized(amount)?;
        self.set_selection(Some(resized), if amount > 0 { "Expand Selection" } else { "Contract Selection" });
        Ok(())
    }
}

fn rand_seed() -> u32 {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    (t as u64 ^ (t >> 64) as u64) as u32 ^ 0x5851_f42d
}
