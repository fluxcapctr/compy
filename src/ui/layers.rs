//! The layers panel: blend and opacity controls for the selected layer above the layer tree, top layer
//! first, each row with its visibility toggle, folder disclosure, thumbnail, name and details.

use super::DocRef;
use crate::document::Place;
use crate::format::BlendMode;
use gtk::gio;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use uuid::Uuid;

pub const WIDTH: i32 = 317;
const ROW_HEIGHT: i32 = 44;
const THUMBNAIL: i32 = 34;
const MASK_THUMBNAIL: i32 = 26;
const DISCLOSURE: i32 = 20;

// The layers being dragged, from whichever panel they started in, so a drop on another project's panel can
// copy them across. GTK carries only a marker string.
thread_local! { static DRAG: RefCell<Option<(DocRef, Vec<Uuid>)>> = const { RefCell::new(None) }; }
const LAYER_DRAG: &str = "compositor-layers";

pub struct LayersPanel {
    pub widget: gtk::Box,
    inner: Rc<Inner>,
}

struct Inner {
    doc: DocRef,
    canvas: gtk::DrawingArea,
    list: gtk::ListBox,
    blend: gtk::DropDown,
    opacity: gtk::Entry,
    percent: gtk::Label,
    count: gtk::Label,
    /// Layer ids by row index, as the list shows them.
    rows: RefCell<Vec<Uuid>>,
    details: RefCell<HashMap<Uuid, gtk::Label>>,
    /// Set while the controls are being synced from the document, so their signals don't write back.
    syncing: Cell<bool>,
    on_select: RefCell<Option<Rc<dyn Fn()>>>,
}

impl LayersPanel {
    pub fn new(doc: DocRef, canvas: gtk::DrawingArea) -> Rc<LayersPanel> {
        let widget = gtk::Box::builder().orientation(gtk::Orientation::Vertical).width_request(WIDTH).css_classes(["layers-panel"]).build();

        // A tab strip like Photoshop's panel header, with the layer count where a second tab would sit.
        let header = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).css_classes(["panel-tabs"]).build();
        header.append(&gtk::Label::builder().label("Layers").css_classes(["panel-tab", "current"]).xalign(0.0).build());
        let count = gtk::Label::builder().css_classes(["panel-tab", "dim-label", "numeric"]).build();
        header.append(&count);
        widget.append(&header);

