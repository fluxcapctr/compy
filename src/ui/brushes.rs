//! Brush presets: the app's two round tips and every sampled tip loaded from Photoshop `.abr` files, kept for
//! the session and remembered by path across launches (`~/.config/compositor/brushes.list`), with the picker
//! that shows them as thumbnails in the brush options.

use super::DocRef;
use crate::abr::Preset;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

thread_local! {
    static PRESETS: RefCell<Vec<Rc<Preset>>> = const { RefCell::new(Vec::new()) };
    static FILES: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    static LOADED: RefCell<bool> = const { RefCell::new(false) };
}

fn list_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".config"));
    base.join("compositor/brushes.list")
}

/// The presets loaded so far. The first time: the bundled set, every `.abr` in the user's brushes folder
/// (`~/.local/share/compositor/brushes`), then the files remembered from Load Brushes.
pub fn presets() -> Vec<Rc<Preset>> {
    let first = LOADED.with(|l| !l.replace(true));
    if first {
        PRESETS.with(|p| p.borrow_mut().extend(crate::brush_set::presets()));
        let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".local/share"));
        let is_brush = |p: &Path| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("abr") || e.eq_ignore_ascii_case("gbr") || e.eq_ignore_ascii_case("gih"));
        if let Ok(entries) = std::fs::read_dir(base.join("compositor/brushes")) {
            let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
            paths.sort();
            for p in paths {
                if p.is_dir() {
                    // A folder is one set, named after it.
                    let set = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let mut files: Vec<PathBuf> = std::fs::read_dir(&p).map(|d| d.flatten().map(|e| e.path()).filter(|f| is_brush(f)).collect()).unwrap_or_default();
                    files.sort();
                    for f in files { let _ = add_file_in_set(&f, Some(&set), false); }
                } else if is_brush(&p) && p.file_stem().is_none_or(|s| s != "Compositor Basics") {
                    // The bundled set is already in memory (with its jitter, which the file cannot carry).
                    let _ = add_file(&p, false);
                }
            }
        }
        if let Ok(text) = std::fs::read_to_string(list_path()) {
            for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) { let _ = add_file(Path::new(line), false); }
        }
    }
    PRESETS.with(|p| p.borrow().clone())
}

/// Loads a brush file's tips into the list (and remembers the file). Returns how many were added.
pub fn add_file(path: &Path, remember: bool) -> anyhow::Result<usize> { add_file_in_set(path, None, remember) }

/// `set` names the group the tips show under (a folder's name); otherwise the file's own.
pub fn add_file_in_set(path: &Path, set: Option<&str>, remember: bool) -> anyhow::Result<usize> {
    let gimp = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("gbr") || e.eq_ignore_ascii_case("gih"));
    let mut loaded = if gimp { crate::gbr::load(path)? } else { crate::abr::load(path)? };
    if let Some(set) = set { for p in loaded.iter_mut() { let mut owned = (**p).clone(); owned.set = set.to_string(); *p = Rc::new(owned); } }
    let count = loaded.len();
    PRESETS.with(|p| { let mut p = p.borrow_mut(); p.retain(|existing| !loaded.iter().any(|n| n.name == existing.name && n.set == existing.set)); p.extend(loaded); });
    if remember {
        FILES.with(|f| { let mut f = f.borrow_mut(); if !f.iter().any(|x| x == path) { f.push(path.to_path_buf()); } });
        let text: String = FILES.with(|f| f.borrow().iter().map(|p| p.to_string_lossy().to_string() + "\n").collect());
        if let Some(dir) = list_path().parent() { let _ = std::fs::create_dir_all(dir); }
        let _ = std::fs::write(list_path(), text);
    } else {
        FILES.with(|f| f.borrow_mut().push(path.to_path_buf()));
    }
    Ok(count)
}

/// A thumbnail of a tip (or the round tip at `hardness`), dark on the theme's ground.
fn thumbnail(preset: Option<&Rc<Preset>>, hardness: f64, size: i32) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder().content_width(size).content_height(size).build();
    let preset = preset.cloned();
    area.set_draw_func(move |area, cr, w, h| {
        let color = area.color();
        let (w, h) = (w as f64, h as f64);
        let inset = 3.0;
        let box_side = w.min(h) - inset * 2.0;
        match &preset {
            Some(p) => {
                let scale = box_side / p.width.max(p.height) as f64;
                let (pw, ph) = (p.width as f64 * scale, p.height as f64 * scale);
                let stride = cairo::Format::A8.stride_for_width(p.width as u32).unwrap_or(p.width as i32) as usize;
                let mut data = vec![0u8; stride * p.height];
                for y in 0..p.height { data[y * stride..y * stride + p.width].copy_from_slice(&p.pixels[y * p.width..(y + 1) * p.width]); }
                if let Ok(mask) = crate::raster::a8_from_data(p.width as i32, p.height as i32, data, stride as i32) {
                    cr.translate((w - pw) / 2.0, (h - ph) / 2.0);
                    cr.scale(scale, scale);
                    cr.set_source_rgba(color.red() as f64, color.green() as f64, color.blue() as f64, color.alpha() as f64);
                    let _ = cr.mask_surface(&mask, 0.0, 0.0);
                }
            }
            None => {
                let r = box_side / 2.0;
                if hardness >= 0.999 {
                    cr.set_source_rgba(color.red() as f64, color.green() as f64, color.blue() as f64, color.alpha() as f64);
                } else {
                    let g = cairo::RadialGradient::new(w / 2.0, h / 2.0, r * hardness, w / 2.0, h / 2.0, r);
                    g.add_color_stop_rgba(0.0, color.red() as f64, color.green() as f64, color.blue() as f64, 1.0);
                    g.add_color_stop_rgba(1.0, color.red() as f64, color.green() as f64, color.blue() as f64, 0.0);
                    let _ = cr.set_source(&g);
                }
                cr.arc(w / 2.0, h / 2.0, r, 0.0, std::f64::consts::TAU);
                let _ = cr.fill();
            }
        }
    });
    area
}

