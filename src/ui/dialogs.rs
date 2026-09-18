//! Small dialogs: New Canvas, Canvas Size, Image Size, Expand / Contract, Rename, JPEG export, and the
//! unsaved-changes prompt. Each builds a transient window with a grid of fields and OK / Cancel.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

fn dialog(parent: &gtk::Window, title: &str) -> (gtk::Window, gtk::Grid, gtk::Button) {
    let window = gtk::Window::builder().title(title).transient_for(parent).modal(true).resizable(false).default_width(340).build();
    window.set_application(parent.application().as_ref());
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_top(14).margin_bottom(14).margin_start(14).margin_end(14).build();
    let grid = gtk::Grid::builder().row_spacing(8).column_spacing(10).build();
    content.append(&grid);
    let buttons = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).halign(gtk::Align::End).build();
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::builder().label("OK").css_classes(["suggested-action"]).build();
    buttons.append(&cancel);
    buttons.append(&ok);
    content.append(&buttons);
    window.set_child(Some(&content));
    { let w = window.clone(); cancel.connect_clicked(move |_| w.close()); }
    window.set_default_widget(Some(&ok));
    (window, grid, ok)
}

fn spin(grid: &gtk::Grid, row: i32, label: &str, low: f64, high: f64, step: f64, value: f64, digits: u32) -> gtk::SpinButton {
    grid.attach(&gtk::Label::builder().label(label).xalign(0.0).build(), 0, row, 1, 1);
    let s = gtk::SpinButton::with_range(low, high, step);
    s.set_digits(digits);
    s.set_value(value);
    s.set_hexpand(true);
    grid.attach(&s, 1, row, 1, 1);
    s
}

/// Width, height and resolution for a new document.
pub fn new_canvas(parent: &gtk::Window, done: impl Fn(i32, i32, f64) + 'static) {
    let (window, grid, ok) = dialog(parent, "New Canvas");
    let width = spin(&grid, 0, "Width (px)", 1.0, 30000.0, 1.0, 1920.0, 0);
    let height = spin(&grid, 1, "Height (px)", 1.0, 30000.0, 1.0, 1080.0, 0);
    let resolution = spin(&grid, 2, "Resolution (ppi)", 1.0, 9600.0, 1.0, 72.0, 0);
    let w = window.clone();
    ok.connect_clicked(move |_| { done(width.value() as i32, height.value() as i32, resolution.value()); w.close(); });
    window.present();
}

/// Canvas Size: new dimensions, an anchor, and an optional fill for the new area.
pub fn canvas_size(parent: &gtk::Window, current: (i32, i32), done: impl Fn(i32, i32, usize, Option<[f64; 3]>) + 'static) {
    let (window, grid, ok) = dialog(parent, "Canvas Size");
    let width = spin(&grid, 0, "Width (px)", 1.0, 30000.0, 1.0, current.0 as f64, 0);
    let height = spin(&grid, 1, "Height (px)", 1.0, 30000.0, 1.0, current.1 as f64, 0);
    grid.attach(&gtk::Label::builder().label("Anchor").xalign(0.0).valign(gtk::Align::Start).build(), 0, 2, 1, 1);
    let anchors = gtk::Grid::builder().row_spacing(2).column_spacing(2).build();
    let chosen = Rc::new(Cell::new(4usize));
    let mut buttons = Vec::new();
    let mut first: Option<gtk::ToggleButton> = None;
    for i in 0..9 {
        let b = gtk::ToggleButton::builder().label(["↖", "↑", "↗", "←", "·", "→", "↙", "↓", "↘"][i]).active(i == 4).width_request(34).build();
        if let Some(f) = &first { b.set_group(Some(f)); } else { first = Some(b.clone()); }
        let chosen = chosen.clone();
        b.connect_toggled(move |b| { if b.is_active() { chosen.set(i); } });
        anchors.attach(&b, (i % 3) as i32, (i / 3) as i32, 1, 1);
        buttons.push(b);
    }
    grid.attach(&anchors, 1, 2, 1, 1);
    let fill = gtk::CheckButton::with_label("Fill new area with");
    let color = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    color.set_rgba(&gdk::RGBA::WHITE);
    let fill_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    fill_row.append(&fill);
    fill_row.append(&color);
    grid.attach(&fill_row, 0, 3, 2, 1);
    let w = window.clone();
    ok.connect_clicked(move |_| {
        let c = color.rgba();
        let fill = fill.is_active().then_some([c.red() as f64, c.green() as f64, c.blue() as f64]);
        done(width.value() as i32, height.value() as i32, chosen.get(), fill);
        w.close();
    });
    window.present();
}