        // Blend mode and opacity on one row.
        let controls = gtk::Grid::builder().column_spacing(6).margin_start(8).margin_end(8).margin_top(6).margin_bottom(6).build();
        let names: Vec<&str> = BlendMode::ALL.iter().map(|m| m.name()).collect();
        let blend = gtk::DropDown::from_strings(&names);
        blend.set_hexpand(true);
        // A typed field, as Photoshop's: Return or leaving it applies, the wheel steps it.
        let opacity = gtk::Entry::builder().width_chars(3).max_length(3).xalign(1.0).input_purpose(gtk::InputPurpose::Digits).tooltip_text("Opacity, percent (type a value, or scroll over it)").build();
        let percent = gtk::Label::builder().label("%").css_classes(["dim-label"]).build();
        controls.attach(&blend, 0, 0, 1, 1);
        controls.attach(&gtk::Label::builder().label("Opacity:").css_classes(["caption", "dim-label"]).build(), 1, 0, 1, 1);
        controls.attach(&opacity, 2, 0, 1, 1);
        controls.attach(&percent, 3, 0, 1, 1);
        widget.append(&controls);
        widget.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).css_classes(["navigation-sidebar"]).build();
        let scroller = gtk::ScrolledWindow::builder().child(&list).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
        widget.append(&scroller);
        widget.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        // The footer: new layer, folder, mask, adjustment, and delete, as the Mac's panel has.
        // The footer, right-aligned as Photoshop's: link, mask, adjustment, folder, new layer, trash.
        let footer = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(0).margin_start(6).margin_end(6).margin_top(2).margin_bottom(2).css_classes(["layers-footer"]).build();
        footer.append(&gtk::Box::builder().hexpand(true).build());
        for (glyph, action, tip) in [("fx", "win.layer-style", "Layer Style: drop shadow, glow, bevel, stroke, overlay"), ("link", "win.toggle-clipping", "Clip to the layer below (Alt+G)"), ("mask", "win.mask-reveal", "Add a layer mask (from the selection, if any)")] {
            let b = gtk::Button::builder().has_frame(false).action_name(action).tooltip_text(tip).build();
            b.set_child(Some(&super::icons::glyph(glyph, 16)));
            footer.append(&b);
        }
        let adjustments = gio::Menu::new();
        for kind in ["Hue/Saturation", "Levels", "Curves", "Exposure", "Gradient Map", "Grain"] { adjustments.append(Some(kind), Some(&format!("win.new-adjustment::{kind}"))); }
        let adjust = gtk::MenuButton::builder().has_frame(false).menu_model(&adjustments).tooltip_text("New adjustment layer").build();
        adjust.set_child(Some(&super::icons::glyph("adjustment", 16)));
        footer.append(&adjust);
        for (glyph, action, tip) in [("new-folder", "win.new-folder", "New folder (Ctrl+G)"), ("new-layer", "win.new-layer", "New blank layer (Ctrl+Shift+N)"), ("trash", "win.delete-layer", "Delete the selected layers")] {
            let b = gtk::Button::builder().has_frame(false).action_name(action).tooltip_text(tip).build();
            b.set_child(Some(&super::icons::glyph(glyph, 16)));
            footer.append(&b);
        }
        widget.append(&footer);

        let inner = Rc::new(Inner { doc, canvas, list, blend, opacity, percent, count, rows: RefCell::new(Vec::new()), details: RefCell::new(HashMap::new()), syncing: Cell::new(false), on_select: RefCell::new(None) });
        inner.connect();
        inner.rebuild();
        Rc::new(LayersPanel { widget, inner })
    }

    pub fn rebuild(&self) { self.inner.rebuild(); }
    pub fn set_on_select(&self, f: Rc<dyn Fn()>) { *self.inner.on_select.borrow_mut() = Some(f); }
}

impl Inner {
    fn connect(self: &Rc<Self>) {
        let this = self.clone();
        self.list.connect_row_selected(move |_, row| {
            if this.syncing.get() { return; }
            let selected = row.and_then(|r| this.rows.borrow().get(r.index() as usize).copied());
            if let Ok(mut d) = this.doc.try_borrow_mut() {
                if d.document.active != selected || d.document.selected.len() > 1 { d.document.select_layer(selected); }
            }
            this.sync_controls();
            if let Some(f) = this.on_select.borrow().as_ref() { f(); }
        });
        let this = self.clone();
        self.blend.connect_selected_notify(move |dropdown| {
            if this.syncing.get() { return; }
            let Some(mode) = BlendMode::ALL.get(dropdown.selected() as usize).copied() else { return };
            let mut d = this.doc.borrow_mut();
            let Some(id) = d.document.active else { return };
            d.document.set_blend_mode(id, mode);
            drop(d);
            this.refresh_detail(id);
            this.canvas.queue_draw();
        });
        let apply = { let this = self.clone(); Rc::new(move |entry: &gtk::Entry| {
            if this.syncing.get() { return; }
            let Ok(value) = entry.text().trim().trim_end_matches('%').parse::<f64>() else { this.sync_controls(); return };
            let value = value.clamp(0.0, 100.0);
            let mut d = this.doc.borrow_mut();
            let Some(id) = d.document.active else { return };
            d.document.set_opacity(id, value / 100.0);
            drop(d);
            this.sync_controls();
            this.refresh_detail(id);
            this.canvas.queue_draw();
        }) };
        { let apply = apply.clone(); self.opacity.connect_activate(move |e| apply(e)); }
        { let (apply, entry) = (apply.clone(), self.opacity.clone()); let focus = gtk::EventControllerFocus::new(); focus.connect_leave(move |_| apply(&entry)); self.opacity.add_controller(focus); }
        {
            let (apply, entry) = (apply.clone(), self.opacity.clone());
            let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
            scroll.connect_scroll(move |_, _, dy| {
                let current = entry.text().trim().parse::<f64>().unwrap_or(100.0);
                entry.set_text(&format!("{}", (current - dy.signum()).clamp(0.0, 100.0)));
                apply(&entry);
                gtk::glib::Propagation::Stop
            });
            self.opacity.add_controller(scroll);
        }
    }

