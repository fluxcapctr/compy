//! The canvas: the tool rail, the options bar, the document on a dark ground with a checkerboard behind it
//! (panned and zoomed by the viewport), the marching ants of the selection, and the status line. Scrolling
//! pans, Ctrl+scroll and pinching zoom around the pointer, a middle drag, a Hand drag or a drag with Space
//! held pans. From 200% document pixels are drawn hard-edged, and from 800% a pixel grid shows.

use super::tools::{OptionsBar, Tool, ToolRail};
use super::DocRef;
use crate::document::StrokeKind;
use crate::raster::new_argb;
use crate::selection::Mode;
use crate::transform::{Drag, Geometry, Mode as DragMode, SNAP_DISTANCE};
use anyhow::Result;
use cairo::{Context, Filter};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// From two screen pixels per document pixel the canvas shows hard-edged pixels, as Photoshop does.
pub const CRISP_ZOOM: f64 = 2.0;
pub const PIXEL_GRID_ZOOM: f64 = 8.0;

pub struct Canvas {
    pub widget: gtk::Box,
    pub area: gtk::DrawingArea,
    pub zoom_label: gtk::Label,
    message: gtk::Label,
    doc: DocRef,
    space_held: Rc<Cell<bool>>,
    pointer: Rc<Cell<(f64, f64)>>,
    dragging: Rc<Cell<bool>>,
    rail: Rc<ToolRail>,
    pub options: OptionsBar,
    ants: Cell<Option<glib::SourceId>>,
    painting: Cell<bool>,
    stroke_start: Cell<(f64, f64)>,
    refresh: RefCell<Option<Rc<dyn Fn()>>>,
    /// A transform drag in progress: the drag, the layers it moves and their transforms when it began.
    transform_drag: RefCell<Option<(Drag, Vec<(uuid::Uuid, crate::format::Transform)>)>>,
    /// The transform drag is moving selected pixels rather than a layer.
    pixel_moving: Cell<bool>,
    picture: gtk::Picture,
    /// A gradient, shape or crop drag: what it started on and where (document pixels).
    tool_drag: Cell<Option<ToolDrag>>,
    /// A guide being dragged: vertical (an x) or not, and its index.
    guide_drag: Cell<Option<(bool, usize)>>,
    options_scroller: gtk::ScrolledWindow,
    status: gtk::Box,
    /// The last composited frame, kept while nothing it shows has changed.
    cache: RefCell<Option<FrameCache>>,
    /// The popover open over the canvas (brush settings or a context menu), closed by the next press.
    popover: RefCell<Option<gtk::Popover>>,
    /// Text being typed on the canvas: the type layer and the caret's byte index in its text.
    text_edit: RefCell<Option<(uuid::Uuid, usize)>>,
    /// A Pen drag in progress: the anchor being placed (its handles follow), or an anchor or handle moved.
    pen_drag: Cell<Option<PenDrag>>,
    /// What the picture last showed straight from the GPU: the document revision and viewport, and the size.
    presented_revision: Cell<Option<(u64, crate::viewport::Viewport)>>,
    presented_size: Cell<(i32, i32, i32)>,
    /// Return with the Move tool puts this layer's transform handles away until the next click on the
    /// canvas, a new transform, another layer, or Transform Controls switched on.
    handles_parked: Cell<Option<uuid::Uuid>>,
    /// A marquee or lasso being drawn.
    draft: RefCell<Option<Draft>>,
    /// Dragging a selection outline: its offset so far (document pixels).
    outline_move: Cell<Option<(f64, f64)>>,
}

/// The composited document as last drawn: the ground, checkerboard, layers, grid and border, at device
/// resolution, with what it was drawn for. Overlays (cursor, ants, handles, drafts) go on top each frame.
struct FrameCache {
    surface: cairo::ImageSurface,
    width: i32,
    height: i32,
    scale: i32,
    viewport: crate::viewport::Viewport,
    revision: u64,
}

/// A selection outline in progress, in document pixels.
pub struct Draft {
    pub points: Vec<(f64, f64)>,
    pub cursor: Option<(f64, f64)>,
    pub mode: Mode,
    pub kind: DraftKind,
    pub anchor: (f64, f64),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DraftKind { Rectangle, Ellipse, Freehand, Polygonal }

/// The rulers' thickness in view points.
const RULER: f64 = 18.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum PenDrag { Place(usize), Anchor(usize), HandleIn(usize), HandleOut(usize) }

/// What a Gradient, Shape or Crop drag is doing, from `start` (document pixels).
#[derive(Clone, Copy, PartialEq)]
enum ToolDrag {
    Gradient { start: (f64, f64) },
    Shape { anchor: (f64, f64) },
    /// Creating a frame, moving it, or dragging one of its handles; `original` is the frame at the press.
    Crop { start: (f64, f64), original: Option<(f64, f64, f64, f64)>, mode: DragMode },
}

impl Drop for Canvas {
    fn drop(&mut self) { if let Some(id) = self.ants.take() { id.remove(); } }
}

impl Canvas {
    pub fn new(doc: DocRef, space_held: Rc<Cell<bool>>) -> Rc<Canvas> {
        let area = gtk::DrawingArea::builder().hexpand(true).vexpand(true).focusable(true).build();
        // The composited frame lives in a texture GTK keeps on the GPU; the drawing area above it only
        // paints overlays, so a cursor move or an ants step costs a few lines, not a 33 MB upload.
        let picture = gtk::Picture::builder().hexpand(true).vexpand(true).can_shrink(true).content_fit(gtk::ContentFit::Fill).build();
        let stage = gtk::Overlay::builder().child(&picture).build();
        stage.add_overlay(&area);
        let zoom_label = gtk::Label::builder().label("100%").width_chars(7).xalign(1.0).css_classes(["numeric"]).build();
        let size_label = gtk::Label::builder().css_classes(["dim-label"]).build();
        let message = gtk::Label::builder().css_classes(["dim-label"]).hexpand(true).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
        {
            let d = doc.borrow();
            let (w, h) = d.size();
            size_label.set_label(&format!("{w} × {h} px · {} ppi", d.document.renderer.resolution()));
        }
        let status = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(16).margin_start(12).margin_end(12).margin_top(4).margin_bottom(4).css_classes(["canvas-status"]).build();
        status.append(&zoom_label);
        status.append(&size_label);
        status.append(&message);
        let options = OptionsBar::new(doc.clone());
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // The bar scrolls sideways (no visible bar) rather than widening the window past the panel.
        let options_scroller = gtk::ScrolledWindow::builder().child(&options.widget).hscrollbar_policy(gtk::PolicyType::External).vscrollbar_policy(gtk::PolicyType::Never).propagate_natural_height(true).build();
        column.append(&options_scroller);
        column.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        column.append(&stage);
        column.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        column.append(&status);
        let widget = gtk::Box::new(gtk::Orientation::Horizontal, 0);

        let canvas = Rc::new_cyclic(|weak: &std::rc::Weak<Canvas>| {
            let weak = weak.clone();
            let rail = ToolRail::new(doc.clone(), Rc::new(move || { if let Some(c) = weak.upgrade() { c.finish_text_edit(); c.tool_changed(); } }));
            widget.append(&rail.widget);
            widget.append(&gtk::Separator::new(gtk::Orientation::Vertical));
            widget.append(&column);
            Canvas { widget: widget.clone(), area: area.clone(), zoom_label, message, doc, space_held, pointer: Rc::new(Cell::new((-1.0e9, -1.0e9))), dragging: Rc::new(Cell::new(false)), rail, options, ants: Cell::new(None), painting: Cell::new(false), stroke_start: Cell::new((0.0, 0.0)), refresh: RefCell::new(None), transform_drag: RefCell::new(None), pixel_moving: Cell::new(false), picture: picture.clone(), tool_drag: Cell::new(None), guide_drag: Cell::new(None), options_scroller: options_scroller.clone(), status: status.clone(), draft: RefCell::new(None), outline_move: Cell::new(None), cache: RefCell::new(None), popover: RefCell::new(None), text_edit: RefCell::new(None), handles_parked: Cell::new(None), pen_drag: Cell::new(None), presented_revision: Cell::new(None), presented_size: Cell::new((0, 0, 0)) }
        });
        canvas.connect();
        canvas.update_cursor();
        canvas
    }

    pub fn doc(&self) -> &DocRef { &self.doc }

    /// Whether the document or view moved on since the last presented frame.
    fn doc_revision_changed(&self, d: &super::Doc) -> bool {
        match self.presented_revision.get() { Some((r, v)) => r != d.document.renderer.revision() || v != d.viewport, None => true }
    }

