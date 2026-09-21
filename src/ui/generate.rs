//! The Generate Image panel: a prompt, a model, a size, Generate; results as thumbnails that each
//! land as a new layer centered on the canvas. The model list puts this machine first, so the free
//! one is the default and the billed ones are a deliberate choice rather than a silent fallback.
//! The model runs on a thread; the panel polls it, as Generative Fill does.

use super::DocRef;
use crate::document::Document;
use crate::genfill;
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

struct Shared { status: String, done: Option<Result<Vec<Vec<u8>>, String>>, cancel: bool }

pub struct Generate {
    doc: DocRef,
    window: gtk::Window,
    prompt: gtk::TextView,
    models: gtk::DropDown,
    width: gtk::SpinButton,
    height: gtk::SpinButton,
    count: gtk::SpinButton,
    transparent: gtk::CheckButton,
    generate: gtk::Button,
    status: gtk::Label,
    cost: gtk::Label,
    results: gtk::FlowBox,
    images: RefCell<Vec<Vec<u8>>>,
    running: Rc<RefCell<Option<Arc<Mutex<Shared>>>>>,
    finished: Rc<dyn Fn()>,
}

impl Generate {
    pub fn open(parent: &gtk::Window, doc: DocRef, finished: Rc<dyn Fn()>) {
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).margin_top(14).margin_bottom(14).margin_start(14).margin_end(14).build();

        // A picture is described in sentences, not in a line, so the prompt is a text view.
        let prompt = gtk::TextView::builder().wrap_mode(gtk::WrapMode::WordChar).top_margin(6).bottom_margin(6).left_margin(6).right_margin(6).build();
        let scroller = gtk::ScrolledWindow::builder().child(&prompt).min_content_height(88).hexpand(true).has_frame(true).build();
        content.append(&scroller);

