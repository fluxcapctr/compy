//! A color picker like Photoshop's: a big saturation/value square with a hue strip beside it, hex and
//! RGB fields, and the colors used lately, in a popover behind a swatch button.

use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const SIZE: i32 = 256;
const STRIP: i32 = 20;
const GAP: i32 = 10;
const RECENT: usize = 10;

thread_local! { static RECENTS: RefCell<Vec<[f64; 3]>> = const { RefCell::new(Vec::new()) }; }

pub fn hsv_to_rgb(h: f64, s: f64, v: f64) -> [f64; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let i = h.floor();
    let f = h - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match i as i32 { 0 => [v, t, p], 1 => [q, v, p], 2 => [p, v, t], 3 => [p, q, v], 4 => [t, p, v], _ => [v, p, q] }
}

pub fn rgb_to_hsv(rgb: [f64; 3]) -> (f64, f64, f64) {
    let (r, g, b) = (rgb[0], rgb[1], rgb[2]);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d <= 0.0 { 0.0 } else if max == r { 60.0 * ((g - b) / d).rem_euclid(6.0) } else if max == g { 60.0 * ((b - r) / d + 2.0) } else { 60.0 * ((r - g) / d + 4.0) };
    (h, if max <= 0.0 { 0.0 } else { d / max }, max)
}

pub fn hex(rgb: [f64; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", (rgb[0] * 255.0).round() as u8, (rgb[1] * 255.0).round() as u8, (rgb[2] * 255.0).round() as u8)
}

pub fn parse_hex(text: &str) -> Option<[f64; 3]> {
    let t = text.trim().trim_start_matches('#');
    let expanded: String = if t.len() == 3 { t.chars().flat_map(|c| [c, c]).collect() } else { t.to_string() };
    if expanded.len() != 6 { return None; }
    let v = u32::from_str_radix(&expanded, 16).ok()?;
    Some([((v >> 16) & 255) as f64 / 255.0, ((v >> 8) & 255) as f64 / 255.0, (v & 255) as f64 / 255.0])
}

/// A swatch button that opens the picker; `changed` gets every color the user settles on.
pub struct ColorButton {
    pub widget: gtk::MenuButton,
    swatch: gtk::DrawingArea,
    color: Rc<Cell<[f64; 3]>>,
}

impl ColorButton {
    pub fn new(initial: [f64; 3], changed: Rc<dyn Fn([f64; 3])>) -> Rc<ColorButton> {
        let color = Rc::new(Cell::new(initial));
        let swatch = gtk::DrawingArea::builder().content_width(34).content_height(18).build();
        { let color = color.clone(); swatch.set_draw_func(move |_, cr, w, h| { let c = color.get(); cr.set_source_rgb(c[0], c[1], c[2]); rounded(cr, 0.5, 0.5, w as f64 - 1.0, h as f64 - 1.0, 3.0); let _ = cr.fill_preserve(); cr.set_source_rgba(0.5, 0.5, 0.5, 0.6); cr.set_line_width(1.0); let _ = cr.stroke(); }); }
        let widget = gtk::MenuButton::builder().child(&swatch).tooltip_text("Brush color").build();
        let button = Rc::new(ColorButton { widget: widget.clone(), swatch: swatch.clone(), color: color.clone() });
        let popover = gtk::Popover::new();
        let picker = Picker::new(initial);
        popover.set_child(Some(&picker.widget));
        widget.set_popover(Some(&popover));
        {
            let (color, swatch, changed) = (color.clone(), swatch.clone(), changed.clone());
            picker.on_change(Rc::new(move |c| { color.set(c); swatch.queue_draw(); changed(c); }));
        }
        {
            // Opening shows the current color; closing files it under the recent ones.
            let (picker, shown) = (picker.clone(), color.clone());
            popover.connect_show(move |_| picker.set_color(shown.get()));
            let color = color.clone();
            popover.connect_closed(move |_| remember(color.get()));
        }
        button
    }

    pub fn set_color(&self, rgb: [f64; 3]) { self.color.set(rgb); self.swatch.queue_draw(); }

    /// A square swatch for the rail's overlapping palette.
    pub fn set_compact(&self) { self.swatch.set_content_width(20); self.swatch.set_content_height(20); self.widget.add_css_class("swatch"); }
    pub fn color(&self) -> [f64; 3] { self.color.get() }
}

/// Saved swatches, kept in `~/.config/compositor/swatches.json` as hex strings.
fn swatches_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default().join(".config"));
    base.join("compositor/swatches.json")
}
pub fn swatches() -> Vec<[f64; 3]> {
    std::fs::read_to_string(swatches_path()).ok().and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok()).map(|v| v.iter().filter_map(|h| parse_hex(h)).collect()).unwrap_or_default()
}
fn save_swatches(list: &[[f64; 3]]) {
    let path = swatches_path();
    if let Some(dir) = path.parent() { let _ = std::fs::create_dir_all(dir); }
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&list.iter().map(|c| hex(*c)).collect::<Vec<_>>()).unwrap_or_default());
}
pub fn add_swatch(c: [f64; 3]) { let mut list = swatches(); list.retain(|x| hex(*x) != hex(c)); list.push(c); if list.len() > 64 { list.remove(0); } save_swatches(&list); }
pub fn remove_swatch(c: [f64; 3]) { let mut list = swatches(); list.retain(|x| hex(*x) != hex(c)); save_swatches(&list); }

