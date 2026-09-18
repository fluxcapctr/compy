//! A filter's settings window: sliders for its kind, a live preview on the canvas as they move, and OK to
//! commit as one undo step or Cancel to leave the layer as it was.

use super::DocRef;
use crate::filters::{self, Kind, Levels, Settings};
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub struct FilterDialog {
    doc: DocRef,
    kind: Kind,
    settings: RefCell<Settings>,
    preview: Cell<bool>,
    scheduled: Cell<bool>,
    canvas: gtk::DrawingArea,
    status: gtk::Label,
    /// Runs after OK, so the layers panel can pick up the new pixels.
    finished: Rc<dyn Fn()>,
    histogram: RefCell<Option<[[f64; 256]; 4]>>,
    channel: Cell<usize>,
    histogram_area: RefCell<Option<gtk::DrawingArea>>,
    /// When editing an adjustment layer rather than filtering pixels: the layer and its settings on open.
    adjustment: Option<(uuid::Uuid, filters::Adjustment)>,
    committed: Cell<bool>,
}

impl FilterDialog {
    pub fn open(parent: &gtk::Window, doc: DocRef, kind: Kind, canvas: gtk::DrawingArea, finished: Rc<dyn Fn()>) {
        let mut settings = Settings::default();
        settings.seed = glib::random_int();
        let histogram = if kind == Kind::Levels { doc.borrow().document.histogram().ok() } else { None };
        let this = Rc::new(FilterDialog {
            doc, kind, settings: RefCell::new(settings), preview: Cell::new(true), scheduled: Cell::new(false), canvas,
            status: gtk::Label::builder().xalign(0.0).css_classes(["dim-label"]).wrap(true).build(), finished,
            histogram: RefCell::new(histogram), channel: Cell::new(0), histogram_area: RefCell::new(None), adjustment: None, committed: Cell::new(false),
        });
        Self::present(this, parent);
    }

    /// The same window over an adjustment layer's settings: the sliders start from the layer's values, the
    /// preview sets them live, OK records one undo step, Cancel restores them.
    pub fn open_adjustment(parent: &gtk::Window, doc: DocRef, id: uuid::Uuid, adjustment: filters::Adjustment, finished: Rc<dyn Fn()>) {
        let (kind, mut settings) = (match &adjustment {
            filters::Adjustment::Levels(_) => Kind::Levels, filters::Adjustment::Curves(_) => Kind::Levels, filters::Adjustment::Exposure(_) => Kind::Exposure,
            filters::Adjustment::GradientMap(_) => Kind::GradientMap, filters::Adjustment::Grain { .. } => Kind::Grain, filters::Adjustment::HueSaturation(_) => Kind::HueSaturation,
        }, Settings::default());
        match &adjustment {
            filters::Adjustment::Levels(l) => settings.levels = l.clone(),
            filters::Adjustment::Exposure(e) => settings.exposure = e.clone(),
            filters::Adjustment::GradientMap(g) => settings.gradient = g.clone(),
            filters::Adjustment::Grain { grain, seed } => { settings.grain = grain.clone(); settings.seed = *seed; }
            filters::Adjustment::HueSaturation(h) => settings.hue_saturation = h.clone(),
            filters::Adjustment::Curves(_) => {}
        }
        let canvas = gtk::DrawingArea::new();
        let this = Rc::new(FilterDialog {
            doc, kind, settings: RefCell::new(settings), preview: Cell::new(true), scheduled: Cell::new(false), canvas,
            status: gtk::Label::builder().xalign(0.0).css_classes(["dim-label"]).wrap(true).build(), finished,
            histogram: RefCell::new(None), channel: Cell::new(0), histogram_area: RefCell::new(None), adjustment: Some((id, adjustment)), committed: Cell::new(false),
        });
        if matches!(this.adjustment, Some((_, filters::Adjustment::Curves(_)))) { this.status.set_label("Curves has no editor yet; its points are kept as saved."); }
        Self::present(this, parent);
    }

