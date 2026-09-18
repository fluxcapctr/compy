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
use gtk::{gdk, glib};
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
    options: OptionsBar,
    ants: Cell<Option<glib::SourceId>>,
    painting: Cell<bool>,
    stroke_start: Cell<(f64, f64)>,
    refresh: RefCell<Option<Rc<dyn Fn()>>>,
    /// A transform drag in progress: the drag, the layers it moves and their transforms when it began.
    transform_drag: RefCell<Option<(Drag, Vec<(uuid::Uuid, crate::format::Transform)>)>>,
    /// A marquee or lasso being drawn.
    draft: RefCell<Option<Draft>>,
    /// Dragging a selection outline: its offset so far (document pixels).
    outline_move: Cell<Option<(f64, f64)>>,
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

impl Drop for Canvas {
    fn drop(&mut self) { if let Some(id) = self.ants.take() { id.remove(); } }
}

impl Canvas {
    pub fn new(doc: DocRef, space_held: Rc<Cell<bool>>) -> Rc<Canvas> {
        let area = gtk::DrawingArea::builder().hexpand(true).vexpand(true).focusable(true).build();
        let zoom_label = gtk::Label::builder().label("100%").width_chars(7).xalign(1.0).css_classes(["numeric"]).build();
        let size_label = gtk::Label::builder().css_classes(["dim-label"]).build();
        let message = gtk::Label::builder().css_classes(["dim-label"]).hexpand(true).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
        {
            let d = doc.borrow();
            let (w, h) = d.size();
            size_label.set_label(&format!("{w} × {h} px · {} ppi", d.document.renderer.resolution()));
        }
        let status = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(16).margin_start(12).margin_end(12).margin_top(4).margin_bottom(4).build();
        status.append(&zoom_label);
        status.append(&size_label);
        status.append(&message);
        let options = OptionsBar::new(doc.clone());
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&options.widget);
        column.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        column.append(&area);
        column.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        column.append(&status);
        let widget = gtk::Box::new(gtk::Orientation::Horizontal, 0);

