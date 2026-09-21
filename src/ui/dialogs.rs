//! Small dialogs: New Canvas, Canvas Size, Image Size, Expand / Contract, Rename, JPEG export, and the
//! unsaved-changes prompt. Each builds a transient window with a grid of fields and OK / Cancel.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

/// A window over the canvas: its own title bar, and a body that drags the window from anywhere blank, so
/// it can be moved off the picture (`gtk::WindowHandle`). `content` becomes its body.
pub fn floating(parent: &gtk::Window, title: &str, modal: bool, width: i32, content: &impl IsA<gtk::Widget>) -> gtk::Window {
    let window = gtk::Window::builder().title(title).transient_for(parent).modal(modal).resizable(false).default_width(width).build();
    window.set_application(parent.application().as_ref());
    { let keys = gtk::EventControllerKey::new(); let w = window.clone(); keys.connect_key_pressed(move |_, key, _, _| { if key == gtk::gdk::Key::Escape { w.close(); glib::Propagation::Stop } else { glib::Propagation::Proceed } }); window.add_controller(keys); }
    let header = gtk::HeaderBar::builder().show_title_buttons(true).build();
    header.set_title_widget(Some(&gtk::Label::builder().label(title).css_classes(["title", "dialog-title"]).build()));
    window.set_titlebar(Some(&header));
    window.set_child(Some(&gtk::WindowHandle::builder().child(content).build()));
    window
}

fn dialog(parent: &gtk::Window, title: &str) -> (gtk::Window, gtk::Grid, gtk::Button) {
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_top(14).margin_bottom(14).margin_start(14).margin_end(14).build();
    let grid = gtk::Grid::builder().row_spacing(8).column_spacing(10).build();
    content.append(&grid);
    let buttons = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).halign(gtk::Align::End).build();
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::builder().label("OK").css_classes(["suggested-action"]).build();
    buttons.append(&cancel);
    buttons.append(&ok);
    content.append(&buttons);
    let window = floating(parent, title, true, 340, &content);
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
    s.set_alignment(0.5);
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

/// Edit > Stroke: width, where the line sits on the selection edge, and opacity; the color is the foreground.
pub fn stroke(parent: &gtk::Window, done: impl Fn(f64, u32, f64) + 'static) {
    let (window, grid, ok) = dialog(parent, "Stroke");
    let width = spin(&grid, 0, "Width (px)", 1.0, 250.0, 1.0, 3.0, 0);
    grid.attach(&gtk::Label::builder().label("Location").xalign(0.0).build(), 0, 1, 1, 1);
    let position = gtk::DropDown::from_strings(&["Inside", "Center", "Outside"]);
    position.set_selected(1);
    grid.attach(&position, 1, 1, 1, 1);
    let opacity = spin(&grid, 2, "Opacity %", 1.0, 100.0, 1.0, 100.0, 0);
    grid.attach(&gtk::Label::builder().label("Painted in the foreground color on the active layer.").xalign(0.0).css_classes(["dim-label"]).build(), 0, 3, 2, 1);
    let w = window.clone();
    ok.connect_clicked(move |_| { done(width.value(), position.selected(), opacity.value() / 100.0); w.close(); });
    window.present();
}

/// Select > Color Range: how far from the foreground color still counts, and what to sample.
pub fn color_range(parent: &gtk::Window, done: impl Fn(f64, bool) + 'static) {
    let (window, grid, ok) = dialog(parent, "Color Range");
    let fuzziness = spin(&grid, 0, "Fuzziness", 0.0, 200.0, 1.0, 40.0, 0);
    let all = gtk::CheckButton::builder().label("Sample all layers").active(true).build();
    grid.attach(&all, 1, 1, 1, 1);
    grid.attach(&gtk::Label::builder().label("Selects every pixel near the foreground color; pick it with the Eyedropper first.").xalign(0.0).wrap(true).css_classes(["dim-label"]).build(), 0, 2, 2, 1);
    let w = window.clone();
    ok.connect_clicked(move |_| { done(fuzziness.value(), all.is_active()); w.close(); });
    window.present();
}