    /// Rebuilds every row from the document. Rows inside collapsed folders are left out.
    fn rebuild(self: &Rc<Self>) {
        struct RowInfo { id: Uuid, depth: usize, visible: bool, own_visible: bool, group: bool, collapsed: bool, name: String, detail: String, thumbnail: Option<cairo::ImageSurface>, kind: &'static str, mask: Option<cairo::ImageSurface>, mask_enabled: bool, mask_target: bool, clipped: bool }
        let (infos, selected, total, multi) = {
            let mut d = self.doc.borrow_mut();
            let mut infos = Vec::new();
            let mut hidden_below: Option<usize> = None;
            for (id, depth, visible) in d.document.renderer.rows() {
                if let Some(limit) = hidden_below { if depth > limit { continue; } hidden_below = None; }
                let layer = d.document.renderer.layer(id).clone();
                let group = layer.is_group();
                let collapsed = d.collapsed.contains(&id);
                if group && collapsed { hidden_below = Some(depth); }
                let thumbnail = if group || layer.adjustment.is_some() { None } else { d.document.renderer.thumbnail(id, THUMBNAIL).ok().flatten() };
                let kind = if group { "folder" } else if layer.adjustment.is_some() { "adjustment" } else { "new-layer" };
                let mask = d.document.renderer.mask_thumbnail(id, MASK_THUMBNAIL).ok().flatten();
                let mask_target = d.document.mask_target() && d.document.active == Some(id);
                infos.push(RowInfo { id, depth, visible, own_visible: layer.is_visible, group, collapsed, name: layer.name.clone(), detail: detail_text(&d.document.renderer, id), thumbnail, kind, mask, mask_enabled: layer.mask_enabled(), mask_target, clipped: layer.mask_source_id.is_some() });
            }
            (infos, d.document.active, d.document.renderer.layers().len(), d.document.selected.clone())
        };
        self.count.set_label(&total.to_string());
        // Rows come and go here without that meaning a click: the selection signal is ignored meanwhile.
        self.syncing.set(true);
        while let Some(child) = self.list.first_child() { self.list.remove(&child); }
        self.details.borrow_mut().clear();
        let mut ids = Vec::with_capacity(infos.len());
        let mut select_index = None;
        for (index, info) in infos.into_iter().enumerate() {
            if Some(info.id) == selected { select_index = Some(index); }
            ids.push(info.id);
            let row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(4).height_request(ROW_HEIGHT).margin_start(2).margin_end(4).build();
            row.append(&gtk::Box::builder().width_request((info.depth * 12 + if info.clipped { 12 } else { 0 }) as i32).build());

            // The eye: shown when the layer is visible, an empty well when hidden.
            let eye = gtk::Button::builder().has_frame(false).tooltip_text("Show or hide this layer").valign(gtk::Align::Center).width_request(24).height_request(24).css_classes(["eye"]).build();
            if info.own_visible { eye.set_child(Some(&super::icons::glyph("eye", 16))); } else { eye.set_child(Some(&gtk::Box::builder().width_request(16).height_request(16).build())); }
            {
                let this = self.clone();
                let (id, visible) = (info.id, info.own_visible);
                eye.connect_clicked(move |_| {
                    this.doc.borrow_mut().document.set_visible(id, !visible);
                    this.canvas.queue_draw();
                    this.rebuild();
                });
            }
            row.append(&eye);

            if info.group {
                let disclosure = gtk::Button::builder().has_frame(false).valign(gtk::Align::Center).width_request(DISCLOSURE).height_request(DISCLOSURE).css_classes(["flat"]).build();
                disclosure.set_child(Some(&super::icons::glyph(if info.collapsed { "triangle-right" } else { "triangle-down" }, 12)));
                let this = self.clone();
                let id = info.id;
                disclosure.connect_clicked(move |_| {
                    { let mut d = this.doc.borrow_mut(); if !d.collapsed.remove(&id) { d.collapsed.insert(id); } }
                    this.rebuild();
                });
                row.append(&disclosure);
            } else {
                row.append(&gtk::Box::builder().width_request(DISCLOSURE).build());
            }

            let slot = gtk::Box::builder().width_request(THUMBNAIL).height_request(THUMBNAIL).valign(gtk::Align::Center).halign(gtk::Align::Start).build();
            let image_target = !info.mask_target && !info.group;
            match info.thumbnail {
                Some(surface) => {
                    let area = gtk::DrawingArea::builder().content_width(THUMBNAIL).content_height(THUMBNAIL).tooltip_text("Click to edit the layer's pixels; Ctrl-click to select them").build();
                    {
                        let (this, id) = (self.clone(), info.id);
                        let click = gtk::GestureClick::new();
                        click.connect_pressed(move |g, _, _, _| {
                            let ctrl = g.current_event_state().contains(gtk::gdk::ModifierType::CONTROL_MASK);
                            { let mut d = this.doc.borrow_mut(); d.document.select_layer(Some(id)); if ctrl { let _ = d.document.select_layer_pixels(id, crate::selection::Mode::Replace); } }
                            this.canvas.queue_draw();
                            this.rebuild();
                        });
                        area.add_controller(click);
                    }
                    area.set_draw_func(move |_, cr, w, h| {
                        let (w, h) = (w as f64, h as f64);
                        cr.set_source_rgb(0.30, 0.30, 0.30);
                        cr.paint().ok();
                        cr.set_source_rgb(0.38, 0.38, 0.38);
                        for row in 0..(h / 6.0).ceil() as i32 { for col in 0..(w / 6.0).ceil() as i32 { if (row + col) % 2 == 0 { cr.rectangle(col as f64 * 6.0, row as f64 * 6.0, 6.0, 6.0); } } }
                        cr.fill().ok();
                        let (sw, sh) = (surface.width() as f64, surface.height() as f64);
                        cr.set_source_surface(&surface, ((w - sw) / 2.0).floor(), ((h - sh) / 2.0).floor()).ok();
                        cr.paint().ok();
                        if image_target { cr.set_source_rgb(0.21, 0.52, 0.89); cr.set_line_width(2.0); cr.rectangle(1.0, 1.0, w - 2.0, h - 2.0); }
                        else { cr.set_source_rgba(1.0, 1.0, 1.0, 0.15); cr.set_line_width(1.0); cr.rectangle(0.5, 0.5, w - 1.0, h - 1.0); }
                        cr.stroke().ok();
                    });
                    slot.append(&area);
                }
                None => {
                    let icon = super::icons::glyph(info.kind, 20);
                    icon.add_css_class("dim-label");
                    slot.append(&icon);
                }
            }
            row.append(&slot);
            if let Some(mask) = info.mask {
                let (enabled, target) = (info.mask_enabled, info.mask_target);
                let area = gtk::DrawingArea::builder().content_width(MASK_THUMBNAIL).content_height(MASK_THUMBNAIL).valign(gtk::Align::Center)
                    .tooltip_text("Click to paint the mask (black hides, white reveals)").build();
                {
                    let (this, id) = (self.clone(), info.id);
                    let click = gtk::GestureClick::new();
                    click.connect_pressed(move |g, _, _, _| {
                        let ctrl = g.current_event_state().contains(gtk::gdk::ModifierType::CONTROL_MASK);
                        { let mut d = this.doc.borrow_mut(); d.document.select_layer(Some(id)); d.document.set_mask_target(true); if ctrl { let _ = d.document.select_mask_pixels(id, crate::selection::Mode::Replace); } }
                        this.canvas.queue_draw();
                        this.rebuild();
                    });
                    area.add_controller(click);
                }
                area.set_draw_func(move |_, cr, w, h| {
                    let (w, h) = (w as f64, h as f64);
                    let (sw, sh) = (mask.width() as f64, mask.height() as f64);
                    cr.set_source_rgb(0.0, 0.0, 0.0);
                    cr.paint().ok();
                    cr.save().ok();
                    cr.translate(((w - sw) / 2.0).floor(), ((h - sh) / 2.0).floor());
                    cr.set_source_rgb(1.0, 1.0, 1.0);
                    cr.mask_surface(&mask, 0.0, 0.0).ok();
                    cr.restore().ok();
                    if !enabled {
                        cr.set_source_rgb(0.9, 0.2, 0.2);
                        cr.set_line_width(2.5);
                        cr.move_to(w * 0.75, h * 0.15); cr.line_to(w * 0.25, h * 0.85);
                        cr.stroke().ok();
                    }
                    if target { cr.set_source_rgb(0.21, 0.52, 0.89); cr.set_line_width(2.0); cr.rectangle(1.0, 1.0, w - 2.0, h - 2.0); }
                    else { cr.set_source_rgba(1.0, 1.0, 1.0, 0.25); cr.set_line_width(1.0); cr.rectangle(0.5, 0.5, w - 1.0, h - 1.0); }
                    cr.stroke().ok();
                });
                row.append(&area);
            }

            let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).valign(gtk::Align::Center).hexpand(true).build();
            let name = gtk::Label::builder().label(&info.name).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
            let detail = gtk::Label::builder().label(&info.detail).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).css_classes(["dim-label", "caption"]).build();
            text.append(&name);
            text.append(&detail);
            self.details.borrow_mut().insert(info.id, detail);
            row.append(&text);
            row.set_opacity(if info.visible { 1.0 } else { 0.45 });

            let list_row = gtk::ListBoxRow::builder().child(&row).build();
            if multi.contains(&info.id) && Some(info.id) != selected { list_row.add_css_class("multi"); }
            self.connect_drag(&list_row, info.id, info.group);
            {
                // Double-click on the name edits it in place.
                let (this, id, text, name_label) = (self.clone(), info.id, text.clone(), name.clone());
                let click = gtk::GestureClick::new();
                click.connect_pressed(move |g, n, _, _| {
                    if n != 2 { return; }
                    g.set_state(gtk::EventSequenceState::Claimed);
                    let entry = gtk::Entry::builder().text(name_label.label().as_str()).hexpand(true).build();
                    text.remove(&name_label);
                    text.prepend(&entry);
                    entry.grab_focus();
                    entry.select_region(0, -1);
                    { let this = this.clone(); entry.connect_activate(move |e| { this.doc.borrow_mut().document.rename_layer(id, &e.text()); this.rebuild(); }); }
                    { let this = this.clone(); let focus = gtk::EventControllerFocus::new(); focus.connect_leave(move |_| { let this = this.clone(); gtk::glib::idle_add_local_once(move || this.rebuild()); }); entry.add_controller(focus); }
                    { let this = this.clone(); let keys = gtk::EventControllerKey::new(); keys.connect_key_pressed(move |_, key, _, _| { if key == gtk::gdk::Key::Escape { this.rebuild(); gtk::glib::Propagation::Stop } else { gtk::glib::Propagation::Proceed } }); entry.add_controller(keys); }
                });
                name.add_controller(click);
            }
            {
                let (this, id) = (self.clone(), info.id);
                let click = gtk::GestureClick::new();
                click.set_propagation_phase(gtk::PropagationPhase::Capture);
                click.connect_pressed(move |g, n, _, _| {
                    if n == 2 {
                        // Adjustment layers open their settings; other rows rename in place (on the name).
                        let is_adjustment = this.doc.borrow().document.renderer.layer(id).adjustment.is_some();
                        if !is_adjustment { return; }
                        this.doc.borrow_mut().document.select_layer(Some(id));
                        if let Some(window) = this.list.root().and_downcast::<gtk::ApplicationWindow>() {
                            if let Some(action) = gtk::prelude::ActionMapExt::lookup_action(&window, "edit-adjustment") { action.activate(None); }
                        }
                        return;
                    }
                    let state = g.current_event_state();
                    // Ctrl adds to or removes from the selection, Shift extends it to this row.
                    if state.intersects(gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK) {
                        g.set_state(gtk::EventSequenceState::Claimed);
                        let (ctrl, shift) = (state.contains(gtk::gdk::ModifierType::CONTROL_MASK), state.contains(gtk::gdk::ModifierType::SHIFT_MASK));
                        {
                            let mut d = this.doc.borrow_mut();
                            // Ctrl-click: the layer's pixels become the selection (ants around what is drawn on it);
                            // Shift-click extends the layer selection; Ctrl+Shift-click toggles a layer in it.
                            if ctrl && shift { d.document.toggle_layer_selected(id); }
                            else if shift { d.document.select_layer_range(id); }
                            else { d.document.select_layer(Some(id)); let _ = d.document.select_layer_pixels(id, crate::selection::Mode::Replace); }
                        }
                        this.canvas.queue_draw();
                        this.rebuild();
                        if let Some(f) = this.on_select.borrow().as_ref() { f(); }
                        return;
                    }
                    if !state.contains(gtk::gdk::ModifierType::ALT_MASK) { return; }
                    g.set_state(gtk::EventSequenceState::Claimed);
                    this.doc.borrow_mut().document.toggle_clipping(id);
                    this.canvas.queue_draw();
                    this.rebuild();
                });
                list_row.add_controller(click);
            }
            self.list.append(&list_row);
        }
        *self.rows.borrow_mut() = ids;
        match select_index {
            Some(index) => { if let Some(row) = self.list.row_at_index(index as i32) { self.list.select_row(Some(&row)); } }
            None => {}
        }
        self.syncing.set(false);
        self.sync_controls();
    }

    fn refresh_detail(&self, id: Uuid) {
        let text = detail_text(&self.doc.borrow().document.renderer, id);
        if let Some(label) = self.details.borrow().get(&id) { label.set_label(&text); }
    }

    /// Points the blend and opacity controls at the selected layer, or disables them.
    /// Rows drag (the selection when the row is part of it) and take drops: above, below, or into a folder.
    /// A drop from another project's panel copies the layers; Ctrl while dropping copies within one.
    fn connect_drag(self: &Rc<Self>, row: &gtk::ListBoxRow, id: Uuid, group: bool) {
        let source = gtk::DragSource::builder().actions(gtk::gdk::DragAction::MOVE | gtk::gdk::DragAction::COPY).build();
        {
            let this = self.clone();
            source.connect_prepare(move |_, _, _| {
                let ids = { let d = this.doc.borrow(); if d.document.selected.contains(&id) && d.document.selected.len() > 1 { d.document.renderer.layers().iter().filter(|l| d.document.selected.contains(&l.id)).map(|l| l.id).collect() } else { vec![id] } };
                DRAG.with(|s| *s.borrow_mut() = Some((this.doc.clone(), ids)));
                Some(gtk::gdk::ContentProvider::for_value(&LAYER_DRAG.to_value()))
            });
        }
        row.add_controller(source);
        let target = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE | gtk::gdk::DragAction::COPY);
        {
            let moved = row.clone();
            target.connect_motion(move |t, _, y| {
                for c in ["drop-above", "drop-below", "drop-into"] { moved.remove_css_class(c); }
                moved.add_css_class(zone(group, y, moved.height() as f64));
                if t.current_drop().is_some_and(|d| d.actions().contains(gtk::gdk::DragAction::COPY) && !d.actions().contains(gtk::gdk::DragAction::MOVE)) { gtk::gdk::DragAction::COPY } else { gtk::gdk::DragAction::MOVE }
            });
            let left = row.clone();
            target.connect_leave(move |_| { for c in ["drop-above", "drop-below", "drop-into"] { left.remove_css_class(c); } });
        }
        {
            let (this, row) = (self.clone(), row.clone());
            target.connect_drop(move |t, value, _, y| {
                for c in ["drop-above", "drop-below", "drop-into"] { row.remove_css_class(c); }
                if value.get::<String>().ok().as_deref() != Some(LAYER_DRAG) { return false; }
                let Some((from, ids)) = DRAG.with(|s| s.borrow_mut().take()) else { return false };
                let place = match zone(group, y, row.height() as f64) { "drop-above" => Place::Above(id), "drop-below" => Place::Below(id), _ => Place::Into(id) };
                let copy = t.current_drop().is_some_and(|d| d.actions() == gtk::gdk::DragAction::COPY);
                let result = if Rc::ptr_eq(&from, &this.doc) {
                    let mut d = this.doc.borrow_mut();
                    if copy { d.document.copy_layers_within(&ids, place).map(|_| ()) } else { d.document.move_layers(&ids, place) }
                } else {
                    let src = from.borrow();
                    let mut d = this.doc.borrow_mut();
                    d.document.copy_layers(&src.document, &ids, place).map(|_| ())
                };
                if let Err(error) = result { if let Some(window) = this.list.root().and_downcast::<gtk::Window>() { gtk::AlertDialog::builder().message("Could not move the layers").detail(format!("{error:#}")).modal(true).build().show(Some(&window)); } }
                this.canvas.queue_draw();
                this.rebuild();
                if let Some(f) = this.on_select.borrow().as_ref() { f(); }
                true
            });
        }
        row.add_controller(target);
    }

    fn sync_controls(&self) {
        let d = self.doc.borrow();
        let layer = d.document.active.map(|id| d.document.renderer.layer(id).clone());
        let editable = layer.as_ref().is_some_and(|l| !l.is_group());
        self.syncing.set(true);
        self.blend.set_sensitive(editable);
        self.opacity.set_sensitive(editable);
        if let Some(layer) = &layer {
            let mode = BlendMode::ALL.iter().position(|m| *m == layer.blend_mode()).unwrap_or(0) as u32;
            self.blend.set_selected(mode);
            self.opacity.set_text(&format!("{}", (layer.opacity() * 100.0).round()));
        } else {
            self.blend.set_selected(0);
            self.opacity.set_text("100");
            self.percent.set_label("%");
        }
        self.syncing.set(false);
    }
}

