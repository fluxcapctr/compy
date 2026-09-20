//! The gradient editor: a bar showing the gradient, opacity stops above it and color stops below it.
//! Click an empty spot on a row to add a stop there, drag a stop to move it, click one to select it and
//! change its color or opacity, Delete removes it; presets fill the bar to start from.

use crate::gradient::{AlphaStop, Gradient, Stop};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const BAR_W: f64 = 360.0;
const BAR_H: f64 = 40.0;
const ROW_H: f64 = 18.0;
const PAD: f64 = 8.0;

#[derive(Clone, Copy, PartialEq)]
enum Pick { Color(usize), Alpha(usize) }

struct Editor {
    gradient: RefCell<Gradient>,
    picked: Cell<Option<Pick>>,
    dragging: Cell<bool>,
    bar: gtk::DrawingArea,
    colors: gtk::DrawingArea,
    alphas: gtk::DrawingArea,
    color_button: Rc<super::color_wheel::ColorButton>,
    position: gtk::SpinButton,
    opacity: gtk::SpinButton,
    delete: gtk::Button,
    syncing: Cell<bool>,
    changed: Rc<dyn Fn(Gradient)>,
}

/// Opens the editor on `initial`; `changed` gets every edit as it happens.
pub fn open(parent: &gtk::Window, initial: Gradient, foreground: [f64; 3], background: [f64; 3], changed: Rc<dyn Fn(Gradient)>) {
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).margin_top(14).margin_bottom(14).margin_start(14).margin_end(14).build();
    let presets_row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).build();
    presets_row.append(&gtk::Label::new(Some("Presets")));
    let names: Vec<String> = super::canvas::gradient_preset_names().into_iter().filter(|n| n != "Custom").collect();
    let presets = gtk::DropDown::from_strings(&names.iter().map(String::as_str).collect::<Vec<_>>());
    presets.set_hexpand(true);
    presets_row.append(&presets);
    content.append(&presets_row);
    let alphas = gtk::DrawingArea::builder().content_width((BAR_W + 2.0 * PAD) as i32).content_height(ROW_H as i32).tooltip_text("Opacity stops: click to add, drag to move, click to pick").build();
    let bar = gtk::DrawingArea::builder().content_width((BAR_W + 2.0 * PAD) as i32).content_height(BAR_H as i32).build();
    let colors = gtk::DrawingArea::builder().content_width((BAR_W + 2.0 * PAD) as i32).content_height(ROW_H as i32).tooltip_text("Color stops: click to add, drag to move, click to pick").build();
    content.append(&alphas);
    content.append(&bar);
    content.append(&colors);
    let grid = gtk::Grid::builder().row_spacing(8).column_spacing(10).build();
    grid.attach(&gtk::Label::builder().label("Color").xalign(0.0).build(), 0, 0, 1, 1);
    // The button reports picks through its callback; the editor does not exist yet, so a holder stands in.
    let holder: Rc<RefCell<Option<Rc<Editor>>>> = Rc::new(RefCell::new(None));
    let color_button = { let holder = holder.clone(); super::color_wheel::ColorButton::new([0.0; 3], Rc::new(move |c| { if let Some(e) = holder.borrow().as_ref() { if !e.syncing.get() { e.set_picked_color(c); } } })) };
    grid.attach(&color_button.widget, 1, 0, 1, 1);
    grid.attach(&gtk::Label::builder().label("Opacity %").xalign(0.0).build(), 2, 0, 1, 1);
    let opacity = gtk::SpinButton::with_range(0.0, 100.0, 1.0);
    grid.attach(&opacity, 3, 0, 1, 1);
    grid.attach(&gtk::Label::builder().label("Location %").xalign(0.0).build(), 0, 1, 1, 1);
    let position = gtk::SpinButton::with_range(0.0, 100.0, 1.0);
    grid.attach(&position, 1, 1, 1, 1);
    let delete = gtk::Button::with_label("Delete Stop");
    grid.attach(&delete, 3, 1, 1, 1);
    content.append(&grid);
    content.append(&gtk::Label::builder().label("Click below the bar to add a color stop, above it for an opacity stop. Stops drag along the bar.").xalign(0.0).wrap(true).css_classes(["dim-label"]).build());
    let close = gtk::Button::builder().label("Done").css_classes(["suggested-action"]).halign(gtk::Align::End).build();
    content.append(&close);
    let editor = Rc::new(Editor { gradient: RefCell::new(initial.normalized()), picked: Cell::new(Some(Pick::Color(0))), dragging: Cell::new(false), bar: bar.clone(), colors: colors.clone(), alphas: alphas.clone(), color_button: color_button.clone(), position: position.clone(), opacity: opacity.clone(), delete: delete.clone(), syncing: Cell::new(false), changed });

    { let e = editor.clone(); bar.set_draw_func(move |_, cr, w, h| { cr.translate(PAD, 0.0); super::tools::draw_gradient_bar(cr, w as f64 - 2.0 * PAD, h as f64, &e.gradient.borrow()); }); }
    { let e = editor.clone(); colors.set_draw_func(move |_, cr, _, h| e.draw_row(cr, h as f64, false)); }
    { let e = editor.clone(); alphas.set_draw_func(move |_, cr, _, h| e.draw_row(cr, h as f64, true)); }
    editor.attach_gestures(&colors, false);
    editor.attach_gestures(&alphas, true);
    {
        let e = editor.clone();
        let fg = foreground;
        let bg = background;
        presets.connect_selected_notify(move |p| { let g = super::canvas::gradient_for(p.selected(), fg, bg, &Gradient::default()); *e.gradient.borrow_mut() = g; e.picked.set(Some(Pick::Color(0))); e.emit(); });
    }
    *holder.borrow_mut() = Some(editor.clone());
    { let e = editor.clone(); position.connect_value_changed(move |s| { if e.syncing.get() { return; } e.set_picked_position(s.value() / 100.0); }); }
    { let e = editor.clone(); opacity.connect_value_changed(move |s| { if e.syncing.get() { return; } e.set_picked_alpha(s.value() / 100.0); }); }
    { let e = editor.clone(); delete.connect_clicked(move |_| e.delete_picked()); }
    let window = super::dialogs::floating(parent, "Gradient Editor", false, (BAR_W + 2.0 * PAD) as i32 + 28, &content);
    { let w = window.clone(); close.connect_clicked(move |_| w.close()); }
    editor.sync_fields();
    window.present();
}