    /// Brings the cached frame up to date: nothing when the document and view are as they were, only the
    /// changed region after a stroke step, everything otherwise. Returns what it did, for tracing.
    fn cached_frame(&self, doc: &mut super::Doc, w: i32, h: i32, scale: i32) -> Result<&'static str> {
        let revision = doc.document.renderer.revision();
        let dirty = doc.document.take_dirty();
        let mut cache = self.cache.borrow_mut();
        let fresh = cache.as_ref().is_none_or(|c| c.width != w || c.height != h || c.scale != scale || c.viewport != doc.viewport);
        if fresh {
            let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w * scale, h * scale)?;
            surface.set_device_scale(scale as f64, scale as f64);
            *cache = Some(FrameCache { surface, width: w, height: h, scale, viewport: doc.viewport, revision: 0 });
        }
        let c = cache.as_mut().unwrap();
        if std::env::var_os("COMPOSITOR_TRACE").is_some() { eprintln!("  revision {} (cached {}), fresh {fresh}, dirty {dirty:?}", revision, c.revision); }
        if c.revision == revision { return Ok("cached"); }
        let cr = Context::new(&c.surface)?;
        let what = match (fresh, dirty) {
            (false, Some((x0, y0, x1, y1))) => {
                // Just the touched part, with a little room for antialiasing and resampling.
                let size = doc.size();
                let (vx0, vy0) = doc.viewport.view_point((x0, y0), size);
                let (vx1, vy1) = doc.viewport.view_point((x1, y1), size);
                cr.rectangle(vx0.min(vx1).floor() - 2.0, vy0.min(vy1).floor() - 2.0, (vx1 - vx0).abs().ceil() + 4.0, (vy1 - vy0).abs().ceil() + 4.0);
                cr.clip();
                "partial"
            }
            _ => "full",
        };
        let t = std::time::Instant::now();
        draw_document(doc, &cr, w as f64, h as f64)?;
        if std::env::var_os("COMPOSITOR_TRACE").is_some() { eprintln!("  render {what} {:.1} ms", t.elapsed().as_secs_f64() * 1000.0); }
        c.revision = revision;
        Ok(what)
    }

    fn connect(self: &Rc<Self>) {
        let area = &self.area;
        {
            let doc = self.doc.clone();
            let label = self.zoom_label.clone();
            area.connect_resize(move |area, w, h| {
                let mut d = doc.borrow_mut();
                let size = d.size();
                d.viewport.resize((w as f64, h as f64), area.scale_factor() as f64, Some(size));
                label.set_label(&zoom_text(d.viewport.zoom()));
            });
        }
        {
            let doc = self.doc.clone();
            let pointer = self.pointer.clone();
            let trace = std::env::var_os("COMPOSITOR_TRACE").is_some();
            let (draft_ref, outline_ref, this) = (self.clone(), self.clone(), self.clone());
            area.set_draw_func(move |area, cr, w, h| {
                let start = std::time::Instant::now();
                let mut d = doc.borrow_mut();
                let draft = draft_ref.draft.borrow();
                let offset = outline_ref.outline_move.get().unwrap_or((0.0, 0.0));
                let scale = area.scale_factor();
                // Straight to the screen through the GPU when it can present; else the CPU frame cache.
                let presented = if this.doc_revision_changed(&d) || this.presented_size.get() != (w, h, scale) { present_frame(&mut d, w, h, scale) } else { None };
                let rendered = if let Some(texture) = presented {
                    d.document.take_dirty();
                    this.picture.set_paintable(Some(&texture));
                    this.presented_size.set((w, h, scale));
                    this.presented_revision.set(Some((d.document.renderer.revision(), d.viewport)));
                    "presented"
                } else if this.presented_revision.get().is_some_and(|(r, v)| r == d.document.renderer.revision() && v == d.viewport) && this.presented_size.get() == (w, h, scale) {
                    "presented"
                } else {
                    this.presented_revision.set(None);
                    match this.cached_frame(&mut d, w, h, scale) {
                        Ok(rendered) => rendered,
                        Err(error) => { eprintln!("canvas draw failed: {error:#}"); "failed" }
                    }
                };
                let t = std::time::Instant::now();
                // A changed frame becomes a new texture for the picture underneath; an unchanged one is left
                // to the GPU copy GTK already holds.
                if rendered != "cached" && rendered != "presented" {
                    if let Some(cache) = this.cache.borrow().as_ref() {
                        let (cw, ch) = (cache.surface.width(), cache.surface.height());
                        let stride = cache.surface.stride() as usize;
                        if let Ok(bytes) = crate::raster::with_bytes(&cache.surface, |d, _| glib::Bytes::from(&d[..stride * ch as usize])) {
                            let texture = gdk::MemoryTexture::new(cw, ch, gdk::MemoryFormat::B8g8r8a8Premultiplied, &bytes, stride);
                            this.picture.set_paintable(Some(&texture));
                        }
                    }
                }
                if trace { eprintln!("  texture {:.1} ms", t.elapsed().as_secs_f64() * 1000.0); }
                let text_edit = *this.text_edit.borrow();
                if let Err(error) = draw_overlays(&mut d, cr, w as f64, h as f64, pointer.get(), draft.as_ref(), offset, text_edit) { eprintln!("canvas overlay failed: {error:#}"); }
                if trace { eprintln!("frame {:.1} ms at {} ({rendered})", start.elapsed().as_secs_f64() * 1000.0, zoom_text(d.viewport.zoom())); }
            });
        }
        // Marching ants advance while a drawable selection exists.
        {
            let (doc, area) = (self.doc.clone(), area.clone());
            let id = glib::timeout_add_local(std::time::Duration::from_millis(120), move || {
                if let Ok(mut d) = doc.try_borrow_mut() {
                    if d.document.selection.as_ref().is_some_and(|s| !s.outline.is_empty()) {
                        d.ants_phase = (d.ants_phase + 1.0) % 8.0;
                        area.queue_draw();
                    }
                }
                glib::ControlFlow::Continue
            });
            self.ants.set(Some(id));
        }
        let motion = gtk::EventControllerMotion::new();
        {
            let this = self.clone();
            motion.connect_motion(move |_, x, y| {
                this.pointer.set((x, y));
                let tool = this.doc.borrow().tool;
                if tool.is_brush() || this.doc.borrow().rulers || (tool == Tool::Pen && !this.doc.borrow().pen_done) { this.area.queue_draw(); }
                if tool == Tool::Move && !this.dragging.get() && this.transform_drag.borrow().is_none() { this.update_cursor(); }
                let polygonal = this.draft.borrow().as_ref().is_some_and(|d| d.kind == DraftKind::Polygonal);
                if polygonal {
                    let point = { let d = this.doc.borrow(); let size = d.size(); d.viewport.document_point((x, y), size) };
                    if let Some(draft) = this.draft.borrow_mut().as_mut() { draft.cursor = Some(point); }
                    this.area.queue_draw();
                }
            });
            let left = self.clone();
            motion.connect_leave(move |_| { left.pointer.set((-1.0e9, -1.0e9)); left.area.queue_draw(); });
        }
        area.add_controller(motion);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        {
            let this = self.clone();
            scroll.connect_scroll(move |ctrl, dx, dy| {
                let wheel = ctrl.unit() == gdk::ScrollUnit::Wheel;
                let mut d = this.doc.borrow_mut();
                let size = d.size();
                if ctrl.current_event_state().intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
                    let delta = if wheel { dy * 10.0 } else { dy };
                    let zoom = d.viewport.zoom() * (-delta * 0.015).exp();
                    d.viewport.set_zoom(zoom, this.pointer.get(), size);
                } else {
                    let step = if wheel { 40.0 } else { 1.0 };
                    d.viewport.translate(-dx * step, -dy * step);
                }
                this.zoom_label.set_label(&zoom_text(d.viewport.zoom()));
                this.area.queue_draw();
                glib::Propagation::Stop
            });
        }
        area.add_controller(scroll);

        let pinch = gtk::GestureZoom::new();
        {
            let this = self.clone();
            let start = Rc::new(Cell::new(1.0));
            { let (start, doc) = (start.clone(), self.doc.clone()); pinch.connect_begin(move |_, _| start.set(doc.borrow().viewport.zoom())); }
            pinch.connect_scale_changed(move |g, scale| {
                let anchor = g.bounding_box_center().unwrap_or((0.0, 0.0));
                let mut d = this.doc.borrow_mut();
                let size = d.size();
                d.viewport.set_zoom(start.get() * scale, anchor, size);
                this.zoom_label.set_label(&zoom_text(d.viewport.zoom()));
                this.area.queue_draw();
            });
        }
        area.add_controller(pinch);

        let context = gtk::GestureClick::new();
        context.set_button(3);
        {
            let this = self.clone();
            context.connect_pressed(move |g, _, x, y| {
                this.close_popover();
                let tool = this.doc.borrow().tool;
                if tool.is_brush() { g.set_state(gtk::EventSequenceState::Claimed); this.brush_popover(x, y); return; }
                if tool == Tool::Pen && !this.doc.borrow().pen.is_empty() {
                    g.set_state(gtk::EventSequenceState::Claimed);
                    let menu = gio::Menu::new();
                    menu.append(Some("Make Selection"), Some("win.path-select"));
                    menu.append(Some("Fill Path"), Some("win.path-fill"));
                    menu.append(Some("Stroke with Brush"), Some("win.path-stroke"));
                    menu.append(Some("Clear Path"), Some("win.path-clear"));
                    let popover = gtk::PopoverMenu::from_model(Some(&menu));
                    popover.set_parent(&this.area);
                    popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
                    this.track_popover(popover.upcast_ref());
                    popover.popup();
                    return;
                }
                // Over a selection: what can be done with it, Generative Fill first.
                let inside = { let d = this.doc.borrow(); let size = d.size(); let p = d.viewport.document_point((x, y), size); d.document.selection.as_ref().is_some_and(|s| !s.is_empty() && s.contains(p.0.floor(), p.1.floor())) };
                g.set_state(gtk::EventSequenceState::Claimed);
                let menu = gio::Menu::new();
                if inside {
                    menu.append(Some("Generative Fill…"), Some("win.generative-fill"));
                    menu.append(Some("Free Transform"), Some("win.free-transform"));
                    menu.append(Some("Layer via Copy"), Some("win.duplicate-layer"));
                    menu.append(Some("Layer via Cut"), Some("win.layer-via-cut"));
                    menu.append(Some("Fill with Foreground"), Some("win.fill-foreground"));
                    menu.append(Some("Fill with Background"), Some("win.fill-background"));
                    menu.append(Some("Content-Aware Fill"), Some("win.content-aware-fill"));
                    menu.append(Some("Clear"), Some("win.clear"));
                    menu.append(Some("Copy"), Some("win.copy"));
                    menu.append(Some("Copy Merged"), Some("win.copy-merged"));
                    menu.append(Some("Feather…"), Some("win.feather-selection"));
                    menu.append(Some("Select Inverse"), Some("win.invert-selection"));
                    menu.append(Some("Crop to Selection"), Some("win.crop"));
                    menu.append(Some("Deselect"), Some("win.deselect"));
                } else if let Some((vertical, index)) = this.guide_at((x, y)) {
                    // Over a guide: the guide itself.
                    menu.append(Some("Delete Guide"), Some(&format!("win.delete-guide::{}{index}", if vertical { "v" } else { "h" })));
                    menu.append(Some("Clear Guides"), Some("win.clear-guides"));
                    menu.append(Some("Snap"), Some("win.toggle-snap"));
                    menu.append(Some("Show Grid"), Some("win.toggle-grid"));
                } else {
                    // Anywhere else: the layers under the pointer to pick from, then what the layer can do.
                    let under = {
                        let d = this.doc.borrow();
                        let size = d.size();
                        let p = d.viewport.document_point((x, y), size);
                        let layers = d.document.renderer.layers();
                        crate::format::entries_ordered(layers, true).into_iter().filter(|e| e.visible && !e.layer.is_group() && e.layer.adjustment.is_none() && e.layer.transform.contains(p)).map(|e| (e.layer.id, e.layer.name.clone())).take(8).collect::<Vec<_>>()
                    };
                    if !under.is_empty() {
                        let pick = gio::Menu::new();
                        for (id, name) in under { pick.append(Some(&name), Some(&format!("win.select-layer-id::{id}"))); }
                        menu.append_section(Some("Layers here"), &pick);
                    }
                    let is_text = { let d = this.doc.borrow(); d.document.active.is_some_and(|id| d.document.text_style(id).is_some()) };
                    let actions = gio::Menu::new();
                    if is_text { actions.append(Some("Edit Text…"), Some("win.edit-text")); }
                    actions.append(Some("Layer Style…"), Some("win.layer-style"));
                    actions.append(Some("Duplicate Layer"), Some("win.duplicate-layer"));
                    actions.append(Some("Delete Layer"), Some("win.delete-layer"));
                    actions.append(Some("Load Layer Pixels"), Some("win.select-layer-pixels"));
                    actions.append(Some("Flip Horizontal"), Some("win.flip-horizontal"));
                    actions.append(Some("Flip Vertical"), Some("win.flip-vertical"));
                    actions.append(Some("Select All"), Some("win.select-all"));
                    actions.append(Some("Paste as New Layer"), Some("win.paste"));
                    menu.append_section(None, &actions);
                }
                let popover = gtk::PopoverMenu::from_model(Some(&menu));
                popover.set_parent(&this.area);
                popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
                this.track_popover(popover.upcast_ref());
                popover.popup();
            });
        }
        area.add_controller(context);
        let click = gtk::GestureClick::new();
        click.set_button(1);
        {
            let this = self.clone();
            click.connect_pressed(move |g, n, x, y| {
                this.close_popover();
                if this.space_held.get() { return; }
                this.handles_parked.set(None);
                if this.text_editing() && this.doc.borrow().tool != Tool::Type { this.finish_text_edit(); }
                if n == 2 && this.doc.borrow().rulers && (x < RULER || y < RULER) {
                    // The single press already made and placed the guide; the second click leaves it there.
                    this.finish_guide_drag();
                    return;
                }
                let tool = this.doc.borrow().tool;
                let state = g.current_event_state();
                match tool {
                    Tool::Lasso if this.doc.borrow().lasso_polygonal => {
                        let point = { let d = this.doc.borrow(); let size = d.size(); d.viewport.document_point((x, y), size) };
                        let mut finish = false;
                        {
                            let mut draft = this.draft.borrow_mut();
                            match draft.as_mut() {
                                Some(d) => {
                                    let first = { let dd = this.doc.borrow(); let size = dd.size(); dd.viewport.view_point(d.points[0], size) };
                                    if (first.0 - x).hypot(first.1 - y) <= 8.0 && d.points.len() >= 3 { finish = true; } else { d.points.push(point); }
                                }
                                None => {
                                    let mode = selection_mode(state, this.doc.borrow().mode);
                                    *draft = Some(Draft { points: vec![point], cursor: Some(point), mode, kind: DraftKind::Polygonal, anchor: point });
                                }
                            }
                        }
                        if finish { this.finish_draft(); }
                        this.area.queue_draw();
                    }
                    Tool::Clone if state.contains(gdk::ModifierType::ALT_MASK) => {
                        let mut d = this.doc.borrow_mut();
                        let size = d.size();
                        let point = d.viewport.document_point((x, y), size);
                        d.clone_source = Some((point.0.round(), point.1.round()));
                        d.clone_offset = None;
                        drop(d);
                        this.notify("Clone source set. Paint to copy from it.");
                        this.area.queue_draw();
                    }
                    Tool::Eyedropper => this.sample_at(x, y, state.contains(gdk::ModifierType::ALT_MASK)),
                    Tool::Type if g.current_button() == 1 && n == 1 => this.type_at(x, y),
                    Tool::Brush if state.contains(gdk::ModifierType::ALT_MASK) => this.sample_at(x, y, false),
                    Tool::Wand => {
                        let mode = selection_mode(state, this.doc.borrow().mode);
                        this.wand_view(x, y, mode);
                    }
                    Tool::Zoom => {
                        let mut d = this.doc.borrow_mut();
                        let size = d.size();
                        let zoom = d.viewport.zoom() * if state.contains(gdk::ModifierType::ALT_MASK) { 0.5 } else { 2.0 };
                        d.viewport.set_zoom(zoom, (x, y), size);
                        this.zoom_label.set_label(&zoom_text(d.viewport.zoom()));
                        this.area.queue_draw();
                    }
                    _ => {}
                }
            });
        }
        area.add_controller(click);

        let drag = gtk::GestureDrag::new();
        drag.set_button(0);
        {
            let this = self.clone();
            let last = Rc::new(Cell::new((0.0, 0.0)));
            {
                let (this, last) = (this.clone(), last.clone());
                drag.connect_drag_begin(move |g, x, y| {
                    let button = g.current_button();
                    let tool = this.doc.borrow().tool;
                    let pans = button == 2 || (button == 1 && (this.space_held.get() || tool == Tool::Hand));
                    this.dragging.set(pans);
                    last.set((0.0, 0.0));
                    if pans { this.area.set_cursor_from_name(Some("grabbing")); return; }
                    let state = g.current_event_state();
                    if button == 1 && tool.is_brush() && !(tool == Tool::Clone && state.contains(gdk::ModifierType::ALT_MASK)) {
                        this.stroke_start.set((x, y));
                        this.begin_stroke((x, y), state.contains(gdk::ModifierType::SHIFT_MASK));
                        return;
                    }
                    if button == 1 && this.begin_guide_from_ruler((x, y)) { this.stroke_start.set((x, y)); return; }
                    if button == 1 && tool == Tool::Move { if let Some(g) = this.guide_at((x, y)) { this.guide_drag.set(Some(g)); this.stroke_start.set((x, y)); return; } }
                    if button == 1 && tool == Tool::Move { if this.begin_transform((x, y), state) { this.stroke_start.set((x, y)); return; } }
                    if button == 1 && tool == Tool::Pen { this.stroke_start.set((x, y)); this.pen_press((x, y)); return; }
                    if button == 1 && matches!(tool, Tool::Gradient | Tool::Shape | Tool::Crop) {
                        this.stroke_start.set((x, y));
                        this.begin_tool_drag((x, y), state);
                        return;
                    }
                    if button == 1 && (tool == Tool::Marquee || (tool == Tool::Lasso && !this.doc.borrow().lasso_polygonal)) {
                        this.stroke_start.set((x, y));
                        this.begin_draft((x, y), state);
                        return;
                    }
                    g.set_state(gtk::EventSequenceState::Denied);
                });
            }
            {
                let (this, last) = (this.clone(), last.clone());
                drag.connect_drag_update(move |g, dx, dy| {
                    if this.painting.get() {
                        let (sx, sy) = this.stroke_start.get();
                        this.continue_stroke((sx + dx, sy + dy));
                        return;
                    }
                    let state = g.current_event_state();
                    if this.transform_drag.borrow().is_some() {
                        let (sx, sy) = this.stroke_start.get();
                        this.update_transform((sx + dx, sy + dy), state);
                        return;
                    }
                    if this.draft.borrow().is_some() || this.outline_move.get().is_some() {
                        let (sx, sy) = this.stroke_start.get();
                        this.update_draft((sx + dx, sy + dy), state);
                        return;
                    }
                    if this.tool_drag.get().is_some() {
                        let (sx, sy) = this.stroke_start.get();
                        this.update_tool_drag((sx + dx, sy + dy), state);
                        return;
                    }
                    if this.pen_drag.get().is_some() {
                        let (sx, sy) = this.stroke_start.get();
                        this.pen_drag_to((sx + dx, sy + dy));
                        return;
                    }
                    if this.guide_drag.get().is_some() {
                        let (sx, sy) = this.stroke_start.get();
                        this.update_guide_drag((sx + dx, sy + dy));
                        return;
                    }
                    if !this.dragging.get() { return; }
                    let (lx, ly) = last.get();
                    this.doc.borrow_mut().viewport.translate(dx - lx, dy - ly);
                    last.set((dx, dy));
                    this.area.queue_draw();
                });
            }
            drag.connect_drag_end(move |_, _, _| {
                if this.painting.get() { this.finish_stroke(); }
                if this.transform_drag.borrow().is_some() { this.finish_transform(); }
                if this.outline_move.get().is_some() { this.finish_outline_move(); }
                if this.tool_drag.get().is_some() { this.finish_tool_drag(); }
                this.pen_drag.set(None);
                if this.guide_drag.get().is_some() { this.finish_guide_drag(); }
                else if this.draft.borrow().as_ref().is_some_and(|d| d.kind != DraftKind::Polygonal) { this.finish_draft(); }
                this.dragging.set(false);
                this.update_cursor();
            });
        }
        area.add_controller(drag);
    }

    fn tool_changed(&self) {
        self.handles_parked.set(None);
        self.pen_drag.set(None);
        { let mut d = self.doc.borrow_mut(); d.crop = None; d.gradient_line = None; d.shape_draft = None; if d.tool != Tool::Gradient { if let Some(id) = d.document.active { d.document.renderer.set_preview(id, None); d.document.renderer.end_mask_preview(id); } } }
        let tool = self.doc.borrow().tool;
        *self.draft.borrow_mut() = None;
        self.options.update(tool);
        self.sync_inspector();
        self.update_cursor();
        self.area.queue_draw();
    }

    pub fn set_tool(&self, tool: Tool) {
        self.rail.select(tool);
    }

    pub fn update_cursor(&self) {
        if self.dragging.get() { return; }
        let name = if self.space_held.get() { "grab" } else {
            let d = self.doc.borrow();
            match d.tool {
                Tool::Hand => "grab", Tool::Zoom => "zoom-in", Tool::Wand => "crosshair", Tool::Marquee | Tool::Lasso => "crosshair",
                Tool::Crop | Tool::Gradient | Tool::Shape | Tool::Eyedropper => "crosshair",
                Tool::Type => "text",
                Tool::Pen => "crosshair",
                t if t.is_brush() => "none",
                Tool::Move if self.guide_at(self.pointer.get()).is_some_and(|(v, _)| v) => "ew-resize",
                Tool::Move if self.guide_at(self.pointer.get()).is_some() => "ns-resize",
                Tool::Move => match self.geometry(&d).and_then(|g| g.hit(self.pointer.get())) {
                    Some(DragMode::Rotate) => "alias",
                    Some(DragMode::Resize(i)) => ["nwse-resize", "ns-resize", "nesw-resize", "ew-resize", "nwse-resize", "ns-resize", "nesw-resize", "ew-resize"][i],
                    _ => "move",
                },
                _ => "default",
            }
        };
        self.area.set_cursor_from_name(Some(name));
    }

    /// The transform box of the active layer on screen, when the Move tool would show one.
    fn geometry(&self, d: &super::Doc) -> Option<Geometry> {
        if !d.show_handles { return None; }
        let id = d.document.active?;
        if self.handles_parked.get() == Some(id) { return None; }
        let size = d.size();
        let vp = d.viewport;
        if d.document.transforms_as_group() { return d.document.group_box().map(|b| Geometry::new(&b, |p| vp.view_point(p, size))); }
        let layer = d.document.renderer.layer(id);
        if layer.is_group() || !d.document.renderer.has_image(id) { return None; }
        if !crate::format::visible_layers(d.document.renderer.layers()).contains(&id) { return None; }
        Some(Geometry::new(&layer.transform, |p| vp.view_point(p, size)))
    }

    pub fn sync_inspector(&self) {
        // Typing stops when the layer being typed into is no longer there (undo, delete, merge).
        let stale = { let d = self.doc.borrow(); self.text_edit.borrow().is_some_and(|(id, _)| !d.document.has_layer(id) || d.document.text_style(id).is_none()) };
        if stale { *self.text_edit.borrow_mut() = None; }
        self.options.sync_move(&self.doc); self.options.sync_type(&self.doc); self.options.show_mask_paint(self.doc.borrow().document.mask_target());
    }

    /// The Type tool's click: existing text under the point opens for editing, anywhere else starts a new
    /// type layer there, in the current style and the foreground color.
    fn type_at(&self, x: f64, y: f64) {
        // A click inside the text being edited moves the caret; anywhere else finishes it first.
        let editing = *self.text_edit.borrow();
        if let Some((id, _)) = editing {
            let hit = { let d = self.doc.borrow(); let size = d.size(); let p = d.viewport.document_point((x, y), size); d.document.text_layer_at(p) == Some(id) };
            if hit {
                let index = { let d = self.doc.borrow(); let size = d.size(); let p = d.viewport.document_point((x, y), size); Self::raster_point(&d, id, p).and_then(|(rx, ry)| d.document.text_style(id).map(|s| crate::text::index_at(&s, rx, ry))) };
                if let Some(index) = index { self.text_edit.borrow_mut().as_mut().map(|e| e.1 = index); }
                self.area.queue_draw();
                return;
            }
        }
        self.finish_text_edit();
        let result = {
            let mut d = self.doc.borrow_mut();
            let size = d.size();
            let point = d.viewport.document_point((x, y), size);
            match d.document.text_layer_at(point) {
                Some(id) => { d.document.select_layer(Some(id)); Ok((id, Some(point))) }
                None => { let mut style = d.text_style.clone(); style.color = d.brush.color; style.text = String::new(); d.document.add_text_layer(&style, point.0, point.1).map(|id| (id, None)) }
            }
        };
        match result {
            Ok((id, clicked)) => {
                if let Some(refresh) = self.refresh.borrow().as_ref() { refresh(); }
                self.sync_inspector();
                let index = { let d = self.doc.borrow(); match (clicked, d.document.text_style(id)) { (Some(p), Some(style)) => Self::raster_point(&d, id, p).map(|(rx, ry)| crate::text::index_at(&style, rx, ry)).unwrap_or(style.text.len()), (_, Some(style)) => style.text.len(), _ => 0 } };
                self.begin_text_edit(id, index);
            }
            Err(error) => self.notify(&format!("{error:#}")),
        }
    }

    /// A document point on a type layer's rendered surface (its raster), through the layer's full
    /// placement (rotation and flips included).
    fn raster_point(d: &super::Doc, id: uuid::Uuid, p: (f64, f64)) -> Option<(f64, f64)> {
        if !d.document.has_layer(id) { return None; }
        let t = d.document.renderer.layer(id).transform;
        let (rw, rh) = d.document.renderer.image_size(id)?;
        let to_raster = crate::selection::document_to_layer(&t, rw, rh).ok()?;
        Some(to_raster.transform_point(p.0, p.1))
    }

    /// Starts typing into a type layer on the canvas, with the caret at byte `index` of its text.
    pub fn begin_text_edit(&self, id: uuid::Uuid, index: usize) {
        *self.text_edit.borrow_mut() = Some((id, index));
        self.area.set_focusable(true);
        self.area.grab_focus();
        self.notify("Type on the canvas. Return adds a line; Escape or Ctrl+Return finishes; the options bar sets the font.");
        self.area.queue_draw();
    }

    /// Edit Text from a menu: typing starts at the end of the layer's text.
    pub fn edit_text(&self, id: uuid::Uuid) {
        let len = self.doc.borrow().document.text_style(id).map(|s| s.text.len()).unwrap_or(0);
        self.begin_text_edit(id, len);
    }

    pub fn text_editing(&self) -> bool { self.text_edit.borrow().is_some() }

    /// Ends typing. A layer left with no text goes, as Photoshop drops an empty type layer.
    pub fn finish_text_edit(&self) {
        let Some((id, _)) = self.text_edit.borrow_mut().take() else { return };
        {
            let mut d = self.doc.borrow_mut();
            if d.document.has_layer(id) && d.document.text_style(id).is_some_and(|s| s.text.trim().is_empty()) {
                d.document.select_layer(Some(id));
                d.document.delete_layer();
            }
        }
        if let Some(refresh) = self.refresh.borrow().as_ref() { refresh(); }
        self.sync_inspector();
        self.area.queue_draw();
    }

    /// A key while typing on the canvas. True when it was taken.
    pub fn text_key(&self, key: gdk::Key, modifiers: gdk::ModifierType) -> bool {
        let Some((id, mut index)) = *self.text_edit.borrow() else { return false };
        // The layer may have gone under the editor (undo, delete): then the edit is simply over.
        if !self.doc.borrow().document.has_layer(id) { self.finish_text_edit(); return false; }
        let Some(mut style) = self.doc.borrow().document.text_style(id) else { self.finish_text_edit(); return false };
        let ctrl = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
        let alt = modifiers.contains(gdk::ModifierType::ALT_MASK);
        index = index.min(style.text.len());
        while !style.text.is_char_boundary(index) { index -= 1; }
        let prev = |t: &str, i: usize| t[..i].chars().next_back().map(|c| i - c.len_utf8()).unwrap_or(0);
        let next = |t: &str, i: usize| t[i..].chars().next().map(|c| i + c.len_utf8()).unwrap_or(i);
        let mut changed = false;
        match key {
            gdk::Key::Escape => { self.finish_text_edit(); return true; }
            gdk::Key::Return | gdk::Key::KP_Enter if ctrl => { self.finish_text_edit(); return true; }
            gdk::Key::Return | gdk::Key::KP_Enter => { style.text.insert(index, '\n'); index += 1; changed = true; }
            gdk::Key::BackSpace => { if index > 0 { let p = if ctrl { word_start(&style.text, index) } else { prev(&style.text, index) }; style.text.replace_range(p..index, ""); index = p; changed = true; } }
            gdk::Key::Delete | gdk::Key::KP_Delete => { if index < style.text.len() { let n = next(&style.text, index); style.text.replace_range(index..n, ""); changed = true; } }
            gdk::Key::Left => { index = if ctrl { word_start(&style.text, index) } else { prev(&style.text, index) }; }
            gdk::Key::Right => { index = if ctrl { word_end(&style.text, index) } else { next(&style.text, index) }; }
            gdk::Key::Home => { index = style.text[..index].rfind('\n').map(|i| i + 1).unwrap_or(0); }
            gdk::Key::End => { index = style.text[index..].find('\n').map(|i| index + i).unwrap_or(style.text.len()); }
            gdk::Key::Up | gdk::Key::Down => {
                let (cx, cy, h) = crate::text::caret(&style, index);
                let target_y = if key == gdk::Key::Up { cy - h / 2.0 } else { cy + h * 1.5 };
                if target_y >= 0.0 { index = crate::text::index_at(&style, cx, target_y); }
            }
            _ if ctrl || alt => return false,
            _ => match key.to_unicode() {
                Some(c) if !c.is_control() => { style.text.insert(index, c); index += c.len_utf8(); changed = true; }
                _ => return false,
            },
        }
        if changed {
            let result = self.doc.borrow_mut().document.set_text(id, &style);
            if let Err(error) = result { self.notify(&format!("{error:#}")); }
            if let Some(refresh) = self.refresh.borrow().as_ref() { refresh(); }
            self.sync_inspector();
        }
        *self.text_edit.borrow_mut() = Some((id, index));
        self.area.queue_draw();
        true
    }

    /// Arrow keys nudge the layer (Move) or the selection outline (selection tools); Escape, Return and
    /// BackSpace drive the polygonal lasso. True when the key was used.
    pub fn special_key(&self, key: gdk::Key, modifiers: gdk::ModifierType) -> bool {
        let step = if modifiers.contains(gdk::ModifierType::SHIFT_MASK) { 10.0 } else { 1.0 };
        let arrow = match key { gdk::Key::Left => Some((-step, 0.0)), gdk::Key::Right => Some((step, 0.0)), gdk::Key::Up => Some((0.0, -step)), gdk::Key::Down => Some((0.0, step)), _ => None };
        let tool = self.doc.borrow().tool;
        if let Some((dx, dy)) = arrow {
            let mut d = self.doc.borrow_mut();
            if tool == Tool::Move { d.document.nudge(dx, dy); } else if tool.is_selection() { let _ = d.document.move_selection(dx, dy); } else { return false; }
            drop(d);
            if let Some(refresh) = self.refresh.borrow().as_ref() { refresh(); }
            self.sync_inspector();
            self.area.queue_draw();
            return true;
        }
        if self.doc.borrow().document.floating.is_some() && matches!(key, gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::Escape) {
            let result = { let mut d = self.doc.borrow_mut(); if key == gdk::Key::Escape { d.document.cancel_free_transform(); Ok(()) } else { d.document.commit_free_transform() } };
            if let Err(error) = result { self.notify(&format!("{error:#}")); } else { self.notify(if key == gdk::Key::Escape { "Free Transform cancelled." } else { "Free Transform applied." }); }
            self.handles_parked.set(self.doc.borrow().document.active);
            if let Some(refresh) = self.refresh.borrow().as_ref() { refresh(); }
            self.sync_inspector();
            self.area.queue_draw();
            return true;
        }
        if tool == Tool::Pen && self.pen_key(key, modifiers) { return true; }
        // Return with the Move tool: the layer is placed; its handles go away until the next click.
        if tool == Tool::Move && self.draft.borrow().is_none() && matches!(key, gdk::Key::Return | gdk::Key::KP_Enter) && !modifiers.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
            self.handles_parked.set(self.doc.borrow().document.active);
            self.update_cursor();
            self.area.queue_draw();
            return true;
        }
        if tool == Tool::Crop && matches!(key, gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::Escape) {
            if key == gdk::Key::Escape { self.doc.borrow_mut().crop = None; } else { self.apply_crop(); }
            self.area.queue_draw();
            return true;
        }
        if self.draft.borrow().is_none() && matches!(key, gdk::Key::Delete | gdk::Key::BackSpace | gdk::Key::KP_Delete) && !modifiers.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
            // Delete clears the selection when there is one; otherwise the targeted mask, or the layer, goes.
            let result = {
                let mut d = self.doc.borrow_mut();
                if d.document.selection.as_ref().is_some_and(|s| !s.is_empty()) { let white = !d.mask_paint_white; d.document.clear_selection(white) }
                else if d.document.mask_target() { d.document.delete_mask(); Ok(()) }
                else { d.document.delete_layer(); Ok(()) }
            };
            if let Err(error) = result { self.notify(&format!("{error:#}")); }
            if let Some(refresh) = self.refresh.borrow().as_ref() { refresh(); }
            self.area.queue_draw();
            return true;
        }
        if self.draft.borrow().is_some() {
            match key {
                gdk::Key::Escape => { *self.draft.borrow_mut() = None; self.area.queue_draw(); return true; }
                gdk::Key::Return | gdk::Key::KP_Enter => { self.finish_draft(); return true; }
                gdk::Key::BackSpace => {
                    let mut empty = false;
                    if let Some(d) = self.draft.borrow_mut().as_mut() { d.points.pop(); empty = d.points.is_empty(); }
                    if empty { *self.draft.borrow_mut() = None; }
                    self.area.queue_draw();
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    // Move tool

    /// Starts a transform drag at a view point: a handle, or a press inside the active layer (or, with
    /// Ctrl or auto-select, the layer under the pointer). False when nothing here can be dragged.
    fn begin_transform(&self, view: (f64, f64), state: gdk::ModifierType) -> bool {
        let mut d = self.doc.borrow_mut();
        if d.document.stroke_active() { return false; }
        let size = d.size();
        let point = d.viewport.document_point(view, size);
        let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
        // Ctrl-drag inside the selection moves its pixels (Alt as well copies them).
        if ctrl && d.document.selection.as_ref().is_some_and(|s| !s.is_empty() && s.contains(point.0.floor(), point.1.floor())) && d.document.active_image().is_some() && !d.document.mask_target() {
            match d.document.begin_pixel_move(state.contains(gdk::ModifierType::ALT_MASK)) {
                Ok(true) => { *self.transform_drag.borrow_mut() = Some((Drag { original: d.document.renderer.layer(d.document.active.unwrap()).transform, start: point, mode: DragMode::Move }, Vec::new())); self.pixel_moving.set(true); drop(d); self.area.set_cursor_from_name(Some("move")); return true; }
                Ok(false) => {}
                Err(error) => { drop(d); self.notify(&format!("{error:#}")); return false; }
            }
        }
        let mut mode = self.geometry(&d).and_then(|g| g.hit(view));
        let mut target = d.document.active;
        if mode.is_none() {
            let picks = state.contains(gdk::ModifierType::CONTROL_MASK) || d.auto_select;
            let under = crate::format::visible_layers(d.document.renderer.layers()).into_iter().rev()
                .find(|id| d.document.renderer.has_image(*id) && d.document.renderer.layer(*id).transform.contains(point));
            let active_hit = target.is_some_and(|id| { let l = d.document.renderer.layer(id); if l.is_group() { !d.document.transform_members(id).is_empty() } else { d.document.renderer.has_image(id) && l.transform.contains(point) } });
            if picks && under.is_some() && !(active_hit && !state.contains(gdk::ModifierType::CONTROL_MASK)) { target = under; }
            else if !active_hit && target.is_some_and(|id| !d.document.renderer.layer(id).is_group()) && !picks {
                // A press on empty canvas lets go of the layer, so its handles disappear; on another
                // layer it leaves things be (Auto-select or Ctrl picks that layer instead).
                if under.is_none() {
                    d.document.select_layer(None);
                    drop(d);
                    if let Some(refresh) = self.refresh.borrow().as_ref() { refresh(); }
                    self.sync_inspector();
                    self.area.queue_draw();
                }
                return false;
            }
            mode = Some(DragMode::Move);
        }
        let (Some(mode), Some(id)) = (mode, target) else { return false };
        if d.document.active != Some(id) && !d.document.selected.contains(&id) { d.document.select_layer(Some(id)); }
        else if d.document.active != Some(id) { d.document.active = Some(id); d.document.set_mask_target(false); }
        let members = d.document.transform_members(id);
        if members.is_empty() { return false; }
        let originals: Vec<(uuid::Uuid, crate::format::Transform)> = members.iter().map(|m| (*m, d.document.renderer.layer(*m).transform)).collect();
        // Several layers, or a folder, drag as one upright box around their contents.
        let grouped = d.document.transforms_as_group();
        let original = if grouped { d.document.group_box().unwrap_or(originals[0].1) } else { originals[0].1 };
        // Ctrl on a corner handle of one pixel layer starts a free distortion.
        if let (true, false, DragMode::Resize(i)) = (ctrl, grouped, mode) {
            if i % 2 == 0 { d.distort = Some(crate::distort::corners(&original)); }
        }
        *self.transform_drag.borrow_mut() = Some((Drag { original, start: point, mode }, originals));
        drop(d);
        if let Some(refresh) = self.refresh.borrow().as_ref() { refresh(); }
        self.area.set_cursor_from_name(Some(match mode { DragMode::Move => "move", DragMode::Rotate => "alias", DragMode::Resize(_) => "nwse-resize" }));
        true
    }

    fn update_transform(&self, view: (f64, f64), state: gdk::ModifierType) {
        let mut d = self.doc.borrow_mut();
        let Some((drag, originals)) = self.transform_drag.borrow().clone() else { return };
        let size = d.size();
        let point = d.viewport.document_point(view, size);
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let alt = state.contains(gdk::ModifierType::ALT_MASK);
        if self.pixel_moving.get() {
            let (mut dx, mut dy) = (point.0 - drag.start.0, point.1 - drag.start.1);
            if shift { if dx.abs() >= dy.abs() { dy = 0.0; } else { dx = 0.0; } }
            let result = d.document.move_pixels(dx, dy);
            drop(d);
            if let Err(error) = result { self.notify(&format!("{error:#}")); }
            self.area.queue_draw();
            return;
        }
        if let (Some(mut corners), DragMode::Resize(i)) = (d.distort, drag.mode) {
            // The dragged corner follows the pointer; Shift keeps it on one axis. A twisted shape is ignored.
            let index = i / 2;
            let start_corner = crate::distort::corners(&drag.original)[index];
            let (mut dx, mut dy) = (point.0 - drag.start.0, point.1 - drag.start.1);
            if shift { if dx.abs() >= dy.abs() { dy = 0.0; } else { dx = 0.0; } }
            corners[index] = ((start_corner.0 + dx).round(), (start_corner.1 + dy).round());
            if crate::distort::is_usable(&corners) {
                d.distort = Some(corners);
                if let Some((id, original)) = originals.first().copied() { if let Err(error) = d.document.preview_distort(id, &original, &corners) { drop(d); self.notify(&format!("{error:#}")); return; } }
            }
            drop(d);
            self.area.queue_draw();
            return;
        }
        let mut draft = drag.updated(point, d.lock_ratio, shift, alt).rounded();
        d.snap_guides = (None, None);
        if drag.mode == DragMode::Move && !state.contains(gdk::ModifierType::CONTROL_MASK) {
            let moving: Vec<uuid::Uuid> = originals.iter().map(|(id, _)| *id).collect();
            let tolerance = SNAP_DISTANCE / d.viewport.points_per_pixel().max(0.0001);
            let (snapped, gx, gy) = d.document.snapped_move(draft, &moving, tolerance);
            draft = snapped;
            d.snap_guides = (gx, gy);
        }
        // Preview without an undo step: each member follows the dragged box.
        for (id, original) in &originals {
            let moved = if originals.len() == 1 && !d.document.renderer.layer(*id).is_group() { draft } else { original.following(&drag.original, &draft) };
            if moved.is_valid() { d.document.renderer.set_layer_transform(*id, moved); }
        }
        drop(d);
        self.sync_inspector();
        self.area.queue_draw();
    }

    fn finish_transform(&self) {
        let Some((drag, originals)) = self.transform_drag.borrow_mut().take() else { return };
        let mut d = self.doc.borrow_mut();
        d.snap_guides = (None, None);
        if self.pixel_moving.replace(false) {
            let result = d.document.finish_pixel_move();
            drop(d);
            if let Err(error) = result { self.notify(&format!("{error:#}")); }
            if let Some(refresh) = self.refresh.borrow().as_ref() { refresh(); }
            self.update_cursor();
            return;
        }
        if let Some(corners) = d.distort.take() {
            let result = match originals.first().copied() {
                Some((id, original)) if corners != crate::distort::corners(&original) => d.document.commit_distort(&[(id, original, corners)]),
                Some((id, _)) => { d.document.renderer.set_preview(id, None); Ok(()) }
                None => Ok(()),
            };
            drop(d);
            if let Err(error) = result { self.notify(&format!("{error:#}")); }
            if let Some(refresh) = self.refresh.borrow().as_ref() { refresh(); }
            self.sync_inspector();
            self.update_cursor();
            return;
        }
        // Put the originals back silently, then apply the result as one undo step with mask carry-over.
        let finals: Vec<(uuid::Uuid, crate::format::Transform)> = originals.iter().map(|(id, _)| (*id, d.document.renderer.layer(*id).transform)).collect();
        for (id, original) in &originals { d.document.renderer.set_layer_transform(*id, *original); }
        if finals.iter().zip(&originals).any(|(f, o)| f.1 != o.1) {
            d.document.begin_edit(match (drag.mode, originals.len() > 1) { (DragMode::Move, false) => "Move Layer", (DragMode::Move, true) => "Move Layers", (DragMode::Rotate, false) => "Rotate Layer", (DragMode::Rotate, true) => "Rotate Layers", (DragMode::Resize(_), false) => "Scale Layer", (DragMode::Resize(_), true) => "Scale Layers" });
            for (id, t) in finals { d.document.set_transform(id, t, "Transform Layer"); }
            d.document.end_edit();
        }
        drop(d);
        if let Some(refresh) = self.refresh.borrow().as_ref() { refresh(); }
        self.sync_inspector();
        self.update_cursor();
    }

    // Eyedropper

    /// Picks the color under a view point into the foreground (or, `background`, the background) swatch.
    fn sample_at(&self, x: f64, y: f64, background: bool) {
        let result = {
            let mut d = self.doc.borrow_mut();
            let size = d.size();
            let point = d.viewport.document_point((x, y), size);
            let all = d.eyedropper_all_layers;
            match d.document.sample_color(point.0, point.1, all) {
                Ok(Some(color)) => {
                    if d.document.mask_target() { d.mask_paint_white = color[0] + color[1] + color[2] > 1.5; }
                    else if background { d.background = color; } else { d.brush.color = color; }
                    Ok(Some((d.brush.color, d.background, color)))
                }
                Ok(None) => Ok(None),
                Err(e) => Err(e),
            }
        };
        match result {
            Ok(Some((fg, bg, c))) => { self.rail.sync_palette(fg, bg); self.notify(&format!("Picked {}", super::color_wheel::hex(c))); }
            Ok(None) => self.notify("Nothing to pick here: the canvas is transparent at that point."),
            Err(error) => self.notify(&format!("{error:#}")),
        }
    }

    // Gradient, shape and crop drags

    /// The gradient's two colors from the palette and options, with alpha.
    fn gradient_colors(d: &super::Doc) -> [[f64; 4]; 2] {
        let fg = d.brush.color;
        let mut colors = if d.gradient_to_transparent { [[fg[0], fg[1], fg[2], 1.0], [fg[0], fg[1], fg[2], 0.0]] } else { let bg = d.background; [[fg[0], fg[1], fg[2], 1.0], [bg[0], bg[1], bg[2], 1.0]] };
        if d.document.mask_target() { let w = if d.mask_paint_white { 1.0 } else { 0.0 }; colors = if d.gradient_to_transparent { [[w, w, w, 1.0], [w, w, w, 0.0]] } else { [[w, w, w, 1.0], [1.0 - w, 1.0 - w, 1.0 - w, 1.0]] }; }
        if d.gradient_reversed { colors.swap(0, 1); }
        colors
    }

    fn crop_ratio(d: &super::Doc) -> Option<f64> {
        match d.crop_ratio { 1 => Some(d.document.width() as f64 / d.document.height() as f64), 2 => Some(1.0), 3 => Some(4.0 / 3.0), 4 => Some(16.0 / 9.0), _ => None }
    }

    fn begin_tool_drag(&self, view: (f64, f64), state: gdk::ModifierType) {
        let mut d = self.doc.borrow_mut();
        let size = d.size();
        let point = d.viewport.document_point(view, size);
        let drag = match d.tool {
            Tool::Gradient => {
                if d.document.active.is_none() { drop(d); self.notify("Select a layer first."); return; }
                d.gradient_line = Some((point, point));
                ToolDrag::Gradient { start: point }
            }
            Tool::Shape => {
                let anchor = (point.0.round(), point.1.round());
                d.shape_draft = Some((anchor.0, anchor.1, 0.0, 0.0));
                ToolDrag::Shape { anchor }
            }
            Tool::Crop => {
                // A handle resizes the frame, a press inside moves it, anywhere else starts a new one.
                let vp = d.viewport;
                let mode = d.crop.and_then(|(x, y, w, h)| {
                    let t = crate::format::Transform { origin: crate::format::Point(x, y), size: crate::format::Size(w, h), rotation: 0.0, flip_x: false, flip_y: false, sampling: Default::default() };
                    let g = Geometry::new(&t, |p| vp.view_point(p, size));
                    match g.hit(view) { Some(DragMode::Resize(i)) => Some(DragMode::Resize(i)), Some(DragMode::Rotate) => None, _ => if t.contains(point) { Some(DragMode::Move) } else { None } }
                });
                let original = d.crop;
                match mode {
                    Some(mode) => ToolDrag::Crop { start: point, original, mode },
                    None => { let anchor = (point.0.round(), point.1.round()); d.crop = Some((anchor.0, anchor.1, 0.0, 0.0)); ToolDrag::Crop { start: anchor, original: None, mode: DragMode::Resize(4) } }
                }
            }
            _ => return,
        };
        self.tool_drag.set(Some(drag));
        let _ = state;
        drop(d);
        self.area.queue_draw();
    }

    fn update_tool_drag(&self, view: (f64, f64), state: gdk::ModifierType) {
        let Some(drag) = self.tool_drag.get() else { return };
        let mut d = self.doc.borrow_mut();
        let size = d.size();
        let point = d.viewport.document_point(view, size);
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let alt = state.contains(gdk::ModifierType::ALT_MASK);
        match drag {
            ToolDrag::Gradient { start } => {
                let mut end = point;
                if shift {
                    // Snap the line to 45 degree steps.
                    let (dx, dy) = (end.0 - start.0, end.1 - start.1);
                    let len = dx.hypot(dy);
                    let angle = (dy.atan2(dx) / (std::f64::consts::PI / 4.0)).round() * (std::f64::consts::PI / 4.0);
                    end = (start.0 + len * angle.cos(), start.1 + len * angle.sin());
                }
                d.gradient_line = Some((start, end));
                let (colors, radial, opacity) = (Self::gradient_colors(&d), d.gradient_radial, d.gradient_opacity);
                let result = d.document.gradient(start, end, radial, colors, opacity, false);
                drop(d);
                if let Err(error) = result { self.notify(&format!("{error:#}")); }
            }
            ToolDrag::Shape { anchor } => {
                d.shape_draft = Some(drag_box(anchor, point, shift, alt));
                drop(d);
            }
            ToolDrag::Crop { start, original, mode } => {
                let ratio = Self::crop_ratio(&d);
                let mut rect = match (mode, original) {
                    (DragMode::Move, Some((x, y, w, h))) => ((x + point.0 - start.0).round(), (y + point.1 - start.1).round(), w, h),
                    (DragMode::Resize(i), Some((x, y, w, h))) => {
                        let t = crate::format::Transform { origin: crate::format::Point(x, y), size: crate::format::Size(w, h), rotation: 0.0, flip_x: false, flip_y: false, sampling: Default::default() };
                        let next = Drag { original: t, start, mode: DragMode::Resize(i) }.updated(point, ratio.is_some(), false, alt);
                        (next.origin.0.round(), next.origin.1.round(), next.size.0.round().max(1.0), next.size.1.round().max(1.0))
                    }
                    _ => {
                        let (mut dx, mut dy) = (point.0 - start.0, point.1 - start.1);
                        if let Some(r) = ratio { if dx.abs() > dy.abs() * r { dy = dy.signum() * dx.abs() / r; } else { dx = dx.signum() * dy.abs() * r; } }
                        if alt { ((start.0 - dx.abs()).round(), (start.1 - dy.abs()).round(), (dx.abs() * 2.0).round(), (dy.abs() * 2.0).round()) }
                        else { (start.0.min(start.0 + dx).round(), start.1.min(start.1 + dy).round(), dx.abs().round(), dy.abs().round()) }
                    }
                };
                // Edges snap to the canvas and the layers' bounds; with a fixed ratio only moves snap.
                if !state.contains(gdk::ModifierType::CONTROL_MASK) && (ratio.is_none() || mode == DragMode::Move) {
                    let tolerance = SNAP_DISTANCE / d.viewport.points_per_pixel().max(0.0001);
                    let (xs, ys) = d.document.crop_snap_targets();
                    let nearest = |v: f64, targets: &[f64]| targets.iter().copied().filter(|t| (t - v).abs() <= tolerance).min_by(|a, b| (a - v).abs().partial_cmp(&(b - v).abs()).unwrap());
                    let (x0, y0, x1, y1) = (rect.0, rect.1, rect.0 + rect.2, rect.1 + rect.3);
                    if mode == DragMode::Move {
                        let sx = [x0, x1].iter().filter_map(|e| nearest(*e, &xs).map(|t| t - e)).min_by(|a, b| a.abs().partial_cmp(&b.abs()).unwrap()).unwrap_or(0.0);
                        let sy = [y0, y1].iter().filter_map(|e| nearest(*e, &ys).map(|t| t - e)).min_by(|a, b| a.abs().partial_cmp(&b.abs()).unwrap()).unwrap_or(0.0);
                        rect.0 += sx; rect.1 += sy;
                    } else {
                        let (mut nx0, mut nx1, mut ny0, mut ny1) = (x0, x1, y0, y1);
                        if (point.0 - x0).abs() <= (point.0 - x1).abs() { if let Some(t) = nearest(x0, &xs) { if t < x1 { nx0 = t; } } } else if let Some(t) = nearest(x1, &xs) { if t > x0 { nx1 = t; } }
                        if (point.1 - y0).abs() <= (point.1 - y1).abs() { if let Some(t) = nearest(y0, &ys) { if t < y1 { ny0 = t; } } } else if let Some(t) = nearest(y1, &ys) { if t > y0 { ny1 = t; } }
                        rect = (nx0, ny0, nx1 - nx0, ny1 - ny0);
                    }
                }
                if rect.2 >= 1.0 && rect.3 >= 1.0 { d.crop = Some(rect); }
                drop(d);
            }
        }
        self.area.queue_draw();
    }

    fn finish_tool_drag(&self) {
        let Some(drag) = self.tool_drag.take() else { return };
        let result = {
            let mut d = self.doc.borrow_mut();
            match drag {
                ToolDrag::Gradient { .. } => {
                    let line = d.gradient_line.take();
                    match line {
                        Some((start, end)) if (end.0 - start.0).hypot(end.1 - start.1) >= 0.5 => {
                            let (colors, radial, opacity) = (Self::gradient_colors(&d), d.gradient_radial, d.gradient_opacity);
                            d.document.gradient(start, end, radial, colors, opacity, true)
                        }
                        _ => { if let Some(id) = d.document.active { d.document.renderer.set_preview(id, None); d.document.renderer.end_mask_preview(id); } Ok(()) }
                    }
                }
                ToolDrag::Shape { .. } => {
                    let draft = d.shape_draft.take();
                    match draft {
                        Some(rect) if rect.2 >= 1.0 && rect.3 >= 1.0 => { let (ellipse, color, radius) = (d.shape_ellipse, d.brush.color, d.shape_radius); d.document.add_shape_layer(ellipse, rect, color, radius).map(|_| ()) }
                        _ => Ok(()),
                    }
                }
                ToolDrag::Crop { .. } => { if d.crop.is_some_and(|c| c.2 < 1.0 || c.3 < 1.0) { d.crop = None; } Ok(()) }
            }
        };
        if let Err(error) = result { self.notify(&format!("{error:#}")); }
        if let Some(refresh) = self.refresh.borrow().as_ref() { refresh(); }
        self.area.queue_draw();
    }

    /// Return with the Crop tool: the canvas becomes the frame.
    pub fn apply_crop(&self) {
        let result = { let mut d = self.doc.borrow_mut(); match d.crop.take() { Some(rect) => d.document.crop(rect), None => Ok(()) } };
        if let Err(error) = result { self.notify(&format!("{error:#}")); }
        if let Some(refresh) = self.refresh.borrow().as_ref() { refresh(); }
        self.fit();
    }

    pub fn cancel_crop(&self) { self.doc.borrow_mut().crop = None; self.area.queue_draw(); }

    /// Preview mode hides everything but the picture.
    // The Pen tool

    /// A press with the Pen: closes the path on its first point, picks up an anchor or handle, or places a
    /// new corner (a drag then pulls out its handles). A finished path starts over on a press away from it.
    fn pen_press(&self, view: (f64, f64)) {
        let mut d = self.doc.borrow_mut();
        let size = d.size();
        let p = d.viewport.document_point(view, size);
        let tolerance = 7.0 / d.viewport.zoom().max(1e-6);
        if d.pen_done {
            match d.pen.hit(p, tolerance) {
                Some(crate::path::Hit::Anchor(i)) => self.pen_drag.set(Some(PenDrag::Anchor(i))),
                Some(crate::path::Hit::HandleIn(i)) => self.pen_drag.set(Some(PenDrag::HandleIn(i))),
                Some(crate::path::Hit::HandleOut(i)) => self.pen_drag.set(Some(PenDrag::HandleOut(i))),
                None => { d.pen = crate::path::Path::default(); d.pen_done = false; d.pen.anchors.push(crate::path::Anchor::corner(p)); self.pen_drag.set(Some(PenDrag::Place(0))); }
            }
        } else {
            let first_hit = d.pen.anchors.len() >= 2 && d.pen.anchors.first().is_some_and(|a| (a.point.0 - p.0).hypot(a.point.1 - p.1) <= tolerance);
            if first_hit { d.pen.closed = true; d.pen_done = true; self.pen_drag.set(Some(PenDrag::HandleIn(0))); drop(d); self.notify("Path closed. Ctrl+Return makes a selection; the options bar fills or strokes it."); self.area.queue_draw(); return; }
            match d.pen.hit(p, tolerance) {
                Some(crate::path::Hit::Anchor(i)) => self.pen_drag.set(Some(PenDrag::Anchor(i))),
                Some(crate::path::Hit::HandleIn(i)) => self.pen_drag.set(Some(PenDrag::HandleIn(i))),
                Some(crate::path::Hit::HandleOut(i)) => self.pen_drag.set(Some(PenDrag::HandleOut(i))),
                None => { d.pen.anchors.push(crate::path::Anchor::corner(p)); let i = d.pen.anchors.len() - 1; self.pen_drag.set(Some(PenDrag::Place(i))); }
            }
        }
        drop(d);
        self.area.queue_draw();
    }

    fn pen_drag_to(&self, view: (f64, f64)) {
        let Some(drag) = self.pen_drag.get() else { return };
        let mut d = self.doc.borrow_mut();
        let size = d.size();
        let p = d.viewport.document_point(view, size);
        match drag {
            PenDrag::Place(i) => { if let Some(a) = d.pen.anchors.get_mut(i) { let m = (2.0 * a.point.0 - p.0, 2.0 * a.point.1 - p.1); if (p.0 - a.point.0).hypot(p.1 - a.point.1) > 1.0 { a.handle_out = Some(p); a.handle_in = Some(m); } } }
            PenDrag::Anchor(i) => { if let Some(a) = d.pen.anchors.get_mut(i) { let (dx, dy) = (p.0 - a.point.0, p.1 - a.point.1); a.point = p; a.handle_in = a.handle_in.map(|h| (h.0 + dx, h.1 + dy)); a.handle_out = a.handle_out.map(|h| (h.0 + dx, h.1 + dy)); } }
            PenDrag::HandleIn(i) => { if let Some(a) = d.pen.anchors.get_mut(i) { a.handle_in = Some(p); } }
            PenDrag::HandleOut(i) => { if let Some(a) = d.pen.anchors.get_mut(i) { a.handle_out = Some(p); } }
        }
        drop(d);
        self.area.queue_draw();
    }

    /// Return ends an open path, Backspace drops the last point, Escape clears. True when taken.
    fn pen_key(&self, key: gdk::Key, modifiers: gdk::ModifierType) -> bool {
        if modifiers.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) { return false; }
        let mut d = self.doc.borrow_mut();
        match key {
            gdk::Key::Return | gdk::Key::KP_Enter => { if d.pen.anchors.len() >= 2 { d.pen_done = true; } }
            gdk::Key::Escape => { d.pen = crate::path::Path::default(); d.pen_done = false; }
            gdk::Key::BackSpace | gdk::Key::Delete if !d.pen_done => { d.pen.anchors.pop(); }
            _ => return false,
        }
        drop(d);
        self.pen_drag.set(None);
        self.area.queue_draw();
        true
    }

    pub fn path_select(&self) {
        let result = { let mut d = self.doc.borrow_mut(); let path = d.pen.clone(); let mode = d.mode; d.document.select_path(&path, mode) };
        match result { Ok(()) => { self.doc.borrow_mut().pen_done = true; if let Some(refresh) = self.refresh.borrow().as_ref() { refresh(); } } Err(e) => self.notify(&format!("{e:#}")) }
        self.area.queue_draw();
    }

    pub fn path_fill(&self) {
        let result = { let mut d = self.doc.borrow_mut(); let (path, color) = (d.pen.clone(), d.brush.color); d.document.fill_path(&path, color) };
        if let Err(e) = result { self.notify(&format!("{e:#}")); }
        if let Some(refresh) = self.refresh.borrow().as_ref() { refresh(); }
        self.area.queue_draw();
    }

    pub fn path_stroke(&self) {
        let result = { let mut d = self.doc.borrow_mut(); let (path, brush) = (d.pen.clone(), d.brush.clone()); d.document.stroke_path(&path, &brush) };
        if let Err(e) = result { self.notify(&format!("{e:#}")); }
        if let Some(refresh) = self.refresh.borrow().as_ref() { refresh(); }
        self.area.queue_draw();
    }

    pub fn path_clear(&self) {
        { let mut d = self.doc.borrow_mut(); d.pen = crate::path::Path::default(); d.pen_done = false; }
        self.pen_drag.set(None);
        self.area.queue_draw();
    }

    /// Puts the active layer's handles away, as Return does.
    pub fn park_handles(&self) { self.handles_parked.set(self.doc.borrow().document.active); self.update_cursor(); self.area.queue_draw(); }

    /// Handles come back: a new transform, or Transform Controls switched on.
    pub fn unpark_handles(&self) { self.handles_parked.set(None); self.update_cursor(); self.area.queue_draw(); }

    pub fn set_panels_hidden(&self, on: bool) {
        self.rail.widget.set_visible(!on);
        self.options_scroller.set_visible(!on);
        if let Some(sep) = self.rail.widget.next_sibling() { sep.set_visible(!on); }
    }

    pub fn set_preview(&self, on: bool) {
        self.rail.widget.set_visible(!on);
        self.options_scroller.set_visible(!on);
        self.status.set_visible(!on);
        if let Some(sep) = self.rail.widget.next_sibling() { sep.set_visible(!on); }
    }

    // Guides

    /// A guide within reach of a view point: vertical (an x) or not, its index, for dragging with the Move tool.
    fn guide_at(&self, view: (f64, f64)) -> Option<(bool, usize)> {
        let d = self.doc.borrow();
        if !d.document.show_guides { return None; }
        let size = d.size();
        let near = |a: f64, b: f64| (a - b).abs() <= 5.0;
        if let Some(i) = d.document.guides_v.iter().position(|x| near(d.viewport.view_point((*x, 0.0), size).0, view.0)) { return Some((true, i)); }
        if let Some(i) = d.document.guides_h.iter().position(|y| near(d.viewport.view_point((0.0, *y), size).1, view.1)) { return Some((false, i)); }
        None
    }

    /// A press on a ruler makes a guide there and starts dragging it (a vertical guide off the top ruler's x,
    /// a horizontal one off the left ruler's y).
    fn begin_guide_from_ruler(&self, view: (f64, f64)) -> bool {
        let mut d = self.doc.borrow_mut();
        if !d.rulers { return false; }
        let on_top = view.1 < RULER && view.0 >= RULER;
        let on_left = view.0 < RULER && view.1 >= RULER;
        if !on_top && !on_left { return false; }
        let size = d.size();
        let p = d.viewport.document_point(view, size);
        d.document.show_guides = true;
        let tolerance = 8.0 / d.viewport.zoom().max(1e-6);
        let entry = if on_top { let x = d.document.snapped_guide(true, p.0.round(), tolerance); d.document.guides_v.push(x); (true, d.document.guides_v.len() - 1) } else { let y = d.document.snapped_guide(false, p.1.round(), tolerance); d.document.guides_h.push(y); (false, d.document.guides_h.len() - 1) };
        self.guide_drag.set(Some(entry));
        drop(d);
        self.area.queue_draw();
        true
    }

    fn update_guide_drag(&self, view: (f64, f64)) {
        let Some((vertical, index)) = self.guide_drag.get() else { return };
        let mut d = self.doc.borrow_mut();
        let size = d.size();
        let p = d.viewport.document_point(view, size);
        let tolerance = 8.0 / d.viewport.zoom().max(1e-6);
        let snapped = d.document.snapped_guide(vertical, if vertical { p.0.round() } else { p.1.round() }, tolerance);
        if vertical { if let Some(g) = d.document.guides_v.get_mut(index) { *g = snapped; } }
        else if let Some(g) = d.document.guides_h.get_mut(index) { *g = snapped; }
        drop(d);
        self.area.queue_draw();
    }

    /// A guide let go off the canvas is removed, as Photoshop does.
    fn finish_guide_drag(&self) {
        let Some((vertical, index)) = self.guide_drag.take() else { return };
        let mut d = self.doc.borrow_mut();
        let (w, h) = (d.document.width() as f64, d.document.height() as f64);
        if vertical { if d.document.guides_v.get(index).is_some_and(|x| *x < 0.0 || *x > w) { d.document.guides_v.remove(index); } }
        else if d.document.guides_h.get(index).is_some_and(|y| *y < 0.0 || *y > h) { d.document.guides_h.remove(index); }
        drop(d);
        self.area.queue_draw();
        self.update_cursor();
    }

    // Marquee and lasso

    fn begin_draft(&self, view: (f64, f64), state: gdk::ModifierType) {
        let d = self.doc.borrow();
        let size = d.size();
        let point = d.viewport.document_point(view, size);
        let mode = selection_mode(state, d.mode);
        // A press inside the selection in New mode drags its outline instead.
        if mode == Mode::Replace && d.document.selection.as_ref().is_some_and(|s| !s.is_empty() && s.contains(point.0.floor(), point.1.floor())) {
            self.outline_move.set(Some((0.0, 0.0)));
            return;
        }
        let kind = match d.tool { Tool::Marquee => if d.marquee_ellipse { DraftKind::Ellipse } else { DraftKind::Rectangle }, _ => DraftKind::Freehand };
        let anchor = (point.0.round(), point.1.round());
        *self.draft.borrow_mut() = Some(Draft { points: vec![if kind == DraftKind::Freehand { point } else { anchor }], cursor: None, mode, kind, anchor });
    }

    fn update_draft(&self, view: (f64, f64), state: gdk::ModifierType) {
        let point = { let d = self.doc.borrow(); let size = d.size(); d.viewport.document_point(view, size) };
        if self.outline_move.get().is_some() {
            let start = { let d = self.doc.borrow(); let size = d.size(); d.viewport.document_point(self.stroke_start.get(), size) };
            self.outline_move.set(Some(((point.0 - start.0).round(), (point.1 - start.1).round())));
            self.area.queue_draw();
            return;
        }
        let mut draft = self.draft.borrow_mut();
        let Some(d) = draft.as_mut() else { return };
        match d.kind {
            DraftKind::Rectangle | DraftKind::Ellipse => {
                let square = state.contains(gdk::ModifierType::SHIFT_MASK);
                let (mut dx, mut dy) = (point.0.round() - d.anchor.0, point.1.round() - d.anchor.1);
                if square { let side = dx.abs().max(dy.abs()); dx = if dx < 0.0 { -side } else { side }; dy = if dy < 0.0 { -side } else { side }; }
                let (x0, y0) = (d.anchor.0.min(d.anchor.0 + dx), d.anchor.1.min(d.anchor.1 + dy));
                let (x1, y1) = (d.anchor.0.max(d.anchor.0 + dx), d.anchor.1.max(d.anchor.1 + dy));
                d.points = vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
            }
            DraftKind::Freehand => {
                if d.points.last().is_none_or(|l| (point.0 - l.0).hypot(point.1 - l.1) >= 0.25) { d.points.push(point); }
            }
            DraftKind::Polygonal => {}
        }
        drop(draft);
        self.area.queue_draw();
    }

    fn finish_outline_move(&self) {
        let Some((dx, dy)) = self.outline_move.take() else { return };
        if dx != 0.0 || dy != 0.0 {
            let result = self.doc.borrow_mut().document.move_selection(dx, dy);
            if let Err(error) = result { self.notify(&format!("{error:#}")); }
        }
        self.area.queue_draw();
    }

    fn finish_draft(&self) {
        let Some(draft) = self.draft.borrow_mut().take() else { return };
        let result = {
            let mut d = self.doc.borrow_mut();
            let antialiased = d.antialiased;
            match draft.kind {
                DraftKind::Rectangle | DraftKind::Ellipse => {
                    if draft.points.len() == 4 {
                        let (x0, y0, x1, y1) = (draft.points[0].0, draft.points[0].1, draft.points[2].0, draft.points[2].1);
                        d.document.select_box(x0, y0, x1 - x0, y1 - y0, draft.kind == DraftKind::Ellipse, draft.mode, antialiased)
                    } else { d.document.select_box(0.0, 0.0, 0.0, 0.0, false, draft.mode, antialiased) }
                }
                DraftKind::Freehand => d.document.select_polygon(&draft.points, draft.mode, antialiased, "Lasso"),
                DraftKind::Polygonal => d.document.select_polygon(&draft.points, draft.mode, antialiased, "Polygonal Lasso"),
            }
        };
        if let Err(error) = result { self.notify(&format!("{error:#}")); }
        self.area.queue_draw();
    }

    pub fn notify(&self, text: &str) { self.message.set_label(text); }

    pub fn set_refresh(&self, f: Rc<dyn Fn()>) { *self.refresh.borrow_mut() = Some(f); }

    /// Drops the composited frame so the next draw rebuilds it (the surround color changed).
    pub fn drop_cache(&self) { *self.cache.borrow_mut() = None; self.area.queue_draw(); }

    pub fn sync_brush_options(&self) { let settings = self.doc.borrow().brush.clone(); self.options.sync_brush(&settings); }

    fn begin_stroke(&self, view: (f64, f64), shift: bool) {
        let result = {
            let mut d = self.doc.borrow_mut();
            let size = d.size();
            let point = d.viewport.document_point(view, size);
            let settings = d.brush.clone();
            let kind = match d.tool {
                Tool::Brush => StrokeKind::Paint,
                Tool::Eraser => StrokeKind::Erase,
                Tool::Heal => StrokeKind::Heal { mode: d.heal_mode },
                Tool::Blur => StrokeKind::Blur,
                Tool::Clone => {
                    let Some(source) = d.clone_source else { drop(d); self.notify("Alt-click where Clone Stamp should copy from first."); return };
                    let offset = match (d.clone_aligned, d.clone_offset) { (true, Some(o)) => o, _ => ((source.0 - point.0).round(), (source.1 - point.1).round()) };
                    d.clone_offset = Some(offset);
                    StrokeKind::Clone { offset, all_layers: d.clone_all_layers }
                }
                _ => return,
            };
            // Shift-click paints a straight line from where the last stroke ended.
            let start = if shift { d.last_brush_point.unwrap_or(point) } else { point };
            let mask = d.document.mask_target() && matches!(d.tool, Tool::Brush | Tool::Eraser | Tool::Blur);
            let white = d.mask_paint_white && d.tool == Tool::Brush;
            let warp = match (d.tool, d.blur_mode) { (Tool::Blur, 0) => Some(crate::warp::WarpMode::Liquify), (Tool::Blur, 2) => Some(crate::warp::WarpMode::Smudge), _ => None };
            // On a mask the Smear tool always blurs: Liquify and Smudge move pixels, which a mask has none of.
            let mut result = if mask && d.tool == Tool::Blur { d.document.begin_mask_blur(start, &settings) }
                else if let Some(mode) = warp { d.document.begin_warp(start, &settings, mode) }
                else if mask { d.document.begin_mask_stroke(start, &settings, white) } else { d.document.begin_stroke(start, &settings, kind) };
            if result.is_ok() && start != point { result = d.document.continue_stroke(point); }
            if result.is_ok() { d.last_brush_point = Some(point); }
            result
        };
        match result {
            Ok(()) => { self.painting.set(true); self.notify(""); self.area.queue_draw(); }
            Err(error) => self.notify(&format!("{error:#}")),
        }
    }

    fn continue_stroke(&self, view: (f64, f64)) {
        let result = {
            let mut d = self.doc.borrow_mut();
            let size = d.size();
            let point = d.viewport.document_point(view, size);
            d.last_brush_point = Some(point);
            d.document.continue_stroke(point)
        };
        if let Err(error) = result { self.notify(&format!("{error:#}")); }
        self.area.queue_draw();
    }

    fn finish_stroke(&self) {
        self.painting.set(false);
        let result = self.doc.borrow_mut().document.finish_stroke();
        if let Err(error) = result { self.notify(&format!("{error:#}")); }
        if let Some(refresh) = self.refresh.borrow().as_ref() { refresh(); }
        self.area.queue_draw();
    }

    /// A drag through document points, as a script would make it with the current tool: a brush stroke, a
    /// transform drag, or a marquee or lasso outline. Brush steps are timed on stderr.
    pub fn scripted_stroke(&self, points: &[(f64, f64)]) {
        let Some(&first) = points.first() else { return };
        let tool = self.doc.borrow().tool;
        let to_view = |p: (f64, f64)| { let d = self.doc.borrow(); let size = d.size(); d.viewport.view_point(p, size) };
        if !tool.is_brush() {
            let view = to_view(first);
            self.stroke_start.set(view);
            let none = gdk::ModifierType::empty();
            match tool {
                Tool::Move => { if !self.begin_transform(view, none) { eprintln!("drag refused: nothing to move"); return; } }
                Tool::Marquee | Tool::Lasso => self.begin_draft(view, none),
                _ => { eprintln!("drag: tool {tool:?} does not drag"); return; }
            }
            for &p in &points[1..] {
                let v = to_view(p);
                if tool == Tool::Move { self.update_transform(v, none); } else { self.update_draft(v, none); }
            }
            if tool == Tool::Move { self.finish_transform(); } else if self.outline_move.get().is_some() { self.finish_outline_move(); } else { self.finish_draft(); }
            return;
        }
        let view = to_view(first);
        let start = std::time::Instant::now();
        self.begin_stroke(view, false);
        eprintln!("stroke begin {:.1} ms{}", start.elapsed().as_secs_f64() * 1000.0, if self.painting.get() { String::new() } else { format!(" (refused: {})", self.message.label()) });
        let mut worst = 0.0f64;
        for &point in &points[1..] {
            let view = { let d = self.doc.borrow(); let size = d.size(); d.viewport.view_point(point, size) };
            let step = std::time::Instant::now();
            self.continue_stroke(view);
            worst = worst.max(step.elapsed().as_secs_f64() * 1000.0);
            // Bring the frame cache up to date between points, as a real pointer's frames would.
            let (w, h, scale) = (self.area.width(), self.area.height(), self.area.scale_factor());
            if w > 0 && h > 0 { let mut d = self.doc.borrow_mut(); let _ = self.cached_frame(&mut d, w, h, scale); }
        }
        eprintln!("stroke steps: {} points, worst {:.1} ms", points.len() - 1, worst);
        let end = std::time::Instant::now();
        self.finish_stroke();
        eprintln!("stroke finish {:.1} ms", end.elapsed().as_secs_f64() * 1000.0);
    }

    /// Right-click with a brush: the tip and its settings in a popover at the pointer.
    pub fn brush_popover(self: &Rc<Self>, x: f64, y: f64) {
        let popover = gtk::Popover::builder().has_arrow(true).build();
        popover.set_parent(&self.area);
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).margin_top(8).margin_bottom(8).margin_start(8).margin_end(8).build();
        let head = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).build();
        let this = self.clone();
        let chosen = popover.clone();
        let picker = super::brushes::BrushPicker::new(self.doc.clone(), Rc::new(move || { this.sync_brush_options(); chosen.popdown(); }));
        head.append(&picker.widget);
        head.append(&gtk::Label::builder().label("Brush").css_classes(["heading"]).build());
        content.append(&head);
        let grid = gtk::Grid::builder().row_spacing(4).column_spacing(8).build();
        let settings = self.doc.borrow().brush.clone();
        let rows: [(&str, f64, f64, f64, fn(&mut crate::brush::BrushSettings, f64)); 5] = [
            ("Size", 1.0, 2000.0, settings.diameter, |b, v| b.diameter = v),
            ("Hardness", 0.0, 100.0, settings.hardness * 100.0, |b, v| b.hardness = v / 100.0),
            ("Opacity", 1.0, 100.0, settings.opacity * 100.0, |b, v| b.opacity = v / 100.0),
            ("Spacing", 0.0, 300.0, settings.spacing.map_or(0.0, |s| s * 100.0), |b, v| b.spacing = if v <= 0.0 { None } else { Some(v / 100.0) }),
            ("Jitter", 0.0, 100.0, settings.angle_jitter * 100.0, |b, v| b.angle_jitter = v / 100.0),
        ];
        for (i, (label, lo, hi, value, set)) in rows.into_iter().enumerate() {
            grid.attach(&gtk::Label::builder().label(label).xalign(0.0).build(), 0, i as i32, 1, 1);
            let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, lo, hi, 1.0);
            scale.set_value(value);
            scale.set_size_request(180, -1);
            scale.set_draw_value(true);
            scale.set_value_pos(gtk::PositionType::Right);
            let this = self.clone();
            scale.connect_value_changed(move |s| { { let mut d = this.doc.borrow_mut(); set(&mut d.brush, s.value()); } this.sync_brush_options(); this.area.queue_draw(); });
            grid.attach(&scale, 1, i as i32, 1, 1);
        }
        content.append(&grid);
        popover.set_child(Some(&content));
        self.track_popover(&popover);
        popover.popup();
    }

    /// Remembers the popover open over the canvas so the next press on the canvas closes it, whether or not
    /// the popover's own outside-click grab saw that press; it unparents itself once closed.
    fn track_popover(&self, popover: &gtk::Popover) {
        self.close_popover();
        *self.popover.borrow_mut() = Some(popover.clone());
        popover.connect_closed(move |p| { let p = p.clone(); glib::idle_add_local_once(move || p.unparent()); });
    }

    pub fn close_popover(&self) {
        if let Some(p) = self.popover.borrow_mut().take() { if p.is_visible() { p.popdown(); } }
    }

    /// Brush keys: [ and ] size, { and } hardness, digits opacity. True when the key was one of those.
    pub fn brush_key(&self, c: char) -> bool {
        let mut d = self.doc.borrow_mut();
        if d.document.stroke_active() { return false; }
        // The palette: X swaps foreground and background, D resets them (on a mask, the paint tone).
        match c {
            'x' | 'X' => {
                if d.document.mask_target() { d.mask_paint_white = !d.mask_paint_white; } else { let fg = d.brush.color; d.brush.color = d.background; d.background = fg; }
                let (fg, bg) = (d.brush.color, d.background); let mask = d.document.mask_target();
                drop(d); self.rail.sync_palette(fg, bg); self.sync_inspector(); if mask { self.options.sync_mask_paint(&self.doc); } return true;
            }
            'd' | 'D' => {
                if d.document.mask_target() { d.mask_paint_white = false; } else { d.brush.color = [0.0; 3]; d.background = [1.0; 3]; }
                let (fg, bg) = (d.brush.color, d.background); let mask = d.document.mask_target();
                drop(d); self.rail.sync_palette(fg, bg); if mask { self.options.sync_mask_paint(&self.doc); } return true;
            }
            'U' if d.tool == Tool::Shape => { d.shape_ellipse = !d.shape_ellipse; let e = d.shape_ellipse; drop(d); self.options.sync_shape_kind(e); return true; }
            _ => {}
        }
        if !d.tool.is_brush() { return false; }
        let b = &mut d.brush;
        match c {
            ']' => { b.diameter = (b.diameter + 1.0).max((b.diameter * 1.2).round()).min(2000.0); }
            '[' => { b.diameter = (b.diameter - 1.0).min((b.diameter / 1.2).round()).max(1.0); }
            '}' => { b.hardness = ((b.hardness * 4.0 + 0.001).floor() + 1.0).min(4.0) / 4.0; }
            '{' => { b.hardness = ((b.hardness * 4.0 - 0.001).ceil() - 1.0).max(0.0) / 4.0; }
            '0'..='9' => { let digit = c as u32 - '0' as u32; b.opacity = if digit == 0 { 1.0 } else { digit as f64 / 10.0 }; }
            _ => return false,
        }
        let settings = b.clone();
        drop(d);
        self.options.sync_brush(&settings);
        self.area.queue_draw();
        true
    }

    /// The Magic Wand at a view point.
    fn wand_view(&self, x: f64, y: f64, mode: Mode) {
        let point = { let d = self.doc.borrow(); let size = d.size(); d.viewport.document_point((x, y), size) };
        self.wand_document(point.0, point.1, mode);
    }

    /// The Magic Wand at a document point, with the options bar's mode.
    pub fn wand_at(&self, x: f64, y: f64) {
        let mode = self.doc.borrow().mode;
        self.wand_document(x, y, mode);
    }

    fn wand_document(&self, x: f64, y: f64, mode: Mode) {
        let result = { let mut d = self.doc.borrow_mut(); let wand = d.wand; d.document.wand(x, y, &wand, mode) };
        match result {
            Ok(()) => {
                let d = self.doc.borrow();
                match &d.document.selection {
                    Some(s) if s.outline.is_empty() && !s.is_empty() => self.notify("That selection is too detailed to outline; it still applies. Try a different Tolerance, or turn on Contiguous."),
                    _ => self.notify(""),
                }
            }
            Err(error) => self.notify(&format!("{error:#}")),
        }
        self.area.queue_draw();
    }

    pub fn zoom_by(&self, factor: f64) {
        let mut d = self.doc.borrow_mut();
        let size = d.size();
        let center = d.viewport.center();
        let zoom = d.viewport.zoom() * factor;
        d.viewport.set_zoom(zoom, center, size);
        self.zoom_label.set_label(&zoom_text(d.viewport.zoom()));
        self.area.queue_draw();
    }

    pub fn zoom_to(&self, zoom: f64) {
        let mut d = self.doc.borrow_mut();
        let size = d.size();
        let center = d.viewport.center();
        d.viewport.set_zoom(zoom, center, size);
        self.zoom_label.set_label(&zoom_text(d.viewport.zoom()));
        self.area.queue_draw();
    }

    pub fn fit(&self) {
        let mut d = self.doc.borrow_mut();
        let size = d.size();
        d.viewport.fit(size);
        self.zoom_label.set_label(&zoom_text(d.viewport.zoom()));
        self.area.queue_draw();
    }
}