/// Edit > Fill with Pattern: which saved pattern, at what scale and opacity.
pub fn pattern_fill(parent: &gtk::Window, names: &[String], done: impl Fn(String, f64, f64) + 'static) {
    let (window, grid, ok) = dialog(parent, "Fill with Pattern");
    grid.attach(&gtk::Label::builder().label("Pattern").xalign(0.0).build(), 0, 0, 1, 1);
    let list = gtk::DropDown::from_strings(&names.iter().map(String::as_str).collect::<Vec<_>>());
    grid.attach(&list, 1, 0, 1, 1);
    let scale = spin(&grid, 1, "Scale %", 5.0, 2000.0, 1.0, 100.0, 0);
    let opacity = spin(&grid, 2, "Opacity %", 1.0, 100.0, 1.0, 100.0, 0);
    let names: Vec<String> = names.to_vec();
    let w = window.clone();
    ok.connect_clicked(move |_| { if let Some(name) = names.get(list.selected() as usize) { done(name.clone(), scale.value() / 100.0, opacity.value() / 100.0); } w.close(); });
    window.present();
}

/// File > Export Sizes: which sizes, how the picture meets each frame, the format, and where to write.
pub fn export_sizes(parent: &gtk::Window, title: &str, done: impl Fn(Vec<crate::export_sizes::SizePreset>, crate::export_sizes::Fit, crate::export_sizes::Format, [f64; 3], Option<std::path::PathBuf>) + 'static) {
    use crate::export_sizes::{Fit, Format, SizePreset};
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).margin_top(14).margin_bottom(14).margin_start(14).margin_end(14).build();
    content.append(&gtk::Label::builder().label("Sizes").xalign(0.0).css_classes(["heading"]).build());
    let flow = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).column_spacing(4).row_spacing(2).max_children_per_line(3).min_children_per_line(2).homogeneous(true).build();
    let presets = crate::export_sizes::presets();
    let checks: Vec<gtk::CheckButton> = presets.iter().map(|p| { let c = gtk::CheckButton::builder().label(format!("{} ({}x{})", p.name, p.width, p.height)).build(); flow.insert(&c, -1); c }).collect();
    content.append(&flow);
    let custom_row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).build();
    let custom = gtk::CheckButton::builder().label("Custom").build();
    let cw = gtk::SpinButton::with_range(1.0, 30_000.0, 1.0);
    cw.set_value(1080.0);
    let ch = gtk::SpinButton::with_range(1.0, 30_000.0, 1.0);
    ch.set_value(1080.0);
    custom_row.append(&custom); custom_row.append(&cw); custom_row.append(&gtk::Label::new(Some("x"))); custom_row.append(&ch);
    content.append(&custom_row);
    let grid = gtk::Grid::builder().row_spacing(8).column_spacing(10).build();
    grid.attach(&gtk::Label::builder().label("Fit").xalign(0.0).build(), 0, 0, 1, 1);
    let fit = gtk::DropDown::from_strings(&["Reframe: the picture fills, elements keep their places", "Fill: scale to cover and crop the overflow", "Pad: scale to fit and fill the rest with a color"]);
    grid.attach(&fit, 1, 0, 2, 1);
    grid.attach(&gtk::Label::builder().label("Format").xalign(0.0).build(), 0, 1, 1, 1);
    let format = gtk::DropDown::from_strings(&["PNG", "JPEG"]);
    grid.attach(&format, 1, 1, 1, 1);
    let quality = gtk::SpinButton::with_range(1.0, 100.0, 1.0);
    quality.set_value(90.0);
    quality.set_tooltip_text(Some("JPEG quality"));
    grid.attach(&quality, 2, 1, 1, 1);
    grid.attach(&gtk::Label::builder().label("Background").xalign(0.0).build(), 0, 2, 1, 1);
    let background = super::color_wheel::ColorButton::new([1.0; 3], std::rc::Rc::new(|_| {}));
    background.widget.set_tooltip_text(Some("Behind the picture when padding, and under JPEGs"));
    grid.attach(&background.widget, 1, 2, 1, 1);
    grid.attach(&gtk::Label::builder().label("Folder").xalign(0.0).build(), 0, 3, 1, 1);
    let folder_label = gtk::Label::builder().label("(not chosen)").xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::Middle).hexpand(true).build();
    let choose = gtk::Button::with_label("Choose…");
    grid.attach(&folder_label, 1, 3, 1, 1);
    grid.attach(&choose, 2, 3, 1, 1);
    let boards = gtk::CheckButton::builder().label("Make artboards in this document instead of files (fix them by hand, then File > Export Artboards)").build();
    grid.attach(&boards, 0, 4, 3, 1);
    content.append(&grid);
    let status = gtk::Label::builder().xalign(0.0).css_classes(["dim-label"]).wrap(true).build();
    content.append(&status);
    let buttons = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).halign(gtk::Align::End).build();
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::builder().label("Export").css_classes(["suggested-action"]).build();
    buttons.append(&cancel); buttons.append(&ok);
    content.append(&buttons);
    let window = floating(parent, &format!("Export Sizes: {title}"), false, 560, &content);
    let folder: std::rc::Rc<std::cell::RefCell<Option<std::path::PathBuf>>> = std::rc::Rc::new(std::cell::RefCell::new(None));
    {
        let (folder, folder_label, window) = (folder.clone(), folder_label.clone(), window.clone());
        choose.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::builder().title("Export into").modal(true).build();
            let (folder, folder_label) = (folder.clone(), folder_label.clone());
            dialog.select_folder(Some(&window), gio::Cancellable::NONE, move |result| { if let Ok(f) = result { if let Some(p) = f.path() { folder_label.set_label(&p.display().to_string()); *folder.borrow_mut() = Some(p); } } });
        });
    }
    { let w = window.clone(); cancel.connect_clicked(move |_| w.close()); }
    {
        let (w, folder, status) = (window.clone(), folder.clone(), status.clone());
        let presets = presets.clone();
        ok.connect_clicked(move |_| {
            let mut sizes: Vec<SizePreset> = checks.iter().zip(&presets).filter(|(c, _)| c.is_active()).map(|(_, p)| p.clone()).collect();
            if custom.is_active() { sizes.push(SizePreset { name: "Custom".into(), width: cw.value() as i32, height: ch.value() as i32 }); }
            if sizes.is_empty() { status.set_label("Tick at least one size."); return; }
            let as_boards = boards.is_active();
            let folder = folder.borrow().clone();
            if !as_boards && folder.is_none() { status.set_label("Choose a folder to export into, or make artboards."); return; }
            let fit = match fit.selected() { 1 => Fit::Fill, 2 => Fit::Pad, _ => Fit::Reframe };
            let format = if format.selected() == 1 { Format::Jpeg(quality.value() / 100.0) } else { Format::Png };
            done(sizes, fit, format, background.color(), if as_boards { None } else { folder });
            w.close();
        });
    }
    window.present();
}