/// The picker: a button showing the current tip, opening a grid of every preset and a Load button.
pub struct BrushPicker {
    pub widget: gtk::MenuButton,
    doc: DocRef,
    swatch: gtk::Box,
    flow: gtk::FlowBox,
    sets: gtk::DropDown,
    changed: Rc<dyn Fn()>,
}

impl BrushPicker {
    pub fn new(doc: DocRef, changed: Rc<dyn Fn()>) -> Rc<BrushPicker> {
        let swatch = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let widget = gtk::MenuButton::builder().child(&swatch).tooltip_text("Brush tip: the round tips, and any loaded from Photoshop brush files (.abr)").build();
        let flow = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).max_children_per_line(6).min_children_per_line(4).column_spacing(4).row_spacing(4).homogeneous(true).build();
        let scroller = gtk::ScrolledWindow::builder().child(&flow).min_content_height(160).max_content_height(320).propagate_natural_height(true).min_content_width(300).hscrollbar_policy(gtk::PolicyType::Never).build();
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).margin_top(8).margin_bottom(8).margin_start(8).margin_end(8).build();
        let sets = gtk::DropDown::from_strings(&["All sets"]);
        sets.set_tooltip_text(Some("Show one set of brushes, or all of them"));
        content.append(&sets);
        content.append(&scroller);
        let load = gtk::Button::with_label("Load Brushes…");
        load.set_tooltip_text(Some("Add the sampled tips from a Photoshop .abr file; they are remembered for next time"));
        content.append(&load);
        let popover = gtk::Popover::builder().child(&content).build();
        widget.set_popover(Some(&popover));
        let picker = Rc::new(BrushPicker { widget, doc, swatch, flow, sets, changed });
        { let p = picker.clone(); picker.sets.connect_selected_notify(move |_| p.fill_grid()); }
        {
            let p = picker.clone();
            load.connect_clicked(move |b| {
                let filter = gtk::FileFilter::new();
                filter.set_name(Some("Brushes (.abr, .gbr, .gih)"));
                for p in ["*.abr", "*.ABR", "*.gbr", "*.GBR", "*.gih", "*.GIH"] { filter.add_pattern(p); }
                let filters = gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                let dialog = gtk::FileDialog::builder().title("Load Brushes").modal(true).filters(&filters).build();
                let window = b.root().and_downcast::<gtk::Window>();
                let (p, parent) = (p.clone(), window.clone());
                dialog.open(window.as_ref(), gio::Cancellable::NONE, move |result| {
                    let Ok(file) = result else { return };
                    let Some(path) = file.path() else { return };
                    match add_file(&path, true) {
                        Ok(_) => p.fill(),
                        Err(e) => { if let Some(w) = parent.as_ref() { gtk::AlertDialog::builder().message("Could not load the brushes").detail(format!("{e:#}")).modal(true).build().show(Some(w)); } }
                    }
                });
            });
        }
        picker.fill();
        picker.sync();
        picker
    }

    /// Rebuilds the set list and the grid.
    pub fn fill(self: &Rc<Self>) {
        let mut names: Vec<String> = vec!["All sets".into(), "Round".into()];
        for p in presets() { if !names.contains(&p.set) { names.push(p.set.clone()); } }
        let current = self.sets.selected();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        self.sets.set_model(Some(&gtk::StringList::new(&refs)));
        self.sets.set_selected(current.min(names.len() as u32 - 1));
        self.fill_grid();
    }

    /// The grid: the round tips and the loaded presets, or just the chosen set.
    fn fill_grid(self: &Rc<Self>) {
        while let Some(c) = self.flow.first_child() { self.flow.remove(&c); }
        let chosen = self.sets.selected_item().and_downcast::<gtk::StringObject>().map(|s| s.string().to_string()).unwrap_or_else(|| "All sets".into());
        let all = chosen == "All sets";
        let mut entries: Vec<(Option<Rc<Preset>>, f64, String)> = Vec::new();
        if all || chosen == "Round" { entries.push((None, 1.0, "Hard Round".into())); entries.push((None, 0.0, "Soft Round".into())); }
        for p in presets() { if all || p.set == chosen { let name = p.name.clone(); entries.push((Some(p), 1.0, name)); } }
        for (preset, hardness, name) in entries {
            let button = gtk::Button::builder().has_frame(false).tooltip_text(&name).build();
            button.set_child(Some(&thumbnail(preset.as_ref(), hardness, 44)));
            let this = self.clone();
            button.connect_clicked(move |b| {
                { let mut d = this.doc.borrow_mut(); d.brush.preset = preset.clone(); match &preset { None => { d.brush.hardness = hardness; d.brush.angle_jitter = 0.0; } Some(p) => { d.brush.spacing = Some(p.spacing / 100.0); d.brush.angle_jitter = p.jitter; } } }
                this.sync();
                (this.changed)();
                if let Some(popover) = b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() { popover.popdown(); }
            });
            self.flow.insert(&button, -1);
        }
    }

    /// Shows the current tip on the button.
    pub fn sync(&self) {
        while let Some(c) = self.swatch.first_child() { self.swatch.remove(&c); }
        let (preset, hardness) = { let d = self.doc.borrow(); (d.brush.preset.clone(), d.brush.hardness) };
        self.swatch.append(&thumbnail(preset.as_ref(), hardness, 26));
    }

    pub fn popup(&self) { let w = self.widget.clone(); glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || w.popup()); }
}