/// Shift adds, Alt subtracts (with or without Shift); otherwise the options bar's mode.
fn selection_mode(state: gdk::ModifierType, chosen: Mode) -> Mode {
    if state.contains(gdk::ModifierType::ALT_MASK) { Mode::Subtract } else if state.contains(gdk::ModifierType::SHIFT_MASK) { Mode::Add } else { chosen }
}

pub fn zoom_text(zoom: f64) -> String {
    let percent = zoom * 100.0;
    if percent < 10.0 { format!("{percent:.1}%") } else { format!("{percent:.0}%") }
}

thread_local! {
    /// The GPU compositor, made on first use; the inner None means there is no usable GPU.
    static GPU: RefCell<Option<Option<crate::gpu::Gpu>>> = const { RefCell::new(None) };
}

fn with_gpu<R>(f: impl FnOnce(&mut crate::gpu::Gpu) -> R) -> Option<R> {
    GPU.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            let gpu = crate::gpu::Gpu::new();
            if std::env::var_os("COMPOSITOR_TRACE").is_some() { match &gpu { Some(g) => eprintln!("gpu: {}", g.name), None => eprintln!("gpu: none, compositing on the CPU") } }
            *slot = Some(gpu);
        }
        slot.as_mut().unwrap().as_mut().map(f)
    })
}

