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
    pub history: History<State>,
    stroke: Option<crate::brush::Stroke>,
    warp: Option<crate::warp::Warp>,
    /// The layer a stroke or warp began on: where it lands, whatever is active when it ends.
    stroke_layer: Option<Uuid>,
    stroke_mask: bool,
    mask_target: bool,
    pub document_id: Uuid,
    pub path: Option<std::path::PathBuf>,
    /// Document pixels the last stroke step changed (x0, y0, x1, y1), when the change was that local.
    dirty: Option<(f64, f64, f64, f64)>,
    /// Every selected layer (the active one among them) for group transforms, merges and deletes; not undone.
    pub selected: std::collections::HashSet<Uuid>,
    /// Selected pixels being dragged (Ctrl-drag with the Move tool).
    pixel_move: Option<PixelMove>,
    /// The background removal model's mask for the image it was made from (by surface pointer), so a slider
    /// only redoes the refining.
    matte_cache: Option<(ImageSurface, Vec<f32>)>,
    /// Vertical guides (x) and horizontal guides (y) in document pixels, and whether they show. Not saved.
    pub guides_v: Vec<f64>,
    pub guides_h: Vec<f64>,
    pub show_guides: bool,
    /// View > Snap: guides and moves settle on canvas edges and centers, layer edges and centers, and the grid.
    pub snap: bool,
    /// The selection before the last Deselect, for Reselect (Ctrl+Shift+D).
    pub last_selection: Option<Selection>,
    /// Free Transform in progress: the floating layer holding the selected pixels, and the layer they came from.
    pub floating: Option<(Uuid, Uuid)>,
    /// The last filter, for Fade: the layer, its pixels before, its pixels after, the filter's name, and
    /// the edit serial right after it (Fade is only offered while nothing else has happened since).
    pub last_filter: Option<(Uuid, ImageSurface, ImageSurface, String, u64)>,
    /// Counts every finished edit, undo and redo.
    pub edit_serial: u64,
    /// A Layer Style dialog previewing effects on a layer: the layer and the record it had on open.
    pub effects_preview: Option<(Uuid, Option<serde_json::Value>)>,
    /// View > Show Grid: (spacing of the major lines, subdivisions per spacing), or None when hidden.
    pub grid: Option<(f64, u32)>,
}

/// Selected pixels lifted off their layer while they are dragged (`PixelMove`): everything in the layer's own
/// pixel grid, the outline they started from, and where they are now.
struct PixelMove {
    id: Uuid,
    lifted: ImageSurface,
    /// Where the lifted pixels came from in the layer's grid.
    region: (i32, i32),
    /// The layer with the hole (or as it was, when duplicating).
    base: ImageSurface,
    origin_selection: Selection,
    duplicate: bool,
    /// Layer pixels moved so far, and the document pixels that is.
    offset: (i32, i32),
    document_offset: (i32, i32),
}

#[derive(Clone)]
pub struct State {
    render: render::State,
    selection: Option<Selection>,
    active: Option<Uuid>,
    /// Ruler guides: they move with the canvas, so they go back with it.
    guides: (Vec<f64>, Vec<f64>),
}

impl PartialEq for State {
    fn eq(&self, other: &Self) -> bool { self.render == other.render && self.selection == other.selection && self.active == other.active && self.guides == other.guides }
}