/// Edit > History: the steps behind and ahead; clicking one moves the document there.
pub fn history(parent: &gtk::Window, doc: super::DocRef, refresh: std::rc::Rc<dyn Fn()>) {
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).margin_top(10).margin_bottom(10).margin_start(10).margin_end(10).build();
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).css_classes(["navigation-sidebar"]).build();
    let scroller = gtk::ScrolledWindow::builder().child(&list).min_content_height(280).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
    content.append(&gtk::Label::builder().label("Click a step to go back to it; the steps after it stay until you make a new edit.").xalign(0.0).wrap(true).css_classes(["dim-label"]).build());
    content.append(&scroller);
    let window = floating(parent, "History", false, 300, &content);
    let syncing = std::rc::Rc::new(std::cell::Cell::new(false));
    // Rebuilds the list from the document: the past, the present, and the steps ahead, dimmed.
    let rebuild: std::rc::Rc<dyn Fn()> = {
        let (list, doc, syncing) = (list.clone(), doc.clone(), syncing.clone());
        std::rc::Rc::new(move || {
            syncing.set(true);
            while let Some(child) = list.first_child() { list.remove(&child); }
            let (past, future) = doc.borrow().document.history_names();
            let row = |text: &str, dim: bool| { let l = gtk::Label::builder().label(text).xalign(0.0).margin_start(6).margin_end(6).margin_top(3).margin_bottom(3).build(); if dim { l.add_css_class("dim-label"); } gtk::ListBoxRow::builder().child(&l).build() };
            list.append(&row("Open", false));
            for name in &past { list.append(&row(name, false)); }
            for name in &future { list.append(&row(name, true)); }
            list.select_row(list.row_at_index(past.len() as i32).as_ref());
            syncing.set(false);
        })
    };
    {
        let (doc, syncing, rebuild, refresh) = (doc.clone(), syncing.clone(), rebuild.clone(), refresh.clone());
        list.connect_row_selected(move |_, row| {
            if syncing.get() { return; }
            let Some(row) = row else { return };
            let target = row.index() as i64;
            let current = doc.borrow().document.history_names().0.len() as i64;
            if target == current { return; }
            doc.borrow_mut().document.step_history(target - current);
            refresh();
            rebuild();
        });
    }
    rebuild();
    // The document changes behind the window: keep the list current while it is open.
    let (doc2, rebuild2, window2) = (doc.clone(), rebuild.clone(), window.clone());
    let mut shown = doc.borrow().document.edit_serial;
    gtk::glib::timeout_add_local(std::time::Duration::from_millis(400), move || {
        if !window2.is_visible() { return gtk::glib::ControlFlow::Break; }
        let serial = doc2.borrow().document.edit_serial;
        if serial != shown { shown = serial; rebuild2(); }
        gtk::glib::ControlFlow::Continue
    });
    window.present();
}