/// The document composited on the GPU for `w` x `h` device pixels whose centers `device_to_document`
/// maps onto the document; None when there is no GPU or the frame needs the CPU path.
thread_local! {
    /// Presentation has failed for good this session (the CPU frame cache draws instead).
    static PRESENT_FAILED: Cell<bool> = const { Cell::new(false) };
}

/// The whole canvas frame composited on the GPU and handed to GTK as a dma-buf texture: nothing crosses
/// back to the CPU. None when that path is unavailable, so the frame cache draws as before.
fn present_frame(doc: &mut super::Doc, w: i32, h: i32, scale: i32) -> Option<gdk::Texture> {
    if PRESENT_FAILED.with(|f| f.get()) || w <= 0 || h <= 0 { return None; }
    if !std::env::var("COMPOSITOR_GPU").is_ok_and(|v| v == "present") { return None; }
    let ds = scale.max(1) as f64;
    let (dw, dh) = ((w as f64 * ds) as u32, (h as f64 * ds) as u32);
    let size = doc.size();
    let vp = doc.viewport;
    let (rx, ry, rw, rh) = vp.document_rect(size);
    let ppp = vp.points_per_pixel();
    let device_to_document = cairo::Matrix::new(1.0 / (ds * ppp), 0.0, 0.0, 1.0 / (ds * ppp), -rx / ppp, -ry / ppp);
    let surround = if doc.preview { (0.0, 0.0, 0.0) } else { super::theme::surround() };
    let background = crate::gpu::Background { surround: [surround.0 as f32, surround.1 as f32, surround.2 as f32], rect: ((rx * ds) as f32, (ry * ds) as f32, (rw * ds) as f32, (rh * ds) as f32), tile: (10.0 * ds) as f32, shadow: !doc.preview };
    let nearest = vp.zoom() >= CRISP_ZOOM;
    let trace = std::env::var_os("COMPOSITOR_TRACE").is_some();
    let t = std::time::Instant::now();
    let result = with_gpu(|gpu| {
        if !gpu.export { return None; }
        let plan = match doc.document.renderer.gpu_plan(device_to_document, dw, dh, gpu.max_dimension(), nearest) { Ok(Some(p)) => p, Ok(None) => return None, Err(e) => { eprintln!("gpu plan: {e:#}"); return None; } };
        if gpu.buffer_size != (dw, dh) || gpu.buffers.len() < 3 {
            gpu.buffers.clear();
            for _ in 0..3 { match gpu.export_buffer(dw, dh) { Ok(b) => gpu.buffers.push(b), Err(e) => { eprintln!("gpu present: {e:#}"); PRESENT_FAILED.with(|f| f.set(true)); return None; } } }
            gpu.buffer_size = (dw, dh);
            gpu.next_buffer = 0;
        }
        let index = gpu.next_buffer;
        gpu.next_buffer = (index + 1) % gpu.buffers.len();
        let buffer = std::mem::take(&mut gpu.buffers);
        let outcome = gpu.present(&plan, background, &buffer[index]);
        let display = gdk::Display::default();
        let texture = match (outcome, display) {
            (Ok(()), Some(display)) => match crate::gpu::present::gdk_texture(&display, &buffer[index], dw, dh) { Ok(t) => Some(t), Err(e) => { eprintln!("gpu present: {e:#}"); PRESENT_FAILED.with(|f| f.set(true)); None } },
            (Err(e), _) => { eprintln!("gpu present: {e:#}"); None }
            _ => None,
        };
        gpu.buffers = buffer;
        texture
    }).flatten();
    if trace { eprintln!("  gpu present {}x{} {:.1} ms{}", dw, dh, t.elapsed().as_secs_f64() * 1000.0, if result.is_some() { "" } else { " (fell back)" }); }
    result
}

