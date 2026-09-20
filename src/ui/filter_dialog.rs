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
    /// Remove Background's download button, shown again when the model on disk fails to load.
    model_button: RefCell<Option<gtk::Button>>,
}

impl FilterDialog {
    pub fn open(parent: &gtk::Window, doc: DocRef, kind: Kind, canvas: gtk::DrawingArea, finished: Rc<dyn Fn()>) {
        let mut settings = Settings::default();
        settings.seed = glib::random_int();
        let histogram = if matches!(kind, Kind::Levels | Kind::Curves) { doc.borrow().document.histogram().ok() } else { None };
        let this = Rc::new(FilterDialog {
            doc, kind, settings: RefCell::new(settings), preview: Cell::new(true), scheduled: Cell::new(false), canvas,
            status: gtk::Label::builder().xalign(0.0).css_classes(["dim-label"]).wrap(true).build(), finished,
            histogram: RefCell::new(histogram), channel: Cell::new(0), histogram_area: RefCell::new(None), adjustment: None, committed: Cell::new(false), model_button: RefCell::new(None),
        });
        Self::present(this, parent);
    }

    /// The same window over an adjustment layer's settings: the sliders start from the layer's values, the
    /// preview sets them live, OK records one undo step, Cancel restores them.
    pub fn open_adjustment(parent: &gtk::Window, doc: DocRef, id: uuid::Uuid, adjustment: filters::Adjustment, finished: Rc<dyn Fn()>) {
        let (kind, mut settings) = (match &adjustment {
            filters::Adjustment::Levels(_) => Kind::Levels, filters::Adjustment::Curves(_) => Kind::Curves, filters::Adjustment::Exposure(_) => Kind::Exposure,
            filters::Adjustment::GradientMap(_) => Kind::GradientMap, filters::Adjustment::Grain { .. } => Kind::Grain, filters::Adjustment::HueSaturation(_) => Kind::HueSaturation,
            filters::Adjustment::BrightnessContrast(_) => Kind::BrightnessContrast, filters::Adjustment::Vibrance(_) => Kind::Vibrance, filters::Adjustment::BlackWhite(_) => Kind::BlackWhite,
            filters::Adjustment::PhotoFilter(_) => Kind::PhotoFilter, filters::Adjustment::Threshold(_) => Kind::Threshold, filters::Adjustment::Posterize(_) => Kind::Posterize,
            filters::Adjustment::ShadowsHighlights(_) => Kind::ShadowsHighlights, filters::Adjustment::SelectiveColor(_) => Kind::SelectiveColor, filters::Adjustment::ChannelMixer(_) => Kind::ChannelMixer,
        }, Settings::default());
        match &adjustment {
            filters::Adjustment::Levels(l) => settings.levels = l.clone(),
            filters::Adjustment::Exposure(e) => settings.exposure = e.clone(),
            filters::Adjustment::GradientMap(g) => settings.gradient = g.clone(),
            filters::Adjustment::Grain { grain, seed } => { settings.grain = grain.clone(); settings.seed = *seed; }
            filters::Adjustment::HueSaturation(h) => settings.hue_saturation = h.clone(),
            filters::Adjustment::Curves(c) => settings.curves = c.clone(),
            filters::Adjustment::BrightnessContrast(b) => settings.brightness = b.clone(),
            filters::Adjustment::Vibrance(v) => settings.vibrance = v.clone(),
            filters::Adjustment::BlackWhite(b) => settings.black_white = b.clone(),
            filters::Adjustment::PhotoFilter(p) => settings.photo_filter = p.clone(),
            filters::Adjustment::Threshold(t) => settings.threshold = t.clone(),
            filters::Adjustment::Posterize(p) => settings.posterize = p.clone(),
            filters::Adjustment::ShadowsHighlights(v) => settings.shadows_highlights = v.clone(),
            filters::Adjustment::SelectiveColor(v) => settings.selective = v.clone(),
            filters::Adjustment::ChannelMixer(v) => settings.mixer = v.clone(),
        }
        let canvas = gtk::DrawingArea::new();
        let this = Rc::new(FilterDialog {
            doc, kind, settings: RefCell::new(settings), preview: Cell::new(true), scheduled: Cell::new(false), canvas,
            status: gtk::Label::builder().xalign(0.0).css_classes(["dim-label"]).wrap(true).build(), finished,
            histogram: RefCell::new(None), channel: Cell::new(0), histogram_area: RefCell::new(None), adjustment: Some((id, adjustment)), committed: Cell::new(false), model_button: RefCell::new(None),
        });
        Self::present(this, parent);
    }