/// Layer > New Artboard: a name, a size from the presets or typed, and a background.
pub fn new_artboard(parent: &gtk::Window, count: usize, done: impl Fn(String, i32, i32, Option<[f64; 3]>) + 'static) {
    let (window, grid, ok) = dialog(parent, "New Artboard");
    grid.attach(&gtk::Label::builder().label("Name").xalign(0.0).build(), 0, 0, 1, 1);
    let name = gtk::Entry::builder().text(format!("Artboard {}", count + 1)).hexpand(true).activates_default(true).build();
    grid.attach(&name, 1, 0, 2, 1);
    grid.attach(&gtk::Label::builder().label("Size").xalign(0.0).build(), 0, 1, 1, 1);
    let presets = crate::export_sizes::presets();
    let mut names: Vec<String> = presets.iter().map(|p| format!("{} ({}x{})", p.name, p.width, p.height)).collect();
    names.insert(0, "Custom".into());
    let preset = gtk::DropDown::from_strings(&names.iter().map(String::as_str).collect::<Vec<_>>());
    preset.set_selected(1);
    grid.attach(&preset, 1, 1, 2, 1);
    let width = spin(&grid, 2, "Width", 1.0, 30_000.0, 1.0, presets[0].width as f64, 0);
    let height = spin(&grid, 3, "Height", 1.0, 30_000.0, 1.0, presets[0].height as f64, 0);
    { let (width, height) = (width.clone(), height.clone()); let presets = presets.clone(); preset.connect_selected_notify(move |p| { if p.selected() >= 1 { if let Some(pr) = presets.get(p.selected() as usize - 1) { width.set_value(pr.width as f64); height.set_value(pr.height as f64); } } }); }
    { let preset = preset.clone(); width.connect_value_changed(move |_| { if preset.selected() != 0 { preset.set_selected(0); } }); }
    grid.attach(&gtk::Label::builder().label("Background").xalign(0.0).build(), 0, 4, 1, 1);
    let background = super::color_wheel::ColorButton::new([1.0; 3], std::rc::Rc::new(|_| {}));
    grid.attach(&background.widget, 1, 4, 1, 1);
    let transparent = gtk::CheckButton::builder().label("Transparent").build();
    grid.attach(&transparent, 2, 4, 1, 1);
    let w = window.clone();
    let entry = name.clone();
    ok.connect_clicked(move |_| { done(entry.text().to_string(), width.value() as i32, height.value() as i32, if transparent.is_active() { None } else { Some(background.color()) }); w.close(); });
    window.present();
    name.grab_focus();
}

/// A quality from 1 to 100.
pub fn quality(parent: &gtk::Window, title: &str, done: impl Fn(f64) + 'static) {
    let (window, grid, ok) = dialog(parent, title);
    let value = spin(&grid, 0, "Quality (100 is lossless)", 1.0, 100.0, 1.0, 80.0, 0);
    let w = window.clone();
    ok.connect_clicked(move |_| { done(value.value()); w.close(); });
    window.present();
}