fn detail_text(renderer: &crate::render::Renderer, id: Uuid) -> String {
    let layer = renderer.layer(id);
    let mut parts = Vec::new();
    if let Some((w, h)) = renderer.image_size(id) { parts.push(format!("{w}×{h}")); }
    if layer.is_group() { parts.push("Folder".into()); }
    if let Some(a) = &layer.adjustment { parts.push(a.kind.clone()); }
    if layer.blend_mode() != BlendMode::Normal { parts.push(layer.blend_mode().name().into()); }
    if layer.opacity() != 1.0 { parts.push(format!("{:.0}%", layer.opacity() * 100.0)); }
    if layer.mask_file.is_some() { parts.push(if layer.mask_enabled() { "Mask".into() } else { "Mask off".into() }); }
    if layer.effects.as_ref().and_then(crate::effects::Effects::from_record).is_some_and(|e| e.is_active()) { parts.push("fx".into()); }
    if layer.mask_source_id.is_some() { parts.push("Clipped".into()); }
    parts.join(" · ")
}

/// Which part of a row the pointer is over: the top or bottom edge places beside it; a folder's middle
/// places inside it.
fn zone(group: bool, y: f64, height: f64) -> &'static str {
    let frac = if height > 0.0 { y / height } else { 0.5 };
    if group { if frac < 0.25 { "drop-above" } else if frac > 0.75 { "drop-below" } else { "drop-into" } }
    else if frac < 0.5 { "drop-above" } else { "drop-below" }
}