/// Where dragged layers land in the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place { Above(Uuid), Below(Uuid), Into(Uuid) }

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
        let (guides_v, guides_h) = project.manifest.guides.as_ref().map(|g| (g.vertical.clone(), g.horizontal.clone())).unwrap_or_default();
        let renderer = Renderer::new(project)?;
        let selected = active.into_iter().collect();
        Ok(Document { renderer, selection: None, active, history: History::new(100, 256 * 1024 * 1024), stroke: None, stroke_layer: None, warp: None, stroke_mask: false, mask_target: false, document_id, path, dirty: None, selected, pixel_move: None, matte_cache: None, guides_v, guides_h, show_guides: true, snap: true, grid: None, last_selection: None, floating: None, last_filter: None, edit_serial: 0, effects_preview: None })
    }

    pub fn width(&self) -> i32 { self.renderer.width() }
    pub fn height(&self) -> i32 { self.renderer.height() }

    fn state(&self) -> State { State { render: self.renderer.snapshot(), selection: self.selection.clone(), active: self.active, guides: (self.guides_v.clone(), self.guides_h.clone()) } }
    fn apply(&mut self, state: &State) {
        self.renderer.restore(&state.render);
        self.selection = state.selection.clone();
        self.active = state.active;
        self.guides_v = state.guides.0.clone();
        self.guides_h = state.guides.1.clone();
        self.selected.retain(|id| self.renderer.layer_index(*id).is_some());
        if let Some(a) = self.active { self.selected.insert(a); }
        // Painting the mask only makes sense while the active layer has one.
        if self.active.and_then(|a| self.renderer.mask(a)).is_none() { self.mask_target = false; }
    }

    // MARK: Selecting layers

    /// Makes `id` the only selected layer (and the active one).
    pub fn select_layer(&mut self, id: Option<Uuid>) {
        self.active = id.filter(|i| self.has_layer(*i));
        self.selected = self.active.into_iter().collect();
        self.mask_target = false;
    }

    /// Ctrl-click in the panel: adds or removes a layer from the selection; the active layer stays selected.
    pub fn toggle_layer_selected(&mut self, id: Uuid) {
        if !self.has_layer(id) { return; }
        if self.selected.contains(&id) && self.selected.len() > 1 { self.selected.remove(&id); if self.active == Some(id) { self.active = self.selected.iter().next().copied(); } }
        else { self.selected.insert(id); self.active = Some(id); }
        self.mask_target = false;
    }

    /// Shift-click in the panel: selects every row between the active layer and `id`, in panel order.
    pub fn select_layer_range(&mut self, id: Uuid) {
        let order: Vec<Uuid> = crate::format::entries_ordered(self.renderer.layers(), true).iter().map(|e| e.layer.id).collect();
        let (Some(a), Some(b)) = (self.active.and_then(|a| order.iter().position(|x| *x == a)), order.iter().position(|x| *x == id)) else { self.select_layer(Some(id)); return };
        let (lo, hi) = (a.min(b), a.max(b));
        self.selected.extend(order[lo..=hi].iter().copied());
        self.mask_target = false;
    }

    /// Several layers, or one folder, transform as one box (`transformsAsGroup`).
    pub fn transforms_as_group(&self) -> bool {
        self.selected.len() > 1 || self.active.is_some_and(|id| self.has_layer(id) && self.renderer.layer(id).is_group())
    }

    /// The upright box around what a group transform moves (`groupTransformBox`).
    pub fn group_box(&self) -> Option<Transform> {
        let members = self.active.map(|id| self.transform_members(id)).unwrap_or_default();
        if members.is_empty() { return None; }
        let boxes: Vec<_> = members.iter().map(|m| self.renderer.layer(*m).transform.bounds()).collect();
        let (x0, y0) = (boxes.iter().map(|b| b.0).fold(f64::MAX, f64::min), boxes.iter().map(|b| b.1).fold(f64::MAX, f64::min));
        let (x1, y1) = (boxes.iter().map(|b| b.2).fold(f64::MIN, f64::max), boxes.iter().map(|b| b.3).fold(f64::MIN, f64::max));
        Some(Transform { origin: crate::format::Point(x0, y0), size: crate::format::Size((x1 - x0).max(1.0), (y1 - y0).max(1.0)), rotation: 0.0, flip_x: false, flip_y: false, sampling: Default::default() })
    }

    pub fn begin_edit(&mut self, name: &str) { let state = self.state(); self.history.begin(name, state); }
    /// Runs `f` as one edit named `name`: ended when it succeeds, abandoned (the document put back) when it
    /// fails, so an early `?` can never leave an edit open.
    pub fn edited<T>(&mut self, name: &str, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        let outer = self.history.depth();
        self.begin_edit(name);
        let result = f(self);
        // Only the edit opened here is closed: one the closure left open is abandoned first, and one the
        // closure closed itself is not closed again (that would take the caller's).
        match &result { Ok(_) => { self.unwind_edits_to(outer + 1); if self.history.depth() == outer + 1 { self.end_edit(); } } Err(_) => self.unwind_edits_to(outer) }
        result
    }
    /// Abandons every edit opened since the history was `depth` deep (a guard around code that may fail
    /// partway through its own begin/end pairs).
    fn unwind_edits_to(&mut self, depth: usize) { while self.history.depth() > depth { self.abort_edit(); } }
    /// Abandons the edit in progress and puts the document back as it was when the edit began.
    pub fn abort_edit(&mut self) {
        if let Some(before) = self.history.cancel() { self.apply(&before); }
    }

    /// Ends an edit that folds into the previous one when it has the same name and came right after it, as
    /// a slider's steps do.
    pub fn end_edit_merging(&mut self) { self.finish_edit(true); }

    pub fn end_edit(&mut self) { self.finish_edit(false); }

    fn finish_edit(&mut self, merge: bool) {
        self.edit_serial += 1;
        let state = self.state();
        self.history.end_with(state, merge, |held, current| {
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
    /// The steps behind and ahead, by name.
    pub fn history_names(&self) -> (Vec<String>, Vec<String>) { self.history.names() }

    /// Moves the document `steps` back (negative) or forward (positive) through its history.
    pub fn step_history(&mut self, steps: i64) {
        if steps < 0 { for _ in 0..(-steps) { if !self.undo() { break; } } } else { for _ in 0..steps { if !self.redo() { break; } } }
    }

    pub fn undo(&mut self) -> bool { if self.preview_busy() { return false; } match self.history.undo() { Some(state) => { self.edit_serial += 1; self.apply(&state); true } None => false } }
    pub fn redo(&mut self) -> bool { if self.preview_busy() { return false; } match self.history.redo() { Some(state) => { self.edit_serial += 1; self.apply(&state); true } None => false } }

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

    pub fn deselect(&mut self) { if let Some(s) = self.selection.clone().filter(|s| !s.is_empty()) { self.last_selection = Some(s); } self.set_selection(None, "Deselect"); }

    /// Brings back the selection the last Deselect dropped (Ctrl+Shift+D).
    pub fn reselect(&mut self) {
        let Some(s) = self.last_selection.clone() else { return };
        if s.width() != self.width() || s.height() != self.height() { return; }
        self.set_selection(Some(s), "Reselect");
    }

    /// Softens the selection's edge by `radius` pixels (Select > Feather, Shift+F6).
    pub fn feather_selection(&mut self, radius: f64) -> Result<()> {
        let Some(selection) = self.selection.clone() else { bail!("Make a selection first.") };
        if selection.is_empty() { bail!("Make a selection first."); }
        if !(0.1..=1000.0).contains(&radius) { bail!("Feather runs from 0.1 to 1000 pixels."); }
        let (w, h) = (selection.width() as usize, selection.height() as usize);
        let mut packed = crate::raster::with_bytes(&selection.mask, |data, stride| (0..h).flat_map(|y| data[y * stride..y * stride + w].iter().copied()).collect::<Vec<u8>>())?;
        crate::blur::gaussian(&mut packed, w, h, 1, radius / 2.0);
        let feathered = Selection::from_packed(&packed, w as i32, h as i32)?;
        self.set_selection(Some(feathered), "Feather");
        Ok(())
    }

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

    /// The filter's result on the active layer, and the placement it needs when the filter spread past the
    /// layer's edge (a blur gets a transparent margin to spread into, then the empty rim is cut away again).
    /// The active mask and the grid it is painted in, when the mask is the target.
    fn active_mask(&self) -> Option<(Uuid, ImageSurface, Transform)> {
        if !self.mask_target { return None; }
        let id = self.active?;
        let layer = self.renderer.layer(id);
        if !layer.mask_enabled() { return None; }
        let mask = self.renderer.mask(id)?.clone();
        Some((id, mask, layer.mask_placement.unwrap_or(layer.transform)))
    }

    /// A filter run on the active mask: the mask as opaque gray, filtered like pixels, back to coverage.
    fn filtered_mask(&self, kind: Kind, settings: &Settings) -> Result<(Uuid, ImageSurface)> {
        let Some((id, mask, grid)) = self.active_mask() else { bail!("Select a layer with a mask first.") };
        if kind.needs_selection() { bail!("{} works on image pixels, not masks.", kind.name()); }
        let (w, h) = (mask.width(), mask.height());
        // A uniform 1 x 1 mask cannot hold a partial result; give it the layer's grid first.
        let (mask, w, h) = if (w, h) == (1, 1) {
            let (gw, gh) = self.renderer.image_size(id).unwrap_or((grid.size.0.round().max(1.0) as i32, grid.size.1.round().max(1.0) as i32));
            let value = with_bytes(&mask, |d, _| d[0])?;
            (crate::raster::a8_filled(gw, gh, value)?, gw, gh)
        } else { (mask, w, h) };
        let gray = gray_from_a8(&mask)?;
        let coverage = match &self.selection { Some(sel) => Some(sel.coverage_on_layer(&grid, w, h)?), None => None };
        let result = filters::run(kind, &gray, settings, coverage.as_ref())?;
        Ok((id, a8_from_gray(&result)?))
    }

    fn filtered(&self, kind: Kind, settings: &Settings) -> Result<(Uuid, ImageSurface, Option<Transform>)> {
        let Some((id, image)) = self.active_image() else { bail!("Select an image layer first.") };
        if kind == Kind::Fade {
            // The last filter's before and after, mixed; only while its result is still what the layer shows.
            let Some((fid, before, after, _, serial)) = &self.last_filter else { bail!("Nothing to fade: run a filter or adjustment first.") };
            if *fid != id || after.to_raw_none() != image.to_raw_none() || *serial != self.edit_serial { bail!("Nothing to fade: something else has happened since the last filter."); }
            let (w, h) = (image.width(), image.height());
            let keep = settings.normalized().fade;
            let b = crate::raster::with_bytes(before, |d, _| d.to_vec())?;
            let out = new_argb(w, h)?;
            crate::raster::with_bytes_raw_mut(&out, |o, stride| {
                crate::raster::with_bytes(&image, |a, astride| {
                    for y in 0..h as usize { for x in 0..(w as usize) * 4 { let i = y * stride + x; let j = y * astride + x; o[i] = (a[j] as f64 * keep + b[j] as f64 * (1.0 - keep)).round() as u8; } }
                }).ok();
            })?;
            return Ok((id, out, None));
        }
        if kind.needs_selection() && self.selection.as_ref().is_none_or(|s| s.is_empty()) { bail!("{} needs a selection.", kind.name()); }
        let transform = self.renderer.layer(id).transform;
        let (iw, ih) = (image.width(), image.height());
        let margin = kind.margin(&settings.normalized()) as i32;
        // Content-Aware Fill extends the layer over any of the selection on the canvas past its edge.
        let extent: Option<(i32, i32, i32, i32)> = if kind == Kind::ContentAwareFill {
            self.selection.as_ref().and_then(|s| s.bounds).and_then(|(bx0, by0, bx1, by1)| {
                let (cx0, cy0, cx1, cy1) = (bx0.max(0) as f64, by0.max(0) as f64, (bx1 as i64).min(self.width() as i64) as f64, (by1 as i64).min(self.height() as i64) as f64);
                if cx1 <= cx0 || cy1 <= cy0 { return None; }
                let to_layer = crate::selection::document_to_layer(&transform, iw, ih).ok()?;
                let corners = [(cx0, cy0), (cx1, cy0), (cx1, cy1), (cx0, cy1)].map(|(x, y)| to_layer.transform_point(x, y));
                let (mut x0, mut y0, mut x1, mut y1) = (0.0f64, 0.0f64, iw as f64, ih as f64);
                for (x, y) in corners { x0 = x0.min(x.floor()); y0 = y0.min(y.floor()); x1 = x1.max(x.ceil()); y1 = y1.max(y.ceil()); }
                let grown = (x0 as i32, y0 as i32, x1 as i32, y1 as i32);
                if grown == (0, 0, iw, ih) { None } else { Some(grown) }
            })
        } else { None };
        if margin == 0 && extent.is_none() {
            let coverage = self.active_coverage(id, &image)?;
            let result = filters::run(kind, &image, settings, coverage.as_ref())?;
            return Ok((id, result, None));
        }
        let (ox, oy, gw, gh) = match extent { Some((x0, y0, x1, y1)) => (-x0, -y0, x1 - x0, y1 - y0), None => (margin, margin, iw + 2 * margin, ih + 2 * margin) };
        if gw > 30_000 || gh > 30_000 || gw as i64 * gh as i64 > 100_000_000 { bail!("The filter needs more room than the 30,000-pixel side or 100-megapixel limit allows."); }
        let grown = new_argb(gw, gh)?;
        {
            let cr = Context::new(&grown)?;
            cr.set_source_surface(&image, ox as f64, oy as f64)?;
            cr.paint()?;
        }
        let mut grown_transform = transform;
        grown_transform.size = crate::format::Size(transform.size.0 * gw as f64 / iw as f64, transform.size.1 * gh as f64 / ih as f64);
        let to_document = crate::render::pixel_to_document(&transform, iw, ih);
        let (mx, my) = to_document.transform_point(gw as f64 / 2.0 - ox as f64, gh as f64 / 2.0 - oy as f64);
        grown_transform.origin = crate::format::Point(mx - grown_transform.size.0 / 2.0, my - grown_transform.size.1 / 2.0);
        let coverage = match &self.selection { Some(sel) => Some(sel.coverage_on_layer(&grown_transform, gw, gh)?), None => None };
        let blurred = filters::run(kind, &grown, settings, coverage.as_ref())?;
        if extent.is_some() { return Ok((id, blurred, Some(grown_transform))); }
        // Trim the transparent rim the blur did not reach.
        let bounds = with_bytes(&blurred, |data, stride| crate::ffi::alpha_bounds(data, gw as usize, gh as usize, stride))?;
        let Some((x0, y0, x1, y1)) = bounds else { return Ok((id, blurred, Some(grown_transform))) };
        let (cw, ch) = ((x1 - x0) as i32, (y1 - y0) as i32);
        let cropped = new_argb(cw, ch)?;
        {
            let cr = Context::new(&cropped)?;
            cr.set_source_surface(&blurred, -(x0 as f64), -(y0 as f64))?;
            cr.set_operator(cairo::Operator::Source);
            cr.paint()?;
        }
        let to_document = crate::render::pixel_to_document(&grown_transform, gw, gh);
        let (mx, my) = to_document.transform_point((x0 + x1) as f64 / 2.0, (y0 + y1) as f64 / 2.0);
        let mut placed = grown_transform;
        placed.size = crate::format::Size(cw as f64 * grown_transform.size.0 / gw as f64, ch as f64 * grown_transform.size.1 / gh as f64);
        placed.origin = crate::format::Point(mx - placed.size.0 / 2.0, my - placed.size.1 / 2.0);
        Ok((id, cropped, if placed.same_placement(&transform) && (cw, ch) == (iw, ih) { None } else { Some(placed) }))
    }

    /// Runs a filter on the active layer as one undo step. Settings that would change nothing (no
    /// distortion, no grain, identity levels) close without a step, as the reference's OK does.
    pub fn apply_filter(&mut self, kind: Kind, settings: &Settings) -> Result<()> {
        let s = settings.normalized();
        let identity = match kind {
            Kind::LensCorrection => s.distortion == 0.0,
            Kind::Grain => s.grain.amount == 0.0,
            Kind::Levels => s.levels.is_identity(),
            Kind::Curves => s.curves.is_identity(),
            Kind::ColorBalance => s.balance.is_identity(),
            Kind::Fade => s.fade >= 1.0,
            Kind::Exposure => s.exposure.is_identity(),
            Kind::HueSaturation => s.hue_saturation.is_identity(),
            _ => false,
        };
        if identity { self.clear_preview(); return Ok(()); }
        if kind == Kind::RemoveBackground { return self.remove_background(&s.matte, true); }
        if self.active_mask().is_some() {
            let (id, mask) = self.filtered_mask(kind, settings)?;
            self.renderer.end_mask_preview(id);
            self.begin_edit(&format!("{} Mask", kind.name()));
            self.renderer.set_mask(id, Some(mask));
            self.end_edit();
            return Ok(());
        }
        let (id, result, placed) = self.filtered(kind, settings)?;
        self.renderer.set_preview(id, None);
        // Fade can blend this result back toward what was there, as long as the grid did not change.
        let before = self.renderer.image(id).cloned();
        let fade = match (placed.is_none(), before, kind) { (true, Some(b), k) if k != Kind::Fade => Some((id, b, result.clone(), kind.name().to_string())), _ => None };
        self.last_filter = None;
        self.begin_edit(if kind == Kind::Fade { "Fade" } else { kind.name() });
        if let Some(placed) = placed {
            // The layer's mask, covering the old grid, is carried onto the new one with its edge tone beyond.
            let layer = self.renderer.layer(id).clone();
            let carried = if layer.mask_file.is_some() && layer.mask_placement.is_none() { self.renderer.mask_on_grid(id, &placed, result.width(), result.height())? } else { None };
            self.renderer.set_image(id, result);
            self.renderer.set_layer_transform(id, placed);
            if let Some(mask) = carried { self.renderer.set_mask(id, Some(mask)); }
        } else {
            self.renderer.set_image(id, result);
        }
        self.end_edit();
        // Offered until the next edit of any kind.
        self.last_filter = fade.map(|(id, b, a, name)| (id, b, a, name, self.edit_serial));
        Ok(())
    }

    /// Shows what the filter would do without committing it.
    pub fn preview_filter(&mut self, kind: Kind, settings: &Settings) -> Result<()> {
        if kind == Kind::RemoveBackground { return self.remove_background(&settings.matte, false); }
        if self.active_mask().is_some() {
            let (id, mask) = self.filtered_mask(kind, settings)?;
            let (w, h) = (mask.width(), mask.height());
            let preview = self.renderer.begin_mask_preview(id, w, h)?;
            let rows = with_bytes(&mask, |d, _| d.to_vec())?;
            crate::raster::with_bytes_raw_mut(&preview, |d, _| d.copy_from_slice(&rows))?;
            return self.renderer.mask_preview_changed(id, (0, 0, w, h));
        }
        let (id, result, placed) = self.filtered(kind, settings)?;
        match placed {
            Some(t) => self.renderer.set_preview_placed(id, result, t),
            None => self.renderer.set_preview(id, Some(result)),
        }
        Ok(())
    }

    pub fn clear_preview(&mut self) {
        if let Some(id) = self.active { self.renderer.set_preview(id, None); self.renderer.end_mask_preview(id); }
    }

    /// Ctrl+I: inverts the active layer's colors (transparency kept) or its mask, inside the selection.
    pub fn invert(&mut self) -> Result<()> { self.apply_filter(Kind::Invert, &Settings::default()) }

    /// Auto Tone, Auto Contrast and Auto Color (Ctrl+Shift+L, Ctrl+Alt+Shift+L, Ctrl+Shift+B): Levels set
    /// from the layer's histogram, applied at once.
    pub fn auto_levels(&mut self, mode: AutoLevels) -> Result<()> {
        let histogram = self.histogram()?;
        let mut settings = Settings::default();
        settings.levels = match mode { AutoLevels::Tone => filters::Levels::auto_tone(&histogram), AutoLevels::Contrast => filters::Levels::auto_contrast(&histogram), AutoLevels::Color => filters::Levels::auto_color(&histogram) };
        self.apply_filter(Kind::Levels, &settings)
    }

    // MARK: Remove Background

    /// The subject mask for the active layer, refined by `settings`, previewed as transparency or committed as
    /// a layer mask that hides the background (what the layer already masks stays hidden: `subjectMask`).
    pub fn remove_background(&mut self, settings: &crate::matte::MatteSettings, commit: bool) -> Result<()> {
        let Some((id, image)) = self.active_image() else { bail!("Select an image layer first.") };
        if self.matte_cache.as_ref().is_none_or(|(k, _)| k.to_raw_none() != image.to_raw_none()) {
            let mask = crate::matte::subject_mask(&image)?;
            if mask.iter().all(|v| *v < 0.05) { bail!("No foreground subject was detected in this layer. Try an image with a more distinct subject."); }
            self.matte_cache = Some((image.clone(), mask));
        }
        let raw = self.matte_cache.as_ref().unwrap().1.clone();
        let (w, h) = (image.width() as usize, image.height() as usize);
        let refined = crate::matte::refine(&raw, &image, settings, if commit { usize::MAX / 2 } else { 1400 })?;
        if commit {
            self.renderer.set_preview(id, None);
            let mut levels = refined;
            // Both masks hide: what the old one hid, wherever it was placed, stays hidden.
            let layer = self.renderer.layer(id).clone();
            if self.renderer.mask(id).is_some() && layer.mask_enabled() {
                if let Some(existing) = self.renderer.mask_on_grid(id, &layer.transform, w as i32, h as i32)? {
                    with_bytes(&existing, |d, stride| { for y in 0..h { for x in 0..w { levels[y * w + x] *= d[y * stride + x] as f32 / 255.0; } } })?;
                }
            }
            let mask = crate::matte::a8_from_levels(&levels, w, h)?;
            self.begin_edit("Remove Background");
            self.renderer.set_mask(id, Some(mask));
            self.renderer.set_mask_placement(id, None);
            self.renderer.set_mask_enabled(id, true);
            self.end_edit();
        } else {
            let mask = crate::matte::a8_from_levels(&refined, w, h)?;
            let shown = new_argb(w as i32, h as i32)?;
            let cr = Context::new(&shown)?;
            cr.set_source_surface(&image, 0.0, 0.0)?;
            cr.mask_surface(&mask, 0.0, 0.0)?;
            drop(cr);
            self.renderer.set_preview(id, Some(shown));
        }
        Ok(())
    }

    // MARK: Generative Fill

    /// The context window a generation works in: the selection's bounds grown by half their size each way
    /// (at least 96 pixels), kept on the canvas.
    pub fn genfill_window(&self) -> Result<(i32, i32, i32, i32)> {
        let Some((x0, y0, x1, y1)) = self.selection.as_ref().and_then(|s| s.bounds) else { bail!("Select the area to fill first.") };
        let (w, h) = ((x1 - x0) as f64, (y1 - y0) as f64);
        let (mx, my) = ((w * crate::genfill::CONTEXT).max(crate::genfill::MIN_CONTEXT), (h * crate::genfill::CONTEXT).max(crate::genfill::MIN_CONTEXT));
        let gx0 = ((x0 as f64 - mx).floor() as i32).max(0);
        let gy0 = ((y0 as f64 - my).floor() as i32).max(0);
        let gx1 = ((x1 as f64 + mx).ceil() as i32).min(self.width());
        let gy1 = ((y1 as f64 + my).ceil() as i32).min(self.height());
        Ok((gx0, gy0, gx1, gy1))
    }

    /// What goes to the model: the window's pixels (every visible layer, or the active layer alone) and the
    /// selection as a white-on-black mask, both scaled to at most `MAX_SIDE`, as PNGs; and the scale used.
    pub fn genfill_inputs(&mut self, composite: bool) -> Result<(Vec<u8>, Vec<u8>, (i32, i32, i32, i32), f64)> {
        let window = self.genfill_window()?;
        let (gx0, gy0, gx1, gy1) = window;
        let (w, h) = (gx1 - gx0, gy1 - gy0);
        if w < 1 || h < 1 { bail!("The selection is off the canvas."); }
        let scale = (crate::genfill::MAX_SIDE as f64 / w.max(h) as f64).min(1.0);
        let (sw, sh) = (((w as f64 * scale).round() as i32).max(1), ((h as f64 * scale).round() as i32).max(1));
        // The pixels, over an opaque neutral so transparency does not read as black to the model.
        let image = new_argb(sw, sh)?;
        {
            let cr = Context::new(&image)?;
            cr.set_source_rgb(0.5, 0.5, 0.5);
            cr.paint()?;
            cr.scale(sw as f64 / w as f64, sh as f64 / h as f64);
            cr.translate(-(gx0 as f64), -(gy0 as f64));
            cr.rectangle(gx0 as f64, gy0 as f64, w as f64, h as f64);
            cr.clip();
            if composite { self.renderer.draw(&cr)?; }
            else if let Some(id) = self.active { if !self.renderer.layer(id).is_group() { self.renderer.draw_layer_plain(id, &cr)?; } }
        }
        let mask = new_argb(sw, sh)?;
        {
            let cr = Context::new(&mask)?;
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.paint()?;
            cr.scale(sw as f64 / w as f64, sh as f64 / h as f64);
            cr.translate(-(gx0 as f64), -(gy0 as f64));
            cr.set_source_rgb(1.0, 1.0, 1.0);
            if let Some(sel) = &self.selection { cr.mask_surface(&sel.mask, 0.0, 0.0)?; }
        }
        Ok((crate::png_io::png_bytes(&image)?, crate::png_io::png_bytes(&mask)?, window, scale))
    }

    /// A generated image laid over `window` as a new layer above the active one, shown only through the
    /// selection (its mask), as one undo step. Returns the layer.
    pub fn apply_genfill(&mut self, png: &[u8], window: (i32, i32, i32, i32), name: &str) -> Result<Uuid> {
        let (surface, iw, ih) = Self::decode_image_bytes(png)?;
        let (gx0, gy0, gx1, gy1) = window;
        let (w, h) = (gx1 - gx0, gy1 - gy0);
        if w < 1 || h < 1 { bail!("the window is empty"); }
        // Back to the window's own pixels.
        let placed = new_argb(w, h)?;
        {
            let cr = Context::new(&placed)?;
            cr.scale(w as f64 / iw as f64, h as f64 / ih as f64);
            cr.set_source_surface(&surface, 0.0, 0.0)?;
            cr.source().set_filter(cairo::Filter::Good);
            cr.paint()?;
        }
        let transform = Transform { origin: crate::format::Point(gx0 as f64, gy0 as f64), size: crate::format::Size(w as f64, h as f64), rotation: 0.0, flip_x: false, flip_y: false, sampling: Default::default() };
        let mask = match &self.selection { Some(sel) => Some(sel.coverage_on_layer(&transform, w, h)?), None => None };
        let (index, parent) = self.insertion();
        let mut record = self.blank_record(self.unique_name(name), parent);
        record.transform = transform;
        record.image_file = Some(format!("{}.png", crate::format::upper(record.id)));
        if mask.is_some() { record.mask_file = Some(format!("{}.mask.png", crate::format::upper(record.id))); }
        let id = record.id;
        self.begin_edit(name);
        self.renderer.insert_layer(index, record, Some(placed), mask);
        self.select_layer(Some(id));
        self.end_edit();
        Ok(id)
    }

    /// Another variation into the layer a generation made: its pixels swapped, as one undo step.
    pub fn replace_genfill(&mut self, id: Uuid, png: &[u8], window: (i32, i32, i32, i32)) -> Result<()> {
        if !self.has_layer(id) { bail!("The layer is gone."); }
        let (surface, iw, ih) = Self::decode_image_bytes(png)?;
        let (w, h) = (window.2 - window.0, window.3 - window.1);
        let placed = new_argb(w, h)?;
        {
            let cr = Context::new(&placed)?;
            cr.scale(w as f64 / iw as f64, h as f64 / ih as f64);
            cr.set_source_surface(&surface, 0.0, 0.0)?;
            cr.source().set_filter(cairo::Filter::Good);
            cr.paint()?;
        }
        self.begin_edit("Generative Fill Variation");
        self.renderer.set_image(id, placed);
        self.end_edit();
        Ok(())
    }

    /// Generative Expand's first half: the canvas grows to `width` x `height` about `anchor`, and the new
    /// margin becomes the selection, ready to be filled.
    pub fn expand_canvas_for_fill(&mut self, width: i32, height: i32, anchor: usize) -> Result<()> {
        let (ow, oh) = (self.width(), self.height());
        if width < ow || height < oh { bail!("Generative Expand only grows the canvas; use Canvas Size to shrink it."); }
        if width == ow && height == oh { bail!("The canvas is already that size."); }
        if width as i64 * height as i64 > 100_000_000 { bail!("A canvas can hold up to 100 megapixels."); }
        self.edited("Generative Expand", |d| {
            d.canvas_size(width, height, anchor, None, None, "Generative Expand")?;
            let dx = (((width - ow) as f64) * (anchor % 3) as f64 / 2.0).floor();
            let dy = (((height - oh) as f64) * (anchor / 3) as f64 / 2.0).floor();
            let inside = Selection::from_shape(width, height, false, |cr| { cr.rectangle(dx, dy, ow as f64, oh as f64); cr.fill()?; Ok(()) })?;
            d.selection = Some(inside.inverted()?);
            Ok(())
        })?;
        Ok(())
    }

    // MARK: Eyedropper

    /// The active layer's pixels inside the selection (or all of them), as PNG bytes, with their place on the
    /// document: what Copy puts on the clipboard.
    pub fn copy_layer_pixels(&mut self) -> Result<Option<(Vec<u8>, (i32, i32, i32, i32))>> {
        match self.selected_pixels()? { Some((out, rect)) => Ok(Some((crate::png_io::png_bytes(&out)?, rect))), None => Ok(None) }
    }

    /// `copy_layer_pixels` with the picture scaled so neither side passes `max_side` (what an image model
    /// takes); the rectangle stays in document pixels, where the result lands.
    pub fn copy_layer_pixels_scaled(&mut self, max_side: usize) -> Result<Option<(Vec<u8>, (i32, i32, i32, i32))>> {
        let Some((out, rect)) = self.selected_pixels()? else { return Ok(None) };
        let (w, h) = (out.width() as f64, out.height() as f64);
        let scale = (max_side as f64 / w.max(h)).min(1.0);
        if scale >= 1.0 { return Ok(Some((crate::png_io::png_bytes(&out)?, rect))); }
        let (sw, sh) = (((w * scale).round() as i32).max(1), ((h * scale).round() as i32).max(1));
        let small = new_argb(sw, sh)?;
        {
            let cr = Context::new(&small)?;
            cr.scale(sw as f64 / w, sh as f64 / h);
            cr.set_source_surface(&out, 0.0, 0.0)?;
            cr.source().set_filter(cairo::Filter::Good);
            cr.paint()?;
        }
        Ok(Some((crate::png_io::png_bytes(&small)?, rect)))
    }

    /// The active layer's pixels inside the selection (or all of them), placed on the document, as a surface
    /// and the document rectangle it covers.
    fn selected_pixels(&mut self) -> Result<Option<(ImageSurface, (i32, i32, i32, i32))>> {
        let Some((id, image)) = self.active_image() else { bail!("Select an image layer first.") };
        let transform = self.renderer.layer(id).transform;
        let (bx0, by0, bx1, by1) = transform.bounds();
        let (mut x0, mut y0, mut x1, mut y1) = (bx0.floor().max(0.0) as i32, by0.floor().max(0.0) as i32, (bx1.ceil() as i32).min(self.width()), (by1.ceil() as i32).min(self.height()));
        if let Some(sel) = &self.selection {
            let Some(b) = sel.bounds else { return Ok(None) };
            x0 = x0.max(b.0 as i32); y0 = y0.max(b.1 as i32); x1 = x1.min(b.2 as i32); y1 = y1.min(b.3 as i32);
        }
        if x1 <= x0 || y1 <= y0 { return Ok(None); }
        let out = new_argb(x1 - x0, y1 - y0)?;
        {
            let cr = Context::new(&out)?;
            cr.translate(-(x0 as f64), -(y0 as f64));
            if let Some(sel) = &self.selection {
                cr.push_group();
                self.renderer.draw_layer_plain(id, &cr)?;
                cr.pop_group_to_source()?;
                cr.mask_surface(&sel.mask, 0.0, 0.0)?;
            } else { self.renderer.draw_layer_plain(id, &cr)?; }
        }
        let _ = image;
        Ok(Some((out, (x0, y0, x1 - x0, y1 - y0))))
    }

    /// Layer via Copy (Ctrl+J with a selection) or Layer via Cut (Ctrl+Shift+J): the selected pixels of the
    /// active layer on a new layer above it, in place; Cut clears them from the original.
    pub fn layer_via(&mut self, cut: bool) -> Result<Uuid> {
        let Some((surface, (x, y, w, h))) = self.selected_pixels()? else { bail!("Nothing is selected on this layer.") };
        let Some(source) = self.active else { bail!("Select a layer first.") };
        let (index, parent) = self.insertion();
        let mut record = self.blank_record(format!("{} copy", self.renderer.layer(source).name), parent);
        record.transform.origin = crate::format::Point(x as f64, y as f64);
        record.transform.size = crate::format::Size(w as f64, h as f64);
        record.image_file = Some(format!("{}.png", crate::format::upper(record.id)));
        let id = record.id;
        self.edited(if cut { "Layer Via Cut" } else { "Layer Via Copy" }, |d| {
            if cut && d.selection.is_some() { d.clear_selection(false)?; }
            d.renderer.insert_layer(index, record, Some(surface), None);
            d.select_layer(Some(id));
            Ok(id)
        })
    }

    // MARK: Free Transform

    /// Free Transform (Ctrl+T) of a selection: the selected pixels lift off the active layer onto a floating
    /// layer that the Move tool's handles scale, rotate and move. One undo step from here to the commit.
    pub fn begin_free_transform(&mut self) -> Result<()> {
        if self.floating.is_some() { return Ok(()); }
        if !self.selection.as_ref().is_some_and(|s| !s.is_empty()) { bail!("Make a selection first; without one, the Move tool's handles transform the whole layer."); }
        let Some((source, _)) = self.active_image() else { bail!("Select an image layer first.") };
        self.begin_edit("Free Transform");
        let id = match self.layer_via(true) { Ok(id) => id, Err(e) => { self.abort_edit(); return Err(e); } };
        self.renderer.set_layer_name(id, "Floating Selection".into());
        self.selection = None;
        self.floating = Some((id, source));
        Ok(())
    }

    /// Return: the floating pixels land on the layer they came from, and the step closes.
    pub fn commit_free_transform(&mut self) -> Result<()> {
        let Some((id, source)) = self.floating.take() else { return Ok(()) };
        if !self.has_layer(id) || !self.has_layer(source) { self.end_edit(); return Ok(()); }
        match self.merge_floating(id, source) {
            Ok(()) => { self.end_edit(); Ok(()) }
            // A merge that cannot be done leaves everything as it was before Ctrl+T (`ProjectError.tooLarge`).
            Err(error) => { self.abort_edit(); Err(error) }
        }
    }

    /// The floating pixels drawn onto the layer they came from, on that layer's own grid, which grows to
    /// hold whatever landed outside it (`FloatingMerge.merge`); nothing off the canvas is lost. The moved
    /// pixels stay selected.
    fn merge_floating(&mut self, float: Uuid, source: Uuid) -> Result<()> {
        let src = self.renderer.layer(source).clone();
        let Some(image) = self.renderer.image(source).cloned() else { bail!("The layer has no pixels.") };
        let (rw, rh) = (image.width(), image.height());
        let to_doc = crate::render::pixel_to_document(&src.transform, rw, rh);
        let to_raster = to_doc.try_invert()?;
        let (x0, y0, x1, y1) = self.renderer.layer(float).transform.bounds();
        let corners = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].map(|(x, y)| to_raster.transform_point(x, y));
        let (minx, miny) = corners.iter().fold((f64::MAX, f64::MAX), |a, c| (a.0.min(c.0), a.1.min(c.1)));
        let (maxx, maxy) = corners.iter().fold((f64::MIN, f64::MIN), |a, c| (a.0.max(c.0), a.1.max(c.1)));
        let (left, top) = ((-minx.floor()).max(0.0) as i32, (-miny.floor()).max(0.0) as i32);
        let (right, bottom) = ((maxx.ceil() - rw as f64).max(0.0) as i32, (maxy.ceil() - rh as f64).max(0.0) as i32);
        let (nw, nh) = (rw as i64 + left as i64 + right as i64, rh as i64 + top as i64 + bottom as i64);
        if nw > 30_000 || nh > 30_000 || nw * nh > 100_000_000 { bail!("The transformed pixels would need a layer over 30,000 pixels on a side or 100 megapixels. Scale them down and try again."); }
        let (nw, nh) = (nw as i32, nh as i32);
        let out = new_argb(nw, nh)?;
        {
            let cr = Context::new(&out)?;
            cr.set_source_surface(&image, left as f64, top as f64)?;
            cr.paint()?;
            // New raster point (x, y) is old raster point (x - left, y - top); through that, the document.
            let mut shift = cairo::Matrix::identity();
            shift.translate(-(left as f64), -(top as f64));
            let new_to_doc = cairo::Matrix::multiply(&shift, &to_doc);
            cr.transform(new_to_doc.try_invert()?);
            self.renderer.draw_layer_plain(float, &cr)?;
        }
        let mut t = src.transform;
        let mut grown_mask = None;
        if (nw, nh) != (rw, rh) {
            let scale = (t.size.0 / rw as f64, t.size.1 / rh as f64);
            let center = to_doc.transform_point(nw as f64 / 2.0 - left as f64, nh as f64 / 2.0 - top as f64);
            t.size = crate::format::Size(nw as f64 * scale.0, nh as f64 * scale.1);
            t.origin = crate::format::Point(center.0 - t.size.0 / 2.0, center.1 - t.size.1 / 2.0);
            // A mask on the layer's own grid grows with it, white where the new pixels land (`FloatingMerge`).
            if self.renderer.mask(source).is_some() && src.mask_placement.is_none() { grown_mask = self.grown_mask(source, &src.transform, &t, nw, nh)?; }
        }
        self.select_layer_pixels(float, Mode::Replace)?;
        self.renderer.set_layer_transform(source, t);
        self.renderer.set_image(source, out);
        if let Some(mask) = grown_mask { self.renderer.set_mask(source, Some(mask)); }
        self.renderer.remove_layer(float);
        self.select_layer(Some(source));
        Ok(())
    }

    /// Escape: everything goes back to how it was before Ctrl+T.
    pub fn cancel_free_transform(&mut self) {
        if self.floating.take().is_none() { return; }
        self.abort_edit();
    }

    /// Every visible layer composited into one, on a new layer above the active one (Stamp Visible,
    /// Ctrl+Alt+Shift+E); the layers stay.
    pub fn stamp_visible(&mut self) -> Result<Uuid> {
        if crate::format::visible_layers(self.renderer.layers()).is_empty() { bail!("No visible layers to stamp."); }
        let flat = self.renderer.render_flat()?;
        let (index, parent) = self.insertion();
        let mut record = self.blank_record(self.unique_name("Stamp"), parent);
        record.transform.size = crate::format::Size(self.width() as f64, self.height() as f64);
        record.image_file = Some(format!("{}.png", crate::format::upper(record.id)));
        let id = record.id;
        self.begin_edit("Stamp Visible");
        self.renderer.insert_layer(index, record, Some(flat), None);
        self.select_layer(Some(id));
        self.end_edit();
        Ok(id)
    }

    /// Merge Visible (Ctrl+Shift+E): the visible layers become one layer where the topmost of them was,
    /// named after it; hidden layers stay.
    pub fn merge_visible(&mut self) -> Result<Uuid> {
        let layers = self.renderer.layers().to_vec();
        let visible: std::collections::HashSet<Uuid> = crate::format::visible_layers(&layers).into_iter().collect();
        if visible.is_empty() { bail!("No visible layers to merge."); }
        let flat = self.renderer.render_flat()?;
        // Everything that shows goes (visible layers inside visible folders, and those folders); a hidden
        // layer inside a merged folder stays, moved up to the nearest folder that remains.
        let going: Vec<Uuid> = crate::format::entries(&layers).into_iter().filter(|e| e.visible).map(|e| e.layer.id).collect();
        let going_set: std::collections::HashSet<Uuid> = going.iter().copied().collect();
        let mut reparent = Vec::new();
        for l in &layers {
            if going_set.contains(&l.id) { continue; }
            let mut parent = l.parent_id;
            while let Some(p) = parent { if !going_set.contains(&p) { break; } parent = layers.iter().find(|x| x.id == p).and_then(|x| x.parent_id); }
            if parent != l.parent_id { reparent.push((l.id, parent)); }
        }
        let top = layers.iter().rposition(|l| going.contains(&l.id)).unwrap_or(0);
        let name = layers[top].name.clone();
        let below = layers[..top].iter().filter(|l| !going.contains(&l.id)).count();
        let mut record = self.blank_record(name, None);
        record.transform.size = crate::format::Size(self.width() as f64, self.height() as f64);
        record.image_file = Some(format!("{}.png", crate::format::upper(record.id)));
        let id = record.id;
        self.begin_edit("Merge Visible");
        for (id, parent) in reparent { self.renderer.set_layer_parent(id, parent); }
        for g in &going { self.renderer.remove_layer(*g); }
        self.renderer.insert_layer(below.min(self.renderer.layers().len()), record, Some(flat), None);
        self.select_layer(Some(id));
        self.end_edit();
        Ok(id)
    }

    /// Bring to Front / Send to Back among the layer's siblings (Ctrl+Shift+] and Ctrl+Shift+[).
    pub fn move_layer_to_end(&mut self, top: bool) {
        let Some(id) = self.active else { return };
        let layers = self.renderer.layers().to_vec();
        let Some(index) = layers.iter().position(|l| l.id == id) else { return };
        let parent = layers[index].parent_id;
        let target = if top { layers.iter().rposition(|l| l.parent_id == parent) } else { layers.iter().position(|l| l.parent_id == parent) };
        let Some(mut target) = target else { return };
        if target == index { return; }
        self.begin_edit(if top { "Bring to Front" } else { "Send to Back" });
        let mut current = index;
        while current != target {
            let next = if top { current + 1 } else { current - 1 };
            self.renderer.swap_layers(current, next);
            current = next;
            if target >= self.renderer.layers().len() { target = self.renderer.layers().len() - 1; }
        }
        self.end_edit();
    }

    /// Select All Layers (Ctrl+Alt+A): every top-level layer, the topmost active.
    pub fn select_all_layers(&mut self) {
        let ids: Vec<Uuid> = self.renderer.layers().iter().map(|l| l.id).collect();
        let Some(top) = ids.last().copied() else { return };
        self.select_layer(Some(top));
        self.selected = ids.into_iter().collect();
    }

    /// Hides or shows the active layer (Ctrl+,).
    pub fn toggle_visible(&mut self) {
        let Some(id) = self.active else { return };
        let visible = self.renderer.layer(id).is_visible;
        self.set_visible(id, !visible);
    }

    /// Desaturate (Ctrl+Shift+U): Hue/Saturation with saturation at -100 on the active layer.
    pub fn desaturate(&mut self) -> Result<()> {
        let mut settings = crate::filters::Settings::default();
        settings.hue_saturation.adjustments = vec![("Master".into(), [0.0, -100.0, 0.0])];
        self.apply_filter(crate::filters::Kind::HueSaturation, &settings)
    }

    /// The color at a document pixel as the canvas shows it (every visible layer) or on the active layer's
    /// own pixels; None outside the canvas or over transparency (`sampleCompositeColor`).
    pub fn sample_color(&mut self, x: f64, y: f64, all_layers: bool) -> Result<Option<[f64; 3]>> {
        let (px, py) = (x.floor(), y.floor());
        if px < 0.0 || py < 0.0 || px >= self.width() as f64 || py >= self.height() as f64 { return Ok(None); }
        let one = new_argb(1, 1)?;
        {
            let cr = Context::new(&one)?;
            cr.translate(-px, -py);
            cr.rectangle(px, py, 1.0, 1.0);
            cr.clip();
            if all_layers { self.renderer.draw(&cr)?; }
            else if let Some(id) = self.active { if !self.renderer.layer(id).is_group() { self.renderer.draw_layer_plain(id, &cr)?; } }
        }
        let p = with_bytes(&one, |d, _| [d[0], d[1], d[2], d[3]])?;
        if p[3] == 0 { return Ok(None); }
        let a = p[3] as f64;
        let un = |c: u8| ((c as f64).min(a) / a * 255.0).round() / 255.0;
        Ok(Some([un(p[2]), un(p[1]), un(p[0])]))
    }

    // MARK: Gradients

    /// A gradient from `start` to `end` (document pixels) over the active layer or its mask, inside the
    /// selection, previewed (`commit` false) or committed as one undo step (`fillGradient`). `colors` are the
    /// two ends with alpha; on a mask their gray is used. Linear runs start to end; radial is centered on
    /// start with end on its rim.
    /// A two-color gradient, as the first Gradient tool drew it: a linear or radial run from one color to
    /// the other. Kept for callers that only need that; `gradient_fill` takes any gradient and shape.
    pub fn gradient(&mut self, start: (f64, f64), end: (f64, f64), radial: bool, colors: [[f64; 4]; 2], opacity: f64, commit: bool) -> Result<()> {
        use crate::gradient::{AlphaStop, Gradient, Shape, Stop};
        let g = Gradient { stops: vec![Stop { position: 0.0, color: [colors[0][0], colors[0][1], colors[0][2]] }, Stop { position: 1.0, color: [colors[1][0], colors[1][1], colors[1][2]] }], alphas: vec![AlphaStop { position: 0.0, alpha: colors[0][3] }, AlphaStop { position: 1.0, alpha: colors[1][3] }] };
        self.gradient_fill(start, end, if radial { Shape::Radial } else { Shape::Linear }, &g, opacity, commit)
    }

    /// The Gradient tool: `gradient` laid from `start` to `end` in `shape` over the active layer (or its
    /// mask, in gray) inside the selection at `opacity`; a preview until `commit`.
    pub fn gradient_fill(&mut self, start: (f64, f64), end: (f64, f64), shape: crate::gradient::Shape, gradient: &crate::gradient::Gradient, opacity: f64, commit: bool) -> Result<()> {
        use crate::gradient::Shape;
        let Some(id) = self.active else { bail!("Select a layer first.") };
        if self.selection.as_ref().is_some_and(|s| s.is_empty()) { bail!("Nothing is selected."); }
        let layer = self.renderer.layer(id).clone();
        if layer.is_group() || layer.adjustment.is_some() { bail!("Select an image layer first."); }
        let mask_target = self.active_mask().is_some();
        let (w, h, grid, base): (i32, i32, Transform, ImageSurface) = if let Some((_, mask, grid)) = self.active_mask() {
            let (w, h) = if mask.width() == 1 && mask.height() == 1 { self.renderer.image_size(id).unwrap_or((grid.size.0.round().max(1.0) as i32, grid.size.1.round().max(1.0) as i32)) } else { (mask.width(), mask.height()) };
            let target = if (w, h) == (mask.width(), mask.height()) { mask } else { let v = with_bytes(&mask, |d, _| d[0])?; crate::raster::a8_filled(w, h, v)? };
            (w, h, grid, gray_from_a8(&target)?)
        } else {
            let (w, h) = self.renderer.image_size(id).unwrap_or((layer.transform.size.0.round().max(1.0) as i32, layer.transform.size.1.round().max(1.0) as i32));
            let base = new_argb(w, h)?;
            if let Some(image) = self.renderer.image(id) { let cr = Context::new(&base)?; cr.set_source_surface(image, 0.0, 0.0)?; cr.paint()?; }
            (w, h, layer.transform, base)
        };
        let g = if mask_target { gradient.grayed() } else { gradient.normalized() };
        // Linear, radial and reflected are Cairo patterns; angle and diamond are rasterized in document space.
        let pattern: cairo::Pattern = match shape {
            Shape::Linear => { let p = cairo::LinearGradient::new(start.0, start.1, end.0, end.1); g.fill_pattern(&p, 0.0, 1.0); p.set_extend(cairo::Extend::Pad); cairo::Pattern::clone(&p) }
            Shape::Radial => { let r = (end.0 - start.0).hypot(end.1 - start.1).max(0.001); let p = cairo::RadialGradient::new(start.0, start.1, 0.0, start.0, start.1, r); g.fill_pattern(&p, 0.0, 1.0); p.set_extend(cairo::Extend::Pad); cairo::Pattern::clone(&p) }
            Shape::Reflected => { let p = cairo::LinearGradient::new(2.0 * start.0 - end.0, 2.0 * start.1 - end.1, end.0, end.1); g.reversed().fill_pattern(&p, 0.0, 0.5); g.fill_pattern(&p, 0.5, 1.0); p.set_extend(cairo::Extend::Pad); cairo::Pattern::clone(&p) }
            Shape::Angle | Shape::Diamond => { let raster = crate::gradient::raster(self.width(), self.height(), shape, start, end, &g)?; let p = cairo::SurfacePattern::create(&raster); p.set_filter(cairo::Filter::Good); p.set_extend(cairo::Extend::Pad); (*p).clone() }
        };
        let result = new_argb(w, h)?;
        {
            let cr = Context::new(&result)?;
            cr.set_source_surface(&base, 0.0, 0.0)?;
            cr.paint()?;
            // The gradient is defined on the document and drawn through the grid's placement.
            cr.transform(crate::selection::document_to_layer(&grid, w, h)?);
            cr.set_source(&pattern)?;
            match &self.selection {
                Some(sel) => {
                    cr.identity_matrix();
                    let coverage = sel.coverage_on_layer(&grid, w, h)?;
                    cr.push_group();
                    cr.transform(crate::selection::document_to_layer(&grid, w, h)?);
                    cr.set_source(&pattern)?;
                    cr.paint_with_alpha(opacity.clamp(0.0, 1.0))?;
                    cr.pop_group_to_source()?;
                    cr.mask_surface(&coverage, 0.0, 0.0)?;
                }
                None => cr.paint_with_alpha(opacity.clamp(0.0, 1.0))?,
            }
        }
        if mask_target {
            let a8 = a8_from_gray(&result)?;
            if commit {
                self.renderer.end_mask_preview(id);
                self.begin_edit("Gradient Mask");
                self.renderer.set_mask(id, Some(a8));
                self.end_edit();
            } else {
                let preview = self.renderer.begin_mask_preview(id, w, h)?;
                let rows = with_bytes(&a8, |d, _| d.to_vec())?;
                crate::raster::with_bytes_raw_mut(&preview, |d, _| d.copy_from_slice(&rows))?;
                self.renderer.mask_preview_changed(id, (0, 0, w, h))?;
            }
        } else if commit {
            self.renderer.set_preview(id, None);
            self.begin_edit("Gradient");
            self.renderer.set_image(id, result);
            self.end_edit();
        } else {
            self.renderer.set_preview(id, Some(result));
        }
        Ok(())
    }

    // MARK: Shape layers

    /// A rectangle (corners rounded by `radius`, at most half the shorter side) or ellipse filling `rect`
    /// (document pixels, whole), in `color`, on a new layer above the active one (`finishShape`).
    pub fn add_shape_layer(&mut self, ellipse: bool, rect: (f64, f64, f64, f64), color: [f64; 3], radius: f64) -> Result<Uuid> {
        let (x, y, w, h) = (rect.0.round(), rect.1.round(), rect.2.round(), rect.3.round());
        if w < 1.0 || h < 1.0 { bail!("Drag out a shape first."); }
        if w * h > 100_000_000.0 || w > 30_000.0 || h > 30_000.0 { bail!("That shape is too large. A shape can cover up to 100 megapixels."); }
        let kind = if ellipse { "Ellipse" } else { "Rectangle" };
        let image = shape_image(ellipse, w as i32, h as i32, color, radius)?;
        let style = serde_json::json!({"kind": kind, "red": color[0], "green": color[1], "blue": color[2], "cornerRadius": radius});
        let (index, parent) = self.insertion();
        let mut record = self.blank_record(self.unique_name(kind), parent);
        record.transform.origin = crate::format::Point(x, y);
        record.transform.size = crate::format::Size(w, h);
        record.image_file = Some(format!("{}.png", crate::format::upper(record.id)));
        record.shape = Some(style);
        let id = record.id;
        self.begin_edit(kind);
        self.renderer.insert_layer(index, record, Some(image), None);
        self.select_layer(Some(id));
        self.end_edit();
        Ok(id)
    }

    /// A shape layer scaled to a new size draws its shape again at that size, so a rounded corner keeps its
    /// radius instead of stretching (`redrawShape`). Part of the edit that changed the size.
    fn redraw_shape(&mut self, id: Uuid) -> Result<()> {
        let layer = self.renderer.layer(id).clone();
        let Some(style) = layer.shape.clone() else { return Ok(()) };
        let (w, h) = (layer.transform.size.0.round().max(1.0) as i32, layer.transform.size.1.round().max(1.0) as i32);
        if self.renderer.image_size(id) == Some((w, h)) || w as i64 * h as i64 > 100_000_000 { return Ok(()); }
        let n = |k: &str| style.get(k).and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        let kind = style.get("kind").and_then(serde_json::Value::as_str).unwrap_or("Rectangle");
        let image = if kind == "Path" {
            let Some(local) = anchors_from_json(&style) else { return Ok(()) };
            path_shape_image(&local, (n("baseWidth").max(1.0), n("baseHeight").max(1.0)), w, h, [n("red"), n("green"), n("blue")])?
        } else { shape_image(kind == "Ellipse", w, h, [n("red"), n("green"), n("blue")], n("cornerRadius"))? };
        // A mask on the layer's grid stays where it is while that grid changes size.
        if self.renderer.mask(id).is_some() && layer.mask_placement.is_none() { self.renderer.set_mask_placement(id, Some(layer.transform)); }
        self.renderer.set_shape_image(id, image, style);
        Ok(())
    }

    // MARK: Type layers

    /// Text set in `style`, rendered onto a new layer above the active one whose top-left corner is at
    /// (`x`, `y`) in document pixels. The style stays on the record so the text can be edited again.
    pub fn add_text_layer(&mut self, style: &crate::text::TextStyle, x: f64, y: f64) -> Result<Uuid> {
        let (image, _, _) = crate::text::render(style)?;
        let (w, h) = (image.width() as f64, image.height() as f64);
        let (index, parent) = self.insertion();
        let mut record = self.blank_record(self.unique_name(&text_layer_name(&style.text)), parent);
        record.name = text_layer_name(&style.text);
        record.transform.origin = crate::format::Point(x.round(), y.round());
        record.transform.size = crate::format::Size(w, h);
        record.image_file = Some(format!("{}.png", crate::format::upper(record.id)));
        record.text = Some(style.to_record());
        let id = record.id;
        self.begin_edit("Type");
        self.renderer.insert_layer(index, record, Some(image), None);
        self.select_layer(Some(id));
        self.end_edit();
        Ok(id)
    }

    /// The style a type layer was set in, or None for any other layer.
    pub fn text_style(&self, id: Uuid) -> Option<crate::text::TextStyle> {
        self.renderer.layer(id).text.as_ref().and_then(crate::text::TextStyle::from_record)
    }

    /// Sets a type layer's text again. The layer keeps its top-left corner and whatever scale a transform
    /// gave it; consecutive edits merge into one undo step ("Edit Text").
    pub fn set_text(&mut self, id: Uuid, style: &crate::text::TextStyle) -> Result<()> {
        let Some(old) = self.text_style(id) else { bail!("That is not a type layer.") };
        let old_name = text_layer_name(&old.text);
        let (image, _, _) = crate::text::render(style)?;
        let layer = self.renderer.layer(id).clone();
        let (rw, rh) = self.renderer.image_size(id).unwrap_or((image.width(), image.height()));
        let (sx, sy) = (layer.transform.size.0 / rw.max(1) as f64, layer.transform.size.1 / rh.max(1) as f64);
        let mut transform = layer.transform;
        transform.size = crate::format::Size((image.width() as f64 * sx).max(1.0), (image.height() as f64 * sy).max(1.0));
        self.begin_edit("Edit Text");
        if self.renderer.mask(id).is_some() && layer.mask_placement.is_none() { self.renderer.set_mask_placement(id, Some(layer.transform)); }
        self.renderer.set_layer_transform(id, transform);
        self.renderer.set_text_image(id, image, style.to_record());
        if layer.name == old_name || layer.name.starts_with("Type") { self.renderer.set_layer_name(id, text_layer_name(&style.text)); }
        self.end_edit_merging();
        Ok(())
    }

    /// The topmost visible type layer under a document point, for the Type tool's click.
    pub fn text_layer_at(&self, point: (f64, f64)) -> Option<Uuid> {
        let layers = self.renderer.layers();
        crate::format::entries_ordered(layers, true).into_iter().filter(|e| e.visible && e.layer.text.is_some() && e.layer.transform.contains(point)).map(|e| e.layer.id).next()
    }

    // MARK: Layer effects

    /// The layer's effects, or None when it has none.
    pub fn effects(&self, id: Uuid) -> Option<crate::effects::Effects> {
        self.renderer.layer(id).effects.as_ref().and_then(crate::effects::Effects::from_record)
    }

    /// The Layer Style dialog's session on one layer: changes preview straight on the renderer, outside
    /// the history, and `end_layer_style` makes them one "Layer Style" step (or puts the original back).
    /// One session at a time; a second call on the same layer is refused.
    pub fn begin_layer_style(&mut self, id: Uuid) -> Result<()> {
        if self.effects_preview.is_some() { bail!("A Layer Style window is already open."); }
        let layer = self.renderer.layer(id);
        if layer.is_group() || layer.adjustment.is_some() { bail!("Layer effects go on pixel layers."); }
        self.effects_preview = Some((id, layer.effects.clone()));
        Ok(())
    }

    /// Shows `effects` on the session's layer without touching the history.
    pub fn preview_effects(&mut self, effects: Option<&crate::effects::Effects>) {
        let Some((id, _)) = self.effects_preview else { return };
        if !self.has_layer(id) { return; }
        let record = effects.filter(|e| **e != crate::effects::Effects::default()).map(|e| e.to_record());
        if self.renderer.layer(id).effects != record { self.renderer.set_effects(id, record); }
    }

    /// Ends the session: the original comes back, and with `keep` the previewed effects become one step.
    pub fn end_layer_style(&mut self, keep: bool) {
        let Some((id, original)) = self.effects_preview.take() else { return };
        if !self.has_layer(id) { return; }
        let shown = self.renderer.layer(id).effects.clone();
        self.renderer.set_effects(id, original.clone());
        if keep && shown != original {
            self.begin_edit("Layer Style");
            self.renderer.set_effects(id, shown);
            self.end_edit();
        }
    }

    /// Sets (or with None clears) a layer's effects; consecutive changes merge into one "Layer Style" step.
    pub fn set_effects(&mut self, id: Uuid, effects: Option<&crate::effects::Effects>) -> Result<()> {
        let layer = self.renderer.layer(id);
        if layer.is_group() || layer.adjustment.is_some() { bail!("Layer effects go on pixel layers."); }
        let record = effects.filter(|e| **e != crate::effects::Effects::default()).map(|e| e.to_record());
        if layer.effects == record { return Ok(()); }
        self.begin_edit("Layer Style");
        self.renderer.set_effects(id, record);
        self.end_edit_merging();
        Ok(())
    }

    // MARK: Crop

    /// Crops the canvas to `rect` (document pixels), as one undo step (`commitCrop`).
    pub fn crop(&mut self, rect: (f64, f64, f64, f64)) -> Result<()> {
        let (x, y, w, h) = (rect.0.round(), rect.1.round(), rect.2.round(), rect.3.round());
        if !(1.0..=30_000.0).contains(&w) || !(1.0..=30_000.0).contains(&h) || x.abs() > 1_000_000.0 || y.abs() > 1_000_000.0 { bail!("Crop sizes run from 1 to 30,000 pixels per side."); }
        if w * h > 100_000_000.0 { bail!("A canvas can hold up to 100 megapixels."); }
        self.edited("Crop", |d| {
            d.canvas_size(w as i32, h as i32, 4, None, Some((-x, -y)), "Crop")?;
            d.selection = None;
            Ok(())
        })
    }

    /// Image > Rotate Canvas: every layer turned by `degrees` (clockwise positive) about the canvas
    /// center, the canvas grown to hold the turned picture (a quarter or half turn keeps it exact).
    pub fn rotate_canvas(&mut self, degrees: f64) -> Result<()> {
        if !degrees.is_finite() { bail!("Give an angle in degrees."); }
        let angle = degrees.rem_euclid(360.0);
        if angle == 0.0 { return Ok(()); }
        let (w, h) = (self.width() as f64, self.height() as f64);
        let (cx, cy) = (w / 2.0, h / 2.0);
        let rad = angle.to_radians();
        let (sn, cs) = rad.sin_cos();
        // The turned canvas corners, and the box around them: the new canvas.
        let corners = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)].map(|(x, y)| ((x - cx) * cs - (y - cy) * sn, (x - cx) * sn + (y - cy) * cs));
        let (nx0, ny0) = (corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min), corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min));
        let (nx1, ny1) = (corners.iter().map(|c| c.0).fold(f64::NEG_INFINITY, f64::max), corners.iter().map(|c| c.1).fold(f64::NEG_INFINITY, f64::max));
        // Rounded outward, so a fractional extent never clips the picture's edge (a hair under a whole
        // number, from floating point at a quarter turn, does not add a pixel).
        let (nw, nh) = ((nx1 - nx0 - 1e-6).ceil().max(1.0), (ny1 - ny0 - 1e-6).ceil().max(1.0));
        if nw > 30_000.0 || nh > 30_000.0 || nw * nh > 100_000_000.0 { bail!("The turned canvas would pass the 30,000-pixel side or 100-megapixel limit."); }
        let (ncx, ncy) = (nw / 2.0, nh / 2.0);
        let turn = |t: Transform| -> Transform {
            let c = t.center();
            let (dx, dy) = (c.0 - cx, c.1 - cy);
            let moved = (ncx + dx * cs - dy * sn, ncy + dx * sn + dy * cs);
            let mut out = t;
            // A flipped layer turns the other way, as its axes are mirrored.
            let sign = if t.flip_x != t.flip_y { -1.0 } else { 1.0 };
            // Kept in (-180, 180], as the inspector shows it.
            let r = (t.rotation + sign * angle).rem_euclid(360.0);
            out.rotation = if r > 180.0 { r - 360.0 } else { r };
            out.origin = crate::format::Point(moved.0 - t.size.0 / 2.0, moved.1 - t.size.1 / 2.0);
            out
        };
        self.begin_edit("Rotate Canvas");
        let depth = self.history.depth();
        let result = self.rotate_canvas_body(angle, rad, (cx, cy), (ncx, ncy), (nw, nh), &turn);
        if result.is_err() { self.unwind_edits_to(depth - 1); return result; }
        self.end_edit();
        Ok(())
    }

    fn rotate_canvas_body(&mut self, angle: f64, rad: f64, (cx, cy): (f64, f64), (ncx, ncy): (f64, f64), (nw, nh): (f64, f64), turn: &dyn Fn(Transform) -> Transform) -> Result<()> {
        let (sn, cs) = rad.sin_cos();
        for layer in self.renderer.layers().to_vec() {
            self.renderer.set_layer_transform(layer.id, turn(layer.transform));
            if let Some(p) = layer.mask_placement { self.renderer.set_mask_placement(layer.id, Some(turn(p))); }
            // An artboard's frame stays upright: it becomes the box around its turned corners (exact at a
            // quarter or half turn).
            if let Some(b) = layer.artboard.as_ref().filter(|_| layer.is_group()) {
                let pts = [(b.x, b.y), (b.x + b.width, b.y), (b.x + b.width, b.y + b.height), (b.x, b.y + b.height)].map(|(x, y)| { let (dx, dy) = (x - cx, y - cy); (ncx + dx * cs - dy * sn, ncy + dx * sn + dy * cs) });
                let (fx0, fy0) = (pts.iter().map(|p| p.0).fold(f64::INFINITY, f64::min), pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min));
                let (fx1, fy1) = (pts.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max), pts.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max));
                self.renderer.set_artboard(layer.id, Some(crate::format::Artboard { x: fx0.round(), y: fy0.round(), width: (fx1 - fx0).round().max(1.0), height: (fy1 - fy0).round().max(1.0), background: b.background }));
            }
        }
        let selection = self.selection.take();
        self.renderer.set_size(nw as i32, nh as i32);
        if let Some(sel) = selection {
            self.selection = Some(Selection::from_shape(nw as i32, nh as i32, sel.antialiased, |cr| { cr.translate(ncx, ncy); cr.rotate(rad); cr.translate(-cx, -cy); cr.set_source_surface(&sel.mask, 0.0, 0.0)?; cr.paint()?; Ok(()) })?);
        }
        // Guides turn with a quarter or half turn (vertical ones become horizontal); any other angle has no
        // upright line to keep.
        let (gv, gh) = (std::mem::take(&mut self.guides_v), std::mem::take(&mut self.guides_h));
        if (angle - 90.0).abs() < 1e-9 { self.guides_h = gv.iter().map(|x| ncy + (x - cx)).collect(); self.guides_v = gh.iter().map(|y| ncx - (y - cy)).collect(); }
        else if (angle - 270.0).abs() < 1e-9 { self.guides_h = gv.iter().map(|x| ncy - (x - cx)).collect(); self.guides_v = gh.iter().map(|y| ncx + (y - cy)).collect(); }
        else if (angle - 180.0).abs() < 1e-9 { self.guides_v = gv.iter().map(|x| ncx - (x - cx)).collect(); self.guides_h = gh.iter().map(|y| ncy - (y - cy)).collect(); }
        Ok(())
    }

    /// Image > Straighten: the canvas turned so a line from `a` to `b` (a horizon drawn with the Pen)
    /// comes level, then cropped to the largest upright rectangle of the same proportions inside it.
    pub fn straighten(&mut self, a: (f64, f64), b: (f64, f64)) -> Result<()> {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        if dx.hypot(dy) < 2.0 { bail!("Draw a line along the horizon first: two Pen points."); }
        let mut angle = dy.atan2(dx).to_degrees();
        // The nearer of level and upright.
        if angle > 45.0 { angle -= 90.0 } else if angle < -45.0 { angle += 90.0 }
        if angle.abs() < 0.01 { return Ok(()); }
        let (w, h) = (self.width() as f64, self.height() as f64);
        self.begin_edit("Straighten");
        if let Err(e) = self.rotate_canvas(-angle) { self.abort_edit(); return Err(e); }
        // The largest axis-aligned rectangle of the original proportions inside the turned picture.
        let rad = angle.abs().to_radians();
        let (sn, cs) = rad.sin_cos();
        let scale = if w >= h { let q = h / w; (q / (q * cs + sn)).min(1.0 / (cs + q * sn)) } else { let q = w / h; (q / (q * cs + sn)).min(1.0 / (cs + q * sn)) };
        let (cw, ch) = ((w * scale).floor().max(1.0), (h * scale).floor().max(1.0));
        let (nw, nh) = (self.width() as f64, self.height() as f64);
        let result = self.crop((((nw - cw) / 2.0).round(), ((nh - ch) / 2.0).round(), cw, ch));
        match result { Ok(()) => { self.end_edit(); Ok(()) } Err(e) => { self.abort_edit(); Err(e) } }
    }

    /// The flattened document as lossless WebP bytes.
    pub fn webp_bytes(&mut self) -> Result<Vec<u8>> {
        if self.width() > 16_383 || self.height() > 16_383 { bail!("WebP holds up to 16,383 pixels per side; this canvas is {} x {}. Export PNG, or use Export Sizes.", self.width(), self.height()); }
        let image = self.renderer.render_flat()?;
        let (rgba, w, h) = crate::png_io::straight_rgba(&image)?;
        let mut out = Vec::new();
        let encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut out);
        use image::ImageEncoder;
        encoder.write_image(&rgba, w as u32, h as u32, image::ExtendedColorType::Rgba8)?;
        Ok(out)
    }

    pub fn export_webp(&mut self, path: &std::path::Path) -> Result<()> { let bytes = self.webp_bytes()?; std::fs::write(path, bytes)?; Ok(()) }

    /// A single-frame GIF: 256 colors chosen for the picture, transparency where the composite is clear.
    pub fn export_gif(&mut self, path: &std::path::Path) -> Result<()> {
        let image = self.renderer.render_flat()?;
        let (rgba, w, h) = crate::png_io::straight_rgba(&image)?;
        let buffer = image::RgbaImage::from_raw(w as u32, h as u32, rgba).ok_or_else(|| anyhow::anyhow!("the picture could not be packed"))?;
        let mut out = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new_with_speed(&mut out, 10);
            encoder.encode_frame(image::Frame::new(buffer))?;
        }
        std::fs::write(path, out)?;
        Ok(())
    }

    /// AVIF through libavif's encoder (`avifenc`), which Omarchy ships: a PNG goes out beside the target
    /// and comes back compressed at `quality` (0 to 100; 100 is lossless).
    pub fn export_avif(&mut self, path: &std::path::Path, quality: f64) -> Result<()> {
        let encoder = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("avifenc")).find(|p| p.exists()));
        let Some(encoder) = encoder else { bail!("AVIF needs libavif's encoder: run `omarchy pkg add libavif`, then try again.") };
        let path = &std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let temp = path.with_extension(format!("avif-source-{}.png", std::process::id()));
        if let Err(e) = self.export_png(&temp) { let _ = std::fs::remove_file(&temp); return Err(e); }
        let q = quality.clamp(0.0, 100.0).round() as i64;
        let mut command = std::process::Command::new(encoder);
        if q >= 100 { command.arg("--lossless"); } else { command.args(["-q", &q.to_string(), "--speed", "6"]); }
        let output = command.arg(&temp).arg(path).output();
        let _ = std::fs::remove_file(&temp);
        let output = output.map_err(|e| anyhow::anyhow!("running avifenc: {e}"))?;
        if !output.status.success() { bail!("avifenc failed: {}", String::from_utf8_lossy(&output.stderr).trim()); }
        Ok(())
    }

    /// File > Export Layers: every visible pixel, type and shape layer as its own PNG in `folder`, as it
    /// shows (through its mask and layer style, at its opacity, with layers clipped to it), at its own
    /// bounds plus its style's reach (`trim`) or on the full canvas, numbered from the bottom. Returns
    /// the files written.
    pub fn export_layers(&mut self, folder: &std::path::Path, trim: bool) -> Result<Vec<std::path::PathBuf>> {
        std::fs::create_dir_all(folder)?;
        let (w, h) = (self.width(), self.height());
        let mut written = Vec::new();
        let layers: Vec<crate::format::Layer> = crate::format::visible_layers(self.renderer.layers()).into_iter().map(|id| self.renderer.layer(id).clone()).filter(|l| l.adjustment.is_none() && l.mask_source_id.is_none()).collect();
        for layer in layers.iter() {
            if !self.renderer.has_image(layer.id) { continue; }
            let reach = layer.effects.as_ref().and_then(crate::effects::Effects::from_record).map(|e| e.reach() as f64).unwrap_or(0.0);
            let (x0, y0, x1, y1) = if trim { let b = layer.transform.bounds(); ((b.0 - reach).floor().max(0.0) as i32, (b.1 - reach).floor().max(0.0) as i32, ((b.2 + reach).ceil() as i32).min(w), ((b.3 + reach).ceil() as i32).min(h)) } else { (0, 0, w, h) };
            if x1 <= x0 || y1 <= y0 { continue; }
            let surface = self.renderer.render_layer(layer.id, x0 as f64, y0 as f64, x1 - x0, y1 - y0, 1.0)?;
            let path = folder.join(format!("{:02}-{}.png", written.len() + 1, clean_file_name(&layer.name)));
            crate::png_io::encode(&surface, &path, self.renderer.resolution())?;
            written.push(path);
        }
        Ok(written)
    }

    /// How much of the canvas a layer's visible pixels cover (through its mask), as a fraction, with the
    /// box around them in document pixels; None when nothing shows. Judged at a small scale.
    pub fn layer_coverage(&mut self, id: Uuid) -> Result<Option<(f64, (f64, f64, f64, f64))>> {
        let (w, h) = (self.width() as f64, self.height() as f64);
        // A hairline can vanish at 256 pixels across; before calling the layer empty, look closer.
        for side in [256.0, 2048.0] {
            let scale = (side / w.max(h)).min(1.0);
            let (sw, sh) = (((w * scale).ceil() as i32).max(1), ((h * scale).ceil() as i32).max(1));
            let surface = self.renderer.render_layer(id, 0.0, 0.0, sw, sh, scale)?;
            let mut count = 0usize;
            let mut b = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
            crate::raster::with_bytes(&surface, |data, stride| {
                for y in 0..sh { for x in 0..sw {
                    if data[y as usize * stride + x as usize * 4 + 3] > 0 { count += 1; b.0 = b.0.min(x); b.1 = b.1.min(y); b.2 = b.2.max(x); b.3 = b.3.max(y); }
                } }
            })?;
            if count == 0 { if scale >= 1.0 { return Ok(None); } continue; }
            let fraction = count as f64 / (sw as f64 * sh as f64);
            return Ok(Some((fraction, (b.0 as f64 / scale, b.1 as f64 / scale, (b.2 + 1) as f64 / scale, (b.3 + 1) as f64 / scale))));
        }
        Ok(None)
    }

    /// What crop edges snap to: the canvas edges and every visible layer's bounds, in whole pixels.
    pub fn crop_snap_targets(&self) -> (Vec<f64>, Vec<f64>) {
        let (w, h) = (self.width() as f64, self.height() as f64);
        let (mut xs, mut ys) = (vec![0.0, w], vec![0.0, h]);
        for id in crate::format::visible_layers(self.renderer.layers()) {
            if !self.renderer.has_image(id) { continue; }
            let (x0, y0, x1, y1) = self.renderer.layer(id).transform.bounds();
            xs.extend([x0.round(), x1.round()]);
            ys.extend([y0.round(), y1.round()]);
        }
        (xs, ys)
    }

    // MARK: Fill and clear

    /// Fills the selection (or the whole layer) with `color` on the active image layer, or with black or
    /// white on its mask when the mask is targeted, as one undo step (`fillSelection`).
    pub fn fill(&mut self, color: [f64; 3]) -> Result<()> { self.fill_with(color, 1.0) }

    /// `fill` at an opacity: on a mask the coverage is scaled by it, on pixels the color is painted at it.
    pub fn fill_with(&mut self, color: [f64; 3], alpha: f64) -> Result<()> {
        let alpha = alpha.clamp(0.0, 1.0);
        let Some(id) = self.active else { bail!("Select a layer first.") };
        if self.selection.as_ref().is_some_and(|s| s.is_empty()) { return Ok(()); }
        if let Some((_, mask, grid)) = self.active_mask() {
            let white = color[0] + color[1] + color[2] > 1.5;
            let (w, h) = if mask.width() == 1 && mask.height() == 1 { self.renderer.image_size(id).unwrap_or((grid.size.0.round().max(1.0) as i32, grid.size.1.round().max(1.0) as i32)) } else { (mask.width(), mask.height()) };
            let target = if (w, h) == (mask.width(), mask.height()) { mask } else { let v = with_bytes(&mask, |d, _| d[0])?; crate::raster::a8_filled(w, h, v)? };
            let coverage = match &self.selection { Some(sel) => Some(sel.coverage_on_layer(&grid, w, h)?), None => None };
            let value = if white { 255 } else { 0 };
            let mut cov = coverage.map(|c| with_bytes(&c, |d, stride| (0..h as usize).flat_map(|y| d[y * stride..y * stride + w as usize].to_vec()).collect::<Vec<u8>>())).transpose()?;
            if alpha < 1.0 { let full = cov.take().unwrap_or_else(|| vec![255u8; w as usize * h as usize]); cov = Some(full.into_iter().map(|c| (c as f64 * alpha).round() as u8).collect()); }
            let filled = crate::raster::a8_filled(w, h, value)?;
            let result = if let Some(cov) = cov {
                let (wu, hu) = (w as usize, h as usize);
                let original = with_bytes(&target, |d, stride| (0..hu).flat_map(|y| d[y * stride..y * stride + wu].to_vec()).collect::<Vec<u8>>())?;
                let stride = cairo::Format::A8.stride_for_width(w as u32)? as usize;
                let mut data = vec![0u8; stride * hu];
                for y in 0..hu { for x in 0..wu { let c = cov[y * wu + x] as u32; let o = original[y * wu + x] as u32; data[y * stride + x] = ((value as u32 * c + o * (255 - c) + 127) / 255) as u8; } }
                crate::raster::a8_from_data(w, h, data, stride as i32)?
            } else { filled };
            self.begin_edit("Fill Mask");
            self.renderer.set_mask(id, Some(result));
            self.end_edit();
            return Ok(());
        }
        let layer = self.renderer.layer(id).clone();
        if layer.is_group() || layer.adjustment.is_some() { bail!("Select an image layer first."); }
        let (w, h) = self.renderer.image_size(id).unwrap_or((layer.transform.size.0.round().max(1.0) as i32, layer.transform.size.1.round().max(1.0) as i32));
        let result = new_argb(w, h)?;
        {
            let cr = Context::new(&result)?;
            if let Some(image) = self.renderer.image(id) { cr.set_source_surface(image, 0.0, 0.0)?; cr.paint()?; }
            cr.set_source_rgba(color[0], color[1], color[2], alpha);
            match &self.selection {
                Some(sel) => { let coverage = sel.coverage_on_layer(&layer.transform, w, h)?; cr.mask_surface(&coverage, 0.0, 0.0)?; }
                None => cr.paint()?,
            }
        }
        self.begin_edit("Fill");
        self.renderer.set_image(id, result);
        self.end_edit();
        Ok(())
    }

    /// Fills a Pen path on the active image layer in `color` (Fill Path).
    pub fn fill_path(&mut self, path: &crate::path::Path, color: [f64; 3]) -> Result<()> {
        if path.anchors.len() < 2 { bail!("Draw a path first."); }
        if self.active_mask().is_some() {
            // The mask is the target: the path becomes the area Fill paints white or black on it.
            let area = path.selection(self.width(), self.height())?;
            self.begin_edit("Fill Path");
            let saved = self.selection.replace(area);
            let result = self.fill(color);
            self.selection = saved;
            match result { Ok(()) => { self.end_edit(); return Ok(()) } Err(e) => { self.abort_edit(); return Err(e) } }
        }
        let Some((id, image)) = self.active_image() else { bail!("Select an image layer first.") };
        let transform = self.renderer.layer(id).transform;
        let (w, h) = (image.width(), image.height());
        let result = new_argb(w, h)?;
        {
            let cr = Context::new(&result)?;
            cr.set_source_surface(&image, 0.0, 0.0)?;
            cr.paint()?;
            cr.transform(crate::selection::document_to_layer(&transform, w, h)?);
            cr.set_source_rgb(color[0], color[1], color[2]);
            let closed = crate::path::Path { anchors: path.anchors.clone(), closed: true };
            closed.trace(&cr, |p| p);
            cr.fill()?;
        }
        self.begin_edit("Fill Path");
        self.renderer.set_image(id, result);
        self.end_edit();
        Ok(())
    }

    /// Paints along a Pen path with the brush (Stroke Path with the Brush tool).
    pub fn stroke_path(&mut self, path: &crate::path::Path, settings: &crate::brush::BrushSettings) -> Result<()> {
        if path.anchors.len() < 2 { bail!("Draw a path first."); }
        let step = (settings.diameter * settings.spacing.unwrap_or(0.25).max(0.02) / 2.0).clamp(0.5, 20.0);
        let points = path.flatten(step);
        if self.active_mask().is_some() {
            // On a mask the brush paints white or black, whichever the color is nearer.
            let white = settings.color[0] + settings.color[1] + settings.color[2] > 1.5;
            return self.replay_mask_stroke(&points, settings, white);
        }
        self.replay_stroke(&points, settings, StrokeKind::Paint)
    }

    /// A whole stroke on the active mask at once, as `replay_stroke` does for pixels.
    pub fn replay_mask_stroke(&mut self, points: &[(f64, f64)], settings: &crate::brush::BrushSettings, white: bool) -> Result<()> {
        let Some((first, rest)) = points.split_first() else { return Ok(()) };
        self.begin_mask_stroke(*first, settings, white)?;
        let Some(id) = self.active else { return Ok(()) };
        if let Some(mut stroke) = self.stroke.take() {
            let result = stroke.replay(rest).and_then(|_| self.sync_mask_preview(id, &stroke));
            self.stroke = Some(stroke); self.stroke_layer = Some(id);
            if let Err(e) = result { self.cancel_stroke(); return Err(e); }
        }
        self.finish_stroke()
    }

    /// Whether an edit is in progress that a tool call should not run across: a stroke or warp mid-drag,
    /// a Layer Style window previewing, or an open history edit (Free Transform).
    pub fn busy_editing(&self) -> bool { self.stroke.is_some() || self.warp.is_some() || self.effects_preview.is_some() || self.pixel_move.is_some() || self.history.is_editing() }
    /// A live preview holds the document: a stroke, a warp, a pixel drag or the Layer Style window. Menu
    /// commands wait for it (they would snapshot the preview into their undo step).
    pub fn preview_busy(&self) -> bool { self.stroke.is_some() || self.warp.is_some() || self.effects_preview.is_some() || self.pixel_move.is_some() }

    /// Marching ants from a Pen path (Make Selection, Ctrl+Return); an open path closes itself.
    pub fn select_path(&mut self, path: &crate::path::Path, mode: Mode) -> Result<()> {
        if path.anchors.len() < 3 { bail!("A selection needs at least three points."); }
        let shape = path.selection(self.width(), self.height())?;
        let combined = match (&self.selection, mode) { (Some(current), m) if m != Mode::Replace => current.combined(&shape, m)?, _ => shape };
        self.set_selection(Some(combined), "Path Selection");
        Ok(())
    }

    /// Delete with a selection: the selected pixels become transparent; on a mask they take the background
    /// (hide) tone (`clearSelectedPixels`). Nothing happens without a selection.
    pub fn clear_selection(&mut self, mask_background_white: bool) -> Result<()> {
        let Some(selection) = self.selection.clone() else { return Ok(()) };
        if selection.is_empty() { return Ok(()); }
        if self.active_mask().is_some() { return self.fill(if mask_background_white { [1.0; 3] } else { [0.0; 3] }); }
        let Some((id, image)) = self.active_image() else { bail!("Select an image layer first.") };
        let transform = self.renderer.layer(id).transform;
        let (w, h) = (image.width(), image.height());
        let result = new_argb(w, h)?;
        {
            let cr = Context::new(&result)?;
            cr.set_source_surface(&image, 0.0, 0.0)?;
            cr.paint()?;
            let coverage = selection.coverage_on_layer(&transform, w, h)?;
            cr.set_operator(cairo::Operator::DestOut);
            cr.set_source_rgba(0.0, 0.0, 0.0, 1.0);
            cr.mask_surface(&coverage, 0.0, 0.0)?;
        }
        self.begin_edit("Clear");
        self.renderer.set_image(id, result);
        self.end_edit();
        Ok(())
    }

    // MARK: Merging

    /// What Ctrl+E merges (`mergePlan`): a folder merges its contents and goes; a layer merges with the
    /// layer beneath it in the same folder. Ids in stacking order, the ids that go, the result's name, parent
    /// and place, and the undo name.
    fn merge_plan(&self) -> Option<(Vec<Uuid>, std::collections::HashSet<Uuid>, String, Option<Uuid>, Uuid, &'static str)> {
        let active = self.active?;
        let layers = self.renderer.layers();
        let record = self.renderer.layer(active);
        if self.selected.len() > 1 {
            let mut picked = self.selected.clone();
            for id in self.selected.clone() { picked.extend(self.descendants(id)); }
            let ordered: Vec<&crate::format::Layer> = layers.iter().filter(|l| picked.contains(&l.id)).collect();
            if !ordered.iter().any(|l| !l.is_group()) { return None; }
            // The topmost selected layer whose folder is not itself going: the result takes its place.
            let top = ordered.iter().rev().find(|l| self.selected.contains(&l.id) && l.parent_id.is_none_or(|p| !picked.contains(&p)))?;
            return Some((ordered.iter().map(|l| l.id).collect(), picked, top.name.clone(), top.parent_id, top.id, "Merge Layers"));
        }
        if record.is_group() {
            let inside = self.descendants(active);
            if !layers.iter().any(|l| inside.contains(&l.id) && !l.is_group()) { return None; }
            let ids: Vec<Uuid> = layers.iter().filter(|l| inside.contains(&l.id) || l.id == active).map(|l| l.id).collect();
            let removed = ids.iter().copied().collect();
            return Some((ids, removed, record.name.clone(), record.parent_id, active, "Merge Group"));
        }
        let index = self.renderer.layer_index(active)?;
        let below = layers[..index].iter().rev().find(|l| l.parent_id == record.parent_id)?;
        if below.is_group() || below.adjustment.is_some() || record.adjustment.is_some() { return None; }
        Some((vec![below.id, active], [below.id, active].into_iter().collect(), below.name.clone(), record.parent_id, active, "Merge Down"))
    }

    fn descendants(&self, id: Uuid) -> std::collections::HashSet<Uuid> {
        let mut out = std::collections::HashSet::new();
        let mut frontier = vec![id];
        while let Some(current) = frontier.pop() {
            for l in self.renderer.layers() { if l.parent_id == Some(current) && out.insert(l.id) { frontier.push(l.id); } }
        }
        out
    }

    pub fn can_merge(&self) -> bool { self.merge_plan().is_some() }
    pub fn merge_title(&self) -> &'static str { self.merge_plan().map_or("Merge Down", |p| p.5) }

    /// Ctrl+E: the layers composited as the canvas shows them (blend modes, opacity, masks, clipping and
    /// adjustments baked in) into one pixel layer, trimmed to what is there, in their place (`mergeLayers`).
    pub fn merge_layers(&mut self) -> Result<()> {
        let Some((ids, removed, name, parent, anchor, action)) = self.merge_plan() else { return Ok(()) };
        let kept: std::collections::HashSet<Uuid> = ids.iter().copied().collect();
        // Only the merged layers, cut loose from anything outside the merge.
        let subset: Vec<crate::format::Layer> = self.renderer.layers().iter().filter(|l| kept.contains(&l.id)).cloned().map(|mut l| {
            if l.parent_id.is_some_and(|p| !kept.contains(&p)) { l.parent_id = None; }
            if l.mask_source_id.is_some_and(|s| !kept.contains(&s)) { l.mask_source_id = None; }
            l
        }).collect();
        let images = self.renderer.images().iter().filter(|(k, _)| kept.contains(k)).map(|(k, v)| (*k, v.clone())).collect();
        let masks = self.renderer.masks().iter().filter(|(k, _)| kept.contains(k)).map(|(k, v)| (*k, v.clone())).collect();
        let mut manifest = self.manifest();
        manifest.layers = subset;
        manifest.active_layer_id = None;
        let mut flat = render::Renderer::new(Project { path: std::path::PathBuf::new(), manifest, images, masks })?;
        let full = flat.render_flat()?;
        let (w, h) = (full.width(), full.height());
        let bounds = with_bytes(&full, |d, stride| crate::ffi::alpha_bounds(d, w as usize, h as usize, stride))?;
        let (x0, y0, x1, y1) = bounds.unwrap_or((0, 0, 1, 1));
        let (cw, ch) = ((x1 - x0).max(1) as i32, (y1 - y0).max(1) as i32);
        let trimmed = new_argb(cw, ch)?;
        {
            let cr = Context::new(&trimmed)?;
            cr.set_source_surface(&full, -(x0 as f64), -(y0 as f64))?;
            cr.set_operator(cairo::Operator::Source);
            cr.paint()?;
        }
        let mut merged = self.blank_record(name, parent);
        merged.transform.origin = crate::format::Point(x0 as f64, y0 as f64);
        merged.transform.size = crate::format::Size(cw as f64, ch as f64);
        merged.image_file = Some(format!("{}.png", crate::format::upper(merged.id)));
        let merged_id = merged.id;
        let layers = self.renderer.layers();
        let slot = self.renderer.layer_index(anchor).unwrap_or(layers.len());
        let insertion = slot - layers[..slot].iter().filter(|l| removed.contains(&l.id)).count();
        // The arrangement the merge would leave, checked before anything changes (`LayerHierarchy.validate`).
        let mut proposed: Vec<crate::format::Layer> = layers.iter().filter(|l| !removed.contains(&l.id)).cloned().collect();
        for l in proposed.iter_mut() { if l.mask_source_id.is_some_and(|s| removed.contains(&s)) { l.mask_source_id = Some(merged_id); } }
        proposed.insert(insertion.min(proposed.len()), merged.clone());
        crate::format::validate::hierarchy(&proposed).map_err(|_| anyhow::anyhow!("These layers cannot be merged as they are arranged."))?;
        self.begin_edit(action);
        // Layers clipped to anything that was merged now clip to the result.
        let reclip: Vec<Uuid> = self.renderer.layers().iter().filter(|l| l.mask_source_id.is_some_and(|s| removed.contains(&s)) && !removed.contains(&l.id)).map(|l| l.id).collect();
        for id in removed.iter() { self.renderer.remove_layer(*id); }
        self.renderer.insert_layer(insertion, merged, Some(trimmed), None);
        for id in reclip { self.renderer.set_mask_source(id, Some(merged_id)); }
        self.select_layer(Some(merged_id));
        self.end_edit();
        Ok(())
    }

    // MARK: Flip canvas

    /// Flips the whole canvas: every layer, placed mask and the selection mirrored across its middle
    /// (`flipCanvas`).
    pub fn flip_canvas(&mut self, horizontally: bool) -> Result<()> {
        let (w, h) = (self.width(), self.height());
        let axis = if horizontally { w as f64 / 2.0 } else { h as f64 / 2.0 };
        self.edited(if horizontally { "Flip Canvas Horizontal" } else { "Flip Canvas Vertical" }, |d| {
            for layer in d.renderer.layers().to_vec() {
                d.renderer.set_layer_transform(layer.id, layer.transform.mirrored(horizontally, axis));
                if let Some(p) = layer.mask_placement { d.renderer.set_mask_placement(layer.id, Some(p.mirrored(horizontally, axis))); }
            }
            // Artboard frames and guides mirror too.
            d.map_artboards(|(x, y, bw, bh)| if horizontally { (w as f64 - x - bw, y, bw, bh) } else { (x, h as f64 - y - bh, bw, bh) });
            if horizontally { for x in &mut d.guides_v { *x = w as f64 - *x; } } else { for y in &mut d.guides_h { *y = h as f64 - *y; } }
            if let Some(selection) = d.selection.clone() {
                d.selection = Some(Selection::from_shape(w, h, selection.antialiased, |cr| {
                    if horizontally { cr.translate(w as f64, 0.0); cr.scale(-1.0, 1.0); } else { cr.translate(0.0, h as f64); cr.scale(1.0, -1.0); }
                    cr.set_source_surface(&selection.mask, 0.0, 0.0)?;
                    cr.paint()?;
                    Ok(())
                })?);
            }
            Ok(())
        })
    }

    /// The active layer's histogram inside the selection, for the Levels dialog.
    pub fn histogram(&self) -> Result<[[f64; 256]; 4]> {
        if self.mask_target() { bail!("Levels reads the layer's pixels; target the layer rather than its mask."); }
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
    Dodge { burn: bool, range: u8 },
    Sponge { desaturate: bool },
    /// The Pattern Stamp: a saved pattern tiled from the document's origin, painted through the tip.
    Pattern { name: String },
}

