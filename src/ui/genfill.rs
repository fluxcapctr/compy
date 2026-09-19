//! The Generative Fill panel: a prompt, a model, how many variations, Generate; results as thumbnails that
//! each land as (or replace) a masked layer over the selection. The service runs on a thread; the panel
//! polls it.

use super::DocRef;
use crate::genfill::{self, Backend};
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

struct Shared { status: String, done: Option<Result<Vec<Vec<u8>>, String>>, cancel: bool }

pub struct GenFill {
    doc: DocRef,
    window: gtk::Window,
    prompt: gtk::Entry,
    models: gtk::DropDown,
    count: gtk::SpinButton,
    composite: gtk::CheckButton,
    generate: gtk::Button,
    status: gtk::Label,
    results: gtk::FlowBox,
    /// The window the results cover, and the layer the first pick made (later picks replace its pixels).
    window_rect: Cell<Option<(i32, i32, i32, i32)>>,
    layer: Cell<Option<uuid::Uuid>>,
    images: RefCell<Vec<Vec<u8>>>,
    running: Rc<RefCell<Option<Arc<Mutex<Shared>>>>>,
    finished: Rc<dyn Fn()>,
}

impl GenFill {
    /// Opens the panel for the current selection (`expand` names it Generative Expand).
    pub fn open(parent: &gtk::Window, doc: DocRef, finished: Rc<dyn Fn()>, expand: bool) {
        let title = if expand { "Generative Expand" } else { "Generative Fill" };
        let window = gtk::Window::builder().title(title).transient_for(parent).modal(false).default_width(420).resizable(false).build();
        window.set_application(parent.application().as_ref());
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).margin_top(14).margin_bottom(14).margin_start(14).margin_end(14).build();
        let prompt = gtk::Entry::builder().placeholder_text("Describe what should appear (empty fills in the surroundings)").hexpand(true).build();
        content.append(&prompt);
        let row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).build();
        let list = genfill::models();
        let names: Vec<&str> = list.iter().map(|m| m.name.as_str()).collect();
        let models = gtk::DropDown::from_strings(&names);
        models.set_hexpand(true);
        models.set_tooltip_text(Some("The fal.ai model; edit ~/.config/compositor/genfill-models.json to add others"));
        row.append(&models);
        row.append(&gtk::Label::new(Some("Variations")));
        let count = gtk::SpinButton::with_range(1.0, 4.0, 1.0);
        count.set_value(2.0);
        row.append(&count);
        content.append(&row);
        let composite = gtk::CheckButton::builder().label("Send every visible layer as context (off: the active layer alone)").active(true).build();
        content.append(&composite);
        // The key, entered once here and kept in ~/.config/compositor/fal.key.
        let key_row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).visible(genfill::key().is_none()).build();
        let key_entry = gtk::PasswordEntry::builder().placeholder_text("fal.ai API key").hexpand(true).show_peek_icon(true).build();
        let key_save = gtk::Button::with_label("Save key");
        key_row.append(&key_entry);
        key_row.append(&key_save);
        content.append(&key_row);
        let results = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).max_children_per_line(4).min_children_per_line(2).column_spacing(6).row_spacing(6).homogeneous(true).build();
        content.append(&results);
        let status = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["dim-label"]).build();
        content.append(&status);
        let buttons = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).halign(gtk::Align::End).build();
        let cancel = gtk::Button::with_label("Cancel");
        let generate = gtk::Button::builder().label("Generate").css_classes(["suggested-action"]).build();
        let close = gtk::Button::with_label("Done");
        buttons.append(&cancel);
        buttons.append(&generate);
        buttons.append(&close);
        content.append(&buttons);
        window.set_child(Some(&content));
        let this = Rc::new(GenFill { doc, window: window.clone(), prompt, models, count, composite, generate, status, results, window_rect: Cell::new(None), layer: Cell::new(None), images: RefCell::new(Vec::new()), running: Rc::new(RefCell::new(None)), finished });
        match genfill::key() {
            Some(_) => this.status.set_label("Only the selection and a margin around it are sent to fal.ai."),
            None => { this.status.set_label("No fal.ai key yet: paste it above and press Save key (it is kept in ~/.config/compositor/fal.key)."); this.generate.set_sensitive(false); }
        }
        {
            let (t, entry, row) = (this.clone(), key_entry.clone(), key_row.clone());
            let save = move || {
                match genfill::save_key(&entry.text()) {
                    Ok(()) => { row.set_visible(false); t.generate.set_sensitive(true); t.status.set_label("Key saved to ~/.config/compositor/fal.key. Only the selection and a margin around it are sent to fal.ai."); }
                    Err(e) => t.status.set_label(&format!("{e:#}")),
                }
            };
            let s2 = save.clone();
            key_save.connect_clicked(move |_| s2());
            key_entry.connect_activate(move |_| save());
        }
        { let t = this.clone(); this.generate.connect_clicked(move |_| t.start()); }
        { let t = this.clone(); this.prompt.connect_activate(move |_| if t.generate.is_sensitive() { t.start() }); }
        { let t = this.clone(); cancel.connect_clicked(move |_| { if let Some(s) = t.running.borrow().as_ref() { if let Ok(mut s) = s.lock() { s.cancel = true; } } }); }
        { let w = window.clone(); close.connect_clicked(move |_| w.close()); }
        { let t = this.clone(); window.connect_close_request(move |_| { if let Some(s) = t.running.borrow().as_ref() { if let Ok(mut s) = s.lock() { s.cancel = true; } } glib::Propagation::Proceed }); }
        window.present();
        this.prompt.grab_focus();
    }

    fn start(self: &Rc<Self>) {
        if self.running.borrow().is_some() { return; }
        let Some(key) = genfill::key() else { self.status.set_label("No fal.ai key found."); return };
        let list = genfill::models();
        let Some(model) = list.get(self.models.selected() as usize).cloned() else { return };
        let inputs = { let mut d = self.doc.borrow_mut(); d.document.genfill_inputs(self.composite.is_active()) };
        let (image_png, mask_png, window, _) = match inputs { Ok(i) => i, Err(e) => { self.status.set_label(&format!("{e:#}")); return } };
        self.window_rect.set(Some(window));
        let request = genfill::Request { model, prompt: self.prompt.text().to_string(), count: self.count.value() as u32, seed: None, image_png, mask_png };
        let shared = Arc::new(Mutex::new(Shared { status: "Starting…".into(), done: None, cancel: false }));
        *self.running.borrow_mut() = Some(shared.clone());
        self.generate.set_sensitive(false);
        {
            let shared = shared.clone();
            std::thread::spawn(move || {
                let backend = genfill::Fal { key };
                let progress = |text: &str| { if let Ok(mut s) = shared.lock() { s.status = text.to_string(); } };
                let cancelled = || shared.lock().map(|s| s.cancel).unwrap_or(false);
                let outcome = backend.generate(&request, &progress, &cancelled).map_err(|e| format!("{e:#}"));
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
                        Ok(images) => { this.status.set_label(&format!("{} result{}. Click one to use it; click another to swap.", images.len(), if images.len() == 1 { "" } else { "s" })); this.show(images); }
                        Err(e) => this.status.set_label(&e),
                    }
                    glib::ControlFlow::Break
                }
            }
        });
    }

    /// Thumbnails of the results; picking one makes (or refills) the layer.
    fn show(self: &Rc<Self>, images: Vec<Vec<u8>>) {
        while let Some(c) = self.results.first_child() { self.results.remove(&c); }
        for (i, png) in images.iter().enumerate() {
            let button = gtk::Button::builder().has_frame(false).tooltip_text(format!("Use variation {}", i + 1)).build();
            match gdk::Texture::from_bytes(&glib::Bytes::from(png)) {
                Ok(texture) => button.set_child(Some(&gtk::Picture::builder().paintable(&texture).content_fit(gtk::ContentFit::Contain).width_request(90).height_request(90).build())),
                Err(_) => button.set_label("?"),
            }
            let this = self.clone();
            button.connect_clicked(move |_| this.pick(i));
            self.results.insert(&button, -1);
        }
        *self.images.borrow_mut() = images;
        if self.images.borrow().len() == 1 { self.pick(0); }
    }

    fn pick(self: &Rc<Self>, index: usize) {
        let Some(window) = self.window_rect.get() else { return };
        let Some(png) = self.images.borrow().get(index).cloned() else { return };
        let name = if self.window.title().is_some_and(|t| t.contains("Expand")) { "Generative Expand" } else { "Generative Fill" };
        let result = {
            let mut d = self.doc.borrow_mut();
            match self.layer.get() {
                Some(id) if d.document.has_layer(id) => d.document.replace_genfill(id, &png, window),
                _ => d.document.apply_genfill(&png, window, name).map(|id| { self.layer.set(Some(id)); }),
            }
        };
        match result { Ok(()) => (self.finished)(), Err(e) => self.status.set_label(&format!("{e:#}")) }
    }
}