        let row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).build();
        let list = genfill::generate_models();
        let names: Vec<&str> = list.iter().map(|m| m.name.as_str()).collect();
        let models = gtk::DropDown::from_strings(&names);
        models.set_hexpand(true);
        models.set_tooltip_text(Some("This machine needs ComfyUI running; the fal models need a key and are billed"));
        row.append(&models);
        content.append(&row);

        let (cw, ch) = { let d = doc.borrow(); (d.document.width() as f64, d.document.height() as f64) };
        let size_row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).build();
        size_row.append(&gtk::Label::new(Some("Size")));
        let width = gtk::SpinButton::with_range(64.0, 4096.0, 16.0);
        let height = gtk::SpinButton::with_range(64.0, 4096.0, 16.0);
        // The canvas is the obvious default, but a large one is more than a local card carries, so the
        // starting size is the canvas shrunk to what will actually run.
        let (dw, dh) = crate::comfy::fit_pixels(cw as usize, ch as usize);
        width.set_value(dw as f64);
        height.set_value(dh as f64);
        size_row.append(&width);
        size_row.append(&gtk::Label::new(Some("x")));
        size_row.append(&height);
        let canvas = gtk::Button::with_label("Canvas");
        canvas.set_tooltip_text(Some("Match the document"));
        size_row.append(&canvas);
        size_row.append(&gtk::Label::new(Some("Variations")));
        let count = gtk::SpinButton::with_range(1.0, 4.0, 1.0);
        count.set_value(1.0);
        size_row.append(&count);
        content.append(&size_row);

        let transparent = gtk::CheckButton::builder().label("Transparent background (a cutout to sit over other layers)").build();
        content.append(&transparent);

        let results = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).max_children_per_line(4).min_children_per_line(2).column_spacing(6).row_spacing(6).homogeneous(true).build();
        content.append(&results);
        let status = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["dim-label"]).build();
        content.append(&status);
        let cost = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["dim-label", "caption"]).build();
        content.append(&cost);

        let buttons = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).halign(gtk::Align::End).build();
        let close = gtk::Button::with_label("Close");
        let generate = gtk::Button::builder().label("Generate").css_classes(["suggested-action"]).build();
        buttons.append(&close);
        buttons.append(&generate);
        content.append(&buttons);

        let window = gtk::Window::builder().title("Generate Image").transient_for(parent).modal(false).default_width(520).child(&content).build();

        let this = Rc::new(Self {
            doc, window: window.clone(), prompt, models, width, height, count, transparent,
            generate: generate.clone(), status, cost, results,
            images: RefCell::new(Vec::new()), running: Rc::new(RefCell::new(None)), finished,
        });

        {
            let this = this.clone();
            canvas.connect_clicked(move |_| { this.width.set_value(cw); this.height.set_value(ch); });
        }
        for w in [&this.width, &this.height, &this.count] {
            let this = this.clone();
            w.connect_value_changed(move |_| this.update_cost());
        }
        {
            let this = this.clone();
            this.models.clone().connect_selected_notify(move |_| this.update_cost());
        }
        {
            let this = this.clone();
            generate.connect_clicked(move |_| this.start());
        }
        {
            let window = window.clone();
            let this = this.clone();
            close.connect_clicked(move |_| { if let Some(s) = this.running.borrow().as_ref() { if let Ok(mut s) = s.lock() { s.cancel = true; } } window.close(); });
        }
        this.update_cost();
        window.present();
    }

    fn model(&self) -> Option<genfill::Model> {
        genfill::generate_models().get(self.models.selected() as usize).cloned()
    }

    fn update_cost(self: &Rc<Self>) {
        let Some(model) = self.model() else { return };
        let (w, h, n) = (self.width.value() as usize, self.height.value() as usize, self.count.value() as u32);
        let text = if genfill::is_local(&model.id) {
            let (fw, fh) = crate::comfy::fit_pixels(w, h);
            if (fw, fh) != (w, h) { format!("Free, on this machine. {w}x{h} is more than the card carries, so it will make {fw}x{fh} and scale it up to fit.") }
            else { "Free, on this machine. Nothing leaves it.".into() }
        } else {
            match genfill::estimate(&model, w, h, n) {
                Some(c) => format!("About ${c:.2} at fal for {n} picture{}.", if n == 1 { "" } else { "s" }),
                None => "Billed to your fal account.".into(),
            }
        };
        self.cost.set_label(&text);
    }

    fn prompt_text(&self) -> String {
        let b = self.prompt.buffer();
        b.text(&b.start_iter(), &b.end_iter(), false).to_string()
    }

    fn start(self: &Rc<Self>) {
        if self.running.borrow().is_some() { return }
        let Some(model) = self.model() else { return };
        let prompt = self.prompt_text();
        if prompt.trim().is_empty() { self.status.set_label("Describe the picture first."); return }
        let key = genfill::key();
        let local = genfill::is_local(&model.id);
        if key.is_none() && !local { self.status.set_label("No fal.ai key found. Generative Fill lets you enter one."); return }
        if local && !crate::comfy::available(&crate::comfy::config().host) {
            self.status.set_label("No ComfyUI is answering. Start it, or pick a fal model.");
            return;
        }

        let (w, h) = (self.width.value() as usize, self.height.value() as usize);
        let count = self.count.value() as i64;
        let transparent = self.transparent.is_active();
        // GPT Image is the only fal family that returns a real alpha channel; locally it is asked for
        // in words. Either way the id the body is shaped for is the one that runs.
        let id = if transparent && !local && !model.id.starts_with("openai/") { genfill::resolve_model("gpt image", false) } else { model.id.clone() };
        let body = genfill::agent_body(&id, &prompt, None, w, h, count, transparent);

        let shared = Arc::new(Mutex::new(Shared { status: "Starting…".into(), done: None, cancel: false }));
        *self.running.borrow_mut() = Some(shared.clone());
        self.generate.set_sensitive(false);
        {
            let shared = shared.clone();
            std::thread::spawn(move || {
                let progress = |t: &str| { if let Ok(mut s) = shared.lock() { s.status = t.to_string(); } };
                let cancelled = || shared.lock().map(|s| s.cancel).unwrap_or(false);
                let outcome = if local {
                    crate::comfy::Comfy::new().run_agent(&body, transparent, &progress, &cancelled)
                } else {
                    genfill::Fal { key: key.unwrap_or_default() }.run(&id, body, &progress, &cancelled)
                }.map_err(|e| format!("{e:#}"));
                if let Ok(mut s) = shared.lock() { s.done = Some(outcome); }
            });
        }

        let this = self.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            let (status, done) = shared.lock().map(|s| (s.status.clone(), s.done.clone())).unwrap_or_default();
            match done {
                None => { this.status.set_label(&status); glib::ControlFlow::Continue }
                Some(outcome) => {
                    *this.running.borrow_mut() = None;
                    this.generate.set_sensitive(true);
                    match outcome {
                        Ok(images) => {
                            this.status.set_label(&format!("{} result{}. Click one to add it as a layer.", images.len(), if images.len() == 1 { "" } else { "s" }));
                            this.show(images);
                        }
                        Err(e) => this.status.set_label(&e),
                    }
                    glib::ControlFlow::Break
                }
            }
        });
    }

    fn show(self: &Rc<Self>, images: Vec<Vec<u8>>) {
        while let Some(c) = self.results.first_child() { self.results.remove(&c); }
        for (i, png) in images.iter().enumerate() {
            let button = gtk::Button::builder().has_frame(false).tooltip_text(format!("Add variation {} as a layer", i + 1)).build();
            match gdk::Texture::from_bytes(&glib::Bytes::from(png)) {
                Ok(texture) => button.set_child(Some(&gtk::Picture::builder().paintable(&texture).content_fit(gtk::ContentFit::Contain).width_request(90).height_request(90).build())),
                Err(_) => button.set_label("?"),
            }
            let this = self.clone();
            button.connect_clicked(move |_| this.pick(i));
            self.results.insert(&button, -1);
        }
        let only = images.len() == 1;
        *self.images.borrow_mut() = images;
        // One result needs no choosing.
        if only { self.pick(0); }
    }

    /// The picture as a new layer, centered on the canvas at the size that was asked for, keeping its
    /// own proportions: a model may return something close to, not exactly, the request.
    fn pick(self: &Rc<Self>, index: usize) {
        let Some(png) = self.images.borrow().get(index).cloned() else { return };
        let result = (|| -> anyhow::Result<uuid::Uuid> {
            let (surface, iw, ih) = Document::decode_image_bytes(&png)?;
            let mut d = self.doc.borrow_mut();
            let (cw, ch) = (d.document.width() as f64, d.document.height() as f64);
            let (pw, ph) = (self.width.value(), self.height.value());
            let scale = (pw / iw as f64).min(ph / ih as f64);
            let (fw, fh) = (iw as f64 * scale, ih as f64 * scale);
            let (x, y) = ((cw - fw) / 2.0, (ch - fh) / 2.0);
            let dd = &mut d.document;
            dd.begin_edit("Generate Image");
            let landed = dd.add_image_surface(surface, "Generated", (x, y), (fw, fh));
            match &landed { Ok(_) => dd.end_edit(), Err(_) => dd.abort_edit() }
            landed
        })();
        match result { Ok(_) => (self.finished)(), Err(e) => self.status.set_label(&format!("{e:#}")) }
    }
}