impl Document {
    pub fn stroke_active(&self) -> bool { self.stroke.is_some() || self.warp.is_some() }

    /// The document region the latest change was confined to, if it was local; None means redraw everything.
    pub fn take_dirty(&mut self) -> Option<(f64, f64, f64, f64)> { self.dirty.take() }

    fn mark_dirty_grid(&mut self, stroke: &crate::brush::Stroke, rect: (i32, i32, i32, i32)) {
        let corners = [(rect.0, rect.1), (rect.2, rect.1), (rect.0, rect.3), (rect.2, rect.3)].map(|(x, y)| stroke.to_document.transform_point(x as f64, y as f64));
        let r = (corners.iter().map(|c| c.0).fold(f64::MAX, f64::min), corners.iter().map(|c| c.1).fold(f64::MAX, f64::min), corners.iter().map(|c| c.0).fold(f64::MIN, f64::max), corners.iter().map(|c| c.1).fold(f64::MIN, f64::max));
        self.dirty = Some(match self.dirty { Some(d) => (d.0.min(r.0), d.1.min(r.1), d.2.max(r.2), d.3.max(r.3)), None => r });
    }

    /// Starts a Smudge or Liquify stroke on the active layer's pixels (never its mask).
    pub fn begin_warp(&mut self, point: (f64, f64), settings: &crate::brush::BrushSettings, mode: crate::warp::WarpMode) -> Result<()> {
        if self.stroke_active() { bail!("a stroke is already in progress"); }
        let Some((id, _)) = self.active_image() else { bail!("Smudge and Liquify work on a layer's pixels; select an image layer first.") };
        if self.mask_target { bail!("Smudge and Liquify work on a layer's pixels, not its mask."); }
        let sample = self.sample(false)?;
        let surface = crate::raster::argb_from_packed(sample.width as i32, sample.height as i32, sample.pixels)?;
        let canvas = Transform { origin: crate::format::Point(0.0, 0.0), size: crate::format::Size(self.width() as f64, self.height() as f64), rotation: 0.0, flip_x: false, flip_y: false, sampling: crate::format::Sampling::High };
        let mut warp = crate::warp::Warp::new(surface.clone(), mode, settings);
        warp.append(point)?;
        self.renderer.set_preview_placed(id, surface, canvas);
        self.warp = Some(warp); self.stroke_layer = Some(id);
        Ok(())
    }