        let canvas = Rc::new_cyclic(|weak: &std::rc::Weak<Canvas>| {
            let weak = weak.clone();
            let rail = ToolRail::new(doc.clone(), Rc::new(move || { if let Some(c) = weak.upgrade() { c.tool_changed(); } }));
            widget.append(&rail.widget);
            widget.append(&gtk::Separator::new(gtk::Orientation::Vertical));
            widget.append(&column);
            Canvas { widget: widget.clone(), area: area.clone(), zoom_label, message, doc, space_held, pointer: Rc::new(Cell::new((-1.0e9, -1.0e9))), dragging: Rc::new(Cell::new(false)), rail, options, ants: Cell::new(None), painting: Cell::new(false), stroke_start: Cell::new((0.0, 0.0)), refresh: RefCell::new(None), transform_drag: RefCell::new(None), draft: RefCell::new(None), outline_move: Cell::new(None) }
        });
        canvas.connect();
        canvas.update_cursor();
        canvas
    }

    pub fn doc(&self) -> &DocRef { &self.doc }

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
            let (draft_ref, outline_ref) = (self.clone(), self.clone());
            area.set_draw_func(move |_, cr, w, h| {
                let start = std::time::Instant::now();
                let mut d = doc.borrow_mut();
                let draft = draft_ref.draft.borrow();
                let offset = outline_ref.outline_move.get().unwrap_or((0.0, 0.0));
                if let Err(error) = draw(&mut d, cr, w as f64, h as f64, pointer.get(), draft.as_ref(), offset) { eprintln!("canvas draw failed: {error:#}"); }
                if trace { eprintln!("frame {:.1} ms at {}", start.elapsed().as_secs_f64() * 1000.0, zoom_text(d.viewport.zoom())); }
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
                if tool.is_brush() { this.area.queue_draw(); }
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

        let click = gtk::GestureClick::new();
        click.set_button(1);
        {
            let this = self.clone();
            click.connect_pressed(move |g, _, x, y| {
                if this.space_held.get() { return; }
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
                    if button == 1 && tool == Tool::Move { if this.begin_transform((x, y), state) { this.stroke_start.set((x, y)); return; } }
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
                else if this.draft.borrow().as_ref().is_some_and(|d| d.kind != DraftKind::Polygonal) { this.finish_draft(); }
                this.dragging.set(false);
                this.update_cursor();
            });
        }
        area.add_controller(drag);
    }

    fn tool_changed(&self) {
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
                t if t.is_brush() => "none",
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
        let id = d.document.active?;
        let layer = d.document.renderer.layer(id);
        if layer.is_group() || !d.document.renderer.has_image(id) { return None; }
        if !crate::format::visible_layers(d.document.renderer.layers()).contains(&id) { return None; }
        let size = d.size();
        let vp = d.viewport;
        Some(Geometry::new(&layer.transform, |p| vp.view_point(p, size)))
    }

    pub fn sync_inspector(&self) { self.options.sync_move(&self.doc); self.options.show_mask_paint(self.doc.borrow().document.mask_target()); }

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
        let mut mode = self.geometry(&d).and_then(|g| g.hit(view));
        let mut target = d.document.active;
        if mode.is_none() {
            let picks = state.contains(gdk::ModifierType::CONTROL_MASK) || d.auto_select;
            let under = crate::format::visible_layers(d.document.renderer.layers()).into_iter().rev()
                .find(|id| d.document.renderer.has_image(*id) && d.document.renderer.layer(*id).transform.contains(point));
            let active_hit = target.is_some_and(|id| { let l = d.document.renderer.layer(id); if l.is_group() { !d.document.transform_members(id).is_empty() } else { d.document.renderer.has_image(id) && l.transform.contains(point) } });
            if picks && under.is_some() && !(active_hit && !state.contains(gdk::ModifierType::CONTROL_MASK)) { target = under; }
            else if !active_hit && target.is_some_and(|id| !d.document.renderer.layer(id).is_group()) && !picks { return false; }
            mode = Some(DragMode::Move);
        }
        let (Some(mode), Some(id)) = (mode, target) else { return false };
        if d.document.active != Some(id) { d.document.active = Some(id); d.document.set_mask_target(false); }
        let members = d.document.transform_members(id);
        if members.is_empty() { return false; }
        let originals: Vec<(uuid::Uuid, crate::format::Transform)> = members.iter().map(|m| (*m, d.document.renderer.layer(*m).transform)).collect();
        let original = if d.document.renderer.layer(id).is_group() {
            // A folder drags as one upright box around its contents; only moving is offered for it.
            let boxes: Vec<_> = originals.iter().map(|(_, t)| t.bounds()).collect();
            let (x0, y0) = (boxes.iter().map(|b| b.0).fold(f64::MAX, f64::min), boxes.iter().map(|b| b.1).fold(f64::MAX, f64::min));
            let (x1, y1) = (boxes.iter().map(|b| b.2).fold(f64::MIN, f64::max), boxes.iter().map(|b| b.3).fold(f64::MIN, f64::max));
            crate::format::Transform { origin: crate::format::Point(x0, y0), size: crate::format::Size((x1 - x0).max(1.0), (y1 - y0).max(1.0)), rotation: 0.0, flip_x: false, flip_y: false, sampling: Default::default() }
        } else { originals[0].1 };
        let mode = if d.document.renderer.layer(id).is_group() { DragMode::Move } else { mode };
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
        // Put the originals back silently, then apply the result as one undo step with mask carry-over.
        let finals: Vec<(uuid::Uuid, crate::format::Transform)> = originals.iter().map(|(id, _)| (*id, d.document.renderer.layer(*id).transform)).collect();
        for (id, original) in &originals { d.document.renderer.set_layer_transform(*id, *original); }
        if finals.iter().zip(&originals).any(|(f, o)| f.1 != o.1) {
            d.document.begin_edit(match drag.mode { DragMode::Move => "Move Layer", DragMode::Rotate => "Rotate Layer", DragMode::Resize(_) => "Scale Layer" });
            for (id, t) in finals { d.document.set_transform(id, t, "Transform Layer"); }
            d.document.end_edit();
        }
        drop(d);
        if let Some(refresh) = self.refresh.borrow().as_ref() { refresh(); }
        self.sync_inspector();
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
            let mask = d.document.mask_target() && matches!(d.tool, Tool::Brush | Tool::Eraser);
            let white = d.mask_paint_white && d.tool == Tool::Brush;
            let warp = match (d.tool, d.blur_mode) { (Tool::Blur, 0) => Some(crate::warp::WarpMode::Liquify), (Tool::Blur, 2) => Some(crate::warp::WarpMode::Smudge), _ => None };
            let mut result = if let Some(mode) = warp { d.document.begin_warp(start, &settings, mode) }
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
        }
        eprintln!("stroke steps: {} points, worst {:.1} ms", points.len() - 1, worst);
        let end = std::time::Instant::now();
        self.finish_stroke();
        eprintln!("stroke finish {:.1} ms", end.elapsed().as_secs_f64() * 1000.0);
    }

    /// Brush keys: [ and ] size, { and } hardness, digits opacity. True when the key was one of those.
    pub fn brush_key(&self, c: char) -> bool {
        let mut d = self.doc.borrow_mut();
        if !d.tool.is_brush() || d.document.stroke_active() { return false; }
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

fn draw(doc: &mut super::Doc, cr: &Context, width: f64, height: f64, pointer: (f64, f64), draft: Option<&Draft>, outline_offset: (f64, f64)) -> Result<()> {
    cr.set_source_rgb(0.105, 0.105, 0.105);
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
            let pixels = new_argb(w, h)?;
            {
                let pcr = Context::new(&pixels)?;
                pcr.translate(-x0, -y0);
                pcr.rectangle(x0, y0, w as f64, h as f64);
                pcr.clip();
                doc.document.renderer.draw(&pcr)?;
            }
            cr.translate(rx, ry);
            cr.scale(ppp, ppp);
            cr.set_source_surface(&pixels, x0, y0)?;
            cr.source().set_filter(Filter::Nearest);
            cr.rectangle(x0, y0, w as f64, h as f64);
            cr.fill()?;
        }
    } else {
        cr.translate(rx, ry);
        cr.scale(ppp, ppp);
        cr.rectangle(0.0, 0.0, size.0, size.1);
        cr.clip();
        doc.document.renderer.draw(cr)?;
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

    // The Move tool's transform box and handles, and the guides a snapped move met.
    if doc.tool == Tool::Move {
        if let Some(id) = doc.document.active {
            let layer = doc.document.renderer.layer(id).clone();
            let visible = crate::format::visible_layers(doc.document.renderer.layers()).contains(&id);
            let boxed = if layer.is_group() {
                let members = doc.document.transform_members(id);
                if members.is_empty() { None } else {
                    let boxes: Vec<_> = members.iter().map(|m| doc.document.renderer.layer(*m).transform.bounds()).collect();
                    let (x0, y0) = (boxes.iter().map(|b| b.0).fold(f64::MAX, f64::min), boxes.iter().map(|b| b.1).fold(f64::MAX, f64::min));
                    let (x1, y1) = (boxes.iter().map(|b| b.2).fold(f64::MIN, f64::max), boxes.iter().map(|b| b.3).fold(f64::MIN, f64::max));
                    Some(crate::format::Transform { origin: crate::format::Point(x0, y0), size: crate::format::Size(x1 - x0, y1 - y0), rotation: 0.0, flip_x: false, flip_y: false, sampling: Default::default() })
                }
            } else if visible && doc.document.renderer.has_image(id) { Some(layer.transform) } else { None };
            if let Some(t) = boxed {
                let g = Geometry::new(&t, |p| vp.view_point(p, size));
                cr.new_path();
                cr.move_to(g.handles[0].0, g.handles[0].1);
                for i in [2, 4, 6] { cr.line_to(g.handles[i].0, g.handles[i].1); }
                cr.close_path();
                if !layer.is_group() { cr.move_to(g.handles[1].0, g.handles[1].1); cr.line_to(g.rotation.0, g.rotation.1); }
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.7);
                cr.set_line_width(3.0);
                cr.stroke_preserve()?;
                cr.set_source_rgb(0.21, 0.52, 0.89);
                cr.set_line_width(1.0);
                cr.stroke()?;
                for (x, y) in g.handles {
                    cr.rectangle(x - 3.5, y - 3.5, 7.0, 7.0);
                    cr.set_source_rgb(1.0, 1.0, 1.0);
                    cr.fill_preserve()?;
                    cr.set_source_rgb(0.21, 0.52, 0.89);
                    cr.stroke()?;
                }
                if !layer.is_group() {
                    cr.arc(g.rotation.0, g.rotation.1, 4.0, 0.0, std::f64::consts::TAU);
                    cr.set_source_rgb(1.0, 1.0, 1.0);
                    cr.fill_preserve()?;
                    cr.set_source_rgb(0.21, 0.52, 0.89);
                    cr.stroke()?;
                }
            }
        }
        cr.set_source_rgb(0.21, 0.52, 0.89);
        cr.set_line_width(1.0);
        if let Some(x) = doc.snap_guides.0 { let (vx0, _) = vp.view_point((x, 0.0), size); cr.move_to(vx0, 0.0); cr.line_to(vx0, height); cr.stroke()?; }
        if let Some(y) = doc.snap_guides.1 { let (_, vy0) = vp.view_point((0.0, y), size); cr.move_to(0.0, vy0); cr.line_to(width, vy0); cr.stroke()?; }
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
            cr.arc(px, py, d / 2.0, 0.0, std::f64::consts::TAU);
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

    // Marching ants: the outline in white, then black dashes walking along it.
    if let Some(selection) = &doc.document.selection {
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
