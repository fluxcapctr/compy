//! The tool rail down the canvas's left edge and the options bar above it.

use super::DocRef;
use crate::selection::Mode;
use gtk::prelude::*;
use gtk::gdk;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool { Move, Marquee, Lasso, Wand, Brush, Eraser, Heal, Clone, Blur, Hand, Zoom }

impl Tool {
    pub const ALL: [Tool; 11] = [Tool::Move, Tool::Marquee, Tool::Lasso, Tool::Wand, Tool::Brush, Tool::Eraser, Tool::Heal, Tool::Clone, Tool::Blur, Tool::Hand, Tool::Zoom];
    pub fn help(self) -> &'static str {
        match self {
            Tool::Move => "Move / Transform (V): drag to move; handles scale, the top handle rotates; Shift constrains; Alt scales from the center; Ctrl-click picks the layer under the pointer; arrow keys nudge",
            Tool::Marquee => "Marquee (M): drag a rectangle or ellipse; Shift adds, Alt subtracts; Shift while dragging squares it; drag inside a selection to move its outline",
            Tool::Lasso => "Lasso (L): drag a freehand outline, or click corners for a polygon and click the first corner to close it",
            Tool::Wand => "Magic Wand (W): click to select similar colors; Shift adds, Alt subtracts",
            Tool::Brush => "Brush (B): drag to paint; Shift-click paints a straight line from the last point",
            Tool::Eraser => "Eraser (E): drag to clear pixels",
            Tool::Heal => "Spot Healing Brush (J): paint over a blemish; it is rebuilt from its surroundings",
            Tool::Clone => "Clone Stamp (S): Alt-click a source, then paint copies of it",
            Tool::Blur => "Smear (R): Liquify pushes pixels, Blur softens, Smudge drags color",
            Tool::Hand => "Hand (H): drag to pan",
            Tool::Zoom => "Zoom (Z): click to zoom in, Alt-click to zoom out",
        }
    }
    pub fn key(self) -> char { match self { Tool::Move => 'v', Tool::Marquee => 'm', Tool::Lasso => 'l', Tool::Wand => 'w', Tool::Brush => 'b', Tool::Eraser => 'e', Tool::Heal => 'j', Tool::Clone => 's', Tool::Blur => 'r', Tool::Hand => 'h', Tool::Zoom => 'z' } }
    pub fn is_brush(self) -> bool { matches!(self, Tool::Brush | Tool::Eraser | Tool::Heal | Tool::Clone | Tool::Blur) }
    pub fn is_selection(self) -> bool { matches!(self, Tool::Marquee | Tool::Lasso | Tool::Wand) }
}

pub struct ToolRail {
    pub widget: gtk::Box,
    buttons: Vec<(Tool, gtk::ToggleButton)>,
}

impl ToolRail {
    /// `changed` runs after the document's tool has been set.
    pub fn new(doc: DocRef, changed: Rc<dyn Fn()>) -> Rc<ToolRail> {
        let widget = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).width_request(56)
            .margin_top(10).margin_start(10).margin_end(10).build();
        let mut buttons = Vec::new();
        let current = doc.borrow().tool;
        let mut group: Option<gtk::ToggleButton> = None;
        for tool in Tool::ALL {
            let button = gtk::ToggleButton::builder().tooltip_text(tool.help()).width_request(36).height_request(36).active(tool == current).build();
            button.set_child(Some(&super::icons::icon(tool)));
            if let Some(first) = &group { button.set_group(Some(first)); } else { group = Some(button.clone()); }
            let (doc, changed) = (doc.clone(), changed.clone());
            button.connect_toggled(move |b| {
                if !b.is_active() { return; }
                if let Ok(mut d) = doc.try_borrow_mut() { if d.tool == tool { return; } d.tool = tool; }
                changed();
            });
            widget.append(&button);
            buttons.push((tool, button));
        }
        Rc::new(ToolRail { widget, buttons })
    }

    pub fn select(&self, tool: Tool) {
        for (t, button) in &self.buttons { if *t == tool && !button.is_active() { button.set_active(true); } }
    }
}

/// The current tool's options, or a hint for the tools without any.
pub struct OptionsBar {
    pub widget: gtk::Stack,
    size: gtk::SpinButton,
    hardness: gtk::SpinButton,
    opacity: gtk::SpinButton,
    move_fields: Vec<gtk::SpinButton>,
    mask_paint: gtk::DropDown,
    syncing: std::cell::Cell<bool>,
}