fn gpu_frame(doc: &mut super::Doc, device_to_document: cairo::Matrix, w: u32, h: u32) -> Option<cairo::ImageSurface> {
    if w == 0 || h == 0 { return None; }
    if !std::env::var("COMPOSITOR_GPU").is_ok_and(|v| v == "readback") { return None; }
    with_gpu(|gpu| {
        let t = std::time::Instant::now();
        let plan = match doc.document.renderer.gpu_plan(device_to_document, w, h, gpu.max_dimension(), false) { Ok(Some(p)) => p, Ok(None) => return None, Err(e) => { eprintln!("gpu plan: {e:#}"); return None; } };
        let bytes = match gpu.render(&plan) { Ok(b) => b, Err(e) => { eprintln!("gpu render: {e:#}"); return None; } };
        if std::env::var_os("COMPOSITOR_TRACE").is_some() { eprintln!("  gpu frame {}x{} {:.1} ms", w, h, t.elapsed().as_secs_f64() * 1000.0); }
        cairo::ImageSurface::create_for_data(bytes, cairo::Format::ARgb32, w as i32, h as i32, (w * 4) as i32).ok()
    }).flatten()
}

fn draw_document(doc: &mut super::Doc, cr: &Context, width: f64, height: f64) -> Result<()> {
    let (sr, sg, sb) = if doc.preview { (0.0, 0.0, 0.0) } else { super::theme::surround() };
    cr.set_source_rgb(sr, sg, sb);
    cr.paint()?;
    let size = doc.size();
    let vp = doc.viewport;
    let (rx, ry, rw, rh) = vp.document_rect(size);
    let visible = intersect((rx, ry, rw, rh), (0.0, 0.0, width, height));
    let Some((vx, vy, vw, vh)) = visible else { return Ok(()) };
    let hairline = 1.0 / vp.backing_scale;

    // A soft shadow under the document: rings of falling alpha stand in for a 14 point blur.
    for i in 1..=8 {
        let spread = i as f64 * 1.8;
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.055 * (9 - i) as f64 / 8.0);
        cr.rectangle(rx - spread, ry - spread + 3.0, rw + spread * 2.0, rh + spread * 2.0);
        cr.fill()?;
    }
    cr.set_source_rgb(0.30, 0.30, 0.30);
    cr.rectangle(rx, ry, rw, rh);
    cr.fill()?;

    cr.save()?;
    cr.rectangle(vx, vy, vw, vh);
    cr.clip();
    // Work scales with the visible viewport, not document dimensions.
    let tile = 10.0;
    let (min_x, max_x) = (((vx - rx) / tile).floor() as i64, ((vx + vw - rx) / tile).ceil() as i64);
    let (min_y, max_y) = (((vy - ry) / tile).floor() as i64, ((vy + vh - ry) / tile).ceil() as i64);
    cr.set_source_rgb(0.35, 0.35, 0.35);
    for row in min_y..max_y {
        for column in min_x..max_x {
            if (row + column) % 2 == 0 { cr.rectangle(rx + column as f64 * tile, ry + row as f64 * tile, tile, tile); }
        }
    }
    cr.fill()?;

    let ppp = vp.points_per_pixel();
    if vp.zoom() >= CRISP_ZOOM {
        // Render the visible document pixels 1:1, then scale them up with no smoothing.
        let x0 = ((vx - rx) / ppp).floor().max(0.0);
        let y0 = ((vy - ry) / ppp).floor().max(0.0);
        let x1 = ((vx + vw - rx) / ppp).ceil().min(size.0);
        let y1 = ((vy + vh - ry) / ppp).ceil().min(size.1);
        let (w, h) = ((x1 - x0) as i32, (y1 - y0) as i32);
        if w > 0 && h > 0 {
            let pixels = match gpu_frame(doc, cairo::Matrix::new(1.0, 0.0, 0.0, 1.0, x0, y0), w as u32, h as u32) {
                Some(surface) => surface,
                None => {
                    let pixels = new_argb(w, h)?;
                    {
                        let pcr = Context::new(&pixels)?;
                        pcr.translate(-x0, -y0);
                        pcr.rectangle(x0, y0, w as f64, h as f64);
                        pcr.clip();
                        doc.document.renderer.draw(&pcr)?;
                    }
                    pixels
                }
            };
            cr.translate(rx, ry);
            cr.scale(ppp, ppp);
            cr.set_source_surface(&pixels, x0, y0)?;
            cr.source().set_filter(Filter::Nearest);
            cr.rectangle(x0, y0, w as f64, h as f64);
            cr.fill()?;
        }
    } else {
        // The visible part of the document at device resolution, composited on the GPU when it can be.
        let ds = cr.target().device_scale().0.max(1.0);
        let (dw, dh) = ((vw * ds).ceil() as u32, (vh * ds).ceil() as u32);
        let device_to_document = cairo::Matrix::new(1.0 / (ds * ppp), 0.0, 0.0, 1.0 / (ds * ppp), (vx - rx) / ppp, (vy - ry) / ppp);
        match gpu_frame(doc, device_to_document, dw, dh) {
            Some(surface) => {
                cr.save()?;
                cr.translate(vx, vy);
                cr.scale(1.0 / ds, 1.0 / ds);
                cr.set_source_surface(&surface, 0.0, 0.0)?;
                cr.source().set_filter(Filter::Nearest);
                cr.rectangle(0.0, 0.0, dw as f64, dh as f64);
                cr.fill()?;
                cr.restore()?;
            }
            None => {
                cr.translate(rx, ry);
                cr.scale(ppp, ppp);
                cr.rectangle(0.0, 0.0, size.0, size.1);
                cr.clip();
                doc.document.renderer.draw(cr)?;
            }
        }
    }
    cr.restore()?;

    if vp.zoom() >= PIXEL_GRID_ZOOM {
        let first = vp.document_point((vx, vy), size);
        let last = vp.document_point((vx + vw, vy + vh), size);
        cr.set_source_rgba(0.55, 0.55, 0.55, 0.45);
        let mut column = first.0.ceil();
        while column <= last.0.floor() {
            let x = vp.view_point((column, 0.0), size).0;
            cr.rectangle(x - hairline / 2.0, vy, hairline, vh);
            column += 1.0;
        }
        let mut row = first.1.ceil();
        while row <= last.1.floor() {
            let y = vp.view_point((0.0, row), size).1;
            cr.rectangle(vx, y - hairline / 2.0, vw, hairline);
            row += 1.0;
        }
        cr.fill()?;
    }
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.13);
    cr.set_line_width(hairline);
    cr.rectangle(rx, ry, rw, rh);
    cr.stroke()?;
    Ok(())
}