/// Image Size: new dimensions with the proportions kept unless unlocked, and resolution.
pub fn image_size(parent: &gtk::Window, current: (i32, i32, f64), done: impl Fn(i32, i32, f64, crate::format::Sampling) + 'static) {
    let (window, grid, ok) = dialog(parent, "Image Size");
    let width = spin(&grid, 0, "Width (px)", 1.0, 30000.0, 1.0, current.0 as f64, 0);
    let height = spin(&grid, 1, "Height (px)", 1.0, 30000.0, 1.0, current.1 as f64, 0);
    let resolution = spin(&grid, 2, "Resolution (ppi)", 1.0, 9600.0, 1.0, current.2, 0);
    let lock = gtk::CheckButton::builder().label("Keep proportions").active(true).build();
    grid.attach(&lock, 1, 3, 1, 1);
    grid.attach(&gtk::Label::builder().label("Resample").xalign(0.0).build(), 0, 4, 1, 1);
    let sampling = gtk::DropDown::from_strings(&["High quality", "Smooth", "Nearest"]);
    grid.attach(&sampling, 1, 4, 1, 1);
    let ratio = current.1 as f64 / current.0 as f64;
    let syncing = Rc::new(Cell::new(false));
    {
        let (h, lock1, syncing1) = (height.clone(), lock.clone(), syncing.clone());
        width.connect_value_changed(move |w| { if syncing1.get() || !lock1.is_active() { return; } syncing1.set(true); h.set_value((w.value() * ratio).round().max(1.0)); syncing1.set(false); });
        let (w, lock2, syncing2) = (width.clone(), lock.clone(), syncing.clone());
        height.connect_value_changed(move |h| { if syncing2.get() || !lock2.is_active() { return; } syncing2.set(true); w.set_value((h.value() / ratio).round().max(1.0)); syncing2.set(false); });
    }
    let win = window.clone();
    ok.connect_clicked(move |_| {
        let s = match sampling.selected() { 1 => crate::format::Sampling::Smooth, 2 => crate::format::Sampling::Nearest, _ => crate::format::Sampling::High };
        done(width.value() as i32, height.value() as i32, resolution.value(), s);
        win.close();
    });
    window.present();
}

/// One pixel amount, for Expand / Contract.
pub fn amount(parent: &gtk::Window, title: &str, label: &str, done: impl Fn(i32) + 'static) {
    let (window, grid, ok) = dialog(parent, title);
    let value = spin(&grid, 0, label, 1.0, 500.0, 1.0, 1.0, 0);
    let w = window.clone();
    ok.connect_clicked(move |_| { done(value.value() as i32); w.close(); });
    window.present();
}

/// A text field, for renaming.
pub fn text(parent: &gtk::Window, title: &str, label: &str, initial: &str, done: impl Fn(String) + 'static) {
    let (window, grid, ok) = dialog(parent, title);
    grid.attach(&gtk::Label::builder().label(label).xalign(0.0).build(), 0, 0, 1, 1);
    let entry = gtk::Entry::builder().text(initial).hexpand(true).activates_default(true).build();
    grid.attach(&entry, 1, 0, 1, 1);
    let (w, e) = (window.clone(), entry.clone());
    ok.connect_clicked(move |_| { done(e.text().to_string()); w.close(); });
    window.present();
    entry.grab_focus();
}

