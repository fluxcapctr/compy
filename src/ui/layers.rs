//! The layers panel: blend and opacity controls for the selected layer above the layer tree, top layer
//! first, each row with its visibility toggle, folder disclosure, thumbnail, name and details.

use super::DocRef;
use crate::format::BlendMode;
use gtk::gio;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use uuid::Uuid;

pub const WIDTH: i32 = 252;
const ROW_HEIGHT: i32 = 52;
const THUMBNAIL: i32 = 36;
const MASK_THUMBNAIL: i32 = 30;
const DISCLOSURE: i32 = 28;

pub struct LayersPanel {
    pub widget: gtk::Box,
    inner: Rc<Inner>,
}

struct Inner {
    doc: DocRef,
    canvas: gtk::DrawingArea,
    list: gtk::ListBox,
    blend: gtk::DropDown,
    opacity: gtk::Scale,
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

        let header = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).margin_start(18).margin_end(18).margin_top(14).margin_bottom(14).build();
        header.append(&gtk::Label::builder().label("Layers").css_classes(["heading"]).hexpand(true).xalign(0.0).build());
        let count = gtk::Label::builder().css_classes(["dim-label", "numeric"]).build();
        header.append(&count);
        widget.append(&header);
        widget.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let controls = gtk::Grid::builder().row_spacing(6).column_spacing(8).margin_start(12).margin_end(12).margin_top(10).margin_bottom(10).build();
        let names: Vec<&str> = BlendMode::ALL.iter().map(|m| m.name()).collect();
        let blend = gtk::DropDown::from_strings(&names);
        blend.set_hexpand(true);
        let opacity = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
        opacity.set_draw_value(false);
        opacity.set_hexpand(true);
        let percent = gtk::Label::builder().label("100%").width_chars(5).xalign(1.0).css_classes(["numeric"]).build();
        controls.attach(&gtk::Label::builder().label("Blend").xalign(0.0).css_classes(["caption"]).build(), 0, 0, 1, 1);
        controls.attach(&blend, 1, 0, 2, 1);
        controls.attach(&gtk::Label::builder().label("Opacity").xalign(0.0).css_classes(["caption"]).build(), 0, 1, 1, 1);
        controls.attach(&opacity, 1, 1, 1, 1);
        controls.attach(&percent, 2, 1, 1, 1);
        widget.append(&controls);
        widget.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).css_classes(["navigation-sidebar"]).build();
        let scroller = gtk::ScrolledWindow::builder().child(&list).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
        widget.append(&scroller);
        widget.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        // The footer: new layer, folder, mask, adjustment, and delete, as the Mac's panel has.
        let footer = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(2).margin_start(8).margin_end(8).margin_top(4).margin_bottom(4).css_classes(["layers-footer"]).build();
        for (icon, action, tip) in [("list-add-symbolic", "win.new-layer", "New blank layer (Ctrl+Shift+N)"), ("folder-new-symbolic", "win.new-folder", "New folder (Ctrl+G)"), ("image-x-generic-symbolic", "win.mask-reveal", "Add mask (from the selection, if any)")] {
            footer.append(&gtk::Button::builder().icon_name(icon).has_frame(false).action_name(action).tooltip_text(tip).build());
        }
        let adjustments = gio::Menu::new();
        for kind in ["Hue/Saturation", "Levels", "Curves", "Exposure", "Gradient Map", "Grain"] { adjustments.append(Some(kind), Some(&format!("win.new-adjustment::{kind}"))); }
        footer.append(&gtk::MenuButton::builder().icon_name("color-select-symbolic").has_frame(false).menu_model(&adjustments).tooltip_text("New adjustment layer").build());
        footer.append(&gtk::Box::builder().hexpand(true).build());
        footer.append(&gtk::Button::builder().icon_name("user-trash-symbolic").has_frame(false).action_name("win.delete-layer").tooltip_text("Delete the selected layer").build());
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
            let selected = row.and_then(|r| this.rows.borrow().get(r.index() as usize).copied());
            if let Ok(mut d) = this.doc.try_borrow_mut() {
                if d.document.active != selected { d.document.active = selected; d.document.set_mask_target(false); }
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
        let this = self.clone();
        self.opacity.connect_value_changed(move |scale| {
            if this.syncing.get() { return; }
            let value = scale.value();
            this.percent.set_label(&format!("{value:.0}%"));
            let mut d = this.doc.borrow_mut();
            let Some(id) = d.document.active else { return };
            d.document.set_opacity(id, value / 100.0);
            drop(d);
            this.refresh_detail(id);
            this.canvas.queue_draw();
        });
    }

    /// Rebuilds every row from the document. Rows inside collapsed folders are left out.
    fn rebuild(self: &Rc<Self>) {
        struct RowInfo { id: Uuid, depth: usize, visible: bool, own_visible: bool, group: bool, collapsed: bool, name: String, detail: String, thumbnail: Option<cairo::ImageSurface>, kind: &'static str, mask: Option<cairo::ImageSurface>, mask_enabled: bool, mask_target: bool, clipped: bool }
        let (infos, selected, total) = {
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
                let kind = if group { "folder-symbolic" } else if layer.adjustment.is_some() { "color-select-symbolic" } else { "image-x-generic-symbolic" };
                let mask = d.document.renderer.mask_thumbnail(id, MASK_THUMBNAIL).ok().flatten();
                let mask_target = d.document.mask_target() && d.document.active == Some(id);
                infos.push(RowInfo { id, depth, visible, own_visible: layer.is_visible, group, collapsed, name: layer.name.clone(), detail: detail_text(&d.document.renderer, id), thumbnail, kind, mask, mask_enabled: layer.mask_enabled(), mask_target, clipped: layer.mask_source_id.is_some() });
            }
            (infos, d.document.active, d.document.renderer.layers().len())
        };
        self.count.set_label(&total.to_string());
        while let Some(child) = self.list.first_child() { self.list.remove(&child); }
        self.details.borrow_mut().clear();
        let mut ids = Vec::with_capacity(infos.len());
        let mut select_index = None;
        for (index, info) in infos.into_iter().enumerate() {
            if Some(info.id) == selected { select_index = Some(index); }
            ids.push(info.id);
            let row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(6).height_request(ROW_HEIGHT).margin_start(4).margin_end(8).build();
            row.append(&gtk::Box::builder().width_request((info.depth * 14 + if info.clipped { 14 } else { 0 }) as i32).build());

            let eye = gtk::CheckButton::builder().active(info.own_visible).tooltip_text("Show or hide this layer").valign(gtk::Align::Center).build();
            {
                let this = self.clone();
                let id = info.id;
                eye.connect_toggled(move |button| {
                    this.doc.borrow_mut().document.set_visible(id, button.is_active());
                    this.canvas.queue_draw();
                    this.rebuild();
                });
            }
            row.append(&eye);

            if info.group {
                let disclosure = gtk::Button::builder().icon_name(if info.collapsed { "pan-end-symbolic" } else { "pan-down-symbolic" })
                    .has_frame(false).valign(gtk::Align::Center).width_request(DISCLOSURE).height_request(DISCLOSURE).css_classes(["flat", "circular"]).build();
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
                            { let mut d = this.doc.borrow_mut(); d.document.active = Some(id); d.document.set_mask_target(false); if ctrl { let _ = d.document.select_layer_pixels(id, crate::selection::Mode::Replace); } }
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
                    let icon = gtk::Image::builder().icon_name(info.kind).pixel_size(20).halign(gtk::Align::Center).valign(gtk::Align::Center).css_classes(["dim-label"]).build();
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
                    click.connect_pressed(move |_, _, _, _| {
                        { let mut d = this.doc.borrow_mut(); d.document.active = Some(id); d.document.set_mask_target(true); }
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
            {
                let (this, id) = (self.clone(), info.id);
                let click = gtk::GestureClick::new();
                click.set_propagation_phase(gtk::PropagationPhase::Capture);
                click.connect_pressed(move |g, n, _, _| {
                    if n == 2 {
                        let is_adjustment = this.doc.borrow().document.renderer.layer(id).adjustment.is_some();
                        this.doc.borrow_mut().document.active = Some(id);
                        let name = if is_adjustment { "edit-adjustment" } else { "rename-layer" };
                        if let Some(window) = this.list.root().and_downcast::<gtk::ApplicationWindow>() {
                            if let Some(action) = gtk::prelude::ActionMapExt::lookup_action(&window, name) { action.activate(None); }
                        }
                        return;
                    }
                    if !g.current_event_state().contains(gtk::gdk::ModifierType::ALT_MASK) { return; }
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
            None => self.sync_controls(),
        }
    }

    fn refresh_detail(&self, id: Uuid) {
        let text = detail_text(&self.doc.borrow().document.renderer, id);
        if let Some(label) = self.details.borrow().get(&id) { label.set_label(&text); }
    }

    /// Points the blend and opacity controls at the selected layer, or disables them.
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
            self.opacity.set_value((layer.opacity() * 100.0).round());
            self.percent.set_label(&format!("{:.0}%", (layer.opacity() * 100.0).round()));
        } else {
            self.blend.set_selected(0);
            self.opacity.set_value(100.0);
            self.percent.set_label("100%");
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
    if layer.mask_source_id.is_some() { parts.push("Clipped".into()); }
    parts.join(" · ")
}
