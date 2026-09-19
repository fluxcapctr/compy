//! The Layer Style dialog: a list of effects to switch on, each with its settings, applied to the layer as
//! you change them (one "Layer Style" undo step); Cancel puts back what the layer had.

use super::DocRef;
use crate::effects::{Bevel, Effects, Glow, Overlay, Shadow, Stroke};
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use uuid::Uuid;

type Apply = Rc<dyn Fn()>;

pub fn open(parent: &gtk::Window, doc: DocRef, id: Uuid, finished: Rc<dyn Fn()>) {
    let original = doc.borrow().document.effects(id).unwrap_or_default();
    let state = Rc::new(RefCell::new(original.clone()));
    let apply: Apply = {
        let (doc, state, finished) = (doc.clone(), state.clone(), finished.clone());
        Rc::new(move || {
            let effects = state.borrow().clone();
            if let Ok(mut d) = doc.try_borrow_mut() { if d.document.has_layer(id) { if let Err(e) = d.document.set_effects(id, Some(&effects)) { eprintln!("layer style: {e:#}"); } } }
            finished();
        })
    };

    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).margin_top(10).margin_bottom(10).margin_start(12).margin_end(12).build();
    let body = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(12).build();
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).css_classes(["navigation-sidebar"]).width_request(170).build();
    let pages = gtk::Stack::builder().vhomogeneous(false).hhomogeneous(true).width_request(300).build();
    body.append(&list);
    body.append(&pages);
    content.append(&body);

    let names = [("dropShadow", "Drop Shadow"), ("innerShadow", "Inner Shadow"), ("outerGlow", "Outer Glow"), ("innerGlow", "Inner Glow"), ("bevel", "Bevel & Emboss"), ("stroke", "Stroke"), ("colorOverlay", "Color Overlay")];
    let mut checks = Vec::new();
    for (key, label) in names {
        let row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(6).build();
        let check = gtk::CheckButton::builder().active(enabled(&state.borrow(), key)).build();
        row.append(&check);
        row.append(&gtk::Label::builder().label(label).xalign(0.0).build());
        let item = gtk::ListBoxRow::builder().child(&row).build();
        list.append(&item);
        {
            let (state, apply, pages, list, item) = (state.clone(), apply.clone(), pages.clone(), list.clone(), item.clone());
            check.connect_toggled(move |c| { set_enabled(&mut state.borrow_mut(), key, c.is_active()); list.select_row(Some(&item)); pages.set_visible_child_name(key); apply(); });
        }
        checks.push((key, check));
        pages.add_named(&page(key, &state, &apply), Some(key));
    }
    { let pages = pages.clone(); list.connect_row_selected(move |_, row| { if let Some(r) = row { pages.set_visible_child_name(names[r.index().max(0) as usize].0); } }); }
    let first = original_page(&original).unwrap_or("dropShadow");
    list.select_row(list.row_at_index(names.iter().position(|(k, _)| *k == first).unwrap_or(0) as i32).as_ref());
    pages.set_visible_child_name(first);

    let buttons = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).halign(gtk::Align::End).build();
    let clear = gtk::Button::with_label("Clear All");
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::builder().label("OK").css_classes(["suggested-action"]).build();
    buttons.append(&clear);
    buttons.append(&cancel);
    buttons.append(&ok);
    content.append(&buttons);
    let window = super::dialogs::floating(parent, "Layer Style", false, 520, &content);
    {
        let (state, apply, checks) = (state.clone(), apply.clone(), checks.clone());
        clear.connect_clicked(move |_| { *state.borrow_mut() = Effects::default(); for (_, c) in &checks { c.set_active(false); } apply(); });
    }
    {
        let (state, apply, window) = (state.clone(), apply.clone(), window.clone());
        cancel.connect_clicked(move |_| { *state.borrow_mut() = original.clone(); apply(); window.close(); });
    }
    { let window = window.clone(); ok.connect_clicked(move |_| window.close()); }
    let keys = gtk::EventControllerKey::new();
    { let cancel = cancel.clone(); keys.connect_key_pressed(move |_, key, _, _| { if key == gtk::gdk::Key::Escape { cancel.emit_clicked(); gtk::glib::Propagation::Stop } else { gtk::glib::Propagation::Proceed } }); }
    window.add_controller(keys);
    window.present();
}

fn enabled(e: &Effects, key: &str) -> bool {
    match key {
        "dropShadow" => e.drop_shadow.as_ref().is_some_and(|x| x.enabled),
        "innerShadow" => e.inner_shadow.as_ref().is_some_and(|x| x.enabled),
        "outerGlow" => e.outer_glow.as_ref().is_some_and(|x| x.enabled),
        "innerGlow" => e.inner_glow.as_ref().is_some_and(|x| x.enabled),
        "bevel" => e.bevel.as_ref().is_some_and(|x| x.enabled),
        "stroke" => e.stroke.as_ref().is_some_and(|x| x.enabled),
        _ => e.color_overlay.as_ref().is_some_and(|x| x.enabled),
    }
}