/// JPEG export: quality and background, with the encoded size and a preview of the result.
pub fn jpeg_export(parent: &gtk::Window, encode: Rc<dyn Fn(f64, [f64; 3]) -> anyhow::Result<Vec<u8>>>, done: impl Fn(f64, [f64; 3]) + 'static) {
    let (window, grid, ok) = dialog(parent, "Export JPEG");
    window.set_default_width(520);
    let quality = spin(&grid, 0, "Quality (%)", 1.0, 100.0, 1.0, 85.0, 0);
    grid.attach(&gtk::Label::builder().label("Background").xalign(0.0).build(), 0, 1, 1, 1);
    let color = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    color.set_rgba(&gdk::RGBA::WHITE);
    grid.attach(&color, 1, 1, 1, 1);
    let size = gtk::Label::builder().xalign(0.0).css_classes(["dim-label"]).build();
    grid.attach(&size, 0, 2, 2, 1);
    let picture = gtk::Picture::builder().content_fit(gtk::ContentFit::Contain).height_request(260).can_shrink(true).build();
    grid.attach(&picture, 0, 3, 2, 1);
    let refresh = {
        let (quality, color, size, picture, encode) = (quality.clone(), color.clone(), size.clone(), picture.clone(), encode.clone());
        Rc::new(move || {
            let c = color.rgba();
            match encode(quality.value() / 100.0, [c.red() as f64, c.green() as f64, c.blue() as f64]) {
                Ok(bytes) => {
                    size.set_label(&format!("{:.1} KB", bytes.len() as f64 / 1024.0));
                    let texture = gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)).ok();
                    picture.set_paintable(texture.as_ref());
                }
                Err(error) => size.set_label(&format!("{error:#}")),
            }
        })
    };
    { let r = refresh.clone(); quality.connect_value_changed(move |_| r()); }
    { let r = refresh.clone(); color.connect_rgba_notify(move |_| r()); }
    refresh();
    let w = window.clone();
    ok.connect_clicked(move |_| { let c = color.rgba(); done(quality.value() / 100.0, [c.red() as f64, c.green() as f64, c.blue() as f64]); w.close(); });
    window.present();
}

/// Save / Discard / Cancel before closing something with unsaved changes. `choice` gets 0 save, 1 discard.
pub fn unsaved(parent: &gtk::Window, title: &str, choice: impl Fn(usize) + 'static) {
    let alert = gtk::AlertDialog::builder().message(format!("Save changes to \"{title}\"?")).detail("Your changes will be lost if you don't save them.")
        .buttons(["Save", "Don't Save", "Cancel"]).default_button(0).cancel_button(2).modal(true).build();
    alert.choose(Some(parent), gio::Cancellable::NONE, move |result| { if let Ok(i) = result { if i < 2 { choice(i as usize); } } });
}

/// A file chooser for saving; `done` gets the chosen path with `extension` ensured.
pub fn save_as(parent: &gtk::Window, title: &str, initial: &str, extension: &'static str, done: impl Fn(std::path::PathBuf) + 'static) {
    let dialog = gtk::FileDialog::builder().title(title).initial_name(format!("{initial}.{extension}")).modal(true).build();
    dialog.save(Some(parent), gio::Cancellable::NONE, move |result| {
        let Ok(file) = result else { return };
        let Some(mut path) = file.path() else { return };
        if path.extension().is_none_or(|e| e != extension) { path.set_extension(extension); }
        done(path);
    });
}

/// A file chooser for images to import.
pub fn open_image(parent: &gtk::Window, done: impl Fn(std::path::PathBuf) + 'static) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Images (PNG, JPEG, TIFF)"));
    for pattern in ["*.png", "*.PNG", "*.jpg", "*.jpeg", "*.JPG", "*.JPEG", "*.tif", "*.tiff", "*.TIF", "*.TIFF"] { filter.add_pattern(pattern); }
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder().title("Import Image").modal(true).filters(&filters).build();
    dialog.open(Some(parent), gio::Cancellable::NONE, move |result| { if let Ok(file) = result { if let Some(path) = file.path() { done(path); } } });
}

/// A file chooser for Photoshop files.
pub fn open_psd(parent: &gtk::Window, done: impl Fn(std::path::PathBuf) + 'static) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Photoshop (PSD)"));
    for pattern in ["*.psd", "*.PSD"] { filter.add_pattern(pattern); }
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder().title("Open PSD").modal(true).filters(&filters).build();
    dialog.open(Some(parent), gio::Cancellable::NONE, move |result| { if let Ok(file) = result { if let Some(path) = file.path() { done(path); } } });
}

pub fn is_project(path: &Path) -> bool { path.is_dir() && path.join("manifest.json").exists() }