    fn continue_warp(&mut self, point: (f64, f64)) -> Result<()> {
        let Some(id) = self.stroke_layer.or(self.active) else { return Ok(()) };
        let Some(warp) = self.warp.as_mut() else { return Ok(()) };
        warp.append(point)?;
        if let Some(rect) = warp.changed {
            let r = (rect.0 as f64, rect.1 as f64, rect.2 as f64, rect.3 as f64);
            self.dirty = Some(match self.dirty { Some(d) => (d.0.min(r.0), d.1.min(r.1), d.2.max(r.2), d.3.max(r.3)), None => r });
            self.renderer.preview_changed(id, rect)?;
        }
        Ok(())
    }

    /// Paints the warp's result into the layer's pixels along the stroke, as one undo step (`finishWarp`).
    fn finish_warp(&mut self) -> Result<()> {
        let Some(warp) = self.warp.take() else { return Ok(()) };
        let Some(id) = self.stroke_layer.take().or(self.active) else { return Ok(()) };
        self.renderer.set_preview(id, None);
        if warp.points.is_empty() { return Ok(()); }
        let (w, h) = (warp.surface.width() as usize, warp.surface.height() as usize);
        let pixels = with_bytes(&warp.surface, |data, stride| { let mut out = vec![0u8; w * h * 4]; for y in 0..h { out[y * w * 4..(y + 1) * w * 4].copy_from_slice(&data[y * stride..y * stride + w * 4]); } out })?;
        let sample = std::rc::Rc::new(crate::brush::Sample { pixels, width: w, height: h, tiled: false });
        // A hard tip a little wider than the brush covers everything the stroke moved.
        let settings = crate::brush::BrushSettings { diameter: (warp.diameter + 4.0).min(2000.0), hardness: 1.0, color: [0.0; 3], opacity: 1.0, ..Default::default() };
        let layer = self.renderer.layer(id).clone();
        let size = self.renderer.image_size(id).unwrap_or((1, 1));
        let canvas = (self.width() as f64, self.height() as f64);
        let renderer = &mut self.renderer;
        let mut stroke = crate::brush::Stroke::new(true, size, &layer.transform, canvas, &settings, crate::brush::Kind::Clone { sample, offset: (0.0, 0.0), replaces: true }, self.selection.clone(), true,
            |x0, y0, gw, gh, transform| renderer.begin_preview(id, -x0, -y0, gw, gh, transform))?;
        stroke.replay(&warp.points)?;
        self.stroke = Some(stroke); self.stroke_layer = Some(id);
        self.stroke_mask = false;
        let name = warp.mode.name();
        self.finish_stroke_named(name)
    }

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
        Ok(crate::brush::Sample { pixels, width: wu, height: hu, tiled: false })
    }

    /// A type or shape layer, or a layer scaled unevenly (its pixels wider than tall on the canvas, or the
    /// other way), turned into plain pixels at the canvas's own scale, as one step: what painting needs.
    /// Layers that are already plain and evenly scaled are left alone.
    pub fn rasterize_for_painting(&mut self, id: Uuid) -> Result<()> {
        let layer = self.renderer.layer(id).clone();
        let Some((iw, ih)) = self.renderer.image_size(id) else { return Ok(()) };
        let t = layer.transform;
        let (sx, sy) = (t.size.0 / iw as f64, t.size.1 / ih as f64);
        let uneven = (sx - sy).abs() > 0.01 * sx.max(sy);
        let vector = layer.text.is_some() || layer.shape.is_some();
        if !uneven && !vector { return Ok(()); }
        if !uneven && t.rotation == 0.0 && !t.flip_x && !t.flip_y {
            // Only the record stands in the way: the pixels are already what shows.
            self.edited("Rasterize Layer", |d| { let image = d.renderer.image(id).cloned().ok_or_else(|| anyhow::anyhow!("no pixels"))?; d.renderer.set_image(id, image); Ok(()) })?;
            return Ok(());
        }
        self.edited("Rasterize Layer", |d| {
            let (bx0, by0, bx1, by1) = t.bounds();
            let (left, top) = (bx0.floor(), by0.floor());
            let (w, h) = ((bx1.ceil() - left).max(1.0) as i32, (by1.ceil() - top).max(1.0) as i32);
            if w as i64 * h as i64 > 100_000_000 { bail!("This layer is too large to rasterize (100 megapixels)."); }
            let surface = new_argb(w, h)?;
            { let cr = Context::new(&surface)?; cr.translate(-left, -top); d.renderer.draw_layer_plain(id, &cr)?; }
            if layer.mask_placement.is_none() && d.renderer.mask(id).is_some_and(|m| m.width() > 1 || m.height() > 1) {
                let a8 = crate::raster::a8_filled(w, h, 0)?;
                { let cr = Context::new(&a8)?; cr.translate(-left, -top); d.renderer.draw_mask_plain(id, &cr)?; }
                d.renderer.set_mask(id, Some(a8));
            }
            d.renderer.set_image(id, surface);
            d.renderer.set_layer_transform(id, Transform { origin: crate::format::Point(left, top), size: crate::format::Size(w as f64, h as f64), rotation: 0.0, flip_x: false, flip_y: false, sampling: t.sampling });
            Ok(())
        })
    }

    /// A whole stroke laid down at once from known points (no provisional tails), as warps and scripts
    /// do; it must match the same points painted live.
    pub fn replay_stroke(&mut self, points: &[(f64, f64)], settings: &crate::brush::BrushSettings, kind: StrokeKind) -> Result<()> {
        let Some((first, rest)) = points.split_first() else { return Ok(()) };
        self.begin_stroke(*first, settings, kind)?;
        let result = (|| -> Result<()> {
            let id = self.stroke_layer.or(self.active).ok_or_else(|| anyhow::anyhow!("no layer for the stroke"))?;
            if let Some(stroke) = self.stroke.as_mut() { stroke.replay(rest)?; if let Some(rect) = stroke.changed { self.renderer.preview_changed(id, rect)?; } }
            Ok(())
        })();
        // A stroke left open would hold the document (no painting, no tools, no autosave).
        if let Err(e) = result { self.cancel_stroke(); return Err(e); }
        self.finish_stroke()
    }

    /// Starts a stroke on the active layer at `point` (document pixels). Nothing starts on a folder, an
    /// adjustment layer, or an explicitly empty selection.
    pub fn begin_stroke(&mut self, point: (f64, f64), settings: &crate::brush::BrushSettings, kind: StrokeKind) -> Result<()> {
        if self.stroke_active() { bail!("a stroke is already in progress"); }
        let Some(id) = self.active else { bail!("Select a layer first.") };
        let layer = self.renderer.layer(id).clone();
        if layer.is_group() || layer.adjustment.is_some() { bail!("Select an image layer first."); }
        if !crate::format::visible_layers(self.renderer.layers()).contains(&id) { bail!("This layer is hidden; show it to paint on it."); }
        if self.selection.as_ref().is_some_and(|s| s.is_empty()) { bail!("Nothing is selected."); }
        // Type and shape layers, and layers scaled unevenly, become plain pixels first: paint on a type
        // layer would vanish at its next edit, and a round brush on a stretched layer would paint ovals.
        self.rasterize_for_painting(id)?;
        let layer = self.renderer.layer(id).clone();
        let kind = match kind {
            StrokeKind::Paint => crate::brush::Kind::Paint,
            StrokeKind::Erase => crate::brush::Kind::Erase,
            StrokeKind::Heal { mode } => crate::brush::Kind::Heal { mode },
            StrokeKind::Clone { offset, all_layers } => crate::brush::Kind::Clone { sample: std::rc::Rc::new(self.sample(all_layers)?), offset, replaces: false },
            StrokeKind::Blur => {
                let sigma = (settings.diameter / 10.0).clamp(1.5, 30.0);
                let sample = self.sample(false)?;
                crate::brush::Kind::Blur { sample: std::rc::Rc::new(crate::blur::LazyBlur::new(sample.pixels, sample.width, sample.height, sigma)) }
            }
            StrokeKind::Dodge { burn, range } => crate::brush::Kind::Dodge { burn, range },
            StrokeKind::Sponge { desaturate } => crate::brush::Kind::Sponge { desaturate },
            StrokeKind::Pattern { name } => {
                let surface = crate::patterns::load(&name)?;
                let (w, h) = (surface.width() as usize, surface.height() as usize);
                let pixels = with_bytes(&surface, |data, stride| { let mut out = vec![0u8; w * h * 4]; for y in 0..h { out[y * w * 4..(y + 1) * w * 4].copy_from_slice(&data[y * stride..y * stride + w * 4]); } out })?;
                crate::brush::Kind::Clone { sample: std::rc::Rc::new(crate::brush::Sample { pixels, width: w, height: h, tiled: true }), offset: (0.0, 0.0), replaces: false }
            }
        };
        let size = self.renderer.image_size(id).unwrap_or((layer.transform.size.0.round().max(1.0) as i32, layer.transform.size.1.round().max(1.0) as i32));
        let canvas = (self.width() as f64, self.height() as f64);
        let renderer = &mut self.renderer;
        let mut stroke = crate::brush::Stroke::new(renderer.image(id).is_some(), size, &layer.transform, canvas, settings, kind, self.selection.clone(), true,
            |x0, y0, w, h, transform| renderer.begin_preview(id, -x0, -y0, w, h, transform))?;
        stroke.append(point)?;
        if let Some(rect) = stroke.changed { self.renderer.preview_changed(id, rect)?; }
        self.stroke = Some(stroke); self.stroke_layer = Some(id);
        Ok(())
    }

    pub fn continue_stroke(&mut self, point: (f64, f64)) -> Result<()> {
        if self.warp.is_some() { return self.continue_warp(point); }
        let Some(id) = self.stroke_layer.or(self.active) else { return Ok(()) };
        let Some(mut stroke) = self.stroke.take() else { return Ok(()) };
        let result = stroke.append(point);
        if result.is_ok() { if let Some(rect) = stroke.changed { self.mark_dirty_grid(&stroke, rect); } }
        let sync = match (&result, self.stroke_mask) {
            (Ok(()), true) => self.sync_mask_preview(id, &stroke),
            (Ok(()), false) => stroke.changed.map_or(Ok(()), |rect| self.renderer.preview_changed(id, rect)),
            _ => Ok(()),
        };
        self.stroke = Some(stroke); self.stroke_layer = Some(id);
        result.and(sync)
    }

    pub fn cancel_stroke(&mut self) {
        self.stroke = None;
        self.warp = None;
        if let Some(id) = self.stroke_layer.take().or(self.active) { self.renderer.set_preview(id, None); self.renderer.end_mask_preview(id); }
        self.stroke_mask = false;
    }

    /// Ends the stroke: the final curve piece, healing if that is what it was, then the layer's pixels (and
    /// its transform, when painting reached past its edge) replaced as one undo step.
    pub fn finish_stroke(&mut self) -> Result<()> {
        if self.warp.is_some() { return self.finish_warp(); }
        self.finish_stroke_named("")
    }

    fn finish_stroke_named(&mut self, name: &str) -> Result<()> {
        let Some(mut stroke) = self.stroke.take() else { return Ok(()) };
        let Some(id) = self.stroke_layer.take().or(self.active) else { return Ok(()) };
        if !self.has_layer(id) { self.stroke_mask = false; return Ok(()); }
        if std::mem::take(&mut self.stroke_mask) {
            let result = stroke.flush().and_then(|_| self.sync_mask_preview(id, &stroke));
            if result.is_ok() && stroke.touched_anything() {
                self.begin_edit("Paint Mask");
                self.renderer.adopt_mask_preview(id);
                self.end_edit();
            } else { self.renderer.end_mask_preview(id); }
            return result;
        }
        let depth = self.history.depth();
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
            self.begin_edit(if name.is_empty() { stroke.kind.name() } else { name });
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
        if result.is_err() { self.unwind_edits_to(depth); }
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
        if old.size != transform.size { let _ = self.redraw_shape(id); }
        self.end_edit();
    }

    /// The layers a transform of `id` moves: with several layers selected, the visible pixel layers selected
    /// or inside selected folders (`groupTransformMembers`); a folder's visible descendants; or the layer.
    pub fn transform_members(&self, id: Uuid) -> Vec<Uuid> {
        let layers = self.renderer.layers();
        let roots: std::collections::HashSet<Uuid> = if self.selected.len() > 1 { self.selected.clone() } else { [id].into_iter().collect() };
        if roots.len() == 1 && !self.renderer.layer(id).is_group() { return if self.renderer.has_image(id) { vec![id] } else { Vec::new() }; }
        let mut result = Vec::new();
        for entry in crate::format::entries(layers) {
            if entry.layer.is_group() || !self.renderer.has_image(entry.layer.id) || !entry.visible { continue; }
            let mut current = Some(entry.layer.id);
            for _ in 0..64 {
                let Some(c) = current else { break };
                if roots.contains(&c) { result.push(entry.layer.id); break; }
                current = layers.iter().find(|l| l.id == c).and_then(|l| l.parent_id);
            }
        }
        result
    }

    // MARK: Moving selected pixels

    /// Starts moving (or, `duplicate`, copying) the selected pixels of the active image layer; false when
    /// there is nothing to move (`beginPixelMove`). The pixels lift off the layer in its own grid.
    pub fn begin_pixel_move(&mut self, duplicate: bool) -> Result<bool> {
        if self.pixel_move.is_some() || self.mask_target { return Ok(false); }
        let Some(selection) = self.selection.clone() else { return Ok(false) };
        let Some((x0, y0, x1, y1)) = selection.bounds else { return Ok(false) };
        let Some((id, image)) = self.active_image() else { return Ok(false) };
        let transform = self.renderer.layer(id).transform;
        let (w, h) = (image.width(), image.height());
        let to_layer = crate::selection::document_to_layer(&transform, w, h)?;
        let corners = [(x0 as f64, y0 as f64), (x1 as f64, y0 as f64), (x1 as f64, y1 as f64), (x0 as f64, y1 as f64)].map(|(x, y)| to_layer.transform_point(x, y));
        let (lx0, ly0) = (corners.iter().map(|c| c.0).fold(f64::MAX, f64::min).floor().max(0.0) as i32, corners.iter().map(|c| c.1).fold(f64::MAX, f64::min).floor().max(0.0) as i32);
        let (lx1, ly1) = ((corners.iter().map(|c| c.0).fold(f64::MIN, f64::max).ceil() as i32).min(w), (corners.iter().map(|c| c.1).fold(f64::MIN, f64::max).ceil() as i32).min(h));
        if lx1 <= lx0 || ly1 <= ly0 { return Ok(false); }
        let coverage = selection.coverage_on_layer(&transform, w, h)?;
        let (rw, rh) = (lx1 - lx0, ly1 - ly0);
        let lifted = new_argb(rw, rh)?;
        {
            let cr = Context::new(&lifted)?;
            cr.set_source_surface(&image, -(lx0 as f64), -(ly0 as f64))?;
            cr.mask_surface(&coverage, -(lx0 as f64), -(ly0 as f64))?;
        }
        let base = if duplicate { image.clone() } else {
            let hole = new_argb(w, h)?;
            let cr = Context::new(&hole)?;
            cr.set_source_surface(&image, 0.0, 0.0)?;
            cr.paint()?;
            cr.set_operator(cairo::Operator::DestOut);
            cr.set_source_rgba(0.0, 0.0, 0.0, 1.0);
            cr.mask_surface(&coverage, 0.0, 0.0)?;
            drop(cr);
            hole
        };
        self.pixel_move = Some(PixelMove { id, lifted, region: (lx0, ly0), base, origin_selection: selection, duplicate, offset: (0, 0), document_offset: (0, 0) });
        Ok(true)
    }

    pub fn pixel_move_active(&self) -> bool { self.pixel_move.is_some() }

    /// Previews the pixels `dx`, `dy` document pixels (whole pixels) from where they started; the outline
    /// follows (`movePixels`).
    pub fn move_pixels(&mut self, dx: f64, dy: f64) -> Result<()> {
        let Some(pm) = self.pixel_move.as_ref() else { return Ok(()) };
        let id = pm.id;
        let transform = self.renderer.layer(id).transform;
        let (dx, dy) = (dx.round(), dy.round());
        // The move in the layer's own grid: the transform's linear part, inverted.
        let (bw, bh) = (pm.base.width(), pm.base.height());
        let mut to_layer = crate::selection::document_to_layer(&transform, bw, bh)?;
        to_layer.set_x0(0.0); to_layer.set_y0(0.0);
        let (mx, my) = to_layer.transform_distance(dx, dy);
        let offset = (mx.round() as i32, my.round() as i32);
        let (composed, placed) = self.compose_pixel_move(offset)?;
        self.renderer.set_preview_placed(id, composed, placed);
        if let Some(pm) = self.pixel_move.as_mut() { pm.offset = offset; pm.document_offset = (dx as i32, dy as i32); }
        let origin = self.pixel_move.as_ref().unwrap().origin_selection.clone();
        self.selection = Some(origin.translated(dx as i32, dy as i32)?);
        Ok(())
    }

    /// The layer with the hole plus the lifted pixels at `offset`, on a grid grown to hold both, and the
    /// transform placing that grid.
    fn compose_pixel_move(&self, offset: (i32, i32)) -> Result<(ImageSurface, Transform)> {
        let pm = self.pixel_move.as_ref().unwrap();
        let transform = self.renderer.layer(pm.id).transform;
        let (bw, bh) = (pm.base.width(), pm.base.height());
        let (lw, lh) = (pm.lifted.width(), pm.lifted.height());
        let (tx, ty) = (pm.region.0 + offset.0, pm.region.1 + offset.1);
        let (x0, y0) = (tx.min(0), ty.min(0));
        let (x1, y1) = ((tx + lw).max(bw), (ty + lh).max(bh));
        let (gw, gh) = (x1 - x0, y1 - y0);
        if gw > 30_000 || gh > 30_000 || gw as i64 * gh as i64 > 100_000_000 { bail!("The moved pixels would take the layer past the 30,000-pixel side or 100-megapixel limit."); }
        let out = new_argb(gw, gh)?;
        {
            let cr = Context::new(&out)?;
            cr.set_source_surface(&pm.base, -(x0 as f64), -(y0 as f64))?;
            cr.paint()?;
            cr.set_source_surface(&pm.lifted, (tx - x0) as f64, (ty - y0) as f64)?;
            cr.paint()?;
        }
        let mut placed = transform;
        placed.size = crate::format::Size(transform.size.0 * gw as f64 / bw as f64, transform.size.1 * gh as f64 / bh as f64);
        let to_document = crate::render::pixel_to_document(&transform, bw, bh);
        let (cx, cy) = to_document.transform_point((x0 + x1) as f64 / 2.0, (y0 + y1) as f64 / 2.0);
        placed.origin = crate::format::Point(cx - placed.size.0 / 2.0, cy - placed.size.1 / 2.0);
        Ok((out, placed))
    }

    /// Commits the pixels and the moved outline together as one undo step (`finishPixelMove`); a move of
    /// nothing leaves no step.
    pub fn finish_pixel_move(&mut self) -> Result<()> {
        let Some(pm) = self.pixel_move.take() else { return Ok(()) };
        self.renderer.set_preview(pm.id, None);
        let moved = self.selection.clone();
        self.selection = Some(pm.origin_selection.clone());
        if pm.offset == (0, 0) && !pm.duplicate { return Ok(()); }
        self.pixel_move = Some(pm);
        let (composed, placed) = self.compose_pixel_move(self.pixel_move.as_ref().unwrap().offset)?;
        let pm = self.pixel_move.take().unwrap();
        let old = self.renderer.layer(pm.id).transform;
        self.begin_edit(if pm.duplicate { "Duplicate Pixels" } else { "Move Pixels" });
        // A mask on the layer's grid grows with it, revealing the new area (`FloatingMerge.merge`).
        let layer = self.renderer.layer(pm.id).clone();
        let carried = if layer.mask_file.is_some() && layer.mask_placement.is_none() && !placed.same_placement(&old) { self.grown_mask(pm.id, &old, &placed, composed.width(), composed.height())? } else { None };
        self.renderer.set_image(pm.id, composed);
        self.renderer.set_layer_transform(pm.id, placed);
        if let Some(mask) = carried { self.renderer.set_mask(pm.id, Some(mask)); }
        self.selection = moved;
        self.end_edit();
        Ok(())
    }

    /// The layer's mask on a grown grid, white where the layer grew.
    fn grown_mask(&mut self, id: Uuid, old: &Transform, grown: &Transform, width: i32, height: i32) -> Result<Option<ImageSurface>> {
        let Some(mask) = self.renderer.mask(id).cloned() else { return Ok(None) };
        let (ow, oh) = self.renderer.image_size(id).unwrap_or((mask.width(), mask.height()));
        let to_document = crate::render::pixel_to_document(old, ow, oh);
        let to_new = crate::selection::document_to_layer(grown, width, height)?;
        let (ox, oy) = { let (x, y) = to_document.transform_point(0.0, 0.0); to_new.transform_point(x, y) };
        let out = crate::raster::a8_filled(width, height, 255)?;
        {
            let cr = Context::new(&out)?;
            cr.translate(ox.round(), oy.round());
            cr.scale(ow as f64 / mask.width() as f64, oh as f64 / mask.height() as f64);
            cr.set_source_surface(&mask, 0.0, 0.0)?;
            cr.set_operator(cairo::Operator::Source);
            cr.rectangle(0.0, 0.0, mask.width() as f64, mask.height() as f64);
            cr.fill()?;
        }
        Ok(Some(out))
    }

    pub fn cancel_pixel_move(&mut self) {
        if let Some(pm) = self.pixel_move.take() { self.renderer.set_preview(pm.id, None); self.selection = Some(pm.origin_selection); }
    }

    /// Ctrl-arrow: moves the selected pixels as one undo step (`nudgePixels`).
    pub fn nudge_pixels(&mut self, dx: f64, dy: f64) -> Result<()> {
        if !self.begin_pixel_move(false)? { return Ok(()); }
        self.move_pixels(dx, dy)?;
        self.finish_pixel_move()
    }

    // MARK: Free distort

    /// The layer warped into `corners` at preview size, and the transform placing it (`distortPreview`).
    pub fn preview_distort(&mut self, id: Uuid, transform: &Transform, corners: &crate::distort::Corners) -> Result<()> {
        let Some(image) = self.renderer.image(id).cloned() else { return Ok(()) };
        let (warped, placed) = crate::distort::warp(&image, transform, corners, false, Some(2048.0))?;
        // The mask goes the way the commit will take it: warped with the pixels, carried on its own
        // placement, or left where it is.
        if let Some((mask, placement)) = self.distorted_mask(id, transform, corners, Some(2048.0))? {
            let (w, h) = (mask.width(), mask.height());
            let preview = self.renderer.begin_mask_preview(id, w, h)?;
            let rows = with_bytes(&mask, |d, _| d.to_vec())?;
            crate::raster::with_bytes_raw_mut(&preview, |d, _| d.copy_from_slice(&rows))?;
            self.renderer.set_mask_preview_placement(id, Some(placement));
            self.renderer.mask_preview_changed(id, (0, 0, w, h))?;
        }
        self.renderer.set_preview_placed(id, warped, placed);
        Ok(())
    }

    /// The layer's mask as a distortion takes it (`distortPreview`'s `warpedMask`): the mask warped over the
    /// pixels' new bounds, or carried on its own placement; None for an unlinked mask, which stays put.
    fn distorted_mask(&mut self, id: Uuid, transform: &Transform, corners: &crate::distort::Corners, limit: Option<f64>) -> Result<Option<(ImageSurface, Transform)>> {
        let layer = self.renderer.layer(id).clone();
        let Some(mask) = self.renderer.mask(id).cloned() else { return Ok(None) };
        let linked = layer.mask_linked.unwrap_or(true);
        if layer.mask_placement.is_none() && linked {
            let (wm, placed) = crate::distort::warp(&mask, transform, corners, true, limit)?;
            return Ok(Some((wm, placed)));
        }
        if let (true, Some(p)) = (linked, layer.mask_placement) {
            let placement = p.following(&layer.transform, transform);
            if let Some(carried) = crate::distort::carried(&placement, transform, corners).filter(crate::distort::is_usable) {
                let background = self.renderer.mask_background(id)?;
                return Ok(Some(crate::distort::warp_mask(&mask, &placement, &carried, background, limit)?));
            }
        }
        Ok(None)
    }

    /// Drops a distortion preview (the pixels and the mask) without applying it.
    pub fn cancel_distort(&mut self, id: Uuid) { self.renderer.set_preview(id, None); self.renderer.end_mask_preview(id); }

    /// Apply for a distortion: the layer's pixels and mask resampled into the shape, as one undo step
    /// (`commitDistort`). `transform` is what the layer showed through while its corners were dragged.
    pub fn commit_distort(&mut self, targets: &[(Uuid, Transform, crate::distort::Corners)]) -> Result<()> {
        for (id, _, _) in targets { self.renderer.set_preview(*id, None); self.renderer.end_mask_preview(*id); }
        let name = if targets.len() == 1 { "Distort" } else { "Distort Layers" };
        self.begin_edit(name);
        let result = (|| -> Result<()> {
            for (id, transform, corners) in targets {
                let Some(image) = self.renderer.image(*id).cloned() else { continue };
                let (warped, placed, crop) = crate::distort::warp_trimmed(&image, transform, corners)?;
                let layer = self.renderer.layer(*id).clone();
                let mut new_mask = None;
                let mut new_placement = layer.mask_placement;
                if let Some(mask) = self.renderer.mask(*id).cloned() {
                    let linked = layer.mask_linked.unwrap_or(true);
                    if layer.mask_placement.is_none() && linked {
                        let (wm, _) = crate::distort::warp(&mask, transform, corners, true, None)?;
                        new_mask = Some(if mask.width() == 1 && mask.height() == 1 { mask } else { crate::distort::crop(&wm, crop.0, crop.1, crop.2, crop.3, true)? });
                    } else if let (true, Some(p)) = (linked, layer.mask_placement) {
                        let placement = p.following(&layer.transform, transform);
                        if let Some(carried) = crate::distort::carried(&placement, transform, corners).filter(crate::distort::is_usable) {
                            let background = self.renderer.mask_background(*id)?;
                            let (moved, moved_placement) = crate::distort::warp_mask(&mask, &placement, &carried, background, None)?;
                            new_mask = Some(moved);
                            new_placement = Some(moved_placement);
                        }
                    } else {
                        new_placement = Some(layer.mask_placement.unwrap_or(layer.transform));
                    }
                }
                self.renderer.set_image(*id, warped);
                self.renderer.set_layer_transform(*id, placed);
                if let Some(m) = new_mask { self.renderer.set_mask(*id, Some(m)); }
                if self.renderer.mask(*id).is_some() { self.renderer.set_mask_placement(*id, new_placement); }
            }
            Ok(())
        })();
        match result { Ok(()) => { self.end_edit(); Ok(()) } Err(e) => { self.abort_edit(); Err(e) } }
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
        let (mut xs, mut ys) = self.guide_snap_targets();
        if self.show_guides { xs.extend(self.guides_v.iter().copied()); ys.extend(self.guides_h.iter().copied()); }
        let _ = excluding;
        (xs, ys)
    }

    /// The grid's lines across the canvas (every subdivision), when the grid shows.
    pub fn grid_lines(&self) -> (Vec<f64>, Vec<f64>) {
        let Some((size, subdivisions)) = self.grid else { return (Vec::new(), Vec::new()) };
        let step = (size / subdivisions.max(1) as f64).max(1.0);
        let (w, h) = (self.width() as f64, self.height() as f64);
        let lines = |extent: f64| { let mut v = Vec::new(); let mut p = 0.0; while p <= extent + 0.5 { v.push(p.round()); p += step; } v };
        (lines(w), lines(h))
    }

    /// What a dragged guide settles on: the canvas edges and center, every visible layer's edges and
    /// center, and the grid when it shows. Guides never snap to other guides.
    pub fn guide_snap_targets(&self) -> (Vec<f64>, Vec<f64>) {
        let (w, h) = (self.width() as f64, self.height() as f64);
        let (mut xs, mut ys) = (vec![0.0, (w / 2.0).round(), w], vec![0.0, (h / 2.0).round(), h]);
        let (gx, gy) = self.grid_lines();
        xs.extend(gx);
        ys.extend(gy);
        for id in crate::format::visible_layers(self.renderer.layers()) {
            if !self.renderer.has_image(id) { continue; }
            let (x0, y0, x1, y1) = self.renderer.layer(id).transform.bounds();
            xs.extend([x0.round(), ((x0 + x1) / 2.0).round(), x1.round()]);
            ys.extend([y0.round(), ((y0 + y1) / 2.0).round(), y1.round()]);
        }
        (xs, ys)
    }

    /// Snap targets without the layers being moved.
    fn snap_targets_excluding(&self, excluding: &[Uuid]) -> (Vec<f64>, Vec<f64>) {
        let (w, h) = (self.width() as f64, self.height() as f64);
        let (mut xs, mut ys) = (vec![0.0, (w / 2.0).round(), w], vec![0.0, (h / 2.0).round(), h]);
        let (gx, gy) = self.grid_lines();
        xs.extend(gx);
        ys.extend(gy);
        if self.show_guides { xs.extend(self.guides_v.iter().copied()); ys.extend(self.guides_h.iter().copied()); }
        for id in crate::format::visible_layers(self.renderer.layers()) {
            if excluding.contains(&id) || !self.renderer.has_image(id) { continue; }
            let (x0, y0, x1, y1) = self.renderer.layer(id).transform.bounds();
            xs.extend([x0.round(), ((x0 + x1) / 2.0).round(), x1.round()]);
            ys.extend([y0.round(), ((y0 + y1) / 2.0).round(), y1.round()]);
        }
        (xs, ys)
    }

    /// A guide position settled on the nearest target within `tolerance` (document pixels), with Snap on.
    pub fn snapped_guide(&self, vertical: bool, position: f64, tolerance: f64) -> f64 {
        if !self.snap { return position; }
        let (xs, ys) = self.guide_snap_targets();
        let targets = if vertical { xs } else { ys };
        targets.into_iter().filter(|t| (t - position).abs() <= tolerance).min_by(|a, b| (a - position).abs().partial_cmp(&(b - position).abs()).unwrap_or(std::cmp::Ordering::Equal)).unwrap_or(position)
    }

    /// `draft` nudged so the layer it places lines up with a nearby edge or center, and the guides it met.
    pub fn snapped_move(&self, draft: Transform, moving: &[Uuid], tolerance: f64) -> (Transform, Option<f64>, Option<f64>) {
        if !self.snap { return (draft, None, None); }
        let (xs, ys) = self.snap_targets_excluding(moving);
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

    /// Ctrl-click on a mask thumbnail: the mask's coverage on the document as a selection.
    pub fn select_mask_pixels(&mut self, id: Uuid, mode: Mode) -> Result<()> {
        if self.renderer.mask(id).is_none() { return Ok(()); }
        let (w, h) = (self.width(), self.height());
        let mask = crate::raster::a8_filled(w, h, 0)?;
        {
            let cr = Context::new(&mask)?;
            self.renderer.draw_mask_plain(id, &cr)?;
        }
        let shape = Selection::from_mask(mask, true)?;
        self.apply_selection(shape, mode, "Load Mask Selection")
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
    pub fn begin_mask_stroke(&mut self, point: (f64, f64), settings: &crate::brush::BrushSettings, white: bool) -> Result<()> { self.begin_mask_stroke_kind(point, settings, white, false) }

    /// The Blur tool on a mask softens it (`blurSample(mask:)`): the sample is the mask as the canvas shows
    /// it, at document size, blurred by an amount that follows the brush size.
    pub fn begin_mask_blur(&mut self, point: (f64, f64), settings: &crate::brush::BrushSettings) -> Result<()> { self.begin_mask_stroke_kind(point, settings, true, true) }

    fn begin_mask_stroke_kind(&mut self, point: (f64, f64), settings: &crate::brush::BrushSettings, white: bool, blur: bool) -> Result<()> {
        if self.stroke_active() { bail!("a stroke is already in progress"); }
        let Some(id) = self.active else { bail!("Select a layer first.") };
        let layer = self.renderer.layer(id).clone();
        if !crate::format::visible_layers(self.renderer.layers()).contains(&id) { bail!("This layer is hidden; show it to paint its mask."); }
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
        let kind = if blur {
            // Past its pixels a mask keeps its edge tone, so blurring near its edge does not pull in the wrong one.
            let (w, h) = (self.width(), self.height());
            let edge = with_bytes(&mask, |d, stride| { let (mw, mh) = (mask.width() as usize, mask.height() as usize); let mut sum = 0u64; let mut n = 0u64; for y in 0..mh { for x in 0..mw { if y == 0 || x == 0 || y + 1 == mh || x + 1 == mw { sum += d[y * stride + x] as u64; n += 1; } } } if n > 0 && sum / n >= 128 { 255u8 } else { 0u8 } })?;
            let doc = crate::raster::a8_filled(w, h, edge)?;
            {
                let cr = Context::new(&doc)?;
                cr.set_operator(cairo::Operator::Source);
                self.renderer.draw_mask_plain(id, &cr)?;
            }
            let (wu, hu) = (w as usize, h as usize);
            let pixels = with_bytes(&doc, |d, stride| { let mut out = vec![0u8; wu * hu * 4]; for y in 0..hu { for x in 0..wu { let v = d[y * stride + x]; let o = (y * wu + x) * 4; out[o] = v; out[o + 1] = v; out[o + 2] = v; out[o + 3] = 255; } } out })?;
            let sigma = (settings.diameter / 10.0).clamp(1.5, 30.0);
            crate::brush::Kind::Blur { sample: std::rc::Rc::new(crate::blur::LazyBlur::new(pixels, wu, hu, sigma)) }
        } else { crate::brush::Kind::Paint };
        let a8 = self.renderer.begin_mask_preview(id, grid.0, grid.1)?;
        let mut stroke = crate::brush::Stroke::new(true, grid, &transform, canvas, &paint, kind, self.selection.clone(), false, |_, _, w, h, _| {
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
        self.stroke = Some(stroke); self.stroke_layer = Some(id);
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
            image_file: None, parent_id: None, is_group: None, opacity: None, blend_mode: None, mask_file: None, mask_enabled: None, mask_source_id: None, adjustment: None, mask_placement: None, mask_linked: None, shape: None, text: None, effects: None, artboard: None,
        };
        let manifest = crate::format::Manifest { format: crate::format::FORMAT.into(), version: crate::format::SAVE_VERSION, color_space: "sRGB".into(), resolution: Some(resolution), document_id: Uuid::new_v4(), width: width as i64, height: height as i64, active_layer_id: Some(id), layers: vec![layer], guides: None };
        let json = serde_json::to_vec(&manifest)?;
        let manifest = crate::format::Manifest::parse(&json)?;
        Document::new(Project { path: std::path::PathBuf::new(), manifest, images: Default::default(), masks: Default::default() })
    }

    /// The manifest as it stands, for saving.
    /// What an autosave writes, copied out so a thread can write it (`autosave::Snapshot::write`).
    pub fn autosave_snapshot(&self, title: &str) -> Result<crate::autosave::Snapshot> {
        let manifest = self.manifest();
        let mut images = std::collections::HashMap::new();
        let mut masks = std::collections::HashMap::new();
        for layer in &manifest.layers {
            if layer.image_file.is_some() { if let Some(s) = self.renderer.image(layer.id) { images.insert(layer.id, crate::autosave::pack(s)?); } }
            if layer.mask_file.is_some() { if let Some(s) = self.renderer.mask(layer.id) { masks.insert(layer.id, crate::autosave::pack(s)?); } }
        }
        Ok(crate::autosave::Snapshot::new(self.document_id, title.to_string(), manifest, images, masks))
    }

    pub fn manifest(&self) -> crate::format::Manifest {
        crate::format::Manifest {
            format: crate::format::FORMAT.into(), version: crate::format::version_for(self.renderer.layers()), color_space: "sRGB".into(),
            resolution: Some(self.renderer.resolution()), document_id: self.document_id, width: self.width() as i64, height: self.height() as i64,
            active_layer_id: self.active, layers: self.renderer.layers().to_vec(),
            guides: if self.guides_v.is_empty() && self.guides_h.is_empty() { None } else { Some(crate::format::Guides { vertical: self.guides_v.clone(), horizontal: self.guides_h.clone() }) },
        }
    }

    pub fn save(&mut self, path: &std::path::Path) -> Result<()> {
        // A floating selection lands first, as the Mac resolves it before writing.
        if self.floating.is_some() { self.commit_free_transform()?; }
        // Effects only being previewed are not written: the file holds what OK or Cancel would keep.
        let previewed = match &self.effects_preview { Some((id, original)) if self.has_layer(*id) => { let shown = self.renderer.layer(*id).effects.clone(); self.renderer.set_effects(*id, original.clone()); Some((*id, shown)) } _ => None };
        let manifest = self.manifest();
        let written = crate::format::save(path, &manifest, self.renderer.images(), self.renderer.masks());
        if let Some((id, shown)) = previewed { self.renderer.set_effects(id, shown); }
        written?;
        self.history.mark_saved();
        Ok(())
    }

    /// Unsaved changes, counting a Free Transform or Layer Style still in progress (their pixels are on
    /// screen even though the step has not closed).
    pub fn is_modified(&self) -> bool { self.history.is_modified() || self.floating.is_some() || self.history.is_editing() }
    pub fn layer_style_open(&self) -> bool { self.effects_preview.is_some() }

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
            image_file: None, parent_id: parent, is_group: None, opacity: None, blend_mode: None, mask_file: None, mask_enabled: None, mask_source_id: None, adjustment: None, mask_placement: None, mask_linked: None, shape: None, text: None, effects: None, artboard: None,
        }
    }

    pub fn add_blank_layer(&mut self) -> Uuid {
        let (index, parent) = self.insertion();
        let record = self.blank_record(self.unique_name("Layer"), parent);
        let id = record.id;
        self.begin_edit("New Layer");
        self.renderer.insert_layer(index, record, None, None);
        self.select_layer(Some(id));
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
        self.select_layer(Some(id));
        self.end_edit();
        id
    }

    /// Every artboard, bottom to top: id, name and frame.
    pub fn artboards(&self) -> Vec<(Uuid, String, (f64, f64, f64, f64))> {
        self.renderer.layers().iter().filter(|l| l.is_artboard()).filter_map(|l| l.artboard.as_ref().map(|b| (l.id, l.name.clone(), b.rect()))).collect()
    }

    /// The artboard a layer belongs to (itself when it is one).
    pub fn artboard_of(&self, id: Uuid) -> Option<Uuid> {
        if !self.has_layer(id) { return None; }
        if self.renderer.layer(id).is_artboard() { return Some(id); }
        let mut folder = self.renderer.layer(id).parent_id;
        for _ in 0..64 { let Some(c) = folder else { return None }; if self.renderer.layer(c).is_artboard() { return Some(c); } folder = self.renderer.layer(c).parent_id; }
        None
    }

    /// Grows the canvas so `rect` fits on it, keeping every layer where it is. Returns how far everything
    /// moved (a rect past the top or left edge pushes the picture right or down), for a frame that is not
    /// yet on the canvas.
    fn make_room_for(&mut self, rect: (f64, f64, f64, f64)) -> Result<(f64, f64)> {
        let (w, h) = (self.width() as f64, self.height() as f64);
        let (x0, y0) = (rect.0.floor().min(0.0), rect.1.floor().min(0.0));
        let (x1, y1) = ((rect.0 + rect.2).ceil().max(w), (rect.1 + rect.3).ceil().max(h));
        if x0 == 0.0 && y0 == 0.0 && x1 == w && y1 == h { return Ok((0.0, 0.0)); }
        self.canvas_size((x1 - x0) as i32, (y1 - y0) as i32, 0, None, Some((-x0, -y0)), "Grow Canvas")?;
        Ok((-x0, -y0))
    }

    /// Where a new board goes when no place is given: to the right of the rightmost one, or the canvas.
    pub fn next_artboard_place(&self, width: f64, height: f64) -> (f64, f64) {
        let boards = self.artboards();
        let gap = 40.0;
        match boards.iter().map(|(_, _, r)| (r.0 + r.2, r.1)).max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)) {
            Some((right, top)) => (right + gap, top),
            None => if self.renderer.layers().iter().any(|l| self.renderer.has_image(l.id)) { (self.width() as f64 + gap, 0.0) } else { let _ = (width, height); (0.0, 0.0) },
        }
    }

    /// Layer > New Artboard: an empty board named `name` with `frame` on the canvas (grown to hold it).
    pub fn add_artboard(&mut self, name: &str, frame: (f64, f64, f64, f64), background: Option<[f64; 3]>) -> Result<Uuid> {
        let board = crate::format::Artboard { x: frame.0.round(), y: frame.1.round(), width: frame.2.round(), height: frame.3.round(), background };
        if !board.is_valid() { bail!("An artboard runs from 1 to 30,000 pixels on a side."); }
        self.begin_edit("New Artboard");
        let board = match self.make_room_for(board.rect()) { Ok((sx, sy)) => crate::format::Artboard { x: board.x + sx, y: board.y + sy, ..board }, Err(e) => { self.abort_edit(); return Err(e); } };
        let layers = self.renderer.layers();
        // Boards sit at the top level, above everything, under the name given (numbered only on a clash).
        let taken = self.renderer.layers().iter().any(|l| l.name == name);
        let mut record = self.blank_record(if taken { self.unique_name(name) } else { name.to_string() }, None);
        record.is_group = Some(true);
        record.artboard = Some(board);
        let id = record.id;
        self.renderer.insert_layer(layers.len(), record, None, None);
        self.select_layer(Some(id));
        self.end_edit();
        Ok(id)
    }

    /// Moves a board's frame (its layers come along) or resizes it.
    pub fn set_artboard_frame(&mut self, id: Uuid, frame: (f64, f64, f64, f64)) -> Result<()> {
        let Some(old) = self.renderer.layer(id).artboard.clone().filter(|_| self.renderer.layer(id).is_group()) else { bail!("That layer is not an artboard.") };
        let board = crate::format::Artboard { x: frame.0.round(), y: frame.1.round(), width: frame.2.round(), height: frame.3.round(), background: old.background };
        if !board.is_valid() { bail!("An artboard runs from 1 to 30,000 pixels on a side."); }
        let (dx, dy) = (board.x - old.x, board.y - old.y);
        self.begin_edit("Move Artboard");
        if let Err(e) = self.make_room_for(board.rect()) { self.abort_edit(); return Err(e); }
        // make_room_for may have shifted everything; the frame is placed after it, from the shifted old one.
        let shifted = self.renderer.layer(id).artboard.clone().unwrap_or(old.clone());
        let target = crate::format::Artboard { x: shifted.x + dx, y: shifted.y + dy, width: board.width, height: board.height, background: old.background };
        self.renderer.set_artboard(id, Some(target));
        if dx != 0.0 || dy != 0.0 {
            let inside: Vec<Uuid> = self.renderer.layers().iter().map(|l| l.id).filter(|l| *l != id && self.artboard_of(*l) == Some(id)).collect();
            for child in inside {
                let mut t = self.renderer.layer(child).transform;
                t.origin = crate::format::Point(t.origin.0 + dx, t.origin.1 + dy);
                self.renderer.set_layer_transform(child, t);
                if let Some(mut p) = self.renderer.layer(child).mask_placement { p.origin = crate::format::Point(p.origin.0 + dx, p.origin.1 + dy); self.renderer.set_mask_placement(child, Some(p)); }
            }
        }
        self.end_edit();
        Ok(())
    }

    /// The board at a document point, topmost first: its frame, or the name strip just above it.
    pub fn artboard_at(&self, point: (f64, f64), strip: f64) -> Option<Uuid> {
        self.artboards().into_iter().rev().find(|(_, _, (x, y, w, h))| point.0 >= *x && point.0 <= x + w && point.1 >= y - strip && point.1 <= y + h).map(|(id, _, _)| id)
    }

    /// Moves a board and its layers by (dx, dy) without recording a step: a drag's motion, between a
    /// `begin_edit` and an `end_edit` of the caller's.
    pub fn nudge_artboard(&mut self, id: Uuid, dx: f64, dy: f64) {
        let Some(mut board) = self.renderer.layer(id).artboard.clone() else { return };
        board.x += dx;
        board.y += dy;
        self.renderer.set_artboard(id, Some(board));
        let inside: Vec<Uuid> = self.renderer.layers().iter().map(|l| l.id).filter(|l| *l != id && self.artboard_of(*l) == Some(id)).collect();
        for child in inside {
            let mut t = self.renderer.layer(child).transform;
            t.origin = crate::format::Point(t.origin.0 + dx, t.origin.1 + dy);
            self.renderer.set_layer_transform(child, t);
            if let Some(mut p) = self.renderer.layer(child).mask_placement { p.origin = crate::format::Point(p.origin.0 + dx, p.origin.1 + dy); self.renderer.set_mask_placement(child, Some(p)); }
        }
    }

    /// Grows the canvas to hold every board (after a drag past the edge).
    pub fn fit_canvas_to_artboards(&mut self) -> Result<()> {
        let boards = self.artboards();
        let mut b = (0.0f64, 0.0f64, self.width() as f64, self.height() as f64);
        for (_, _, (x, y, w, h)) in &boards { b.0 = b.0.min(*x); b.1 = b.1.min(*y); b.2 = b.2.max(x + w); b.3 = b.3.max(y + h); }
        self.make_room_for((b.0, b.1, b.2 - b.0, b.3 - b.1)).map(|_| ())
    }

    /// Export Sizes as artboards: `source` (a remade copy of this document at one size) becomes a board
    /// named `name` at `place`, its layers copied in.
    pub fn import_as_artboard(&mut self, name: &str, source: &Document, place: (f64, f64), background: Option<[f64; 3]>) -> Result<Uuid> {
        let (w, h) = (source.width() as f64, source.height() as f64);
        self.begin_edit("Artboard from Size");
        let board = match self.add_artboard(name, (place.0, place.1, w, h), background) { Ok(id) => id, Err(e) => { self.abort_edit(); return Err(e); } };
        // add_artboard may have moved everything to make room; the board's frame says where it ended up.
        let frame = self.renderer.layer(board).artboard.as_ref().map(|b| b.rect()).unwrap_or((place.0, place.1, w, h));
        let tops: Vec<Uuid> = source.renderer.layers().iter().filter(|l| l.parent_id.is_none()).map(|l| l.id).collect();
        let copied = match self.copy_layers(source, &tops, Place::Into(board)) { Ok(ids) => ids, Err(e) => { self.abort_edit(); return Err(e); } };
        for id in copied {
            // A board inside a board would clip and fill at the source's coordinates: the copies come in
            // as plain folders (the source's board backgrounds were baked when it was remade).
            if self.renderer.layer(id).artboard.is_some() { self.renderer.set_artboard(id, None); }
            let mut t = self.renderer.layer(id).transform;
            t.origin = crate::format::Point(t.origin.0 + frame.0, t.origin.1 + frame.1);
            self.renderer.set_layer_transform(id, t);
            if let Some(mut p) = self.renderer.layer(id).mask_placement { p.origin = crate::format::Point(p.origin.0 + frame.0, p.origin.1 + frame.1); self.renderer.set_mask_placement(id, Some(p)); }
        }
        self.select_layer(Some(board));
        self.end_edit();
        Ok(board)
    }

    pub fn set_artboard_background(&mut self, id: Uuid, background: Option<[f64; 3]>) -> Result<()> {
        let Some(mut board) = self.renderer.layer(id).artboard.clone() else { bail!("That layer is not an artboard.") };
        board.background = background;
        self.begin_edit("Artboard Background");
        self.renderer.set_artboard(id, Some(board));
        self.end_edit();
        Ok(())
    }

    /// Layer > Artboard from Layers: the selected layers (or the active one) moved into a new board drawn
    /// around them.
    pub fn artboard_from_layers(&mut self, name: &str) -> Result<Uuid> {
        let mut ids: Vec<Uuid> = self.selected.iter().copied().filter(|id| self.has_layer(*id)).collect();
        if ids.is_empty() { if let Some(a) = self.active { ids.push(a); } }
        ids.retain(|id| self.artboard_of(*id).is_none() || Some(*id) != self.artboard_of(*id));
        if ids.is_empty() { bail!("Select the layers the artboard should hold first."); }
        let mut b = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for id in &ids { let (x0, y0, x1, y1) = self.renderer.layer(*id).transform.bounds(); b.0 = b.0.min(x0); b.1 = b.1.min(y0); b.2 = b.2.max(x1); b.3 = b.3.max(y1); }
        if !b.0.is_finite() { bail!("Those layers have no bounds."); }
        let frame = (b.0.floor(), b.1.floor(), (b.2 - b.0.floor()).ceil().max(1.0), (b.3 - b.1.floor()).ceil().max(1.0));
        self.begin_edit("Artboard from Layers");
        let board = match self.add_artboard(name, frame, None) { Ok(id) => id, Err(e) => { self.abort_edit(); return Err(e); } };
        ids.sort_by_key(|id| self.renderer.layer_index(*id));
        if let Err(e) = self.move_layers(&ids, Place::Into(board)) { self.abort_edit(); return Err(e); }
        self.select_layer(Some(board));
        self.end_edit();
        Ok(board)
    }

    /// File > Export Artboards: each board's own picture (its background and layers, nothing from an
    /// overlapping board or outside it) as a file in `folder`, named after it, at the board's full size.
    pub fn export_artboards(&mut self, folder: &std::path::Path, jpeg_quality: Option<f64>) -> Result<Vec<std::path::PathBuf>> {
        let boards = self.artboards();
        if boards.is_empty() { bail!("There are no artboards in this document."); }
        std::fs::create_dir_all(folder)?;
        let mut written = Vec::new();
        let mut used = std::collections::HashSet::new();
        for (id, name, _) in boards {
            let out = self.renderer.render_artboard(id)?;
            let path = match jpeg_quality {
                Some(q) => {
                    let path = unique_file(folder, &clean_file_name(&name), "jpg", &mut used);
                    let (rgba, cw, ch) = crate::png_io::straight_rgba(&out)?;
                    let mut rgb = vec![0u8; cw * ch * 3];
                    for i in 0..cw * ch { let a = rgba[i * 4 + 3] as f64 / 255.0; for k in 0..3 { rgb[i * 3 + k] = (rgba[i * 4 + k] as f64 * a + 255.0 * (1.0 - a)).round().clamp(0.0, 255.0) as u8; } }
                    let mut bytes = Vec::new();
                    { let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, (q.clamp(0.0, 1.0) * 100.0).round().max(1.0) as u8); use image::ImageEncoder; encoder.write_image(&rgb, cw as u32, ch as u32, image::ExtendedColorType::Rgb8)?; }
                    std::fs::write(&path, bytes)?;
                    path
                }
                None => { let path = unique_file(folder, &clean_file_name(&name), "png", &mut used); crate::png_io::encode(&out, &path, self.renderer.resolution())?; path }
            };
            written.push(path);
        }
        Ok(written)
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
        self.select_layer(Some(id));
        self.end_edit();
        Some(id)
    }

    pub fn adjustment(&self, id: Uuid) -> Option<crate::filters::Adjustment> {
        self.renderer.layer_index(id)?;
        self.renderer.layer(id).adjustment.as_ref().and_then(crate::filters::Adjustment::from_record)
    }

    pub fn has_layer(&self, id: Uuid) -> bool { self.renderer.layer_index(id).is_some() }

    /// Changes an adjustment layer's settings; `commit` makes it an undo step, otherwise it is a live preview.
    /// False when the layer is gone (a dialog can outlive its layer).
    pub fn set_adjustment(&mut self, id: Uuid, adjustment: &crate::filters::Adjustment, commit: bool) -> bool {
        if !self.has_layer(id) || self.renderer.layer(id).adjustment.is_none() { return false; }
        if commit { self.begin_edit(&format!("{} Adjustment", adjustment.kind_name())); }
        self.renderer.set_adjustment(id, Some(adjustment.to_record()));
        if commit { self.end_edit(); }
        true
    }

    // Appearance: visibility, opacity and blend mode are undo steps; opacity changes from a slider merge.

    pub fn set_visible(&mut self, id: Uuid, visible: bool) {
        if !self.has_layer(id) || self.renderer.layer(id).is_visible == visible { return; }
        self.begin_edit(if visible { "Show Layer" } else { "Hide Layer" });
        self.renderer.set_visible(id, visible);
        self.end_edit();
    }

    pub fn set_opacity(&mut self, id: Uuid, opacity: f64) {
        if !self.has_layer(id) || self.renderer.layer(id).is_group() { return; }
        self.begin_edit("Opacity");
        self.renderer.set_opacity(id, opacity);
        self.end_edit_merging();
    }

    pub fn set_blend_mode(&mut self, id: Uuid, mode: crate::format::BlendMode) {
        if !self.has_layer(id) || self.renderer.layer(id).is_group() { return; }
        self.begin_edit("Blend Mode");
        self.renderer.set_blend_mode(id, mode);
        self.end_edit();
    }

    /// Deletes the active layer (a folder with its contents); clipping links to it are dropped.
    pub fn delete_layer(&mut self) {
        let Some(id) = self.active else { return };
        let mut ids: Vec<Uuid> = if self.selected.len() > 1 { self.renderer.layers().iter().filter(|l| self.selected.contains(&l.id)).map(|l| l.id).collect() } else { vec![id] };
        let layers = self.renderer.layers().to_vec();
        let mut i = 0;
        while i < ids.len() { let parent = ids[i]; for l in &layers { if l.parent_id == Some(parent) { ids.push(l.id); } } i += 1; }
        let index = self.renderer.layer_index(id).unwrap_or(0);
        self.begin_edit("Delete Layer");
        for id in ids { self.renderer.remove_layer(id); }
        let remaining = self.renderer.layers();
        let next = if remaining.is_empty() { None } else { Some(remaining[index.min(remaining.len() - 1)].id) };
        self.select_layer(next);
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
        self.select_layer(Some(new_id));
        self.end_edit();
        Some(new_id)
    }

    /// Where dragged layers land in the panel: above or below another row, or inside a folder (at its top).
    pub fn move_layers(&mut self, ids: &[Uuid], place: Place) -> Result<()> {
        let layers = self.renderer.layers().to_vec();
        // The whole blocks: each dragged layer with everything inside it, in stacking order.
        let mut moving: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
        for id in ids { if self.has_layer(*id) { moving.insert(*id); moving.extend(self.descendants(*id)); } }
        let ids: Vec<Uuid> = ids.iter().copied().filter(|id| self.has_layer(*id)).collect();
        let ids = ids.as_slice();
        let roots: Vec<Uuid> = layers.iter().filter(|l| ids.contains(&l.id)).map(|l| l.id).collect();
        if roots.is_empty() { return Ok(()); }
        let target = match place { Place::Above(a) | Place::Below(a) | Place::Into(a) => a };
        if !self.has_layer(target) { bail!("That layer is gone."); }
        let (anchor, parent, after) = match place {
            Place::Above(a) => (a, self.renderer.layer(a).parent_id, true),
            Place::Below(a) => (a, self.renderer.layer(a).parent_id, false),
            Place::Into(f) => (f, Some(f), true),
        };
        if moving.contains(&anchor) { if matches!(place, Place::Into(_)) { bail!("A folder cannot go inside itself."); } return Ok(()); }
        if let Some(p) = parent {
            if !self.has_layer(p) || !self.renderer.layer(p).is_group() { bail!("Layers can only go inside a folder."); }
            if moving.contains(&p) { bail!("A folder cannot go inside itself."); }
        }
        // Rebuild the array: the moved blocks lifted out, then put back around the anchor.
        let kept: Vec<crate::format::Layer> = layers.iter().filter(|l| !moving.contains(&l.id)).cloned().collect();
        // Only the top of each dragged block takes the new parent; what is inside keeps its nesting.
        let roots_only: std::collections::HashSet<Uuid> = ids.iter().copied().filter(|id| { let mut p = self.renderer.layer(*id).parent_id; let mut inside = false; for _ in 0..64 { match p { Some(q) if moving.contains(&q) => { inside = true; break; } Some(q) => p = self.renderer.layer(q).parent_id, None => break } } !inside }).collect();
        let block: Vec<crate::format::Layer> = layers.iter().filter(|l| moving.contains(&l.id)).cloned().map(|mut l| { if roots_only.contains(&l.id) { l.parent_id = parent; } l }).collect();
        let anchor_index = Self::block_end(&kept, anchor, matches!(place, Place::Into(_)), self);
        let insert_at = if after { anchor_index + 1 } else { anchor_index };
        let mut next = kept;
        for (i, l) in block.into_iter().enumerate() { next.insert(insert_at + i, l); }
        if next.iter().map(|l| l.id).collect::<Vec<_>>() == layers.iter().map(|l| l.id).collect::<Vec<_>>() && next.iter().zip(&layers).all(|(a, b)| a.parent_id == b.parent_id) { return Ok(()); }
        // A clipped layer whose base ends up in another folder is released: a clip only reads a sibling.
        let parents: std::collections::HashMap<Uuid, Option<Uuid>> = next.iter().map(|l| (l.id, l.parent_id)).collect();
        for l in &mut next { if let Some(base) = l.mask_source_id { if parents.get(&base) != Some(&l.parent_id) { l.mask_source_id = None; } } }
        crate::format::validate::hierarchy(&next).map_err(|_| anyhow::anyhow!("That arrangement is not allowed."))?;
        self.begin_edit("Reorder Layers");
        self.renderer.replace_layers(next);
        self.end_edit();
        Ok(())
    }

    /// Copies layers (from this or another document) to `place`, with new ids; masks, clipping and folders
    /// inside the copied set come along (`Duplicate` by drag, and dragging between projects).
    pub fn copy_layers(&mut self, source: &Document, ids: &[Uuid], place: Place) -> Result<Vec<Uuid>> {
        let layers = source.renderer.layers().to_vec();
        let mut picked: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
        for id in ids { if source.has_layer(*id) { picked.insert(*id); picked.extend(source.descendants(*id)); } }
        let originals: Vec<crate::format::Layer> = layers.iter().filter(|l| picked.contains(&l.id)).cloned().collect();
        if originals.is_empty() { return Ok(Vec::new()); }
        let (anchor, parent, after) = match place {
            Place::Above(a) => (a, self.renderer.layer(a).parent_id, true),
            Place::Below(a) => (a, self.renderer.layer(a).parent_id, false),
            Place::Into(f) => (f, Some(f), true),
        };
        if let Some(p) = parent { if !self.has_layer(p) || !self.renderer.layer(p).is_group() { bail!("Layers can only go inside a folder."); } }
        let fresh: std::collections::HashMap<Uuid, Uuid> = originals.iter().map(|l| (l.id, Uuid::new_v4())).collect();
        let budget: i64 = originals.iter().filter_map(|l| source.renderer.image_size(l.id)).map(|(w, h)| w as i64 * h as i64).sum::<i64>() + self.renderer.images().values().map(|s| s.width() as i64 * s.height() as i64).sum::<i64>();
        if budget > 100_000_000 { bail!("Copying these layers would pass the 100-megapixel budget."); }
        let mut records = Vec::with_capacity(originals.len());
        let mut images = Vec::new();
        let mut masks = Vec::new();
        for l in &originals {
            let mut copy = l.clone();
            copy.id = fresh[&l.id];
            copy.parent_id = match l.parent_id { Some(p) if fresh.contains_key(&p) => Some(fresh[&p]), _ if ids.contains(&l.id) => parent, _ => parent };
            copy.mask_source_id = l.mask_source_id.and_then(|s| fresh.get(&s).copied());
            if copy.image_file.is_some() { copy.image_file = Some(format!("{}.png", crate::format::upper(copy.id))); }
            if copy.mask_file.is_some() { copy.mask_file = Some(format!("{}.mask.png", crate::format::upper(copy.id))); }
            if source.document_id == self.document_id && ids.contains(&l.id) { copy.name = format!("{} copy", l.name); }
            images.push(source.renderer.image(l.id).cloned());
            masks.push(source.renderer.mask(l.id).cloned());
            records.push(copy);
        }
        let anchor_index = Self::block_end(self.renderer.layers(), anchor, matches!(place, Place::Into(_)), self);
        let insert_at = if after { anchor_index + 1 } else { anchor_index };
        let new_ids: Vec<Uuid> = records.iter().map(|r| r.id).collect();
        let mut next = self.renderer.layers().to_vec();
        for (i, r) in records.iter().enumerate() { next.insert((insert_at + i).min(next.len()), r.clone()); }
        crate::format::validate::hierarchy(&next).map_err(|_| anyhow::anyhow!("That arrangement is not allowed."))?;
        self.begin_edit(if source.document_id == self.document_id { "Duplicate Layers" } else { "Copy Layers" });
        for (i, r) in records.into_iter().enumerate() { self.renderer.insert_layer((insert_at + i).min(self.renderer.layers().len()), r, images[i].clone(), masks[i].clone()); }
        let top = ids.iter().filter_map(|id| fresh.get(id)).copied().last();
        self.select_layer(top);
        self.selected = ids.iter().filter_map(|id| fresh.get(id)).copied().collect();
        if let Some(t) = top { self.selected.insert(t); }
        self.end_edit();
        Ok(new_ids)
    }

    /// A separate copy of the document with the same layers and pixels (shared, copy on write) and no
    /// history: what Export Sizes reshapes without touching the original.
    pub fn duplicate(&self) -> Result<Document> {
        let mut copy = Document::new(Project { path: std::path::PathBuf::new(), manifest: self.manifest(), images: self.renderer.images().clone(), masks: self.renderer.masks().clone() })?;
        copy.history = History::new(1, 1);
        Ok(copy)
    }

    /// Ctrl-drag within one document: the dragged layers duplicated at `place`.
    pub fn copy_layers_within(&mut self, ids: &[Uuid], place: Place) -> Result<Vec<Uuid>> {
        let snapshot = Document { renderer: Renderer::new(Project { path: std::path::PathBuf::new(), manifest: self.manifest(), images: self.renderer.images().clone(), masks: self.renderer.masks().clone() })?, selection: None, active: None, history: History::new(1, 1), stroke: None, stroke_layer: None, warp: None, stroke_mask: false, mask_target: false, document_id: self.document_id, path: None, dirty: None, selected: Default::default(), pixel_move: None, matte_cache: None, guides_v: Vec::new(), guides_h: Vec::new(), show_guides: true, snap: true, grid: None, last_selection: None, floating: None, last_filter: None, edit_serial: 0, effects_preview: None };
        self.copy_layers(&snapshot, ids, place)
    }

    /// The array index of `anchor`, or, `whole`, of the last layer inside it (a folder's top).
    fn block_end(layers: &[crate::format::Layer], anchor: Uuid, whole: bool, doc: &Document) -> usize {
        let own = layers.iter().position(|l| l.id == anchor).unwrap_or(layers.len().saturating_sub(1));
        if !whole { return own; }
        let inside = doc.descendants(anchor);
        layers.iter().enumerate().filter(|(_, l)| inside.contains(&l.id) || l.id == anchor).map(|(i, _)| i).max().unwrap_or(own)
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

    /// Opens a Photoshop file as a new document, with what the file had that was not carried over.
    pub fn open_psd(path: &std::path::Path) -> Result<(Document, Vec<String>)> {
        let (project, warnings) = crate::psd::read(path)?;
        let mut document = Document::new(project)?;
        document.history.mark_saved();
        Ok((document, warnings))
    }

    /// Writes the document as a Photoshop file; returns what was left out.
    pub fn export_psd(&mut self, path: &std::path::Path) -> Result<Vec<String>> { crate::psd::write(self, path) }

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
    /// Decodes an image file (PNG, JPEG, TIFF, GIF's first frame, WebP, BMP) with EXIF orientation applied.
    pub fn decode_image(path: &std::path::Path) -> Result<(ImageSurface, usize, usize)> {
        use image::ImageDecoder;
        if crate::heic::is_heic(path) { return crate::heic::decode(path); }
        let mut decoder = image::ImageReader::open(path)?.with_guessed_format()?.into_decoder()?;
        // The header's size, before any pixel buffer exists.
        let (dw, dh) = decoder.dimensions();
        if dw == 0 || dh == 0 || dw > 30_000 || dh > 30_000 || dw as u64 * dh as u64 > 100_000_000 { bail!("This image is {dw} x {dh}; sides run to 30,000 pixels and the whole to 100 megapixels."); }
        let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
        let mut decoded = image::DynamicImage::from_decoder(decoder)?;
        decoded.apply_orientation(orientation);
        let rgba = decoded.to_rgba8();
        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
        if w == 0 || h == 0 || w > 30_000 || h > 30_000 || w * h > 100_000_000 { bail!("This image is larger than the 30,000-pixel side or 100-megapixel limit."); }
        Ok((crate::png_io::from_straight_rgba(rgba.as_raw(), w, h)?, w, h))
    }

    /// Opens an image file as a new document of its size, the image as the only layer.
    pub fn open_image(path: &std::path::Path) -> Result<Document> {
        let (surface, w, h) = Self::decode_image(path)?;
        // The file's own resolution (a scan at 300 ppi stays 300 ppi); 72 when it says nothing.
        let head = std::fs::File::open(path).ok().and_then(|mut f| { use std::io::Read; let mut buf = vec![0u8; 65_536]; let n = f.read(&mut buf).ok()?; buf.truncate(n); Some(buf) }).unwrap_or_default();
        let resolution = crate::png_io::file_resolution(&head).unwrap_or(72.0);
        let mut document = Document::blank(w as i32, h as i32, resolution)?;
        let id = document.active.ok_or_else(|| anyhow::anyhow!("blank document has a layer"))?;
        let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Image".into());
        document.renderer.set_layer_name(id, name);
        document.renderer.set_image(id, surface);
        document.history.mark_saved();
        Ok(document)
    }

    /// Image bytes (PNG, JPEG and the rest) rather than a file: what a drop from another app carries.
    pub fn decode_image_bytes(bytes: &[u8]) -> Result<(ImageSurface, usize, usize)> {
        use image::ImageDecoder;
        let mut decoder = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?.into_decoder()?;
        let (dw, dh) = decoder.dimensions();
        if dw == 0 || dh == 0 || dw > 30_000 || dh > 30_000 || dw as u64 * dh as u64 > 100_000_000 { bail!("This image is {dw} x {dh}; sides run to 30,000 pixels and the whole to 100 megapixels."); }
        // The same EXIF turn a file opened from disk gets.
        let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
        let mut decoded = image::DynamicImage::from_decoder(decoder)?;
        decoded.apply_orientation(orientation);
        let rgba = decoded.to_rgba8();
        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
        if w == 0 || h == 0 || w > 30_000 || h > 30_000 || w * h > 100_000_000 { bail!("This image is larger than the 30,000-pixel side or 100-megapixel limit."); }
        Ok((crate::png_io::from_straight_rgba(rgba.as_raw(), w, h)?, w, h))
    }

    pub fn open_image_bytes(bytes: &[u8]) -> Result<Document> {
        let (surface, w, h) = Self::decode_image_bytes(bytes)?;
        let mut document = Document::blank(w as i32, h as i32, crate::png_io::file_resolution(bytes).unwrap_or(72.0))?;
        let id = document.active.ok_or_else(|| anyhow::anyhow!("blank document has a layer"))?;
        document.renderer.set_image(id, surface);
        document.history.mark_saved();
        Ok(document)
    }

    /// A new layer from image bytes placed at `origin` with `size` (document pixels), above the active one.
    pub fn add_image_layer(&mut self, bytes: &[u8], name: &str, origin: (f64, f64), size: (f64, f64)) -> Result<Uuid> {
        let (surface, _, _) = Self::decode_image_bytes(bytes)?;
        self.add_image_surface(surface, name, origin, size)
    }

    /// `add_image_layer` for pixels already decoded.
    pub fn add_image_surface(&mut self, surface: ImageSurface, name: &str, origin: (f64, f64), size: (f64, f64)) -> Result<Uuid> {
        let (index, parent) = self.insertion();
        let mut record = self.blank_record(name.to_string(), parent);
        record.transform.origin = crate::format::Point(origin.0, origin.1);
        record.transform.size = crate::format::Size(size.0.max(1.0), size.1.max(1.0));
        record.image_file = Some(format!("{}.png", crate::format::upper(record.id)));
        let id = record.id;
        self.begin_edit(name);
        self.renderer.insert_layer(index, record, Some(surface), None);
        self.select_layer(Some(id));
        self.end_edit();
        Ok(id)
    }

    pub fn import_image_bytes(&mut self, bytes: &[u8], name: &str) -> Result<Uuid> {
        let (surface, w, h) = Self::decode_image_bytes(bytes)?;
        self.import_surface(surface, w, h, name.to_string())
    }

    pub fn import_image(&mut self, path: &std::path::Path) -> Result<Uuid> {
        let (surface, w, h) = Self::decode_image(path)?;
        let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Image".into());
        self.import_surface(surface, w, h, name)
    }

    /// Decoded pixels as a new layer centered on the canvas at their own size.
    fn import_surface(&mut self, surface: ImageSurface, w: usize, h: usize, name: String) -> Result<Uuid> {
        let (index, parent) = self.insertion();
        let mut record = self.blank_record(name, parent);
        record.transform.origin = crate::format::Point(((self.width() as f64 - w as f64) / 2.0).round(), ((self.height() as f64 - h as f64) / 2.0).round());
        record.transform.size = crate::format::Size(w as f64, h as f64);
        record.image_file = Some(format!("{}.png", crate::format::upper(record.id)));
        let id = record.id;
        self.begin_edit("Import Image");
        self.renderer.insert_layer(index, record, Some(surface), None);
        self.select_layer(Some(id));
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
        if width as i64 * height as i64 > 100_000_000 { bail!("A canvas can hold up to 100 megapixels."); }
        let (ow, oh) = (self.width(), self.height());
        let (dx, dy) = offset.unwrap_or_else(|| ((((width - ow) as f64) * (anchor % 3) as f64 / 2.0).floor(), (((height - oh) as f64) * (anchor / 3) as f64 / 2.0).floor()));
        if width == ow && height == oh && dx == 0.0 && dy == 0.0 { return Ok(()); }
        self.edited(name, |d| d.canvas_size_body(width, height, fill, (dx, dy), (ow, oh)))
    }

    fn canvas_size_body(&mut self, width: i32, height: i32, fill: Option<[f64; 3]>, (dx, dy): (f64, f64), (ow, oh): (i32, i32)) -> Result<()> {
        for layer in self.renderer.layers().to_vec() {
            let mut t = layer.transform;
            t.origin = crate::format::Point(t.origin.0 + dx, t.origin.1 + dy);
            self.renderer.set_layer_transform(layer.id, t);
            if let Some(mut p) = layer.mask_placement { p.origin = crate::format::Point(p.origin.0 + dx, p.origin.1 + dy); self.renderer.set_mask_placement(layer.id, Some(p)); }
        }
        // Artboard frames and guides move with the pixels.
        self.map_artboards(|(x, y, w, h)| (x + dx, y + dy, w, h));
        for x in &mut self.guides_v { *x += dx; }
        for y in &mut self.guides_h { *y += dy; }
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
        Ok(())
    }

    /// Every artboard's frame put through `f` (a canvas that moved, scaled or flipped under it).
    fn map_artboards(&mut self, f: impl Fn((f64, f64, f64, f64)) -> (f64, f64, f64, f64)) {
        for (id, _, r) in self.artboards() {
            let (x, y, w, h) = f(r);
            let background = self.renderer.layer(id).artboard.as_ref().and_then(|b| b.background);
            self.renderer.set_artboard(id, Some(crate::format::Artboard { x, y, width: w.max(1.0), height: h.max(1.0), background }));
        }
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
        if width as i64 * height as i64 > 100_000_000 { bail!("An image can hold up to 100 megapixels."); }
        self.begin_edit("Image Size");
        match self.resample(width, height, resolution, sampling) {
            Ok(()) => { self.end_edit(); Ok(()) }
            Err(error) => { self.abort_edit(); Err(error) }
        }
    }

    fn resample(&mut self, width: i32, height: i32, resolution: f64, sampling: crate::format::Sampling) -> Result<()> {
        let (ow, oh) = (self.width(), self.height());
        if width != ow || height != oh {
            let (sx, sy) = (width as f64 / ow as f64, height as f64 / oh as f64);
            // Every layer's new box must be one the file can hold, before anything changes.
            for layer in self.renderer.layers() {
                let (bx0, by0, bx1, by1) = layer.transform.bounds();
                if (bx1 - bx0) * sx > 30_000.0 || (by1 - by0) * sy > 30_000.0 { bail!("\"{}\" would be wider than 30,000 pixels at that size.", layer.name); }
            }
            for layer in self.renderer.layers().to_vec() {
                let (bx0, by0, bx1, by1) = layer.transform.bounds();
                let (left, top) = ((bx0 * sx).floor(), (by0 * sy).floor());
                let (w, h) = (((bx1 * sx).ceil() - left).max(1.0) as i32, ((by1 * sy).ceil() - top).max(1.0) as i32);
                let placed = Transform { origin: crate::format::Point(left, top), size: crate::format::Size(w as f64, h as f64), rotation: 0.0, flip_x: false, flip_y: false, sampling };
                if let (Some(mut style), true) = (layer.text.clone(), self.renderer.has_image(layer.id)) {
                    // Type stays type: the style scales and the text is set again from it, in the same
                    // turn and flips, so a later edit keeps what the resize showed.
                    for (key, k) in [("size", sy), ("tracking", sy), ("width", sx)] { if let Some(v) = style.get(key).and_then(serde_json::Value::as_f64) { style[key] = serde_json::json!(v * k); } }
                    if let Some(parsed) = crate::text::TextStyle::from_record(&style) {
                        if let Ok((image, _, _)) = crate::text::render(&parsed) {
                            let t = layer.transform;
                            let (rw, rh) = self.renderer.image_size(layer.id).unwrap_or((1, 1));
                            // The layer's own stretch over its raster, carried along (sideways stretch follows sx over sy).
                            let stretch = (t.size.0 / rw.max(1) as f64 * sx / sy, t.size.1 / rh.max(1) as f64);
                            let size = crate::format::Size((image.width() as f64 * stretch.0).max(1.0), (image.height() as f64 * stretch.1).max(1.0));
                            let c = t.center();
                            let center = (c.0 * sx, c.1 * sy);
                            let turned = Transform { origin: crate::format::Point(center.0 - size.0 / 2.0, center.1 - size.1 / 2.0), size, rotation: t.rotation, flip_x: t.flip_x, flip_y: t.flip_y, sampling: t.sampling };
                            if self.renderer.mask(layer.id).is_some() && layer.mask_placement.is_none() { self.renderer.set_mask_placement(layer.id, Some(t)); }
                            self.renderer.set_text_image(layer.id, image, style);
                            self.renderer.set_layer_transform(layer.id, turned);
                            if let Some(p) = self.renderer.layer(layer.id).mask_placement { let mut scale = cairo::Matrix::identity(); scale.scale(sx, sy); self.renderer.set_mask_placement(layer.id, Some(p.placing(&cairo::Matrix::multiply(&p.unit_to_document(), &scale)))); }
                            continue;
                        }
                    }
                }
                if self.renderer.has_image(layer.id) {
                    self.renderer.set_sampling(layer.id, sampling);
                    let surface = new_argb(w, h)?;
                    {
                        let cr = Context::new(&surface)?;
                        cr.translate(-left, -top);
                        cr.scale(sx, sy);
                        self.renderer.draw_layer_plain(layer.id, &cr)?;
                    }
                    // Shape layers stay editable: a Path's points and base scale together (the redraw
                    // maps points through size over base, so both must move); other kinds redraw from
                    // the layer's size alone.
                    if let Some(mut style) = layer.shape.clone() {
                        if style.get("kind").and_then(serde_json::Value::as_str) == Some("Path") {
                            for (key, k) in [("baseWidth", sx), ("baseHeight", sy)] { if let Some(v) = style.get(key).and_then(serde_json::Value::as_f64) { style[key] = serde_json::json!(v * k); } }
                            if let Some(anchors) = style.get_mut("anchors").and_then(serde_json::Value::as_array_mut) {
                                for a in anchors.iter_mut() { for (key, k) in [("x", sx), ("ix", sx), ("ox", sx), ("y", sy), ("iy", sy), ("oy", sy)] { if let Some(v) = a.get(key).and_then(serde_json::Value::as_f64) { a[key] = serde_json::json!(v * k); } } }
                            }
                        } else if let Some(r) = style.get("radius").and_then(serde_json::Value::as_f64) { style["radius"] = serde_json::json!(r * sx.min(sy)); }
                        self.renderer.set_shape_image(layer.id, surface, style);
                    } else {
                        self.renderer.set_image(layer.id, surface);
                    }
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
            self.map_artboards(|(x, y, w, h)| (x * sx, y * sy, w * sx, h * sy));
            for x in &mut self.guides_v { *x *= sx; }
            for y in &mut self.guides_h { *y *= sy; }
            self.renderer.set_size(width, height);
            if let Some(selection) = self.selection.clone() {
                self.selection = Some(Selection::from_shape(width, height, selection.antialiased, |cr| { cr.scale(sx, sy); cr.set_source_surface(&selection.mask, 0.0, 0.0)?; cr.paint()?; Ok(()) })?);
            }
        }
        self.renderer.set_resolution(resolution);
        Ok(())
    }

    /// Expand (positive) or Contract (negative) the selection by whole pixels.
    /// Edit > Stroke: a line of `width` pixels in `color` along the selection's edge, inside it
    /// (`position` 0), centered on it (1) or outside it (2), at `opacity`, painted onto the active layer
    /// (or its mask when that is the target).
    pub fn stroke_selection(&mut self, width: f64, color: [f64; 3], position: u32, opacity: f64) -> Result<()> {
        let Some(current) = self.selection.clone() else { bail!("Make a selection first.") };
        if current.is_empty() { bail!("Make a selection first."); }
        if !(1.0..=250.0).contains(&width) { bail!("The stroke width runs from 1 to 250 pixels."); }
        let w = width.round() as i32;
        let band = match position {
            0 => { let inner = current.resized(-w)?; current.combined(&inner, Mode::Subtract)? }
            2 => { let outer = current.resized(w)?; outer.combined(&current, Mode::Subtract)? }
            _ => { let outside = (w + 1) / 2; let outer = current.resized(outside)?; let inner = current.resized(-(w - outside))?; outer.combined(&inner, Mode::Subtract)? }
        };
        let opacity = opacity.clamp(0.0, 1.0);
        if let Some((id, mask, grid)) = self.active_mask() {
            // On a mask the stroke is white or black through the band, as Fill would paint it.
            let _ = (id, mask, grid);
            self.begin_edit("Stroke");
            let saved = self.selection.replace(band);
            let result = self.fill_with(color, opacity);
            self.selection = saved;
            return match result { Ok(()) => { self.end_edit(); Ok(()) } Err(e) => { self.abort_edit(); Err(e) } };
        }
        let Some(id) = self.active else { bail!("Select a layer first.") };
        let layer = self.renderer.layer(id).clone();
        if layer.is_group() || layer.adjustment.is_some() { bail!("Select an image layer first."); }
        let (gw, gh) = self.renderer.image_size(id).unwrap_or((layer.transform.size.0.round().max(1.0) as i32, layer.transform.size.1.round().max(1.0) as i32));
        let result = new_argb(gw, gh)?;
        {
            let cr = Context::new(&result)?;
            if let Some(image) = self.renderer.image(id) { cr.set_source_surface(image, 0.0, 0.0)?; cr.paint()?; }
            let coverage = band.coverage_on_layer(&layer.transform, gw, gh)?;
            cr.set_source_rgba(color[0], color[1], color[2], opacity);
            cr.mask_surface(&coverage, 0.0, 0.0)?;
        }
        self.begin_edit("Stroke");
        self.renderer.set_image(id, result);
        self.end_edit();
        Ok(())
    }

    /// Select > Color Range: every pixel within `fuzziness` (0 to 200) of `color`, softly at the edge of
    /// that distance, from the composite or from the active layer alone.
    pub fn select_color_range(&mut self, color: [f64; 3], fuzziness: f64, all_layers: bool, mode: Mode) -> Result<()> {
        let (w, h) = (self.width(), self.height());
        let fuzz = fuzziness.clamp(0.0, 200.0);
        let sample = new_argb(w, h)?;
        {
            let cr = Context::new(&sample)?;
            cr.rectangle(0.0, 0.0, w as f64, h as f64);
            cr.clip();
            if all_layers { self.renderer.draw(&cr)?; }
            else if let Some(id) = self.active.filter(|id| !self.renderer.layer(*id).is_group()) { self.renderer.draw_layer_plain(id, &cr)?; }
            else { bail!("Select a layer first, or sample all layers."); }
        }
        let target = color.map(|c| (c.clamp(0.0, 1.0) * 255.0).round());
        let (wu, hu) = (w as usize, h as usize);
        let mut packed = vec![0u8; wu * hu];
        let mut count = 0usize;
        with_bytes(&sample, |data, stride| {
            for y in 0..hu {
                for x in 0..wu {
                    let p = &data[y * stride + x * 4..y * stride + x * 4 + 4];
                    let a = p[3] as f64;
                    if a <= 0.0 { continue; }
                    let straight = [p[2] as f64 * 255.0 / a, p[1] as f64 * 255.0 / a, p[0] as f64 * 255.0 / a];
                    // The distance is the largest channel difference; full coverage inside half the
                    // fuzziness, fading to none at the fuzziness itself.
                    let d = (straight[0] - target[0]).abs().max((straight[1] - target[1]).abs()).max((straight[2] - target[2]).abs());
                    let c = if fuzz <= 0.0 { if d < 0.5 { 1.0 } else { 0.0 } } else { ((fuzz - d) / (fuzz * 0.5)).clamp(0.0, 1.0) };
                    let v = (c * a).round() as u8;
                    if v > 0 { count += 1; }
                    packed[y * wu + x] = v;
                }
            }
        })?;
        if count == 0 {
            if mode == Mode::Replace { self.deselect(); }
            bail!("No pixels are within {} of that color.", fuzz.round());
        }
        let shape = Selection::from_packed(&packed, w, h)?;
        self.apply_selection(shape, mode, "Color Range")
    }

    /// The layers an alignment works on: every selected layer, or the active one.
    fn alignment_targets(&self) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = self.selected.iter().copied().filter(|id| self.has_layer(*id) && !self.renderer.layer(*id).is_group()).collect();
        if ids.is_empty() { if let Some(a) = self.active.filter(|a| !self.renderer.layer(*a).is_group()) { ids.push(a); } }
        ids.sort_by_key(|id| self.renderer.layer_index(*id));
        ids
    }

    /// Layer > Align: the selected layers' edges or centers to the selection's bounds, or to the canvas.
    /// `edge` is left, center, right, top, middle or bottom.
    pub fn align_layers(&mut self, edge: &str) -> Result<()> {
        let ids = self.alignment_targets();
        if ids.is_empty() { bail!("Select a layer first."); }
        let frame = match self.selection.as_ref().and_then(|s| s.bounds) { Some((x0, y0, x1, y1)) => (x0 as f64, y0 as f64, x1 as f64, y1 as f64), None => (0.0, 0.0, self.width() as f64, self.height() as f64) };
        self.begin_edit("Align Layers");
        for id in ids {
            let t = self.renderer.layer(id).transform;
            let (bx0, by0, bx1, by1) = t.bounds();
            let (dx, dy) = match edge {
                "left" => (frame.0 - bx0, 0.0),
                "center" => ((frame.0 + frame.2) / 2.0 - (bx0 + bx1) / 2.0, 0.0),
                "right" => (frame.2 - bx1, 0.0),
                "top" => (0.0, frame.1 - by0),
                "middle" => (0.0, (frame.1 + frame.3) / 2.0 - (by0 + by1) / 2.0),
                "bottom" => (0.0, frame.3 - by1),
                other => { self.abort_edit(); bail!("unknown edge {other}; use left, center, right, top, middle or bottom") }
            };
            if dx == 0.0 && dy == 0.0 { continue; }
            let mut moved = t;
            moved.origin = crate::format::Point(t.origin.0 + dx.round(), t.origin.1 + dy.round());
            self.set_transform(id, moved, "Align Layers");
        }
        self.end_edit();
        Ok(())
    }

    /// Layer > Distribute: three or more selected layers spaced evenly by their centers, left to right
    /// or top to bottom, the outermost two staying put.
    pub fn distribute_layers(&mut self, horizontal: bool) -> Result<()> {
        let ids = self.alignment_targets();
        if ids.len() < 3 { bail!("Select at least three layers to distribute."); }
        let mut centers: Vec<(Uuid, f64)> = ids.iter().map(|id| { let (x0, y0, x1, y1) = self.renderer.layer(*id).transform.bounds(); (*id, if horizontal { (x0 + x1) / 2.0 } else { (y0 + y1) / 2.0 }) }).collect();
        centers.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let (first, last) = (centers[0].1, centers[centers.len() - 1].1);
        let step = (last - first) / (centers.len() - 1) as f64;
        self.begin_edit("Distribute Layers");
        for (i, (id, center)) in centers.iter().enumerate() {
            let delta = (first + step * i as f64 - center).round();
            if delta == 0.0 { continue; }
            let mut t = self.renderer.layer(*id).transform;
            if horizontal { t.origin.0 += delta; } else { t.origin.1 += delta; }
            self.set_transform(*id, t, "Distribute Layers");
        }
        self.end_edit();
        Ok(())
    }

    /// Edit > Define Pattern: the selection's part of the composite (or the whole active layer without
    /// one) saved under `name` for Fill with Pattern and the Pattern Stamp.
    pub fn define_pattern(&mut self, name: &str) -> Result<()> {
        let (w, h) = (self.width(), self.height());
        let (x0, y0, x1, y1) = match self.selection.as_ref().and_then(|s| s.bounds) {
            Some(b) => (b.0 as i32, b.1 as i32, b.2 as i32, b.3 as i32),
            None => {
                let Some(id) = self.active else { bail!("Make a selection, or select a layer to use whole.") };
                let (bx0, by0, bx1, by1) = self.renderer.layer(id).transform.bounds();
                (bx0.floor().max(0.0) as i32, by0.floor().max(0.0) as i32, bx1.ceil().min(w as f64) as i32, by1.ceil().min(h as f64) as i32)
            }
        };
        let (pw, ph) = (x1 - x0, y1 - y0);
        if pw < 1 || ph < 1 { bail!("The pattern would be empty."); }
        if pw > 4096 || ph > 4096 { bail!("A pattern can be up to 4096 pixels on a side; select a smaller area."); }
        let whole_layer = self.selection.as_ref().and_then(|s| s.bounds).is_none();
        let tile = new_argb(pw, ph)?;
        {
            let cr = Context::new(&tile)?;
            cr.translate(-x0 as f64, -y0 as f64);
            cr.rectangle(x0 as f64, y0 as f64, pw as f64, ph as f64);
            cr.clip();
            // The selection takes the composite; a whole layer is that layer alone, background and all others left out.
            match (whole_layer, self.active) { (true, Some(id)) => self.renderer.draw_layer_plain(id, &cr)?, _ => self.renderer.draw(&cr)? }
        }
        crate::patterns::save(name, &tile)?;
        Ok(())
    }

    /// Edit > Fill with Pattern: `name` tiled from the document's origin at `scale` (1 is its own size)
    /// over the active layer inside the selection, at `opacity`; gray on a mask.
    pub fn fill_pattern(&mut self, name: &str, scale: f64, opacity: f64) -> Result<()> {
        let tile = crate::patterns::load(name)?;
        let scale = if scale.is_finite() { scale.clamp(0.05, 20.0) } else { 1.0 };
        let Some(id) = self.active else { bail!("Select a layer first.") };
        if self.selection.as_ref().is_some_and(|s| s.is_empty()) { bail!("Nothing is selected."); }
        let mask_target = self.active_mask().is_some();
        let (w, h, grid, base): (i32, i32, Transform, ImageSurface) = if let Some((_, mask, grid)) = self.active_mask() {
            let (w, h) = if mask.width() == 1 && mask.height() == 1 { self.renderer.image_size(id).unwrap_or((grid.size.0.round().max(1.0) as i32, grid.size.1.round().max(1.0) as i32)) } else { (mask.width(), mask.height()) };
            let target = if (w, h) == (mask.width(), mask.height()) { mask } else { let v = with_bytes(&mask, |d, _| d[0])?; crate::raster::a8_filled(w, h, v)? };
            (w, h, grid, gray_from_a8(&target)?)
        } else {
            let layer = self.renderer.layer(id).clone();
            if layer.is_group() || layer.adjustment.is_some() { bail!("Select an image layer first."); }
            let (w, h) = self.renderer.image_size(id).unwrap_or((layer.transform.size.0.round().max(1.0) as i32, layer.transform.size.1.round().max(1.0) as i32));
            let base = new_argb(w, h)?;
            if let Some(image) = self.renderer.image(id) { let cr = Context::new(&base)?; cr.set_source_surface(image, 0.0, 0.0)?; cr.paint()?; }
            (w, h, layer.transform, base)
        };
        let source = if mask_target { let (tw, th) = (tile.width(), tile.height()); let g = new_argb(tw, th)?; { let cr = Context::new(&g)?; cr.set_source_surface(&tile, 0.0, 0.0)?; cr.paint()?; } let (packed, pw, ph) = { let mut p = vec![0u8; tw as usize * th as usize * 4]; with_bytes(&g, |d, stride| { for y in 0..th as usize { p[y * tw as usize * 4..(y + 1) * tw as usize * 4].copy_from_slice(&d[y * stride..y * stride + tw as usize * 4]); } })?; (p, tw, th) }; let mut p = packed; crate::filters::map_straight(&mut p, |rgb| { let v = 0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2]; [v, v, v] }); crate::raster::argb_from_packed(pw, ph, p)? } else { tile };
        let result = new_argb(w, h)?;
        {
            let cr = Context::new(&result)?;
            cr.set_source_surface(&base, 0.0, 0.0)?;
            cr.paint()?;
            cr.transform(crate::selection::document_to_layer(&grid, w, h)?);
            let pattern = cairo::SurfacePattern::create(&source);
            pattern.set_extend(cairo::Extend::Repeat);
            pattern.set_filter(cairo::Filter::Good);
            let mut m = cairo::Matrix::identity();
            m.scale(1.0 / scale, 1.0 / scale);
            pattern.set_matrix(m);
            match &self.selection {
                Some(sel) => {
                    cr.identity_matrix();
                    let coverage = sel.coverage_on_layer(&grid, w, h)?;
                    cr.push_group();
                    cr.transform(crate::selection::document_to_layer(&grid, w, h)?);
                    cr.set_source(&pattern)?;
                    cr.paint_with_alpha(opacity.clamp(0.0, 1.0))?;
                    cr.pop_group_to_source()?;
                    cr.mask_surface(&coverage, 0.0, 0.0)?;
                }
                None => { cr.set_source(&pattern)?; cr.paint_with_alpha(opacity.clamp(0.0, 1.0))?; }
            }
        }
        if mask_target {
            let a8 = a8_from_gray(&result)?;
            self.begin_edit("Fill Mask with Pattern");
            self.renderer.set_mask(id, Some(a8));
            self.end_edit();
        } else {
            self.begin_edit("Fill with Pattern");
            self.renderer.set_image(id, result);
            self.end_edit();
        }
        Ok(())
    }

    /// A shape layer from a Pen path, in `color`: the path's own bounds become the layer, and the anchors
    /// are kept in the record so the shape redraws at any size and can be edited again.
    pub fn add_path_shape_layer(&mut self, path: &crate::path::Path, color: [f64; 3]) -> Result<Uuid> {
        if path.anchors.len() < 2 { bail!("Draw a path with at least two points first."); }
        let (x0, y0, x1, y1) = path_bounds(path);
        let (ox, oy) = (x0.floor(), y0.floor());
        let (w, h) = ((x1 - ox).ceil().max(1.0), (y1 - oy).ceil().max(1.0));
        if w * h > 100_000_000.0 || w > 30_000.0 || h > 30_000.0 { bail!("That shape is too large."); }
        let local = shift_path(path, -ox, -oy);
        let style = serde_json::json!({"kind": "Path", "red": color[0], "green": color[1], "blue": color[2], "cornerRadius": 0.0, "closed": local.closed, "baseWidth": w, "baseHeight": h, "anchors": anchors_json(&local)});
        let image = path_shape_image(&local, (w, h), w as i32, h as i32, color)?;
        let (index, parent) = self.insertion();
        let mut record = self.blank_record(self.unique_name("Shape"), parent);
        record.transform.origin = crate::format::Point(ox, oy);
        record.transform.size = crate::format::Size(w, h);
        record.image_file = Some(format!("{}.png", crate::format::upper(record.id)));
        record.shape = Some(style);
        let id = record.id;
        self.begin_edit("Shape from Path");
        self.renderer.insert_layer(index, record, Some(image), None);
        self.select_layer(Some(id));
        self.end_edit();
        Ok(id)
    }

    /// The path a Path shape layer was drawn from, in document pixels at the layer's current size
    /// (its rotation is not applied), or None for any other layer.
    pub fn shape_path(&self, id: Uuid) -> Option<crate::path::Path> {
        self.renderer.layer_index(id)?;
        let layer = self.renderer.layer(id);
        let style = layer.shape.as_ref()?;
        if style.get("kind").and_then(serde_json::Value::as_str) != Some("Path") { return None; }
        let local = anchors_from_json(style)?;
        let n = |k: &str| style.get(k).and_then(serde_json::Value::as_f64).unwrap_or(1.0).max(1.0);
        let (sx, sy) = (layer.transform.size.0 / n("baseWidth"), layer.transform.size.1 / n("baseHeight"));
        let (ox, oy) = (layer.transform.origin.0, layer.transform.origin.1);
        let map = |p: (f64, f64)| (ox + p.0 * sx, oy + p.1 * sy);
        Some(crate::path::Path { anchors: local.anchors.iter().map(|a| crate::path::Anchor { point: map(a.point), handle_in: a.handle_in.map(map), handle_out: a.handle_out.map(map) }).collect(), closed: local.closed })
    }

    /// Replaces a Path shape layer's path (Apply to Shape): the layer moves and resizes to the new bounds.
    pub fn set_shape_path(&mut self, id: Uuid, path: &crate::path::Path) -> Result<()> {
        if self.shape_path(id).is_none() { bail!("The active layer is not a shape made from a path."); }
        if path.anchors.len() < 2 { bail!("The path needs at least two points."); }
        let layer = self.renderer.layer(id).clone();
        let style = layer.shape.clone().unwrap_or_default();
        let n = |k: &str| style.get(k).and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        let color = [n("red"), n("green"), n("blue")];
        let (x0, y0, x1, y1) = path_bounds(path);
        let (ox, oy) = (x0.floor(), y0.floor());
        let (w, h) = ((x1 - ox).ceil().max(1.0), (y1 - oy).ceil().max(1.0));
        if w * h > 100_000_000.0 || w > 30_000.0 || h > 30_000.0 { bail!("That shape is too large."); }
        let local = shift_path(path, -ox, -oy);
        let mut new_style = style.clone();
        new_style["closed"] = serde_json::json!(local.closed);
        new_style["baseWidth"] = serde_json::json!(w);
        new_style["baseHeight"] = serde_json::json!(h);
        new_style["anchors"] = anchors_json(&local);
        let image = path_shape_image(&local, (w, h), w as i32, h as i32, color)?;
        let mut transform = layer.transform;
        transform.origin = crate::format::Point(ox, oy);
        transform.size = crate::format::Size(w, h);
        self.begin_edit("Edit Shape");
        if self.renderer.mask(id).is_some() && layer.mask_placement.is_none() { self.renderer.set_mask_placement(id, Some(layer.transform)); }
        self.renderer.set_layer_transform(id, transform);
        self.renderer.set_shape_image(id, image, new_style);
        self.end_edit();
        Ok(())
    }

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

/// A mask as an opaque gray ARGB image, so pixel filters can run on it.
fn gray_from_a8(mask: &ImageSurface) -> Result<ImageSurface> {
    let (w, h) = (mask.width(), mask.height());
    let (wu, hu) = (w as usize, h as usize);
    let out = new_argb(w, h)?;
    let rows = with_bytes(mask, |d, stride| (0..hu).flat_map(|y| d[y * stride..y * stride + wu].to_vec()).collect::<Vec<u8>>())?;
    crate::raster::with_bytes_raw_mut(&out, |d, stride| { for y in 0..hu { for x in 0..wu { let v = rows[y * wu + x]; let o = y * stride + x * 4; d[o] = v; d[o + 1] = v; d[o + 2] = v; d[o + 3] = 255; } } })?;
    Ok(out)
}

/// Opaque gray ARGB back to an A8 mask (the blue channel).
fn a8_from_gray(gray: &ImageSurface) -> Result<ImageSurface> {
    let (w, h) = (gray.width(), gray.height());
    let (wu, hu) = (w as usize, h as usize);
    let stride = cairo::Format::A8.stride_for_width(w as u32)? as usize;
    let mut data = vec![0u8; stride * hu];
    // Luminance, so a filter that colors the gray (Photo Filter, a colorized Hue/Saturation) leaves a
    // sensible mask rather than one channel of it.
    with_bytes(gray, |d, s| { for y in 0..hu { for x in 0..wu { let o = y * s + x * 4; data[y * stride + x] = (0.114 * d[o] as f64 + 0.587 * d[o + 1] as f64 + 0.299 * d[o + 2] as f64).round().clamp(0.0, 255.0) as u8; } } })?;
    crate::raster::a8_from_data(w, h, data, stride as i32)
}

/// The shape filling its box, anti-aliased where it curves (`shapeImage`).
fn shape_image(ellipse: bool, w: i32, h: i32, color: [f64; 3], radius: f64) -> Result<ImageSurface> {
    let out = new_argb(w, h)?;
    let cr = Context::new(&out)?;
    cr.set_source_rgb(color[0], color[1], color[2]);
    let (fw, fh) = (w as f64, h as f64);
    if ellipse {
        cr.save()?;
        cr.translate(fw / 2.0, fh / 2.0);
        cr.scale(fw / 2.0, fh / 2.0);
        cr.arc(0.0, 0.0, 1.0, 0.0, std::f64::consts::TAU);
        cr.restore()?;
    } else {
        let r = radius.max(0.0).min(fw / 2.0).min(fh / 2.0);
        if r > 0.0 {
            use std::f64::consts::PI;
            cr.new_sub_path();
            cr.arc(fw - r, r, r, -PI / 2.0, 0.0);
            cr.arc(fw - r, fh - r, r, 0.0, PI / 2.0);
            cr.arc(r, fh - r, r, PI / 2.0, PI);
            cr.arc(r, r, r, PI, 3.0 * PI / 2.0);
            cr.close_path();
        } else { cr.rectangle(0.0, 0.0, fw, fh); }
    }
    cr.fill()?;
    drop(cr);
    Ok(out)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AutoLevels { Tone, Contrast, Color }

/// A type layer is named after its first line, as Photoshop names them.
fn text_layer_name(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("Type").trim();
    let name: String = line.chars().take(40).collect();
    if name.is_empty() { "Type".into() } else { name }
}


/// The box around a path's anchors and handles.
fn path_bounds(path: &crate::path::Path) -> (f64, f64, f64, f64) {
    let mut b = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for a in &path.anchors { for p in [Some(a.point), a.handle_in, a.handle_out].into_iter().flatten() { b.0 = b.0.min(p.0); b.1 = b.1.min(p.1); b.2 = b.2.max(p.0); b.3 = b.3.max(p.1); } }
    if !b.0.is_finite() { (0.0, 0.0, 1.0, 1.0) } else { b }
}

fn shift_path(path: &crate::path::Path, dx: f64, dy: f64) -> crate::path::Path {
    let m = |p: (f64, f64)| (p.0 + dx, p.1 + dy);
    crate::path::Path { anchors: path.anchors.iter().map(|a| crate::path::Anchor { point: m(a.point), handle_in: a.handle_in.map(m), handle_out: a.handle_out.map(m) }).collect(), closed: path.closed }
}

fn anchors_json(path: &crate::path::Path) -> serde_json::Value {
    serde_json::Value::Array(path.anchors.iter().map(|a| serde_json::json!({"x": a.point.0, "y": a.point.1, "ix": a.handle_in.map(|h| h.0), "iy": a.handle_in.map(|h| h.1), "ox": a.handle_out.map(|h| h.0), "oy": a.handle_out.map(|h| h.1)})).collect())
}

fn anchors_from_json(style: &serde_json::Value) -> Option<crate::path::Path> {
    let list = style.get("anchors")?.as_array()?;
    let f = |v: &serde_json::Value, k: &str| v.get(k).and_then(serde_json::Value::as_f64);
    let anchors = list.iter().filter_map(|a| Some(crate::path::Anchor { point: (f(a, "x")?, f(a, "y")?), handle_in: f(a, "ix").zip(f(a, "iy")), handle_out: f(a, "ox").zip(f(a, "oy")) })).collect();
    Some(crate::path::Path { anchors, closed: style.get("closed").and_then(serde_json::Value::as_bool).unwrap_or(true) })
}

/// A Path shape's pixels at `w` x `h`: the local path (drawn at `base` size) scaled to fit, filled.
fn path_shape_image(local: &crate::path::Path, base: (f64, f64), w: i32, h: i32, color: [f64; 3]) -> Result<ImageSurface> {
    let out = new_argb(w, h)?;
    let cr = Context::new(&out)?;
    cr.set_source_rgb(color[0], color[1], color[2]);
    let (sx, sy) = (w as f64 / base.0.max(1.0), h as f64 / base.1.max(1.0));
    let closed = crate::path::Path { anchors: local.anchors.clone(), closed: true };
    closed.trace(&cr, |p| (p.0 * sx, p.1 * sy));
    cr.fill()?;
    Ok(out)
}

/// A layer or board name as a file name: letters, digits, spaces, dashes and underscores.
pub fn clean_file_name(name: &str) -> String {
    let clean: String = name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' }).collect();
    // Well under the 255 bytes a file name may have, with room for a suffix and an extension.
    let mut clean = clean.trim().to_string();
    while clean.len() > 120 { clean.pop(); }
    let clean = clean.trim().to_string();
    if clean.is_empty() { "untitled".to_string() } else { clean }
}

/// `stem.ext` in `folder`, or `stem-2.ext` and on when that name is already taken in this batch, so two
/// names that clean to the same text never overwrite each other.
pub fn unique_file(folder: &std::path::Path, stem: &str, ext: &str, used: &mut std::collections::HashSet<String>) -> std::path::PathBuf {
    let mut n = 1;
    loop {
        let name = if n == 1 { format!("{stem}.{ext}") } else { format!("{stem}-{n}.{ext}") };
        // Taken in this batch, or already on disk from an earlier one.
        if !folder.join(&name).exists() && used.insert(name.to_lowercase()) { return folder.join(name); }
        n += 1;
    }
}
