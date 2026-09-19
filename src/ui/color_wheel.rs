//! A color picker with a hue ring around a saturation/value square, a hex field, and the colors used
//! lately, in a popover behind a swatch button. Replaces GTK's chooser, whose editor has no wheel.

use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const SIZE: i32 = 220;
const RING: f64 = 22.0;
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

/// The picker's contents: wheel, hex field, recents.
struct Picker {
    widget: gtk::Box,
    wheel: gtk::DrawingArea,
    entry: gtk::Entry,
    recents: gtk::Box,
    hsv: Cell<(f64, f64, f64)>,
    ring_image: RefCell<Option<cairo::ImageSurface>>,
    square_image: RefCell<Option<(f64, cairo::ImageSurface)>>,
    dragging: Cell<Option<bool>>,
    on_change: RefCell<Option<Rc<dyn Fn([f64; 3])>>>,
    syncing: Cell<bool>,
}

impl Picker {
    fn new(initial: [f64; 3]) -> Rc<Picker> {
        let widget = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).margin_top(8).margin_bottom(8).margin_start(8).margin_end(8).build();
        let wheel = gtk::DrawingArea::builder().content_width(SIZE).content_height(SIZE).build();
        let entry = gtk::Entry::builder().max_length(7).width_chars(8).css_classes(["monospace"]).tooltip_text("Hex, like #ff8800").build();
        let recents = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(4).build();
        let row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).build();
        row.append(&gtk::Label::new(Some("Hex")));
        row.append(&entry);
        widget.append(&wheel);
        widget.append(&row);
        widget.append(&recents);
        let picker = Rc::new(Picker { widget, wheel: wheel.clone(), entry: entry.clone(), recents, hsv: Cell::new(rgb_to_hsv(initial)), ring_image: RefCell::new(None), square_image: RefCell::new(None), dragging: Cell::new(None), on_change: RefCell::new(None), syncing: Cell::new(false) });
        { let p = picker.clone(); wheel.set_draw_func(move |_, cr, w, h| p.draw(cr, w as f64, h as f64)); }
        let drag = gtk::GestureDrag::new();
        { let p = picker.clone(); drag.connect_drag_begin(move |_, x, y| { p.dragging.set(p.region(x, y)); p.pick(x, y); }); }
        { let p = picker.clone(); drag.connect_drag_update(move |g, dx, dy| { if let Some((x, y)) = g.start_point() { p.pick(x + dx, y + dy); } }); }
        { let p = picker.clone(); drag.connect_drag_end(move |_, _, _| p.dragging.set(None)); }
        wheel.add_controller(drag);
        { let p = picker.clone(); entry.connect_activate(move |e| { if let Some(rgb) = parse_hex(&e.text()) { p.set_color(rgb); p.emit(); } }); }
        { let p = picker.clone(); entry.connect_changed(move |e| { if p.syncing.get() { return; } if let Some(rgb) = parse_hex(&e.text()) { if e.text().len() == 7 { p.hsv.set(rgb_to_hsv(rgb)); p.square_image.borrow_mut().take(); p.wheel.queue_draw(); p.emit(); } } }); }
        picker.sync_entry();
        picker.fill_recents();
        picker
    }

    fn on_change(&self, f: Rc<dyn Fn([f64; 3])>) { *self.on_change.borrow_mut() = Some(f); }
    fn color(&self) -> [f64; 3] { let (h, s, v) = self.hsv.get(); hsv_to_rgb(h, s, v) }
    fn set_color(self: &Rc<Self>, rgb: [f64; 3]) {
        // Keep the hue when the new color is gray, so the ring marker does not jump.
        let (h, s, v) = rgb_to_hsv(rgb);
        let old = self.hsv.get();
        self.hsv.set((if s <= 0.0 || v <= 0.0 { old.0 } else { h }, s, v));
        self.square_image.borrow_mut().take();
        self.sync_entry();
        self.fill_recents();
        self.wheel.queue_draw();
    }
    fn emit(&self) { if let Some(f) = self.on_change.borrow().as_ref() { f(self.color()); } }
    fn sync_entry(&self) { self.syncing.set(true); self.entry.set_text(&hex(self.color())); self.syncing.set(false); }

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

    fn geometry(&self) -> (f64, f64, f64, f64) {
        let c = SIZE as f64 / 2.0;
        let outer = c - 2.0;
        let inner = outer - RING;
        let half = (inner - 6.0) / std::f64::consts::SQRT_2;
        (c, outer, inner, half)
    }

    /// Some(true) on the ring, Some(false) in the square, None elsewhere.
    fn region(&self, x: f64, y: f64) -> Option<bool> {
        let (c, outer, inner, half) = self.geometry();
        let d = ((x - c).powi(2) + (y - c).powi(2)).sqrt();
        if d <= outer + 4.0 && d >= inner - 4.0 { return Some(true); }
        if (x - c).abs() <= half + 4.0 && (y - c).abs() <= half + 4.0 { return Some(false); }
        None
    }

    fn pick(&self, x: f64, y: f64) {
        let Some(on_ring) = self.dragging.get() else { return };
        let (c, _, _, half) = self.geometry();
        let (mut h, mut s, mut v) = self.hsv.get();
        if on_ring {
            h = (y - c).atan2(x - c).to_degrees().rem_euclid(360.0);
            self.square_image.borrow_mut().take();
        } else {
            s = ((x - c + half) / (2.0 * half)).clamp(0.0, 1.0);
            v = 1.0 - ((y - c + half) / (2.0 * half)).clamp(0.0, 1.0);
        }
        self.hsv.set((h, s, v));
        self.sync_entry();
        self.wheel.queue_draw();
        self.emit();
    }

    fn draw(&self, cr: &cairo::Context, _w: f64, _h: f64) {
        let (c, outer, inner, half) = self.geometry();
        let (h, s, v) = self.hsv.get();
        // The ring, rendered once.
        if self.ring_image.borrow().is_none() {
            if let Ok(image) = crate::raster::new_argb(SIZE, SIZE) {
                let _ = crate::raster::with_bytes_raw_mut(&image, |data, stride| {
                    for y in 0..SIZE { for x in 0..SIZE {
                        let (dx, dy) = (x as f64 + 0.5 - c, y as f64 + 0.5 - c);
                        let d = (dx * dx + dy * dy).sqrt();
                        let cover = (outer - d + 0.5).clamp(0.0, 1.0) * (d - inner + 0.5).clamp(0.0, 1.0);
                        if cover <= 0.0 { continue; }
                        let rgb = hsv_to_rgb(dy.atan2(dx).to_degrees(), 1.0, 1.0);
                        let i = y as usize * stride + x as usize * 4;
                        let a = (cover * 255.0).round() as u8;
                        data[i] = (rgb[2] * a as f64).round() as u8; data[i + 1] = (rgb[1] * a as f64).round() as u8; data[i + 2] = (rgb[0] * a as f64).round() as u8; data[i + 3] = a;
                    } }
                });
                *self.ring_image.borrow_mut() = Some(image);
            }
        }
        if let Some(ring) = self.ring_image.borrow().as_ref() { let _ = cr.set_source_surface(ring, 0.0, 0.0); let _ = cr.paint(); }
        // The saturation/value square for the current hue.
        let stale = self.square_image.borrow().as_ref().is_none_or(|(hue, _)| *hue != h);
        if stale {
            let side = (half * 2.0).round() as i32;
            if let Ok(image) = crate::raster::new_argb(side, side) {
                let _ = crate::raster::with_bytes_raw_mut(&image, |data, stride| {
                    for y in 0..side { for x in 0..side {
                        let rgb = hsv_to_rgb(h, x as f64 / (side - 1) as f64, 1.0 - y as f64 / (side - 1) as f64);
                        let i = y as usize * stride + x as usize * 4;
                        data[i] = (rgb[2] * 255.0).round() as u8; data[i + 1] = (rgb[1] * 255.0).round() as u8; data[i + 2] = (rgb[0] * 255.0).round() as u8; data[i + 3] = 255;
                    } }
                });
                *self.square_image.borrow_mut() = Some((h, image));
            }
        }
        if let Some((_, square)) = self.square_image.borrow().as_ref() { let _ = cr.set_source_surface(square, (c - half).round(), (c - half).round()); let _ = cr.paint(); }
        // Markers: hue on the ring, the color in the square.
        let a = h.to_radians();
        let r = (outer + inner) / 2.0;
        cr.arc(c + a.cos() * r, c + a.sin() * r, 6.0, 0.0, std::f64::consts::TAU);
        cr.set_source_rgb(1.0, 1.0, 1.0); cr.set_line_width(2.0); let _ = cr.stroke_preserve();
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.6); cr.set_line_width(1.0); let _ = cr.stroke();
        let (mx, my) = (c - half + s * 2.0 * half, c - half + (1.0 - v) * 2.0 * half);
        cr.arc(mx, my, 6.0, 0.0, std::f64::consts::TAU);
        cr.set_source_rgb(if v > 0.5 { 0.0 } else { 1.0 }, if v > 0.5 { 0.0 } else { 1.0 }, if v > 0.5 { 0.0 } else { 1.0 }); cr.set_line_width(2.0); let _ = cr.stroke_preserve();
        cr.set_source_rgb(if v > 0.5 { 1.0 } else { 0.0 }, if v > 0.5 { 1.0 } else { 0.0 }, if v > 0.5 { 1.0 } else { 0.0 }); cr.set_line_width(1.0); let _ = cr.stroke();
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