/// Switches an effect on (with its defaults the first time) or off, keeping its settings.
fn set_enabled(e: &mut Effects, key: &str, on: bool) {
    match key {
        "dropShadow" => e.drop_shadow.get_or_insert_with(Shadow::drop_default).enabled = on,
        "innerShadow" => e.inner_shadow.get_or_insert_with(Shadow::inner_default).enabled = on,
        "outerGlow" => e.outer_glow.get_or_insert_with(Glow::outer_default).enabled = on,
        "innerGlow" => e.inner_glow.get_or_insert_with(Glow::inner_default).enabled = on,
        "bevel" => e.bevel.get_or_insert_with(Bevel::default).enabled = on,
        "stroke" => e.stroke.get_or_insert_with(Stroke::default).enabled = on,
        _ => e.color_overlay.get_or_insert_with(Overlay::default).enabled = on,
    }
}

fn original_page(e: &Effects) -> Option<&'static str> {
    ["dropShadow", "innerShadow", "outerGlow", "innerGlow", "bevel", "stroke", "colorOverlay"].into_iter().find(|k| enabled(e, k))
}

struct Grid { grid: gtk::Grid, row: i32 }

impl Grid {
    fn new() -> Grid { Grid { grid: gtk::Grid::builder().row_spacing(6).column_spacing(10).build(), row: 0 } }
    fn spin(&mut self, label: &str, range: (f64, f64, f64), digits: u32, value: f64, changed: impl Fn(f64) + 'static) {
        self.grid.attach(&gtk::Label::builder().label(label).xalign(1.0).build(), 0, self.row, 1, 1);
        let spin = gtk::SpinButton::with_range(range.0, range.1, range.2);
        spin.set_digits(digits);
        spin.set_value(value);
        spin.set_hexpand(true);
        spin.connect_value_changed(move |s| changed(s.value()));
        self.grid.attach(&spin, 1, self.row, 1, 1);
        self.row += 1;
    }
    fn color(&mut self, label: &str, value: [f64; 3], changed: impl Fn([f64; 3]) + 'static) {
        self.grid.attach(&gtk::Label::builder().label(label).xalign(1.0).build(), 0, self.row, 1, 1);
        let button = super::color_wheel::ColorButton::new(value, Rc::new(changed));
        button.widget.set_tooltip_text(Some("Color"));
        button.widget.set_halign(gtk::Align::Start);
        self.grid.attach(&button.widget, 1, self.row, 1, 1);
        self.row += 1;
    }
    fn choice(&mut self, label: &str, options: &[&str], selected: u32, changed: impl Fn(u32) + 'static) {
        self.grid.attach(&gtk::Label::builder().label(label).xalign(1.0).build(), 0, self.row, 1, 1);
        let dropdown = gtk::DropDown::from_strings(options);
        dropdown.set_selected(selected);
        dropdown.set_halign(gtk::Align::Start);
        dropdown.connect_selected_notify(move |d| changed(d.selected()));
        self.grid.attach(&dropdown, 1, self.row, 1, 1);
        self.row += 1;
    }
}