impl Editor {
    fn x_of(position: f64) -> f64 { PAD + position * BAR_W }
    fn position_of(x: f64) -> f64 { ((x - PAD) / BAR_W).clamp(0.0, 1.0) }

    fn draw_row(&self, cr: &cairo::Context, h: f64, alphas: bool) {
        let g = self.gradient.borrow();
        let picked = self.picked.get();
        let count = if alphas { g.alphas.len() } else { g.stops.len() };
        for i in 0..count {
            let (x, fill, on) = if alphas { let a = g.alphas[i]; (Self::x_of(a.position), [a.alpha; 3], picked == Some(Pick::Alpha(i))) } else { let s = g.stops[i]; (Self::x_of(s.position), s.color, picked == Some(Pick::Color(i))) };
            // A house-shaped marker pointing at the bar: up for opacity stops, down for color stops.
            let (tip, base) = if alphas { (h - 1.0, 4.0) } else { (1.0, h - 4.0) };
            cr.move_to(x, tip); cr.line_to(x + 6.0, tip + (base - tip) * 0.45); cr.line_to(x + 6.0, base); cr.line_to(x - 6.0, base); cr.line_to(x - 6.0, tip + (base - tip) * 0.45); cr.close_path();
            cr.set_source_rgb(fill[0], fill[1], fill[2]);
            let _ = cr.fill_preserve();
            cr.set_line_width(if on { 2.0 } else { 1.0 });
            if on { cr.set_source_rgb(0.2, 0.55, 1.0); } else { cr.set_source_rgba(0.0, 0.0, 0.0, 0.7); }
            let _ = cr.stroke();
        }
    }

    fn hit(&self, x: f64, alphas: bool) -> Option<usize> {
        let g = self.gradient.borrow();
        let positions: Vec<f64> = if alphas { g.alphas.iter().map(|a| a.position).collect() } else { g.stops.iter().map(|s| s.position).collect() };
        positions.iter().enumerate().filter(|(_, p)| (Self::x_of(**p) - x).abs() <= 7.0).min_by(|a, b| (Self::x_of(*a.1) - x).abs().partial_cmp(&(Self::x_of(*b.1) - x).abs()).unwrap()).map(|(i, _)| i)
    }