fn remember(c: [f64; 3]) {
    RECENTS.with(|r| { let mut r = r.borrow_mut(); r.retain(|x| hex(*x) != hex(c)); r.insert(0, c); r.truncate(RECENT); });
}

fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::PI;
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * PI / 2.0);
    cr.close_path();
}

/// The picker's contents: the square and hue strip, the hex and RGB fields, recents, swatches.
struct Picker {
    widget: gtk::Box,
    wheel: gtk::DrawingArea,
    entry: gtk::Entry,
    channels: [gtk::Entry; 3],
    recents: gtk::Box,
    saved: gtk::FlowBox,
    hsv: Cell<(f64, f64, f64)>,
    strip_image: RefCell<Option<cairo::ImageSurface>>,
    square_image: RefCell<Option<(f64, cairo::ImageSurface)>>,
    dragging: Cell<Option<bool>>,
    on_change: RefCell<Option<Rc<dyn Fn([f64; 3])>>>,
    syncing: Cell<bool>,
}

impl Picker {
    fn new(initial: [f64; 3]) -> Rc<Picker> {
        let widget = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).margin_top(8).margin_bottom(8).margin_start(8).margin_end(8).build();
        let wheel = gtk::DrawingArea::builder().content_width(SIZE + GAP + STRIP).content_height(SIZE).build();
        let entry = gtk::Entry::builder().max_length(7).width_chars(8).max_width_chars(8).css_classes(["monospace"]).tooltip_text("Hex, like #ff8800").build();
        let recents = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(4).build();
        // Hex on the left, R G B on the right, one row.
        let row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(6).build();
        row.append(&gtk::Label::builder().label("Hex").css_classes(["dim-label", "caption"]).build());
        row.append(&entry);
        let channels: [gtk::Entry; 3] = std::array::from_fn(|_| gtk::Entry::builder().max_length(3).width_chars(4).max_width_chars(4).xalign(0.5).input_purpose(gtk::InputPurpose::Digits).css_classes(["monospace"]).build());
        for (label, spin) in ["R", "G", "B"].iter().zip(&channels) {
            row.append(&gtk::Label::builder().label(*label).css_classes(["dim-label", "caption"]).margin_start(4).build());
            row.append(spin);
        }
        widget.append(&wheel);
        widget.append(&row);
        widget.append(&recents);
        // Swatches: colors kept on purpose, across sessions. Plus saves the current one; right-click removes.
        let saved_row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(6).build();
        saved_row.append(&gtk::Label::builder().label("Swatches").css_classes(["dim-label", "caption"]).build());
        let add = gtk::Button::builder().label("+").has_frame(false).tooltip_text("Save this color as a swatch; right-click a swatch to remove it").build();
        saved_row.append(&add);
        widget.append(&saved_row);
        let saved = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).column_spacing(2).row_spacing(2).max_children_per_line(12).min_children_per_line(1).build();
        widget.append(&saved);
        let picker = Rc::new(Picker { widget, wheel: wheel.clone(), entry: entry.clone(), channels: channels.clone(), recents, saved, hsv: Cell::new(rgb_to_hsv(initial)), strip_image: RefCell::new(None), square_image: RefCell::new(None), dragging: Cell::new(None), on_change: RefCell::new(None), syncing: Cell::new(false) });
        { let p = picker.clone(); wheel.set_draw_func(move |_, cr, w, h| p.draw(cr, w as f64, h as f64)); }
        let drag = gtk::GestureDrag::new();
        { let p = picker.clone(); drag.connect_drag_begin(move |_, x, y| { p.dragging.set(p.region(x, y)); p.pick(x, y); }); }
        { let p = picker.clone(); drag.connect_drag_update(move |g, dx, dy| { if let Some((x, y)) = g.start_point() { p.pick(x + dx, y + dy); } }); }
        { let p = picker.clone(); drag.connect_drag_end(move |_, _, _| p.dragging.set(None)); }
        wheel.add_controller(drag);
        { let p = picker.clone(); entry.connect_activate(move |e| { if let Some(rgb) = parse_hex(&e.text()) { p.set_color(rgb); p.emit(); } }); }
        { let p = picker.clone(); entry.connect_changed(move |e| { if p.syncing.get() { return; } if let Some(rgb) = parse_hex(&e.text()) { if e.text().len() == 7 { p.hsv.set(rgb_to_hsv(rgb)); p.square_image.borrow_mut().take(); p.sync_channels(); p.wheel.queue_draw(); p.emit(); } } }); }
        for field in &channels {
            let p = picker.clone();
            field.connect_changed(move |_| {
                if p.syncing.get() { return; }
                let values: Vec<f64> = p.channels.iter().filter_map(|e| e.text().trim().parse::<u32>().ok()).map(|v| v.min(255) as f64 / 255.0).collect();
                let Ok(rgb) = <[f64; 3]>::try_from(values) else { return };
                // Keep the hue when the typed color is gray, so the square does not jump.
                let (h, sat, v) = rgb_to_hsv(rgb);
                let old = p.hsv.get();
                p.hsv.set((if sat <= 0.0 || v <= 0.0 { old.0 } else { h }, sat, v));
                p.square_image.borrow_mut().take();
                p.syncing.set(true); p.entry.set_text(&hex(rgb)); p.syncing.set(false);
                p.wheel.queue_draw();
                p.emit();
            });
        }
        { let p = picker.clone(); add.connect_clicked(move |_| { add_swatch(p.color()); p.fill_swatches(); }); }
        picker.sync_entry();
        picker.fill_recents();
        picker.fill_swatches();
        picker
    }

    fn fill_swatches(self: &Rc<Self>) {
        while let Some(child) = self.saved.first_child() { self.saved.remove(&child); }
        for c in swatches() {
            let swatch = gtk::Button::builder().width_request(18).height_request(18).has_frame(false).tooltip_text(hex(c)).build();
            let area = gtk::DrawingArea::builder().content_width(16).content_height(16).build();
            area.set_draw_func(move |_, cr, w, h| { cr.set_source_rgb(c[0], c[1], c[2]); rounded(cr, 0.5, 0.5, w as f64 - 1.0, h as f64 - 1.0, 3.0); let _ = cr.fill_preserve(); cr.set_source_rgba(0.5, 0.5, 0.5, 0.6); cr.set_line_width(1.0); let _ = cr.stroke(); });
            swatch.set_child(Some(&area));
            { let p = self.clone(); swatch.connect_clicked(move |_| { p.set_color(c); p.emit(); }); }
            {
                let p = self.clone();
                let right = gtk::GestureClick::builder().button(3).build();
                right.connect_released(move |_, _, _, _| { remove_swatch(c); p.fill_swatches(); });
                swatch.add_controller(right);
            }
            self.saved.insert(&swatch, -1);
        }
    }

    fn on_change(&self, f: Rc<dyn Fn([f64; 3])>) { *self.on_change.borrow_mut() = Some(f); }
    fn color(&self) -> [f64; 3] { let (h, s, v) = self.hsv.get(); hsv_to_rgb(h, s, v) }
    fn set_color(self: &Rc<Self>, rgb: [f64; 3]) {
        // Keep the hue when the new color is gray, so the strip marker does not jump.
        let (h, s, v) = rgb_to_hsv(rgb);
        let old = self.hsv.get();
        self.hsv.set((if s <= 0.0 || v <= 0.0 { old.0 } else { h }, s, v));
        self.square_image.borrow_mut().take();
        self.sync_entry();
        self.fill_recents();
        self.wheel.queue_draw();
    }
    fn emit(&self) { if let Some(f) = self.on_change.borrow().as_ref() { f(self.color()); } }
    /// The hex field and the RGB fields, from the color.
    fn sync_entry(&self) { self.syncing.set(true); self.entry.set_text(&hex(self.color())); self.syncing.set(false); self.sync_channels(); }
    fn sync_channels(&self) {
        self.syncing.set(true);
        let c = self.color();
        for (field, v) in self.channels.iter().zip(c) { field.set_text(&((v * 255.0).round() as u8).to_string()); }
        self.syncing.set(false);
    }

    fn fill_recents(self: &Rc<Self>) {
        while let Some(child) = self.recents.first_child() { self.recents.remove(&child); }
        let colors = RECENTS.with(|r| r.borrow().clone());
        for c in colors {
            let swatch = gtk::Button::builder().width_request(18).height_request(18).has_frame(false).tooltip_text(hex(c)).build();
            let area = gtk::DrawingArea::builder().content_width(16).content_height(16).build();
            area.set_draw_func(move |_, cr, w, h| { cr.set_source_rgb(c[0], c[1], c[2]); rounded(cr, 0.5, 0.5, w as f64 - 1.0, h as f64 - 1.0, 3.0); let _ = cr.fill_preserve(); cr.set_source_rgba(0.5, 0.5, 0.5, 0.6); cr.set_line_width(1.0); let _ = cr.stroke(); });
            swatch.set_child(Some(&area));
            let p = self.clone();
            swatch.connect_clicked(move |_| { p.set_color(c); p.emit(); });
            self.recents.append(&swatch);
        }
    }

    /// Some(true) on the hue strip, Some(false) in the square, None in the gap.
    fn region(&self, x: f64, _y: f64) -> Option<bool> {
        if x <= SIZE as f64 + 2.0 { return Some(false); }
        if x >= (SIZE + GAP) as f64 - 2.0 { return Some(true); }
        None
    }

    fn pick(&self, x: f64, y: f64) {
        let Some(on_strip) = self.dragging.get() else { return };
        let side = SIZE as f64;
        let (mut h, mut s, mut v) = self.hsv.get();
        if on_strip {
            h = (y / side).clamp(0.0, 1.0) * 360.0;
            if h >= 360.0 { h = 359.999; }
            self.square_image.borrow_mut().take();
        } else {
            s = (x / side).clamp(0.0, 1.0);
            v = 1.0 - (y / side).clamp(0.0, 1.0);
        }
        self.hsv.set((h, s, v));
        self.sync_entry();
        self.wheel.queue_draw();
        self.emit();
    }

    fn draw(&self, cr: &cairo::Context, _w: f64, _h: f64) {
        let side = SIZE as f64;
        let (h, s, v) = self.hsv.get();
        // The saturation/value square for the current hue: white to the hue across, to black down.
        let stale = self.square_image.borrow().as_ref().is_none_or(|(hue, _)| *hue != h);
        if stale {
            if let Ok(image) = crate::raster::new_argb(SIZE, SIZE) {
                let _ = crate::raster::with_bytes_raw_mut(&image, |data, stride| {
                    for y in 0..SIZE { for x in 0..SIZE {
                        let rgb = hsv_to_rgb(h, x as f64 / (SIZE - 1) as f64, 1.0 - y as f64 / (SIZE - 1) as f64);
                        let i = y as usize * stride + x as usize * 4;
                        data[i] = (rgb[2] * 255.0).round() as u8; data[i + 1] = (rgb[1] * 255.0).round() as u8; data[i + 2] = (rgb[0] * 255.0).round() as u8; data[i + 3] = 255;
                    } }
                });
                *self.square_image.borrow_mut() = Some((h, image));
            }
        }
        if let Some((_, square)) = self.square_image.borrow().as_ref() { let _ = cr.set_source_surface(square, 0.0, 0.0); let _ = cr.paint(); }
        cr.set_source_rgba(0.5, 0.5, 0.5, 0.6); cr.set_line_width(1.0); cr.rectangle(0.5, 0.5, side - 1.0, side - 1.0); let _ = cr.stroke();
        // The hue strip, rendered once, red at the top through the spectrum back to red.
        if self.strip_image.borrow().is_none() {
            if let Ok(image) = crate::raster::new_argb(STRIP, SIZE) {
                let _ = crate::raster::with_bytes_raw_mut(&image, |data, stride| {
                    for y in 0..SIZE {
                        let rgb = hsv_to_rgb(y as f64 / SIZE as f64 * 360.0, 1.0, 1.0);
                        for x in 0..STRIP {
                            let i = y as usize * stride + x as usize * 4;
                            data[i] = (rgb[2] * 255.0).round() as u8; data[i + 1] = (rgb[1] * 255.0).round() as u8; data[i + 2] = (rgb[0] * 255.0).round() as u8; data[i + 3] = 255;
                        }
                    }
                });
                *self.strip_image.borrow_mut() = Some(image);
            }
        }
        let sx = (SIZE + GAP) as f64;
        if let Some(strip) = self.strip_image.borrow().as_ref() { let _ = cr.set_source_surface(strip, sx, 0.0); let _ = cr.paint(); }
        cr.set_source_rgba(0.5, 0.5, 0.5, 0.6); cr.rectangle(sx + 0.5, 0.5, STRIP as f64 - 1.0, side - 1.0); let _ = cr.stroke();
        // Markers: a bar across the strip at the hue, a ring in the square at the color.
        let hy = (h / 360.0 * side).round() + 0.5;
        cr.rectangle(sx - 2.0, hy - 3.0, STRIP as f64 + 4.0, 6.0);
        cr.set_source_rgb(1.0, 1.0, 1.0); cr.set_line_width(2.0); let _ = cr.stroke_preserve();
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.6); cr.set_line_width(1.0); let _ = cr.stroke();
        let (mx, my) = (s * (side - 1.0), (1.0 - v) * (side - 1.0));
        cr.arc(mx, my, 6.0, 0.0, std::f64::consts::TAU);
        let dark = v > 0.5 && !(s > 0.6 && (h < 30.0 || h > 330.0 || (200.0..280.0).contains(&h)));
        let (a, b) = if dark { (0.0, 1.0) } else { (1.0, 0.0) };
        cr.set_source_rgb(a, a, a); cr.set_line_width(2.0); let _ = cr.stroke_preserve();
        cr.set_source_rgb(b, b, b); cr.set_line_width(1.0); let _ = cr.stroke();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hsv_round_trips() {
        for rgb in [[1.0, 0.0, 0.0], [0.2, 0.7, 0.4], [0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [0.5, 0.5, 0.5]] {
            let (h, s, v) = rgb_to_hsv(rgb);
            let back = hsv_to_rgb(h, s, v);
            for k in 0..3 { assert!((back[k] - rgb[k]).abs() < 1e-9, "{rgb:?} -> {back:?}"); }
        }
        assert_eq!(hex([1.0, 0.5, 0.0]), "#ff8000");
        assert_eq!(parse_hex("#ff8000"), Some([1.0, 128.0 / 255.0, 0.0]));
        assert_eq!(parse_hex("f80").map(hex), Some("#ff8800".to_string()));
        assert_eq!(parse_hex("nope"), None);
    }
}