    fn present(this: Rc<Self>, parent: &gtk::Window) {
        let kind = this.kind;
        let title = match &this.adjustment { Some((_, a)) => format!("{} Adjustment", a.kind_name()), None => kind.name().to_string() };
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).margin_top(14).margin_bottom(14).margin_start(14).margin_end(14).build();
        this.build_controls(&content);
        content.append(&this.status);
        let buttons = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).halign(gtk::Align::End).build();
        let preview = gtk::CheckButton::builder().label("Preview").active(true).hexpand(true).halign(gtk::Align::Start).build();
        { let this = this.clone(); preview.connect_toggled(move |c| { this.preview.set(c.is_active()); this.schedule(); }); }
        if let Some((id, _)) = &this.adjustment {
            // An adjustment layer works on everything below it unless clipped to the layer directly below.
            let clipped = this.doc.borrow().document.renderer.layer(*id).mask_source_id.is_some();
            let clip = gtk::CheckButton::builder().label("Only the layer below").active(clipped).tooltip_text("Clip the adjustment to the layer directly below it (Alt+G); off, it affects every layer below").build();
            let (this2, id) = (this.clone(), *id);
            clip.connect_toggled(move |_| { { let mut d = this2.doc.borrow_mut(); if d.document.has_layer(id) { d.document.toggle_clipping(id); } } (this2.finished)(); });
            content.append(&clip);
        }
        let cancel = gtk::Button::with_label("Cancel");
        let ok = gtk::Button::builder().label("OK").css_classes(["suggested-action"]).build();
        buttons.append(&preview);
        buttons.append(&cancel);
        buttons.append(&ok);
        content.append(&buttons);
        // Modal: the preview and the commit belong to the layer the dialog opened on.
        let window = super::dialogs::floating(parent, &title, true, 360, &content);
        { let window = window.clone(); cancel.connect_clicked(move |_| window.close()); }
        { let (this, window) = (this.clone(), window.clone()); ok.connect_clicked(move |_| { this.commit(); window.close(); }); }
        { let this = this.clone(); window.connect_close_request(move |_| {
            match &this.adjustment {
                Some((id, original)) => { if !this.committed.get() && this.doc.borrow_mut().document.set_adjustment(*id, original, false) { (this.finished)(); } }
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
                reverse.set_active(s.gradient.reversed);
                { let this = self.clone(); reverse.connect_toggled(move |c| { this.settings.borrow_mut().gradient.reversed = c.is_active(); this.schedule(); }); }
                grid.attach(&reverse, 1, 2, 2, 1);
                // More colors than two: the gradient editor fills the map's stops.
                let bar = gtk::DrawingArea::builder().content_height(18).hexpand(true).tooltip_text("The map's gradient; click to edit it with more colors").build();
                { let this = self.clone(); bar.set_draw_func(move |_, cr, w, h| { let g = this.settings.borrow().gradient.gradient(); super::tools::draw_gradient_bar(cr, w as f64, h as f64, &g); }); }
                {
                    let (this, bar2) = (self.clone(), bar.clone());
                    let click = gtk::GestureClick::new();
                    click.connect_released(move |g, _, _, _| {
                        let Some(root) = g.widget().and_then(|w| w.root()).and_downcast::<gtk::Window>() else { return };
                        let initial = { let st = this.settings.borrow(); let mut m = st.gradient.clone(); m.reversed = false; m.gradient() };
                        let (t2, b2) = (this.clone(), bar2.clone());
                        super::gradient_editor::open(&root, initial, [0.0; 3], [1.0; 3], Rc::new(move |g: crate::gradient::Gradient| { t2.settings.borrow_mut().gradient.stops = g.normalized().stops; b2.queue_draw(); t2.schedule(); }));
                    });
                    bar.add_controller(click);
                }
                grid.attach(&gtk::Label::builder().label("Gradient").xalign(0.0).build(), 0, 3, 1, 1);
                grid.attach(&bar, 1, 3, 2, 1);
            }
            Kind::Levels => {
                let channel = gtk::DropDown::from_strings(&["RGB", "Red", "Green", "Blue"]);
                channel.set_tooltip_text(Some("Which channel the sliders and histogram show; RGB moves all three together"));
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
            Kind::Curves => self.build_curves(&grid),
            Kind::ColorBalance => {
                let tone = gtk::DropDown::from_strings(&["Shadows", "Midtones", "Highlights"]);
                tone.set_selected(1);
                self.channel.set(1);
                grid.attach(&gtk::Label::builder().label("Tone").xalign(0.0).build(), 0, 0, 1, 1);
                grid.attach(&tone, 1, 0, 2, 1);
                let rows: Rc<RefCell<Vec<(gtk::Scale, gtk::SpinButton)>>> = Rc::new(RefCell::new(Vec::new()));
                for (i, label) in ["Cyan / Red", "Magenta / Green", "Yellow / Blue"].into_iter().enumerate() {
                    let row = 1 + i as i32;
                    grid.attach(&gtk::Label::builder().label(label).xalign(0.0).build(), 0, row, 1, 1);
                    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, -100.0, 100.0, 1.0);
                    scale.set_draw_value(false); scale.set_hexpand(true); scale.set_value(0.0);
                    let spin = gtk::SpinButton::with_range(-100.0, 100.0, 1.0);
                    let syncing = Rc::new(Cell::new(false));
                    let set = move |this: &Rc<Self>, v: f64| {
                        let mut st = this.settings.borrow_mut();
                        let range = match this.channel.get() { 0 => &mut st.balance.shadows, 2 => &mut st.balance.highlights, _ => &mut st.balance.midtones };
                        range[i] = v;
                    };
                    { let (this, spin, syncing) = (self.clone(), spin.clone(), syncing.clone()); scale.connect_value_changed(move |sc| { if syncing.get() { return; } syncing.set(true); spin.set_value(sc.value()); syncing.set(false); set(&this, sc.value()); this.schedule(); }); }
                    { let (this, scale, syncing) = (self.clone(), scale.clone(), syncing.clone()); spin.connect_value_changed(move |sp| { if syncing.get() { return; } syncing.set(true); scale.set_value(sp.value()); syncing.set(false); set(&this, sp.value()); this.schedule(); }); }
                    grid.attach(&scale, 1, row, 1, 1);
                    grid.attach(&spin, 2, row, 1, 1);
                    rows.borrow_mut().push((scale, spin));
                }
                {
                    let (this, rows) = (self.clone(), rows.clone());
                    tone.connect_selected_notify(move |d| {
                        this.channel.set(d.selected() as usize);
                        // Copy the range out first: setting the sliders runs their handlers, which borrow the settings.
                        let range = { let st = this.settings.borrow(); match d.selected() { 0 => st.balance.shadows, 2 => st.balance.highlights, _ => st.balance.midtones } };
                        for (i, (scale, spin)) in rows.borrow().iter().enumerate() { scale.set_value(range[i]); spin.set_value(range[i]); }
                    });
                }
                let keep = gtk::CheckButton::with_label("Preserve luminosity");
                keep.set_active(s.balance.preserve_luminosity);
                { let this = self.clone(); keep.connect_toggled(move |c| { this.settings.borrow_mut().balance.preserve_luminosity = c.is_active(); this.schedule(); }); }
                grid.attach(&keep, 1, 4, 2, 1);
            }
            Kind::Fade => {
                self.slider(&grid, 0, "Opacity %", 0.0, 100.0, 1.0, s.fade * 100.0, |s, v| s.fade = v / 100.0, self);
                let name = self.doc.borrow().document.last_filter.as_ref().map(|f| f.3.clone()).unwrap_or_default();
                grid.attach(&gtk::Label::builder().label(&format!("Blends {name} back toward the pixels it replaced.")).xalign(0.0).css_classes(["dim-label"]).build(), 0, 1, 3, 1);
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
            Kind::UnsharpMask => {
                self.slider(&grid, 0, "Amount %", 1.0, 500.0, 1.0, s.sharpen.amount, |s, v| s.sharpen.amount = v, self);
                self.slider(&grid, 1, "Radius", 0.1, 250.0, 0.1, s.sharpen.radius, |s, v| s.sharpen.radius = v, self);
                self.slider(&grid, 2, "Threshold", 0.0, 255.0, 1.0, s.sharpen.threshold, |s, v| s.sharpen.threshold = v, self);
            }
            Kind::SmartSharpen => {
                self.slider(&grid, 0, "Amount %", 1.0, 500.0, 1.0, s.sharpen.amount, |s, v| s.sharpen.amount = v, self);
                self.slider(&grid, 1, "Radius", 0.1, 250.0, 0.1, s.sharpen.radius, |s, v| s.sharpen.radius = v, self);
                self.slider(&grid, 2, "Reduce Noise %", 0.0, 100.0, 1.0, s.sharpen.noise, |s, v| s.sharpen.noise = v, self);
                grid.attach(&gtk::Label::builder().label("Sharpens brightness only, so colors stay put and grain stays quiet.").xalign(0.0).css_classes(["dim-label"]).build(), 0, 3, 3, 1);
            }
            Kind::BrightnessContrast => {
                self.slider(&grid, 0, "Brightness", -150.0, 150.0, 1.0, s.brightness.brightness, |s, v| s.brightness.brightness = v, self);
                self.slider(&grid, 1, "Contrast", -50.0, 100.0, 1.0, s.brightness.contrast, |s, v| s.brightness.contrast = v, self);
            }
            Kind::Vibrance => {
                self.slider(&grid, 0, "Vibrance", -100.0, 100.0, 1.0, s.vibrance.vibrance, |s, v| s.vibrance.vibrance = v, self);
                self.slider(&grid, 1, "Saturation", -100.0, 100.0, 1.0, s.vibrance.saturation, |s, v| s.vibrance.saturation = v, self);
            }
            Kind::BlackWhite => {
                self.slider(&grid, 0, "Reds", -200.0, 300.0, 1.0, s.black_white.reds, |s, v| s.black_white.reds = v, self);
                self.slider(&grid, 1, "Yellows", -200.0, 300.0, 1.0, s.black_white.yellows, |s, v| s.black_white.yellows = v, self);
                self.slider(&grid, 2, "Greens", -200.0, 300.0, 1.0, s.black_white.greens, |s, v| s.black_white.greens = v, self);
                self.slider(&grid, 3, "Cyans", -200.0, 300.0, 1.0, s.black_white.cyans, |s, v| s.black_white.cyans = v, self);
                self.slider(&grid, 4, "Blues", -200.0, 300.0, 1.0, s.black_white.blues, |s, v| s.black_white.blues = v, self);
                self.slider(&grid, 5, "Magentas", -200.0, 300.0, 1.0, s.black_white.magentas, |s, v| s.black_white.magentas = v, self);
            }
            Kind::PhotoFilter => {
                grid.attach(&gtk::Label::builder().label("Filter").xalign(0.0).build(), 0, 0, 1, 1);
                let presets: [(&str, [f64; 3]); 8] = [("Warming (85)", [0.925, 0.541, 0.0]), ("Warming (81)", [0.922, 0.694, 0.0]), ("Cooling (80)", [0.0, 0.427, 1.0]), ("Cooling (82)", [0.0, 0.706, 1.0]), ("Red", [0.918, 0.098, 0.106]), ("Yellow", [0.976, 0.910, 0.0]), ("Green", [0.098, 0.694, 0.298]), ("Sepia", [0.675, 0.475, 0.16])];
                let names: Vec<&str> = presets.iter().map(|p| p.0).collect();
                let preset = gtk::DropDown::from_strings(&names);
                if let Some(i) = presets.iter().position(|p| p.1 == s.photo_filter.color) { preset.set_selected(i as u32); }
                { let this = self.clone(); preset.connect_selected_notify(move |d| { this.settings.borrow_mut().photo_filter.color = presets[d.selected() as usize].1; this.schedule(); }); }
                grid.attach(&preset, 1, 0, 2, 1);
                self.slider(&grid, 1, "Density %", 1.0, 100.0, 1.0, s.photo_filter.density, |s, v| s.photo_filter.density = v, self);
                let keep = gtk::CheckButton::builder().label("Preserve Luminosity").active(s.photo_filter.preserve_luminosity).build();
                { let this = self.clone(); keep.connect_toggled(move |c| { this.settings.borrow_mut().photo_filter.preserve_luminosity = c.is_active(); this.schedule(); }); }
                grid.attach(&keep, 1, 2, 2, 1);
            }
            Kind::Threshold => {
                self.slider(&grid, 0, "Level", 1.0, 255.0, 1.0, s.threshold.level, |s, v| s.threshold.level = v, self);
            }
            Kind::Posterize => {
                self.slider(&grid, 0, "Levels", 2.0, 255.0, 1.0, s.posterize.levels, |s, v| s.posterize.levels = v, self);
            }
            Kind::ShadowsHighlights => {
                self.slider(&grid, 0, "Shadows %", 0.0, 100.0, 1.0, s.shadows_highlights.shadows, |s, v| s.shadows_highlights.shadows = v, self);
                self.slider(&grid, 1, "Highlights %", 0.0, 100.0, 1.0, s.shadows_highlights.highlights, |s, v| s.shadows_highlights.highlights = v, self);
                self.slider(&grid, 2, "Radius", 1.0, 500.0, 1.0, s.shadows_highlights.radius, |s, v| s.shadows_highlights.radius = v, self);
            }
            Kind::HighPass => {
                self.slider(&grid, 0, "Radius", 0.1, 250.0, 0.1, s.high_pass, |s, v| s.high_pass = v, self);
                grid.attach(&gtk::Label::builder().label("Set the layer to Overlay or Soft Light to sharpen with it.").xalign(0.0).css_classes(["dim-label"]).build(), 0, 1, 3, 1);
            }
            Kind::RadialBlur => {
                grid.attach(&gtk::Label::builder().label("Method").xalign(0.0).build(), 0, 0, 1, 1);
                let method = gtk::DropDown::from_strings(&["Spin", "Zoom"]);
                method.set_selected(if s.radial.zoom { 1 } else { 0 });
                { let this = self.clone(); method.connect_selected_notify(move |d| { this.settings.borrow_mut().radial.zoom = d.selected() == 1; this.schedule(); }); }
                grid.attach(&method, 1, 0, 2, 1);
                self.slider(&grid, 1, "Amount", 1.0, 100.0, 1.0, s.radial.amount, |s, v| s.radial.amount = v, self);
                self.slider(&grid, 2, "Center X %", 0.0, 100.0, 1.0, s.radial.center.0 * 100.0, |s, v| s.radial.center.0 = v / 100.0, self);
                self.slider(&grid, 3, "Center Y %", 0.0, 100.0, 1.0, s.radial.center.1 * 100.0, |s, v| s.radial.center.1 = v / 100.0, self);
            }
            Kind::ChannelMixer => {
                let mono = gtk::CheckButton::builder().label("Monochrome (the red row makes the gray)").active(s.mixer.monochrome).build();
                { let this = self.clone(); mono.connect_toggled(move |c| { this.settings.borrow_mut().mixer.monochrome = c.is_active(); this.schedule(); }); }
                grid.attach(&mono, 0, 0, 3, 1);
                let mut row = 1;
                for (out, name) in [(0usize, "Red"), (1, "Green"), (2, "Blue")] {
                    for (i, input) in ["from Red %", "from Green %", "from Blue %", "Constant %"].into_iter().enumerate() {
                        let current = match out { 0 => s.mixer.red[i], 1 => s.mixer.green[i], _ => s.mixer.blue[i] };
                        self.slider(&grid, row, &format!("{name} {input}"), -200.0, 200.0, 1.0, current, move |s, v| { let r = match out { 0 => &mut s.mixer.red, 1 => &mut s.mixer.green, _ => &mut s.mixer.blue }; r[i] = v; }, self);
                        row += 1;
                    }
                }
            }
            Kind::SelectiveColor => {
                let range = gtk::DropDown::from_strings(&filters::SELECTIVE_RANGES);
                if let Some(i) = filters::SELECTIVE_RANGES.iter().position(|r| *r == s.selective.range) { range.set_selected(i as u32); }
                grid.attach(&gtk::Label::builder().label("Colors").xalign(0.0).build(), 0, 0, 1, 1);
                grid.attach(&range, 1, 0, 2, 1);
                let sliders: Rc<RefCell<Vec<gtk::Scale>>> = Rc::new(RefCell::new(Vec::new()));
                let current = s.selective.adjustment(&s.selective.range);
                for (i, label) in ["Cyan", "Magenta", "Yellow", "Black"].into_iter().enumerate() {
                    let row = 1 + i as i32;
                    grid.attach(&gtk::Label::builder().label(label).xalign(0.0).build(), 0, row, 1, 1);
                    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, -100.0, 100.0, 1.0);
                    scale.set_draw_value(true); scale.set_hexpand(true); scale.set_value(current[i]);
                    let this = self.clone();
                    scale.connect_value_changed(move |sc| {
                        let mut st = this.settings.borrow_mut();
                        let range = st.selective.range.clone();
                        let mut a = st.selective.adjustment(&range);
                        a[i] = sc.value();
                        st.selective.set_adjustment(&range, a);
                        drop(st);
                        this.schedule();
                    });
                    grid.attach(&scale, 1, row, 2, 1);
                    sliders.borrow_mut().push(scale);
                }
                {
                    let (this, sliders) = (self.clone(), sliders.clone());
                    range.connect_selected_notify(move |d| {
                        let name = filters::SELECTIVE_RANGES[d.selected() as usize].to_string();
                        let values = { let mut st = this.settings.borrow_mut(); st.selective.range = name.clone(); st.selective.adjustment(&name) };
                        for (scale, v) in sliders.borrow().iter().zip(values) { scale.set_value(v); }
                    });
                }
                let relative = gtk::CheckButton::builder().label("Relative").active(s.selective.relative).build();
                { let this = self.clone(); relative.connect_toggled(move |c| { this.settings.borrow_mut().selective.relative = c.is_active(); this.schedule(); }); }
                grid.attach(&relative, 1, 5, 2, 1);
            }
            Kind::MotionBlur => {
                self.slider(&grid, 0, "Angle", -90.0, 90.0, 1.0, s.angle, |s, v| s.angle = v, self);
                self.slider(&grid, 1, "Distance", 1.0, 2000.0, 1.0, s.distance, |s, v| s.distance = v, self);
            }
            Kind::ContentAwareFill | Kind::SpotHeal | Kind::Invert => {}
            Kind::RemoveBackground => {
                let quality = gtk::DropDown::from_strings(&["Basic", "Advanced"]);
                quality.set_selected(if s.matte.advanced { 1 } else { 0 });
                quality.set_tooltip_text(Some("Basic is the model's mask as it comes; Advanced refines its edges with the controls below"));
                { let this = self.clone(); quality.connect_selected_notify(move |d| { this.settings.borrow_mut().matte.advanced = d.selected() == 1; this.schedule(); }); }
                grid.attach(&gtk::Label::builder().label("Quality").xalign(0.0).build(), 0, 0, 1, 1);
                grid.attach(&quality, 1, 0, 1, 1);
                self.slider(&grid, 1, "Refine Edges", 0.0, 40.0, 1.0, s.matte.refine_edges, |s, v| s.matte.refine_edges = v, self);
                self.slider(&grid, 2, "Contrast", 0.0, 100.0, 1.0, s.matte.contrast, |s, v| s.matte.contrast = v, self);
                self.slider(&grid, 3, "Shift Edge", -10.0, 10.0, 1.0, s.matte.shift_edge, |s, v| s.matte.shift_edge = v, self);
                {
                    // First use: the model is fetched on request, with progress, then the preview starts. The
                    // button stays around, hidden, in case the file on disk turns out to be damaged.
                    let ready = crate::matte::model_ready();
                    let download = gtk::Button::builder().label(format!("Download model ({} MB)", crate::matte::MODEL_BYTES / 1_000_000)).css_classes(["suggested-action"]).visible(!ready).build();
                    let note = gtk::Label::builder().label("Remove Background needs a segmentation model (ISNet), fetched once from GitHub into ~/.local/share/compositor.").wrap(true).xalign(0.0).css_classes(["dim-label"]).visible(!ready).build();
                    grid.attach(&note, 0, 4, 2, 1);
                    grid.attach(&download, 0, 5, 2, 1);
                    *self.model_button.borrow_mut() = Some(download.clone());
                    let this = self.clone();
                    download.connect_clicked(move |button| {
                        button.set_sensitive(false);
                        let progress = Rc::new(Cell::new((0u64, crate::matte::MODEL_BYTES)));
                        let done: Rc<RefCell<Option<Result<(), String>>>> = Rc::new(RefCell::new(None));
                        let shared = std::sync::Arc::new(std::sync::Mutex::new((0u64, crate::matte::MODEL_BYTES, None::<Result<(), String>>)));
                        {
                            let shared = shared.clone();
                            std::thread::spawn(move || {
                                let outcome = crate::matte::download_model(|d, t| { if let Ok(mut s) = shared.lock() { s.0 = d; s.1 = t; } }).map_err(|e| format!("{e:#}"));
                                if let Ok(mut s) = shared.lock() { s.2 = Some(outcome); }
                            });
                        }
                        let (this, button) = (this.clone(), button.clone());
                        glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
                            let (d, t, outcome) = shared.lock().map(|s| (s.0, s.1, s.2.clone())).unwrap_or((0, 1, None));
                            progress.set((d, t));
                            match outcome {
                                None => { this.status.set_label(&format!("Downloading: {} of {} MB", d / 1_000_000, t / 1_000_000)); glib::ControlFlow::Continue }
                                Some(Ok(())) => { *done.borrow_mut() = Some(Ok(())); button.set_visible(false); this.status.set_label("Model ready."); this.schedule(); glib::ControlFlow::Break }
                                Some(Err(e)) => { this.status.set_label(&format!("Download failed: {e}")); button.set_sensitive(true); glib::ControlFlow::Break }
                            }
                        });
                    });
                }
            }
        }
        content.append(&grid);
    }

    /// The Curves editor (`CurvesControls`): the histogram behind a grid, the curve through its points,
    /// click to add a point, drag to move one, the ends only up and down.
    fn build_curves(self: &Rc<Self>, grid: &gtk::Grid) {
        let channel = gtk::DropDown::from_strings(&["RGB", "Red", "Green", "Blue"]);
        grid.attach(&gtk::Label::builder().label("Channel").xalign(0.0).build(), 0, 0, 1, 1);
        grid.attach(&channel, 1, 0, 2, 1);
        let area = gtk::DrawingArea::builder().content_height(260).content_width(300).hexpand(true).build();
        let selected: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
        let dragging: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
        let readout = gtk::Label::builder().xalign(0.0).css_classes(["dim-label", "numeric"]).label("Click to add a point. Drag to adjust.").width_chars(34).max_width_chars(34).ellipsize(gtk::pango::EllipsizeMode::End).build();
        {
            let (this, selected) = (self.clone(), selected.clone());
            area.set_draw_func(move |_, cr, w, h| {
                let (w, h) = (w as f64, h as f64);
                this.draw_histogram(cr, w, h);
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.12);
                cr.set_line_width(1.0);
                for i in 0..=4 { let f = i as f64 / 4.0; cr.move_to((f * w).round() + 0.5, 0.0); cr.line_to((f * w).round() + 0.5, h); cr.move_to(0.0, (f * h).round() + 0.5); cr.line_to(w, (f * h).round() + 0.5); }
                cr.stroke().ok();
                let c = this.channel.get();
                let curves = this.settings.borrow().curves.clone();
                let pos = |x: f64, y: f64| (x / 255.0 * w, (1.0 - y / 255.0) * h);
                cr.set_source_rgb(1.0, 1.0, 1.0);
                cr.set_line_width(2.0);
                for x in 0..=255 { let p = pos(x as f64, curves.value(x as f64, c)); if x == 0 { cr.move_to(p.0, p.1); } else { cr.line_to(p.0, p.1); } }
                cr.stroke().ok();
                let accent = super::theme::current().map(|p| p.accent).unwrap_or((0.2, 0.6, 1.0));
                for (i, point) in curves.channels[c].iter().enumerate() {
                    let p = pos(point.0, point.1);
                    if selected.get() == Some(i) { cr.set_source_rgb(accent.0, accent.1, accent.2); } else { cr.set_source_rgb(1.0, 1.0, 1.0); }
                    cr.arc(p.0, p.1, 4.0, 0.0, std::f64::consts::TAU);
                    cr.fill().ok();
                }
            });
        }
        let drag = gtk::GestureDrag::new();
        let start: Rc<Cell<(f64, f64)>> = Rc::new(Cell::new((0.0, 0.0)));
        {
            let (this, area, selected, dragging, start, readout) = (self.clone(), area.clone(), selected.clone(), dragging.clone(), start.clone(), readout.clone());
            drag.connect_drag_begin(move |g, x, y| {
                // Ours, not the window handle's: a point drag must not move the dialog.
                g.set_state(gtk::EventSequenceState::Claimed);
                start.set((x, y));
                let (w, h) = (area.width().max(1) as f64, area.height().max(1) as f64);
                let (px, py) = ((x / w * 255.0).clamp(0.0, 255.0), (255.0 - y / h * 255.0).clamp(0.0, 255.0));
                let c = this.channel.get();
                let mut st = this.settings.borrow_mut();
                let points = &mut st.curves.channels[c];
                let nearest = (0..points.len()).min_by(|a, b| { let da = (points[*a].0 - px).hypot(points[*a].1 - py); let db = (points[*b].0 - px).hypot(points[*b].1 - py); da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal) });
                let hit = nearest.filter(|i| (points[*i].0 - px).hypot(points[*i].1 - py) < 14.0);
                let index = match hit {
                    Some(i) => Some(i),
                    None if points.len() < 32 && px > 1.0 && px < 254.0 && points.iter().all(|p| (p.0 - px).abs() > 1.0) => {
                        points.push((px, py));
                        points.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                        points.iter().position(|p| p.0 == px)
                    }
                    None => None,
                };
                dragging.set(index);
                selected.set(index);
                if let Some(i) = index { readout.set_label(&format!("Input {}   Output {}", points[i].0.round(), points[i].1.round())); }
                drop(st);
                this.schedule();
                area.queue_draw();
            });
        }
        {
            let (this, area, dragging, start, readout) = (self.clone(), area.clone(), dragging.clone(), start.clone(), readout.clone());
            drag.connect_drag_update(move |_, dx, dy| {
                let Some(i) = dragging.get() else { return };
                let (w, h) = (area.width().max(1) as f64, area.height().max(1) as f64);
                let (sx, sy) = start.get();
                let (px, py) = (((sx + dx) / w * 255.0).clamp(0.0, 255.0), (255.0 - (sy + dy) / h * 255.0).clamp(0.0, 255.0));
                let c = this.channel.get();
                {
                    let mut st = this.settings.borrow_mut();
                    let points = &mut st.curves.channels[c];
                    if i >= points.len() { return; }
                    points[i].1 = py;
                    if i > 0 && i < points.len() - 1 { points[i].0 = px.clamp(points[i - 1].0 + 1.0, points[i + 1].0 - 1.0); }
                    readout.set_label(&format!("Input {}   Output {}", points[i].0.round(), points[i].1.round()));
                }
                this.schedule();
                area.queue_draw();
            });
        }
        { let dragging = dragging.clone(); drag.connect_drag_end(move |_, _, _| dragging.set(None)); }
        area.add_controller(drag);
        grid.attach(&area, 0, 1, 3, 1);
        grid.attach(&readout, 0, 2, 2, 1);
        let remove = gtk::Button::with_label("Remove point");
        {
            let (this, area, selected, readout) = (self.clone(), area.clone(), selected.clone(), readout.clone());
            remove.connect_clicked(move |_| {
                let c = this.channel.get();
                if let Some(i) = selected.get() {
                    let mut st = this.settings.borrow_mut();
                    let points = &mut st.curves.channels[c];
                    if i > 0 && i + 1 < points.len() { points.remove(i); selected.set(None); readout.set_label("Click to add a point. Drag to adjust."); }
                }
                this.schedule();
                area.queue_draw();
            });
        }
        grid.attach(&remove, 2, 2, 1, 1);
        let reset = gtk::Button::with_label("Reset curve");
        {
            let (this, area, selected) = (self.clone(), area.clone(), selected.clone());
            reset.connect_clicked(move |_| { let c = this.channel.get(); this.settings.borrow_mut().curves.channels[c] = vec![(0.0, 0.0), (255.0, 255.0)]; selected.set(None); this.schedule(); area.queue_draw(); });
        }
        grid.attach(&reset, 0, 3, 1, 1);
        {
            let (this, area, selected, dragging) = (self.clone(), area.clone(), selected.clone(), dragging.clone());
            channel.connect_selected_notify(move |d| { this.channel.set(d.selected() as usize); selected.set(None); dragging.set(None); area.queue_draw(); });
        }
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
            filters::Adjustment::Curves(_) => filters::Adjustment::Curves(s.curves.clone()),
            filters::Adjustment::Exposure(_) => filters::Adjustment::Exposure(s.exposure.clone()),
            filters::Adjustment::GradientMap(_) => filters::Adjustment::GradientMap(s.gradient.clone()),
            filters::Adjustment::Grain { .. } => filters::Adjustment::Grain { grain: s.grain.clone(), seed: s.seed },
            filters::Adjustment::HueSaturation(_) => filters::Adjustment::HueSaturation(s.hue_saturation.clone()),
            filters::Adjustment::BrightnessContrast(_) => filters::Adjustment::BrightnessContrast(s.brightness.clone()),
            filters::Adjustment::Vibrance(_) => filters::Adjustment::Vibrance(s.vibrance.clone()),
            filters::Adjustment::BlackWhite(_) => filters::Adjustment::BlackWhite(s.black_white.clone()),
            filters::Adjustment::PhotoFilter(_) => filters::Adjustment::PhotoFilter(s.photo_filter.clone()),
            filters::Adjustment::Threshold(_) => filters::Adjustment::Threshold(s.threshold.clone()),
            filters::Adjustment::Posterize(_) => filters::Adjustment::Posterize(s.posterize.clone()),
            filters::Adjustment::ShadowsHighlights(_) => filters::Adjustment::ShadowsHighlights(s.shadows_highlights.clone()),
            filters::Adjustment::SelectiveColor(_) => filters::Adjustment::SelectiveColor(s.selective.clone()),
            filters::Adjustment::ChannelMixer(_) => filters::Adjustment::ChannelMixer(s.mixer.clone()),
        })
    }

    fn render_preview(&self) {
        if let Some((id, original)) = &self.adjustment {
            let shown = if self.preview.get() { self.current_adjustment().unwrap_or_else(|| original.clone()) } else { original.clone() };
            if self.doc.borrow_mut().document.set_adjustment(*id, &shown, false) { (self.finished)(); }
            else { self.status.set_label("This adjustment layer no longer exists."); }
            return;
        }
        let settings = self.settings.borrow().clone();
        let mut d = self.doc.borrow_mut();
        let result = if self.preview.get() { d.document.preview_filter(self.kind, &settings) } else { d.document.clear_preview(); Ok(()) };
        match result {
            Ok(()) => self.status.set_label(""),
            Err(error) => {
                let text = format!("{error:#}");
                if text.contains("background removal model") {
                    // The file on disk is not a usable model: drop it and offer the download again.
                    crate::matte::discard_model();
                    if let Some(b) = self.model_button.borrow().as_ref() { b.set_visible(true); b.set_sensitive(true); }
                    self.status.set_label("The model on disk could not be loaded; download it again.");
                } else { self.status.set_label(&text); }
            }
        }
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