/// An angle in degrees.
pub fn angle(parent: &gtk::Window, title: &str, label: &str, done: impl Fn(f64) + 'static) {
    let (window, grid, ok) = dialog(parent, title);
    let value = spin(&grid, 0, label, -360.0, 360.0, 0.1, 0.0, 1);
    let w = window.clone();
    ok.connect_clicked(move |_| { done(value.value()); w.close(); });
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
        // "Poster v1.2" keeps its dot: the extension is added, not swapped in.
        if path.extension().is_none_or(|e| !e.eq_ignore_ascii_case(extension)) { path = std::path::PathBuf::from(format!("{}.{extension}", path.display())); }
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

/// A file chooser for everything that opens as a document: images, Photoshop files, and a `.comp` package
/// picked by its `manifest.json` (a folder chooser for packages is under Open Project Folder).
pub fn open_file(parent: &gtk::Window, done: impl Fn(std::path::PathBuf) + 'static) {
    let all = gtk::FileFilter::new();
    all.set_name(Some("Images, Photoshop and Compositor files"));
    let images = gtk::FileFilter::new();
    images.set_name(Some("Images (PNG, JPEG, TIFF, GIF, WebP, BMP)"));
    for ext in super::IMAGE_EXTENSIONS { for f in [&all, &images] { f.add_pattern(&format!("*.{ext}")); f.add_pattern(&format!("*.{}", ext.to_uppercase())); } }
    let psd = gtk::FileFilter::new();
    psd.set_name(Some("Photoshop (PSD)"));
    for pattern in ["*.psd", "*.PSD"] { psd.add_pattern(pattern); all.add_pattern(pattern); }
    all.add_pattern("manifest.json");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    for f in [&all, &images, &psd] { filters.append(f); }
    let dialog = gtk::FileDialog::builder().title("Open").modal(true).filters(&filters).build();
    dialog.open(Some(parent), gio::Cancellable::NONE, move |result| { if let Ok(file) = result { if let Some(path) = file.path() { done(path); } } });
}

/// View > New Guide: vertical or horizontal, at a document position.
pub fn new_guide(parent: &gtk::Window, done: impl Fn(bool, f64) + 'static) {
    let (window, grid, ok) = dialog(parent, "New Guide");
    grid.attach(&gtk::Label::builder().label("Orientation").xalign(0.0).build(), 0, 0, 1, 1);
    let orientation = gtk::DropDown::from_strings(&["Vertical", "Horizontal"]);
    grid.attach(&orientation, 1, 0, 1, 1);
    let position = spin(&grid, 1, "Position (px)", -100000.0, 100000.0, 1.0, 0.0, 0);
    { let window = window.clone(); ok.connect_clicked(move |_| { done(orientation.selected() == 0, position.value()); window.close(); }); }
    window.present();
}

pub fn is_project(path: &Path) -> bool { path.is_dir() && path.join("manifest.json").exists() }

/// The fal.ai key, pasted once: kept in ~/.config/compositor/fal.key (mode 600). `done` runs after a save.
pub fn fal_key(parent: &gtk::Window, done: impl Fn() + 'static) {
    let (window, grid, ok) = dialog(parent, "fal.ai key");
    grid.attach(&gtk::Label::builder().label("Generative Fill, Expand, new pictures, upscaling and relighting run on fal.ai and are billed to your fal account, a few cents a picture.").wrap(true).max_width_chars(48).xalign(0.0).build(), 0, 0, 2, 1);
    let link = gtk::LinkButton::with_label("https://fal.ai/dashboard/keys", "Make a key at fal.ai/dashboard/keys");
    link.set_halign(gtk::Align::Start);
    grid.attach(&link, 0, 1, 2, 1);
    grid.attach(&gtk::Label::builder().label("Key").xalign(0.0).build(), 0, 2, 1, 1);
    let entry = gtk::PasswordEntry::builder().show_peek_icon(true).hexpand(true).build();
    grid.attach(&entry, 1, 2, 1, 1);
    let note = gtk::Label::builder().css_classes(["dim-label", "caption"]).xalign(0.0).wrap(true).max_width_chars(48).build();
    if crate::genfill::key().is_some() { note.set_label("A key is already saved; a new one replaces it."); }
    grid.attach(&note, 0, 3, 2, 1);
    ok.set_label("Save key");
    { let (w, entry, note) = (window.clone(), entry.clone(), note.clone()); ok.connect_clicked(move |_| { match crate::genfill::save_key(&entry.text()) { Ok(()) => { done(); w.close(); } Err(e) => note.set_label(&format!("{e:#}")) } }); }
    window.present();
}