    fn attach_gestures(self: &Rc<Self>, area: &gtk::DrawingArea, alphas: bool) {
        let drag = gtk::GestureDrag::new();
        {
            let e = self.clone();
            drag.connect_drag_begin(move |_, x, _| {
                match e.hit(x, alphas) {
                    Some(i) => { e.picked.set(Some(if alphas { Pick::Alpha(i) } else { Pick::Color(i) })); e.dragging.set(true); }
                    None => {
                        // A new stop where the click landed, taking the value the gradient has there.
                        let position = Self::position_of(x);
                        let value = e.gradient.borrow().at(position);
                        let index = { let mut g = e.gradient.borrow_mut(); if alphas { g.alphas.push(AlphaStop { position, alpha: value[3] }); g.alphas.len() - 1 } else { g.stops.push(Stop { position, color: [value[0], value[1], value[2]] }); g.stops.len() - 1 } };
                        e.picked.set(Some(if alphas { Pick::Alpha(index) } else { Pick::Color(index) }));
                        e.dragging.set(true);
                    }
                }
                e.sync_fields();
                e.redraw();
            });
        }
        {
            let e = self.clone();
            drag.connect_drag_update(move |g, dx, _| {
                if !e.dragging.get() { return; }
                let Some((sx, _)) = g.start_point() else { return };
                e.set_picked_position(Self::position_of(sx + dx));
            });
        }
        { let e = self.clone(); drag.connect_drag_end(move |_, _, _| { e.dragging.set(false); e.normalize_keeping_pick(); }); }
        area.add_controller(drag);
    }

    /// Sorting the stops after a drag would renumber them; the picked one is found again by value.
    fn normalize_keeping_pick(&self) {
        let picked = self.picked.get();
        let before = self.gradient.borrow().clone();
        let n = before.normalized();
        let new_pick = match picked {
            Some(Pick::Color(i)) => before.stops.get(i).and_then(|s| n.stops.iter().position(|t| t == s)).map(Pick::Color),
            Some(Pick::Alpha(i)) => before.alphas.get(i).and_then(|a| n.alphas.iter().position(|t| t == a)).map(Pick::Alpha),
            None => None,
        };
        *self.gradient.borrow_mut() = n;
        self.picked.set(new_pick);
        self.sync_fields();
        self.redraw();
    }

    fn set_picked_position(&self, position: f64) {
        { let mut g = self.gradient.borrow_mut(); match self.picked.get() { Some(Pick::Color(i)) => { if let Some(s) = g.stops.get_mut(i) { s.position = position; } } Some(Pick::Alpha(i)) => { if let Some(a) = g.alphas.get_mut(i) { a.position = position; } } None => {} } }
        self.sync_fields();
        self.emit_unsorted();
    }
    fn set_picked_color(&self, color: [f64; 3]) {
        if let Some(Pick::Color(i)) = self.picked.get() { if let Some(s) = self.gradient.borrow_mut().stops.get_mut(i) { s.color = color; } }
        self.emit_unsorted();
    }
    fn set_picked_alpha(&self, alpha: f64) {
        if let Some(Pick::Alpha(i)) = self.picked.get() { if let Some(a) = self.gradient.borrow_mut().alphas.get_mut(i) { a.alpha = alpha; } }
        self.emit_unsorted();
    }
    fn delete_picked(&self) {
        {
            let mut g = self.gradient.borrow_mut();
            match self.picked.get() {
                Some(Pick::Color(i)) if g.stops.len() > 2 && i < g.stops.len() => { g.stops.remove(i); }
                Some(Pick::Alpha(i)) if g.alphas.len() > 2 && i < g.alphas.len() => { g.alphas.remove(i); }
                _ => return,
            }
        }
        self.picked.set(Some(Pick::Color(0)));
        self.emit();
    }

    /// The fields follow the picked stop.
    fn sync_fields(&self) {
        self.syncing.set(true);
        let g = self.gradient.borrow();
        match self.picked.get() {
            Some(Pick::Color(i)) if i < g.stops.len() => { let s = g.stops[i]; self.color_button.set_color(s.color); self.position.set_value(s.position * 100.0); self.color_button.widget.set_sensitive(true); self.opacity.set_sensitive(false); self.delete.set_sensitive(g.stops.len() > 2); }
            Some(Pick::Alpha(i)) if i < g.alphas.len() => { let a = g.alphas[i]; self.opacity.set_value(a.alpha * 100.0); self.position.set_value(a.position * 100.0); self.color_button.widget.set_sensitive(false); self.opacity.set_sensitive(true); self.delete.set_sensitive(g.alphas.len() > 2); }
            _ => { self.color_button.widget.set_sensitive(false); self.opacity.set_sensitive(false); self.delete.set_sensitive(false); }
        }
        self.syncing.set(false);
    }

    fn redraw(&self) { self.bar.queue_draw(); self.colors.queue_draw(); self.alphas.queue_draw(); }
    /// Reports the gradient as it stands, stops still in edit order (a drag in progress must not renumber them).
    fn emit_unsorted(&self) { self.redraw(); (self.changed)(self.gradient.borrow().clone()); }
    fn emit(&self) { self.normalize_keeping_pick(); (self.changed)(self.gradient.borrow().clone()); }
}