/// Everything that sits on top of the composited document and changes without it: the transform box and
/// guides, selection drafts, the brush cursor, the clone crosshair, and the marching ants.
fn draw_overlays(doc: &mut super::Doc, cr: &Context, width: f64, height: f64, pointer: (f64, f64), draft: Option<&Draft>, outline_offset: (f64, f64), text_edit: Option<(uuid::Uuid, usize)>) -> Result<()> {
    let size = doc.size();
    let vp = doc.viewport;
    let (rx, ry, rw, rh) = vp.document_rect(size);
    let Some((vx, vy, vw, vh)) = intersect((rx, ry, rw, rh), (0.0, 0.0, width, height)) else { return Ok(()) };
    let ppp = vp.points_per_pixel();
    if doc.preview { return Ok(()); }

    // The grid: light lines every subdivision, stronger ones at each major spacing, over the canvas only.
    if let Some((spacing, subdivisions)) = doc.document.grid {
        let (rx, ry, rw, rh) = vp.document_rect(size);
        cr.save()?;
        cr.rectangle(rx, ry, rw, rh);
        cr.clip();
        let step = (spacing / subdivisions.max(1) as f64).max(1.0);
        let view_step = step * vp.zoom();
        if view_step >= 4.0 {
            cr.set_line_width(1.0);
            let (gx, gy) = doc.document.grid_lines();
            for major in [false, true] {
                if major { cr.set_source_rgba(0.5, 0.5, 0.5, 0.55); } else { cr.set_source_rgba(0.5, 0.5, 0.5, 0.22); }
                for (i, x) in gx.iter().enumerate() { if (i as u32 % subdivisions.max(1) == 0) == major { let (vx, _) = vp.view_point((*x, 0.0), size); cr.move_to(vx.round() + 0.5, ry); cr.line_to(vx.round() + 0.5, ry + rh); } }
                for (i, y) in gy.iter().enumerate() { if (i as u32 % subdivisions.max(1) == 0) == major { let (_, vy) = vp.view_point((0.0, *y), size); cr.move_to(rx, vy.round() + 0.5); cr.line_to(rx + rw, vy.round() + 0.5); } }
                cr.stroke()?;
            }
        }
        cr.restore()?;
    }
    // Guides: cyan lines across the canvas, as Photoshop draws them.
    if doc.document.show_guides && !doc.preview && !doc.hide_extras && !(doc.document.guides_v.is_empty() && doc.document.guides_h.is_empty()) {
        cr.set_source_rgb(0.0, 0.85, 1.0);
        cr.set_line_width(1.0);
        for x in &doc.document.guides_v { let (vx, _) = vp.view_point((*x, 0.0), size); cr.move_to(vx.round() + 0.5, 0.0); cr.line_to(vx.round() + 0.5, height); }
        for y in &doc.document.guides_h { let (_, vy) = vp.view_point((0.0, *y), size); cr.move_to(0.0, vy.round() + 0.5); cr.line_to(width, vy.round() + 0.5); }
        cr.stroke()?;
    }
    // Rulers along the top and left edges, in document pixels, with the pointer's position marked.
    if doc.rulers && !doc.preview {
        let thickness = RULER;
        let (p_bg, p_fg, p_line) = { let pal = super::theme::current(); match pal {
            Some(p) => (hex_rgb(&p.dark_background), hex_rgb(&p.foreground), hex_rgb(&p.muted)),
            None => ((0.16, 0.16, 0.16), (0.85, 0.85, 0.85), (0.45, 0.45, 0.45)),
        } };
        // A step in document pixels that lands ticks 60 to 150 points apart.
        let mut step = 1.0;
        for candidate in [1.0, 2.0, 5.0, 10.0, 20.0, 25.0, 50.0, 100.0, 200.0, 250.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0] { step = candidate; if candidate * ppp >= 60.0 { break; } }
        let minor = step / 5.0;
        cr.set_source_rgb(p_bg.0, p_bg.1, p_bg.2);
        cr.rectangle(0.0, 0.0, width, thickness);
        cr.rectangle(0.0, 0.0, thickness, height);
        cr.fill()?;
        cr.set_source_rgb(p_line.0, p_line.1, p_line.2);
        cr.set_line_width(1.0);
        cr.move_to(0.0, thickness + 0.5); cr.line_to(width, thickness + 0.5);
        cr.move_to(thickness + 0.5, 0.0); cr.line_to(thickness + 0.5, height);
        cr.stroke()?;
        cr.select_font_face("sans-serif", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
        cr.set_font_size(9.0);
        // Horizontal: ticks from the first step left of the visible edge to past the right.
        let (dx0, _) = vp.document_point((thickness, 0.0), size);
        let (dx1, _) = vp.document_point((width, 0.0), size);
        let mut x = (dx0 / minor).floor() * minor;
        while x <= dx1 {
            let (vx, _) = vp.view_point((x, 0.0), size);
            let major = ((x / step).round() * step - x).abs() < minor / 2.0;
            let len = if major { thickness } else { 5.0 };
            cr.set_source_rgb(p_line.0, p_line.1, p_line.2);
            cr.move_to(vx.round() + 0.5, thickness - len); cr.line_to(vx.round() + 0.5, thickness);
            cr.stroke()?;
            if major { cr.set_source_rgb(p_fg.0, p_fg.1, p_fg.2); cr.move_to(vx.round() + 3.0, 10.0); cr.show_text(&format!("{}", x.round() as i64))?; }
            x += minor;
        }
        // Vertical, labels turned to read along the ruler.
        let (_, dy0) = vp.document_point((0.0, thickness), size);
        let (_, dy1) = vp.document_point((0.0, height), size);
        let mut y = (dy0 / minor).floor() * minor;
        while y <= dy1 {
            let (_, vy) = vp.view_point((0.0, y), size);
            let major = ((y / step).round() * step - y).abs() < minor / 2.0;
            let len = if major { thickness } else { 5.0 };
            cr.set_source_rgb(p_line.0, p_line.1, p_line.2);
            cr.move_to(thickness - len, vy.round() + 0.5); cr.line_to(thickness, vy.round() + 0.5);
            cr.stroke()?;
            if major {
                cr.set_source_rgb(p_fg.0, p_fg.1, p_fg.2);
                cr.save()?;
                cr.translate(10.0, vy.round() - 3.0);
                cr.rotate(-std::f64::consts::PI / 2.0);
                cr.move_to(0.0, 0.0);
                cr.show_text(&format!("{}", y.round() as i64))?;
                cr.restore()?;
            }
            y += minor;
        }
        // The pointer's place on each ruler.
        let (px, py) = pointer;
        if px > -1.0e8 {
            let (ar, ag, ab) = super::theme::accent();
            cr.set_source_rgb(ar, ag, ab);
            cr.rectangle(px.round(), 0.0, 1.0, thickness);
            cr.rectangle(0.0, py.round(), thickness, 1.0);
            cr.fill()?;
        }
        // The corner square.
        cr.set_source_rgb(p_bg.0, p_bg.1, p_bg.2);
        cr.rectangle(0.0, 0.0, thickness, thickness);
        cr.fill()?;
    }

    // The Move tool's transform box and handles, and the guides a snapped move met.
    if doc.tool == Tool::Move {
        if let Some(id) = doc.document.active {
            let layer = doc.document.renderer.layer(id).clone();
            let visible = crate::format::visible_layers(doc.document.renderer.layers()).contains(&id);
            let grouped = doc.document.transforms_as_group();
            let boxed = if grouped { doc.document.group_box() } else if !layer.is_group() && visible && doc.document.renderer.has_image(id) { Some(layer.transform) } else { None };
            if let Some(t) = boxed {
                let mut g = Geometry::new(&t, |p| vp.view_point(p, size));
                if let Some(corners) = doc.distort {
                    // While distorting, the box is the shape's four corners.
                    let c = corners.map(|p| vp.view_point(p, size));
                    for (i, p) in c.iter().enumerate() { g.handles[i * 2] = *p; }
                    for i in 0..4 { let (a, b) = (c[i], c[(i + 1) % 4]); g.handles[i * 2 + 1] = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0); }
                }
                cr.new_path();
                cr.move_to(g.handles[0].0, g.handles[0].1);
                for i in [2, 4, 6] { cr.line_to(g.handles[i].0, g.handles[i].1); }
                cr.close_path();
                if doc.distort.is_none() { cr.move_to(g.handles[1].0, g.handles[1].1); cr.line_to(g.rotation.0, g.rotation.1); }
                let (ar, ag, ab) = super::theme::accent();
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.7);
                cr.set_line_width(3.0);
                cr.stroke_preserve()?;
                cr.set_source_rgb(ar, ag, ab);
                cr.set_line_width(1.0);
                cr.stroke()?;
                for (x, y) in g.handles {
                    cr.rectangle(x - 3.5, y - 3.5, 7.0, 7.0);
                    cr.set_source_rgb(1.0, 1.0, 1.0);
                    cr.fill_preserve()?;
                    cr.set_source_rgb(ar, ag, ab);
                    cr.stroke()?;
                }
                if doc.distort.is_none() {
                    cr.arc(g.rotation.0, g.rotation.1, 4.0, 0.0, std::f64::consts::TAU);
                    cr.set_source_rgb(1.0, 1.0, 1.0);
                    cr.fill_preserve()?;
                    cr.set_source_rgb(ar, ag, ab);
                    cr.stroke()?;
                }
            }
        }
        cr.set_source_rgb(0.21, 0.52, 0.89);
        cr.set_line_width(1.0);
        if let Some(x) = doc.snap_guides.0 { let (vx0, _) = vp.view_point((x, 0.0), size); cr.move_to(vx0, 0.0); cr.line_to(vx0, height); cr.stroke()?; }
        if let Some(y) = doc.snap_guides.1 { let (_, vy0) = vp.view_point((0.0, y), size); cr.move_to(0.0, vy0); cr.line_to(width, vy0); cr.stroke()?; }
    }

    // The gradient's line while it is dragged.
    if let Some((start, end)) = doc.gradient_line {
        let (a, b) = (vp.view_point(start, size), vp.view_point(end, size));
        cr.move_to(a.0, a.1); cr.line_to(b.0, b.1);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.8); cr.set_line_width(3.0); cr.stroke_preserve()?;
        cr.set_source_rgb(1.0, 1.0, 1.0); cr.set_line_width(1.0); cr.stroke()?;
        for p in [a, b] { cr.arc(p.0, p.1, 4.0, 0.0, std::f64::consts::TAU); cr.set_source_rgb(1.0, 1.0, 1.0); cr.fill_preserve()?; cr.set_source_rgb(0.0, 0.0, 0.0); cr.stroke()?; }
    }
    // The shape being dragged out, in the color it will be.
    if let Some((x, y, w, h)) = doc.shape_draft {
        if w >= 1.0 && h >= 1.0 {
            let (a, b) = (vp.view_point((x, y), size), vp.view_point((x + w, y + h), size));
            let c = doc.brush.color;
            if doc.shape_ellipse { cr.save()?; cr.translate((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0); cr.scale((b.0 - a.0) / 2.0, (b.1 - a.1) / 2.0); cr.arc(0.0, 0.0, 1.0, 0.0, std::f64::consts::TAU); cr.restore()?; }
            else { cr.rectangle(a.0, a.1, b.0 - a.0, b.1 - a.1); }
            cr.set_source_rgba(c[0], c[1], c[2], 0.6); cr.fill_preserve()?;
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.8); cr.set_line_width(1.0); cr.stroke()?;
        }
    }
    // The crop frame: everything outside it dimmed, handles on its edges.
    if doc.tool == Tool::Crop {
        if let Some((x, y, w, h)) = doc.crop {
            let (a, b) = (vp.view_point((x, y), size), vp.view_point((x + w, y + h), size));
            cr.set_fill_rule(cairo::FillRule::EvenOdd);
            cr.rectangle(0.0, 0.0, width, height);
            cr.rectangle(a.0, a.1, b.0 - a.0, b.1 - a.1);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
            cr.fill()?;
            cr.set_fill_rule(cairo::FillRule::Winding);
            let t = crate::format::Transform { origin: crate::format::Point(x, y), size: crate::format::Size(w, h), rotation: 0.0, flip_x: false, flip_y: false, sampling: Default::default() };
            let g = Geometry::new(&t, |p| vp.view_point(p, size));
            cr.rectangle(a.0, a.1, b.0 - a.0, b.1 - a.1);
            cr.set_source_rgb(1.0, 1.0, 1.0); cr.set_line_width(1.0); cr.stroke()?;
            for (hx, hy) in g.handles { cr.rectangle(hx - 3.5, hy - 3.5, 7.0, 7.0); cr.set_source_rgb(1.0, 1.0, 1.0); cr.fill_preserve()?; cr.set_source_rgb(0.0, 0.0, 0.0); cr.stroke()?; }
        }
    }
    // A marquee or lasso being drawn.
    if let Some(draft) = draft {
        let mut points: Vec<(f64, f64)> = draft.points.iter().map(|p| vp.view_point(*p, size)).collect();
        if draft.kind == DraftKind::Polygonal { if let Some(c) = draft.cursor { points.push(vp.view_point(c, size)); } }
        if let Some(first) = points.first().copied() {
            cr.new_path();
            if draft.kind == DraftKind::Ellipse && points.len() == 4 {
                let (x0, y0, x1, y1) = (points[0].0, points[0].1, points[2].0, points[2].1);
                if x1 > x0 && y1 > y0 {
                    cr.save()?;
                    cr.translate((x0 + x1) / 2.0, (y0 + y1) / 2.0);
                    cr.scale((x1 - x0) / 2.0, (y1 - y0) / 2.0);
                    cr.arc(0.0, 0.0, 1.0, 0.0, std::f64::consts::TAU);
                    cr.restore()?;
                }
            } else {
                cr.move_to(first.0, first.1);
                for p in &points[1..] { cr.line_to(p.0, p.1); }
                if draft.kind == DraftKind::Rectangle { cr.close_path(); }
            }
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.8);
            cr.set_line_width(2.0);
            cr.stroke_preserve()?;
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.set_line_width(1.0);
            cr.stroke()?;
            if draft.kind == DraftKind::Polygonal {
                cr.rectangle(first.0 - 4.0, first.1 - 4.0, 8.0, 8.0);
                cr.set_source_rgb(1.0, 1.0, 1.0);
                cr.fill_preserve()?;
                cr.set_source_rgb(0.0, 0.0, 0.0);
                cr.stroke()?;
            }
        }
    }

    // The brush's outline under the pointer, and Clone Stamp's source crosshair.
    if doc.tool.is_brush() {
        let (px, py) = pointer;
        if px > -1.0e8 {
            let d = doc.brush.diameter * ppp;
            cr.new_path();
            if doc.brush.shaped() {
                // The tip's box, squashed and turned as the tip is.
                let (bw, bh) = doc.brush.preset.as_ref().map_or((1.0, 1.0), |p| { let m = p.width.max(p.height) as f64; (p.width as f64 / m, p.height as f64 / m) });
                cr.save()?;
                cr.translate(px, py);
                cr.rotate(doc.brush.angle.to_radians());
                cr.scale(d / 2.0 * bw, d / 2.0 * bh * doc.brush.roundness.clamp(0.05, 1.0));
                cr.arc(0.0, 0.0, 1.0, 0.0, std::f64::consts::TAU);
                cr.restore()?;
            } else { cr.arc(px, py, d / 2.0, 0.0, std::f64::consts::TAU); }
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.set_line_width(2.5);
            cr.stroke_preserve()?;
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.set_line_width(1.0);
            cr.stroke()?;
        }
        if let (Tool::Clone, Some(source)) = (doc.tool, doc.clone_source) {
            let (sx, sy) = vp.view_point(source, size);
            let reach = 7.0;
            for (color, width) in [((1.0, 1.0, 1.0), 3.0), ((0.0, 0.0, 0.0), 1.0)] {
                cr.new_path();
                cr.move_to(sx - reach, sy); cr.line_to(sx + reach, sy);
                cr.move_to(sx, sy - reach); cr.line_to(sx, sy + reach);
                cr.set_source_rgb(color.0, color.1, color.2);
                cr.set_line_width(width);
                cr.set_line_cap(cairo::LineCap::Round);
                cr.stroke()?;
            }
        }
    }

    // The Pen path: the curve, its anchors as squares, handles as lines with round ends, and while it is
    // being drawn, a rubber band from the last anchor to the pointer.
    if doc.tool == Tool::Pen && !doc.pen.is_empty() && !doc.preview {
        let to_view = |p: (f64, f64)| vp.view_point(p, size);
        cr.save()?;
        cr.set_line_width(1.0);
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.9);
        doc.pen.trace(cr, to_view);
        cr.stroke_preserve()?;
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.6);
        cr.set_dash(&[4.0, 4.0], 0.0);
        cr.stroke()?;
        cr.set_dash(&[], 0.0);
        if !doc.pen_done {
            if let Some(last) = doc.pen.anchors.last() {
                let from = to_view(last.handle_out.unwrap_or(last.point));
                let start = to_view(last.point);
                cr.set_source_rgba(0.3, 0.7, 1.0, 0.8);
                if last.handle_out.is_some() { cr.move_to(start.0, start.1); cr.curve_to(from.0, from.1, pointer.0, pointer.1, pointer.0, pointer.1); } else { cr.move_to(start.0, start.1); cr.line_to(pointer.0, pointer.1); }
                cr.stroke()?;
            }
        }
        for a in &doc.pen.anchors {
            let p = to_view(a.point);
            for h in [a.handle_in, a.handle_out].into_iter().flatten() {
                let v = to_view(h);
                cr.set_source_rgba(0.3, 0.7, 1.0, 0.9);
                cr.move_to(p.0, p.1); cr.line_to(v.0, v.1); cr.stroke()?;
                cr.arc(v.0, v.1, 2.5, 0.0, std::f64::consts::TAU); cr.fill()?;
            }
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.rectangle(p.0.round() - 3.0, p.1.round() - 3.0, 6.0, 6.0);
            cr.fill_preserve()?;
            cr.set_source_rgb(0.1, 0.4, 0.8);
            cr.stroke()?;
        }
        cr.restore()?;
    }
    // Text being typed: a dashed frame around the type layer and the caret.
    if let Some((id, index)) = text_edit {
        if doc.document.has_layer(id) {
            if let Some(style) = doc.document.text_style(id) {
                let t = doc.document.renderer.layer(id).transform;
                cr.save()?;
                cr.set_source_rgba(0.3, 0.7, 1.0, 0.9);
                cr.set_line_width(1.0);
                cr.set_dash(&[4.0, 3.0], 0.0);
                if let Some((rw, rh)) = doc.document.renderer.image_size(id) {
                    // The frame follows the layer's placement: its raster corners through the transform.
                    let to_doc = crate::render::pixel_to_document(&t, rw, rh);
                    let corners = [(0.0, 0.0), (rw as f64, 0.0), (rw as f64, rh as f64), (0.0, rh as f64)].map(|(x, y)| { let d = to_doc.transform_point(x, y); vp.view_point(d, size) });
                    cr.move_to(corners[0].0, corners[0].1);
                    for c in &corners[1..] { cr.line_to(c.0, c.1); }
                    cr.close_path();
                    cr.stroke()?;
                    cr.set_dash(&[], 0.0);
                    let (cx, cy, ch) = crate::text::caret(&style, index);
                    let top = vp.view_point(to_doc.transform_point(cx, cy), size);
                    let bottom = vp.view_point(to_doc.transform_point(cx, cy + ch), size);
                    cr.set_source_rgba(1.0, 1.0, 1.0, 0.95);
                    cr.set_line_width(3.0);
                    cr.move_to(top.0.round() + 0.5, top.1);
                    cr.line_to(bottom.0.round() + 0.5, bottom.1);
                    cr.stroke()?;
                    cr.set_source_rgba(0.0, 0.0, 0.0, 0.95);
                    cr.set_line_width(1.0);
                    cr.move_to(top.0.round() + 0.5, top.1);
                    cr.line_to(bottom.0.round() + 0.5, bottom.1);
                    cr.stroke()?;
                }
                cr.restore()?;
            }
        }
    }
    // Marching ants: the outline in white, then black dashes walking along it.
    if let Some(selection) = doc.document.selection.as_ref().filter(|_| !doc.hide_extras) {
        if !selection.outline.is_empty() {
            cr.save()?;
            cr.rectangle(vx, vy, vw, vh);
            cr.clip();
            cr.new_path();
            let (ox, oy) = outline_offset;
            for outline in selection.outline.iter() {
                for (i, &(x, y)) in outline.iter().enumerate() {
                    let (px, py) = vp.view_point((x as f64 + ox, y as f64 + oy), size);
                    if i == 0 { cr.move_to(px, py); } else { cr.line_to(px, py); }
                }
                cr.close_path();
            }
            cr.set_line_width(1.0);
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.stroke_preserve()?;
            cr.set_dash(&[4.0, 4.0], doc.ants_phase);
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.stroke()?;
            cr.restore()?;
        }
    }
    Ok(())
}