/// New / Add / Subtract, kept in step with the document's mode.
fn mode_buttons(doc: &DocRef) -> gtk::Box {
    let modes = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).css_classes(["linked"]).build();
    let mut first: Option<gtk::ToggleButton> = None;
    for (mode, label, tip) in [(Mode::Replace, "New", "Replace the selection"), (Mode::Add, "Add", "Add to the selection (Shift)"), (Mode::Subtract, "Subtract", "Subtract from the selection (Alt)")] {
        let b = gtk::ToggleButton::builder().label(label).tooltip_text(tip).active(mode == doc.borrow().mode).build();
        if let Some(f) = &first { b.set_group(Some(f)); } else { first = Some(b.clone()); }
        let doc = doc.clone();
        b.connect_toggled(move |b| { if b.is_active() { if let Ok(mut d) = doc.try_borrow_mut() { d.mode = mode; } } });
        modes.append(&b);
    }
    modes
}

impl OptionsBar {
    pub fn new(doc: DocRef) -> OptionsBar {
        let stack = gtk::Stack::builder().vhomogeneous(false).build();
        let hint = gtk::Label::builder().xalign(0.0).margin_start(12).margin_top(8).margin_bottom(8).css_classes(["dim-label"]).build();
        stack.add_named(&hint, Some("hint"));

        // Move: the transform inspector.
        let mv = row();
        let mut move_fields = Vec::new();
        for (label, low, high, tip) in [("X", -1.0e6, 1.0e6, "Left edge, document pixels"), ("Y", -1.0e6, 1.0e6, "Top edge, document pixels"), ("W", 1.0, 300_000.0, "Width, document pixels"), ("H", 1.0, 300_000.0, "Height, document pixels"), ("Angle", -3600.0, 3600.0, "Clockwise rotation in degrees")] {
            mv.append(&gtk::Label::new(Some(label)));
            let spin = gtk::SpinButton::with_range(low, high, 1.0);
            spin.set_digits(if label == "Angle" { 1 } else { 0 });
            spin.set_width_chars(6);
            spin.set_tooltip_text(Some(tip));
            mv.append(&spin);
            move_fields.push(spin);
        }
        let ratio = gtk::CheckButton::builder().label("Lock ratio").active(true).tooltip_text("Corner handles keep the proportions (Shift reverses)").build();
        { let doc = doc.clone(); ratio.connect_toggled(move |c| { if let Ok(mut d) = doc.try_borrow_mut() { d.lock_ratio = c.is_active(); } }); }
        mv.append(&ratio);
        let auto = gtk::CheckButton::builder().label("Auto-select").tooltip_text("A press picks the layer under the pointer (Ctrl-click does too)").build();
        { let doc = doc.clone(); auto.connect_toggled(move |c| { if let Ok(mut d) = doc.try_borrow_mut() { d.auto_select = c.is_active(); } }); }
        mv.append(&auto);
        for (label, horizontal) in [("Flip H", true), ("Flip V", false)] {
            let b = gtk::Button::builder().label(label).action_name(if horizontal { "win.flip-horizontal" } else { "win.flip-vertical" }).build();
            mv.append(&b);
        }
        stack.add_named(&mv, Some("move"));

        // Marquee and Lasso.
        let marquee = row();
        marquee.append(&mode_buttons(&doc));
        let shape = gtk::DropDown::from_strings(&["Rectangle", "Ellipse"]);
        { let doc = doc.clone(); shape.connect_selected_notify(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.marquee_ellipse = s.selected() == 1; } }); }
        marquee.append(&shape);
        let smooth = gtk::CheckButton::builder().label("Anti-alias").active(true).build();
        { let doc = doc.clone(); smooth.connect_toggled(move |c| { if let Ok(mut d) = doc.try_borrow_mut() { d.antialiased = c.is_active(); } }); }
        marquee.append(&smooth);
        stack.add_named(&marquee, Some("marquee"));
        let lasso = row();
        lasso.append(&mode_buttons(&doc));
        let kind = gtk::DropDown::from_strings(&["Freehand", "Polygonal"]);
        { let doc = doc.clone(); kind.connect_selected_notify(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.lasso_polygonal = s.selected() == 1; } }); }
        lasso.append(&kind);
        stack.add_named(&lasso, Some("lasso"));

        // Magic Wand.
        let wand = row();
        wand.append(&mode_buttons(&doc));
        wand.append(&gtk::Label::new(Some("Tolerance")));
        let tolerance = gtk::SpinButton::with_range(0.0, 255.0, 1.0);
        tolerance.set_value(doc.borrow().wand.tolerance as f64);
        tolerance.set_tooltip_text(Some("How far each color channel (0 to 255) can differ from the clicked color and still be selected"));
        { let doc = doc.clone(); tolerance.connect_value_changed(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.wand.tolerance = s.value() as i32; } }); }
        wand.append(&tolerance);
        let sample = gtk::DropDown::from_strings(&["Point Sample", "3 by 3 Average", "5 by 5 Average"]);
        sample.set_tooltip_text(Some("Match the clicked pixel, or the average of the pixels around it"));
        { let doc = doc.clone(); sample.connect_selected_notify(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.wand.sample_radius = s.selected() as usize; } }); }
        wand.append(&sample);
        let source = gtk::DropDown::from_strings(&["This Layer", "All Layers"]);
        source.set_tooltip_text(Some("Read colors from the active layer only, or from every visible layer as shown"));
        { let doc = doc.clone(); source.connect_selected_notify(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.wand.sample_all_layers = s.selected() == 1; } }); }
        wand.append(&source);
        let contiguous = gtk::CheckButton::builder().label("Contiguous").active(true).tooltip_text("Select only similar pixels connected to the one you click; off selects them everywhere").build();
        { let doc = doc.clone(); contiguous.connect_toggled(move |c| { if let Ok(mut d) = doc.try_borrow_mut() { d.wand.contiguous = c.is_active(); } }); }
        wand.append(&contiguous);
        stack.add_named(&wand, Some("wand"));

        // Brush tools share size, hardness and opacity; each adds its own controls after them.
        let brushes = row();
        let settings = doc.borrow().brush.clone();
        brushes.append(&gtk::Label::new(Some("Size")));
        let size = gtk::SpinButton::with_range(1.0, 2000.0, 1.0);
        size.set_value(settings.diameter);
        size.set_tooltip_text(Some("Brush diameter in document pixels ([ and ] step it)"));
        { let doc = doc.clone(); size.connect_value_changed(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.brush.diameter = s.value(); } }); }
        brushes.append(&size);
        brushes.append(&gtk::Label::new(Some("Hardness")));
        let hardness = gtk::SpinButton::with_range(0.0, 100.0, 1.0);
        hardness.set_value(settings.hardness * 100.0);
        hardness.set_tooltip_text(Some("Percent of the radius painted at full strength (Shift+[ and Shift+] step it)"));
        { let doc = doc.clone(); hardness.connect_value_changed(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.brush.hardness = s.value() / 100.0; } }); }
        brushes.append(&hardness);
        brushes.append(&gtk::Label::new(Some("Opacity")));
        let opacity = gtk::SpinButton::with_range(1.0, 100.0, 1.0);
        opacity.set_value(settings.opacity * 100.0);
        opacity.set_tooltip_text(Some("The whole stroke's opacity cap (number keys set it: 1 is 10%, 0 is 100%)"));
        { let doc = doc.clone(); opacity.connect_value_changed(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.brush.opacity = s.value() / 100.0; } }); }
        brushes.append(&opacity);
        let mask_paint = gtk::DropDown::from_strings(&["Black · Hide", "White · Reveal"]);
        mask_paint.set_tooltip_text(Some("What the brush paints on a mask"));
        mask_paint.set_visible(false);
        { let doc = doc.clone(); mask_paint.connect_selected_notify(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.mask_paint_white = s.selected() == 1; } }); }
        brushes.append(&mask_paint);
        let extra = gtk::Stack::builder().vhomogeneous(false).hhomogeneous(false).build();
        // Brush: the paint color.
        let paint = row();
        paint.append(&gtk::Label::new(Some("Color")));
        let color = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
        let c = settings.color;
        color.set_rgba(&gdk::RGBA::new(c[0] as f32, c[1] as f32, c[2] as f32, 1.0));
        { let doc = doc.clone(); color.connect_rgba_notify(move |b| { let c = b.rgba(); if let Ok(mut d) = doc.try_borrow_mut() { d.brush.color = [c.red() as f64, c.green() as f64, c.blue() as f64]; } }); }
        paint.append(&color);
        extra.add_named(&paint, Some("brush"));
        extra.add_named(&gtk::Box::new(gtk::Orientation::Horizontal, 0), Some("eraser"));
        let blur = row();
        blur.append(&gtk::Label::new(Some("Mode")));
        let blur_mode = gtk::DropDown::from_strings(&["Liquify", "Blur", "Smudge"]);
        blur_mode.set_tooltip_text(Some("Liquify pushes pixels along the drag, Blur softens under the tip, Smudge drags color along"));
        { let doc = doc.clone(); blur_mode.connect_selected_notify(move |m| { if let Ok(mut d) = doc.try_borrow_mut() { d.blur_mode = m.selected(); } }); }
        blur.append(&blur_mode);
        extra.add_named(&blur, Some("blur"));
        let heal = row();
        heal.append(&gtk::Label::new(Some("Type")));
        let mode = gtk::DropDown::from_strings(&["Content-Aware", "Create Texture", "Proximity Match"]);
        { let doc = doc.clone(); mode.connect_selected_notify(move |m| { if let Ok(mut d) = doc.try_borrow_mut() { d.heal_mode = m.selected() as i32; } }); }
        heal.append(&mode);
        extra.add_named(&heal, Some("heal"));
        let clone = row();
        let aligned = gtk::CheckButton::builder().label("Aligned").active(true).tooltip_text("The source moves with the brush and keeps its offset between strokes; off, every stroke starts again at the source point").build();
        { let doc = doc.clone(); aligned.connect_toggled(move |c| { if let Ok(mut d) = doc.try_borrow_mut() { d.clone_aligned = c.is_active(); d.clone_offset = None; } }); }
        clone.append(&aligned);
        let clone_source = gtk::DropDown::from_strings(&["This Layer", "All Layers"]);
        { let doc = doc.clone(); clone_source.connect_selected_notify(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.clone_all_layers = s.selected() == 1; } }); }
        clone.append(&clone_source);
        clone.append(&gtk::Label::builder().label("Alt-click to set the source").css_classes(["dim-label"]).build());
        extra.add_named(&clone, Some("clone"));
        brushes.append(&extra);
        stack.add_named(&brushes, Some("brushes"));

        let bar = OptionsBar { widget: stack, size, hardness, opacity, move_fields, mask_paint, syncing: std::cell::Cell::new(false) };
        bar.connect_move_fields(&doc);
        bar.update(doc.borrow().tool);
        bar
    }

    /// Typing into X, Y, W, H or Angle places the active layer, as one undo step per change.
    fn connect_move_fields(&self, doc: &DocRef) {
        for (i, spin) in self.move_fields.iter().enumerate() {
            let doc = doc.clone();
            let fields: Vec<gtk::SpinButton> = self.move_fields.clone();
            spin.connect_value_changed(move |_| {
                let Ok(mut d) = doc.try_borrow_mut() else { return };
                if d.syncing_inspector { return; }
                let Some(id) = d.document.active else { return };
                let mut t = d.document.renderer.layer(id).transform;
                let v: Vec<f64> = fields.iter().map(|f| f.value()).collect();
                match i { 0 => t.origin.0 = v[0], 1 => t.origin.1 = v[1], 2 => t.size.0 = v[2], 3 => t.size.1 = v[3], _ => t.rotation = v[4] }
                d.document.set_transform(id, t, "Transform Layer");
                d.needs_redraw = true;
            });
        }
    }

    /// Shows the active layer's placement in the inspector, or blanks it.
    pub fn sync_move(&self, doc: &DocRef) {
        let (values, sensitive) = {
            let d = doc.borrow();
            match d.document.active.filter(|id| !d.document.renderer.layer(*id).is_group()) {
                Some(id) => { let t = d.document.renderer.layer(id).transform; ([t.origin.0, t.origin.1, t.size.0, t.size.1, t.rotation], true) }
                None => ([0.0; 5], false),
            }
        };
        doc.borrow_mut().syncing_inspector = true;
        for (spin, v) in self.move_fields.iter().zip(values) { spin.set_sensitive(sensitive); spin.set_value(v); }
        doc.borrow_mut().syncing_inspector = false;
    }

    pub fn show_mask_paint(&self, on: bool) { self.mask_paint.set_visible(on); }

    pub fn update(&self, tool: Tool) {
        let hint = self.widget.child_by_name("hint").and_downcast::<gtk::Label>();
        let _ = self.syncing.get();
        match tool {
            Tool::Move => self.widget.set_visible_child_name("move"),
            Tool::Marquee => self.widget.set_visible_child_name("marquee"),
            Tool::Lasso => self.widget.set_visible_child_name("lasso"),
            Tool::Wand => self.widget.set_visible_child_name("wand"),
            t if t.is_brush() => {
                self.widget.set_visible_child_name("brushes");
                if let Some(brushes) = self.widget.child_by_name("brushes") {
                    if let Some(extra) = brushes.last_child().and_downcast::<gtk::Stack>() {
                        extra.set_visible_child_name(match t { Tool::Brush => "brush", Tool::Eraser => "eraser", Tool::Heal => "heal", Tool::Clone => "clone", _ => "blur" });
                    }
                }
            }
            other => { if let Some(hint) = hint { hint.set_label(other.help()); } self.widget.set_visible_child_name("hint"); }
        }
    }

    /// Reflects settings changed from the keyboard.
    pub fn sync_brush(&self, settings: &crate::brush::BrushSettings) {
        self.size.set_value(settings.diameter);
        self.hardness.set_value((settings.hardness * 100.0).round());
        self.opacity.set_value((settings.opacity * 100.0).round());
    }
}

fn row() -> gtk::Box {
    gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(10).margin_start(12).margin_end(12).margin_top(6).margin_bottom(6).build()
}