    fn present(this: Rc<Self>, parent: &gtk::Window) {
        let kind = this.kind;
        let title = match &this.adjustment { Some((_, a)) => format!("{} Adjustment", a.kind_name()), None => kind.name().to_string() };
        let window = gtk::Window::builder().title(title).transient_for(parent).modal(false).default_width(360).resizable(false).build();
        window.set_application(parent.application().as_ref());
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).margin_top(14).margin_bottom(14).margin_start(14).margin_end(14).build();
        this.build_controls(&content);
        content.append(&this.status);
        let buttons = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).halign(gtk::Align::End).build();
        let preview = gtk::CheckButton::builder().label("Preview").active(true).hexpand(true).halign(gtk::Align::Start).build();
        { let this = this.clone(); preview.connect_toggled(move |c| { this.preview.set(c.is_active()); this.schedule(); }); }
        let cancel = gtk::Button::with_label("Cancel");
        let ok = gtk::Button::builder().label("OK").css_classes(["suggested-action"]).build();
        buttons.append(&preview);
        buttons.append(&cancel);
        buttons.append(&ok);
        content.append(&buttons);
        window.set_child(Some(&content));
        { let window = window.clone(); cancel.connect_clicked(move |_| window.close()); }
        { let (this, window) = (this.clone(), window.clone()); ok.connect_clicked(move |_| { this.commit(); window.close(); }); }
        { let this = this.clone(); window.connect_close_request(move |_| {
            match &this.adjustment {
                Some((id, original)) => { if !this.committed.get() { this.doc.borrow_mut().document.set_adjustment(*id, original, false); (this.finished)(); } }
                None => { this.doc.borrow_mut().document.clear_preview(); }
            }
            this.canvas.queue_draw();
            glib::Propagation::Proceed
        }); }
        window.present();
        this.schedule();
    }

    fn slider(&self, grid: &gtk::Grid, row: i32, label: &str, low: f64, high: f64, step: f64, value: f64, set: impl Fn(&mut Settings, f64) + 'static, this: &Rc<Self>) {
        grid.attach(&gtk::Label::builder().label(label).xalign(0.0).build(), 0, row, 1, 1);
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, low, high, step);
        scale.set_value(value);
        scale.set_hexpand(true);
        scale.set_draw_value(false);
        let spin = gtk::SpinButton::with_range(low, high, step);
        spin.set_digits(if step < 1.0 { 2 } else { 0 });
        spin.set_value(value);
        let syncing = Rc::new(Cell::new(false));
        let set = Rc::new(set);
        {
            let (this, spin, syncing, set) = (this.clone(), spin.clone(), syncing.clone(), set.clone());
            scale.connect_value_changed(move |s| {
                if syncing.get() { return; }
                syncing.set(true); spin.set_value(s.value()); syncing.set(false);
                set(&mut this.settings.borrow_mut(), s.value());
                this.schedule();
            });
        }
        {
            let (this, scale, syncing) = (this.clone(), scale.clone(), syncing.clone());
            spin.connect_value_changed(move |s| {
                if syncing.get() { return; }
                syncing.set(true); scale.set_value(s.value()); syncing.set(false);
                set(&mut this.settings.borrow_mut(), s.value());
                this.schedule();
            });
        }
        grid.attach(&scale, 1, row, 1, 1);
        grid.attach(&spin, 2, row, 1, 1);
    }

    fn build_controls(self: &Rc<Self>, content: &gtk::Box) {
        let grid = gtk::Grid::builder().row_spacing(8).column_spacing(10).build();
        let s = self.settings.borrow().clone();
        match self.kind {
            Kind::AddNoise => {
                self.slider(&grid, 0, "Amount", 0.1, 400.0, 0.1, s.amount, |s, v| s.amount = v, self);
                let distribution = gtk::DropDown::from_strings(&["Uniform", "Gaussian"]);
                { let this = self.clone(); distribution.connect_selected_notify(move |d| { this.settings.borrow_mut().gaussian = d.selected() == 1; this.schedule(); }); }
                grid.attach(&gtk::Label::builder().label("Distribution").xalign(0.0).build(), 0, 1, 1, 1);
                grid.attach(&distribution, 1, 1, 2, 1);
                let mono = gtk::CheckButton::with_label("Monochromatic");
                { let this = self.clone(); mono.connect_toggled(move |c| { this.settings.borrow_mut().monochromatic = c.is_active(); this.schedule(); }); }
                grid.attach(&mono, 1, 2, 2, 1);
            }
            Kind::Grain => {
                self.slider(&grid, 0, "Amount", 0.0, 100.0, 1.0, s.grain.amount, |s, v| s.grain.amount = v, self);
                self.slider(&grid, 1, "Size", 0.5, 20.0, 0.1, s.grain.size, |s, v| s.grain.size = v, self);
                self.slider(&grid, 2, "Roughness", 0.0, 100.0, 1.0, s.grain.roughness, |s, v| s.grain.roughness = v, self);
            }
            Kind::LensCorrection => {
                self.slider(&grid, 0, "Remove Distortion", -100.0, 100.0, 1.0, s.distortion, |s, v| s.distortion = v, self);
            }
            Kind::GradientMap => {
                for (row, label, which) in [(0, "Shadows", 0usize), (1, "Highlights", 1usize)] {
                    let color = if which == 0 { s.gradient.shadows } else { s.gradient.highlights };
                    let button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
                    button.set_rgba(&gdk::RGBA::new(color[0] as f32, color[1] as f32, color[2] as f32, 1.0));
                    let this = self.clone();
                    button.connect_rgba_notify(move |b| {
                        let c = b.rgba();
                        let value = [c.red() as f64, c.green() as f64, c.blue() as f64];
                        { let mut s = this.settings.borrow_mut(); if which == 0 { s.gradient.shadows = value; } else { s.gradient.highlights = value; } }
                        this.schedule();
                    });
                    grid.attach(&gtk::Label::builder().label(label).xalign(0.0).build(), 0, row, 1, 1);
                    grid.attach(&button, 1, row, 1, 1);
                }
                let reverse = gtk::CheckButton::with_label("Reverse");
                { let this = self.clone(); reverse.connect_toggled(move |c| { this.settings.borrow_mut().gradient.reversed = c.is_active(); this.schedule(); }); }
                grid.attach(&reverse, 1, 2, 2, 1);
            }
            Kind::Levels => {
                let channel = gtk::DropDown::from_strings(&["RGB", "Red", "Green", "Blue"]);
                grid.attach(&gtk::Label::builder().label("Channel").xalign(0.0).build(), 0, 0, 1, 1);
                grid.attach(&channel, 1, 0, 2, 1);
                let area = gtk::DrawingArea::builder().content_height(110).hexpand(true).build();
                { let this = self.clone(); area.set_draw_func(move |_, cr, w, h| this.draw_histogram(cr, w as f64, h as f64)); }
                *self.histogram_area.borrow_mut() = Some(area.clone());
                grid.attach(&area, 0, 1, 3, 1);
                let rows: Rc<RefCell<Vec<(gtk::Scale, gtk::SpinButton)>>> = Rc::new(RefCell::new(Vec::new()));
                let fields: [(&str, f64, f64, f64, fn(&Levels, usize) -> f64, fn(&mut Levels, usize, f64)); 5] = [
                    ("Input black", 0.0, 254.0, 1.0, |l, c| l.ranges[c].black, |l, c, v| l.ranges[c].black = v),
                    ("Gamma", 0.1, 9.99, 0.01, |l, c| l.ranges[c].gamma, |l, c, v| l.ranges[c].gamma = v),
                    ("Input white", 1.0, 255.0, 1.0, |l, c| l.ranges[c].white, |l, c, v| l.ranges[c].white = v),
                    ("Output black", 0.0, 255.0, 1.0, |l, c| l.ranges[c].output_black, |l, c, v| l.ranges[c].output_black = v),
                    ("Output white", 0.0, 255.0, 1.0, |l, c| l.ranges[c].output_white, |l, c, v| l.ranges[c].output_white = v),
                ];
                for (i, (label, low, high, step, get, set)) in fields.into_iter().enumerate() {
                    let row = 2 + i as i32;
                    grid.attach(&gtk::Label::builder().label(label).xalign(0.0).build(), 0, row, 1, 1);
                    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, low, high, step);
                    scale.set_draw_value(false); scale.set_hexpand(true); scale.set_value(get(&s.levels, 0));
                    let spin = gtk::SpinButton::with_range(low, high, step);
                    spin.set_digits(if step < 1.0 { 2 } else { 0 }); spin.set_value(get(&s.levels, 0));
                    let syncing = Rc::new(Cell::new(false));
                    {
                        let (this, spin, syncing) = (self.clone(), spin.clone(), syncing.clone());
                        scale.connect_value_changed(move |sc| {
                            if syncing.get() { return; }
                            syncing.set(true); spin.set_value(sc.value()); syncing.set(false);
                            let c = this.channel.get();
                            { let mut st = this.settings.borrow_mut(); set(&mut st.levels, c, sc.value()); st.levels = st.levels.normalized(); }
                            this.schedule();
                        });
                    }
                    {
                        let (this, scale, syncing) = (self.clone(), scale.clone(), syncing.clone());
                        spin.connect_value_changed(move |sp| {
                            if syncing.get() { return; }
                            syncing.set(true); scale.set_value(sp.value()); syncing.set(false);
                            let c = this.channel.get();
                            { let mut st = this.settings.borrow_mut(); set(&mut st.levels, c, sp.value()); st.levels = st.levels.normalized(); }
                            this.schedule();
                        });
                    }
                    grid.attach(&scale, 1, row, 1, 1);
                    grid.attach(&spin, 2, row, 1, 1);
                    rows.borrow_mut().push((scale, spin));
                }
                let getters: [fn(&Levels, usize) -> f64; 5] = [|l, c| l.ranges[c].black, |l, c| l.ranges[c].gamma, |l, c| l.ranges[c].white, |l, c| l.ranges[c].output_black, |l, c| l.ranges[c].output_white];
                let sync_rows = {
                    let (this, rows) = (self.clone(), rows.clone());
                    Rc::new(move || {
                        let c = this.channel.get();
                        let levels = this.settings.borrow().levels.clone();
                        for (i, (scale, spin)) in rows.borrow().iter().enumerate() {
                            let v = getters[i](&levels, c);
                            scale.set_value(v); spin.set_value(v);
                        }
                    })
                };
                {
                    let (this, sync_rows, area) = (self.clone(), sync_rows.clone(), area.clone());
                    channel.connect_selected_notify(move |d| { this.channel.set(d.selected() as usize); sync_rows(); area.queue_draw(); });
                }
                let auto = gtk::Button::with_label("Auto");
                auto.set_tooltip_text(Some("Auto Contrast: stretch the tones to the histogram's ends"));
                {
                    let (this, sync_rows) = (self.clone(), sync_rows.clone());
                    auto.connect_clicked(move |_| {
                        if let Some(h) = this.histogram.borrow().as_ref() { this.settings.borrow_mut().levels = Levels::auto_contrast(h); }
                        sync_rows();
                        this.schedule();
                    });
                }
                grid.attach(&auto, 2, 0, 1, 1);
            }
            Kind::Exposure => {
                self.slider(&grid, 0, "Exposure", -20.0, 20.0, 0.01, s.exposure.exposure, |s, v| s.exposure.exposure = v, self);
                self.slider(&grid, 1, "Offset", -0.5, 0.5, 0.001, s.exposure.offset, |s, v| s.exposure.offset = v, self);
                self.slider(&grid, 2, "Gamma", 0.01, 9.99, 0.01, s.exposure.gamma, |s, v| s.exposure.gamma = v, self);
            }
            Kind::HueSaturation => {
                let range = gtk::DropDown::from_strings(&filters::COLOR_RANGES);
                grid.attach(&gtk::Label::builder().label("Edit").xalign(0.0).build(), 0, 0, 1, 1);
                grid.attach(&range, 1, 0, 2, 1);
                let sliders: Rc<RefCell<Vec<gtk::Scale>>> = Rc::new(RefCell::new(Vec::new()));
                for (i, (label, low, high)) in [("Hue", -180.0, 180.0), ("Saturation", -100.0, 100.0), ("Lightness", -100.0, 100.0)].into_iter().enumerate() {
                    let row = 1 + i as i32;
                    grid.attach(&gtk::Label::builder().label(label).xalign(0.0).build(), 0, row, 1, 1);
                    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, low, high, 1.0);
                    scale.set_draw_value(true); scale.set_hexpand(true); scale.set_value(0.0);
                    let this = self.clone();
                    scale.connect_value_changed(move |sc| {
                        let mut st = this.settings.borrow_mut();
                        let range = st.hue_saturation.range.clone();
                        let mut a = st.hue_saturation.adjustment(&range);
                        a[i] = sc.value();
                        st.hue_saturation.set_adjustment(&range, a);
                        drop(st);
                        this.schedule();
                    });
                    grid.attach(&scale, 1, row, 2, 1);
                    sliders.borrow_mut().push(scale);
                }
                {
                    let (this, sliders) = (self.clone(), sliders.clone());
                    range.connect_selected_notify(move |d| {
                        let name = filters::COLOR_RANGES[d.selected() as usize].to_string();
                        let values = { let mut st = this.settings.borrow_mut(); st.hue_saturation.range = name.clone(); st.hue_saturation.adjustment(&name) };
                        for (scale, v) in sliders.borrow().iter().zip(values) { scale.set_value(v); }
                    });
                }
                let colorize = gtk::CheckButton::with_label("Colorize");
                { let this = self.clone(); colorize.connect_toggled(move |c| { this.settings.borrow_mut().hue_saturation.colorize = c.is_active(); this.schedule(); }); }
                grid.attach(&colorize, 1, 4, 2, 1);
            }
            Kind::GaussianBlur => {
                self.slider(&grid, 0, "Radius", 0.1, 250.0, 0.1, s.radius, |s, v| s.radius = v, self);
            }
            Kind::MotionBlur => {
                self.slider(&grid, 0, "Angle", -90.0, 90.0, 1.0, s.angle, |s, v| s.angle = v, self);
                self.slider(&grid, 1, "Distance", 1.0, 2000.0, 1.0, s.distance, |s, v| s.distance = v, self);
            }
            Kind::ContentAwareFill | Kind::SpotHeal => {}
        }
        content.append(&grid);
    }

    fn draw_histogram(&self, cr: &cairo::Context, w: f64, h: f64) {
        cr.set_source_rgb(0.12, 0.12, 0.12);
        cr.paint().ok();
        let Some(bins) = self.histogram.borrow().clone() else { return };
        let channel = self.channel.get();
        let data = &bins[channel];
        // Cap isolated spikes so a large flat area cannot flatten the useful distribution (`LevelsHistogramDisplay`).
        let peak = data.iter().cloned().fold(0.0, f64::max);
        let mut interior: Vec<f64> = data[1..255].iter().cloned().filter(|v| *v > 0.0).collect();
        interior.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let scale = if interior.is_empty() { peak } else { peak.min(interior[(interior.len() - 1) * 95 / 100] * 4.0) };
        if scale <= 0.0 { return; }
        let (r, g, b) = match channel { 1 => (0.9, 0.3, 0.3), 2 => (0.3, 0.9, 0.3), 3 => (0.4, 0.5, 1.0), _ => (0.85, 0.85, 0.85) };
        cr.set_source_rgb(r, g, b);
        for (i, v) in data.iter().enumerate() {
            let height = (v / scale).min(1.0) * (h - 4.0);
            cr.rectangle(i as f64 / 256.0 * w, h - height, w / 256.0 + 0.5, height);
        }
        cr.fill().ok();
    }

    /// Coalesces slider changes into one preview per main-loop pass.
    fn schedule(self: &Rc<Self>) {
        if self.scheduled.replace(true) { return; }
        let this = self.clone();
        glib::idle_add_local_once(move || { this.scheduled.set(false); this.render_preview(); });
    }

    /// The adjustment the current sliders describe, when editing an adjustment layer.
    fn current_adjustment(&self) -> Option<filters::Adjustment> {
        let (_, original) = self.adjustment.as_ref()?;
        let s = self.settings.borrow();
        Some(match original {
            filters::Adjustment::Levels(_) => filters::Adjustment::Levels(s.levels.clone()),
            filters::Adjustment::Curves(c) => filters::Adjustment::Curves(c.clone()),
            filters::Adjustment::Exposure(_) => filters::Adjustment::Exposure(s.exposure.clone()),
            filters::Adjustment::GradientMap(_) => filters::Adjustment::GradientMap(s.gradient.clone()),
            filters::Adjustment::Grain { .. } => filters::Adjustment::Grain { grain: s.grain.clone(), seed: s.seed },
            filters::Adjustment::HueSaturation(_) => filters::Adjustment::HueSaturation(s.hue_saturation.clone()),
        })
    }

    fn render_preview(&self) {
        if let Some((id, original)) = &self.adjustment {
            let shown = if self.preview.get() { self.current_adjustment().unwrap_or_else(|| original.clone()) } else { original.clone() };
            self.doc.borrow_mut().document.set_adjustment(*id, &shown, false);
            (self.finished)();
            return;
        }
        let settings = self.settings.borrow().clone();
        let mut d = self.doc.borrow_mut();
        let result = if self.preview.get() { d.document.preview_filter(self.kind, &settings) } else { d.document.clear_preview(); Ok(()) };
        match result { Ok(()) => self.status.set_label(""), Err(error) => self.status.set_label(&format!("{error:#}")) }
        self.canvas.queue_draw();
    }

    fn commit(&self) {
        if let Some((id, original)) = &self.adjustment {
            let Some(current) = self.current_adjustment() else { return };
            let mut d = self.doc.borrow_mut();
            d.document.set_adjustment(*id, original, false);
            d.document.set_adjustment(*id, &current, true);
            drop(d);
            self.committed.set(true);
            (self.finished)();
            return;
        }
        let settings = self.settings.borrow().clone();
        let result = { let mut d = self.doc.borrow_mut(); d.document.clear_preview(); d.document.apply_filter(self.kind, &settings) };
        if let Err(error) = result { eprintln!("{}: {error:#}", self.kind.name()); }
        (self.finished)();
    }
}