fn intersect(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> Option<(f64, f64, f64, f64)> {
    let x = a.0.max(b.0);
    let y = a.1.max(b.1);
    let right = (a.0 + a.2).min(b.0 + b.2);
    let bottom = (a.1 + a.3).min(b.1 + b.3);
    if right > x && bottom > y { Some((x, y, right - x, bottom - y)) } else { None }
}

/// A box dragged from `anchor` to `point` in whole document pixels: Shift squares it, Alt grows it from the
/// anchor as its center (`DragBox.rect`).
fn drag_box(anchor: (f64, f64), point: (f64, f64), square: bool, from_center: bool) -> (f64, f64, f64, f64) {
    let (mut dx, mut dy) = (point.0.round() - anchor.0, point.1.round() - anchor.1);
    if square { let side = dx.abs().max(dy.abs()); dx = if dx < 0.0 { -side } else { side }; dy = if dy < 0.0 { -side } else { side }; }
    if from_center { (anchor.0 - dx.abs(), anchor.1 - dy.abs(), dx.abs() * 2.0, dy.abs() * 2.0) }
    else { (anchor.0.min(anchor.0 + dx), anchor.1.min(anchor.1 + dy), dx.abs(), dy.abs()) }
}

/// "#rrggbb" as cairo components.
fn hex_rgb(text: &str) -> (f64, f64, f64) {
    let t = text.trim().trim_start_matches('#');
    let v = u32::from_str_radix(t, 16).unwrap_or(0x808080);
    (((v >> 16) & 255) as f64 / 255.0, ((v >> 8) & 255) as f64 / 255.0, (v & 255) as f64 / 255.0)
}

/// The start of the word before byte `i` (Ctrl+Backspace, Ctrl+Left).
fn word_start(text: &str, i: usize) -> usize {
    let head = &text[..i];
    let trimmed = head.trim_end_matches(|c: char| !c.is_alphanumeric());
    let cut = trimmed.trim_end_matches(|c: char| c.is_alphanumeric());
    cut.len()
}

/// The end of the word after byte `i` (Ctrl+Right).
fn word_end(text: &str, i: usize) -> usize {
    let tail = &text[i..];
    let skipped = tail.trim_start_matches(|c: char| !c.is_alphanumeric());
    let after = skipped.trim_start_matches(|c: char| c.is_alphanumeric());
    text.len() - after.len()
}