/// One effect's settings. Each control edits the state and applies it; an effect gets its defaults when its
/// page is built before it exists, so the controls have values to show.
fn page(key: &'static str, state: &Rc<RefCell<Effects>>, apply: &Apply) -> gtk::Grid {
    let mut g = Grid::new();
    macro_rules! edit {
        ($field:ident, $default:expr, |$e:ident, $v:ident| $body:expr) => {{
            let (state, apply) = (state.clone(), apply.clone());
            move |$v| { { let mut s = state.borrow_mut(); let $e = s.$field.get_or_insert_with($default); $body; } apply(); }
        }};
    }
    match key {
        "dropShadow" | "innerShadow" => {
            let inner = key == "innerShadow";
            let current = { let mut s = state.borrow_mut(); if inner { s.inner_shadow.get_or_insert_with(Shadow::inner_default).clone() } else { s.drop_shadow.get_or_insert_with(Shadow::drop_default).clone() } };
            if inner {
                g.color("Color", current.color, edit!(inner_shadow, Shadow::inner_default, |e, v| e.color = v));
                g.spin("Opacity %", (0.0, 100.0, 1.0), 0, current.opacity * 100.0, edit!(inner_shadow, Shadow::inner_default, |e, v| e.opacity = v / 100.0));
                g.spin("Angle", (-180.0, 180.0, 1.0), 0, current.angle, edit!(inner_shadow, Shadow::inner_default, |e, v| e.angle = v));
                g.spin("Distance px", (0.0, 1000.0, 1.0), 0, current.distance, edit!(inner_shadow, Shadow::inner_default, |e, v| e.distance = v));
                g.spin("Size px", (0.0, 250.0, 1.0), 0, current.size, edit!(inner_shadow, Shadow::inner_default, |e, v| e.size = v));
            } else {
                g.color("Color", current.color, edit!(drop_shadow, Shadow::drop_default, |e, v| e.color = v));
                g.spin("Opacity %", (0.0, 100.0, 1.0), 0, current.opacity * 100.0, edit!(drop_shadow, Shadow::drop_default, |e, v| e.opacity = v / 100.0));
                g.spin("Angle", (-180.0, 180.0, 1.0), 0, current.angle, edit!(drop_shadow, Shadow::drop_default, |e, v| e.angle = v));
                g.spin("Distance px", (0.0, 1000.0, 1.0), 0, current.distance, edit!(drop_shadow, Shadow::drop_default, |e, v| e.distance = v));
                g.spin("Size px", (0.0, 250.0, 1.0), 0, current.size, edit!(drop_shadow, Shadow::drop_default, |e, v| e.size = v));
            }
        }
        "outerGlow" | "innerGlow" => {
            let inner = key == "innerGlow";
            let current = { let mut s = state.borrow_mut(); if inner { s.inner_glow.get_or_insert_with(Glow::inner_default).clone() } else { s.outer_glow.get_or_insert_with(Glow::outer_default).clone() } };
            if inner {
                g.color("Color", current.color, edit!(inner_glow, Glow::inner_default, |e, v| e.color = v));
                g.spin("Opacity %", (0.0, 100.0, 1.0), 0, current.opacity * 100.0, edit!(inner_glow, Glow::inner_default, |e, v| e.opacity = v / 100.0));
                g.spin("Size px", (0.0, 250.0, 1.0), 0, current.size, edit!(inner_glow, Glow::inner_default, |e, v| e.size = v));
            } else {
                g.color("Color", current.color, edit!(outer_glow, Glow::outer_default, |e, v| e.color = v));
                g.spin("Opacity %", (0.0, 100.0, 1.0), 0, current.opacity * 100.0, edit!(outer_glow, Glow::outer_default, |e, v| e.opacity = v / 100.0));
                g.spin("Size px", (0.0, 250.0, 1.0), 0, current.size, edit!(outer_glow, Glow::outer_default, |e, v| e.size = v));
            }
        }
        "bevel" => {
            let current = state.borrow_mut().bevel.get_or_insert_with(Bevel::default).clone();
            g.choice("Style", &["Inner Bevel", "Outer Bevel", "Emboss"], current.style, edit!(bevel, Bevel::default, |e, v| e.style = v));
            g.spin("Depth %", (1.0, 1000.0, 1.0), 0, current.depth, edit!(bevel, Bevel::default, |e, v| e.depth = v));
            g.spin("Size px", (0.0, 250.0, 1.0), 0, current.size, edit!(bevel, Bevel::default, |e, v| e.size = v));
            g.spin("Angle", (-180.0, 180.0, 1.0), 0, current.angle, edit!(bevel, Bevel::default, |e, v| e.angle = v));
            g.spin("Altitude", (0.0, 90.0, 1.0), 0, current.altitude, edit!(bevel, Bevel::default, |e, v| e.altitude = v));
            g.spin("Highlight %", (0.0, 100.0, 1.0), 0, current.highlight_opacity * 100.0, edit!(bevel, Bevel::default, |e, v| e.highlight_opacity = v / 100.0));
            g.spin("Shadow %", (0.0, 100.0, 1.0), 0, current.shadow_opacity * 100.0, edit!(bevel, Bevel::default, |e, v| e.shadow_opacity = v / 100.0));
        }
        "stroke" => {
            let current = state.borrow_mut().stroke.get_or_insert_with(Stroke::default).clone();
            g.spin("Size px", (1.0, 250.0, 1.0), 0, current.size, edit!(stroke, Stroke::default, |e, v| e.size = v));
            g.choice("Position", &["Outside", "Inside", "Center"], current.position, edit!(stroke, Stroke::default, |e, v| e.position = v));
            g.color("Color", current.color, edit!(stroke, Stroke::default, |e, v| e.color = v));
            g.spin("Opacity %", (0.0, 100.0, 1.0), 0, current.opacity * 100.0, edit!(stroke, Stroke::default, |e, v| e.opacity = v / 100.0));
        }
        _ => {
            let current = state.borrow_mut().color_overlay.get_or_insert_with(Overlay::default).clone();
            g.color("Color", current.color, edit!(color_overlay, Overlay::default, |e, v| e.color = v));
            g.spin("Opacity %", (0.0, 100.0, 1.0), 0, current.opacity * 100.0, edit!(color_overlay, Overlay::default, |e, v| e.opacity = v / 100.0));
        }
    }
    g.grid
}
