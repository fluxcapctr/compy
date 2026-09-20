//! The tool rail down the canvas's left edge and the options bar above it.

use super::DocRef;
use crate::selection::Mode;
use gtk::prelude::*;
use gtk::glib;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool { Move, Marquee, Lasso, Wand, Crop, Eyedropper, Brush, Eraser, Heal, Clone, Blur, Dodge, Gradient, Pen, Type, Shape, Hand, Zoom }

impl Tool {
    pub const ALL: [Tool; 18] = [Tool::Move, Tool::Marquee, Tool::Lasso, Tool::Wand, Tool::Crop, Tool::Eyedropper, Tool::Brush, Tool::Eraser, Tool::Heal, Tool::Clone, Tool::Blur, Tool::Dodge, Tool::Gradient, Tool::Pen, Tool::Type, Tool::Shape, Tool::Hand, Tool::Zoom];
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
            Tool::Dodge => "Dodge / Burn / Sponge (O): paint to lighten, darken, or change the saturation; the options pick the mode and the tonal range; brush opacity is the exposure",
            Tool::Crop => "Crop (C): drag a frame, then Return crops the canvas to it; edges snap to layers; Alt keeps the center; Escape cancels",
            Tool::Eyedropper => "Eyedropper (I): click to pick the foreground color from the canvas; Alt-click sets the background color",
            Tool::Gradient => "Gradient (G): drag a line to fill the layer (or its mask) with a gradient inside the selection; Shift snaps the angle",
            Tool::Pen => "Pen (P): click to place corner points, drag to pull out curve handles; click the first point to close; Return ends an open path, Backspace removes the last point, Escape clears; then Make Selection, Fill or Stroke with the brush",
            Tool::Type => "Type (T): click to set text on a new layer in the foreground color, or click existing text to edit it; the options set the font",
            Tool::Shape => "Shape (U): drag a rectangle or ellipse onto a new layer in the foreground color; Shift squares it, Alt grows from the center; Shift+U swaps the kind",
            Tool::Hand => "Hand (H): drag to pan",
            Tool::Zoom => "Zoom (Z): click to zoom in, Alt-click to zoom out",
        }
    }
    /// The tool's name with its key, as the rail's tooltip shows it: "Move (V)".
    pub fn name(self) -> &'static str { self.help().split(':').next().unwrap_or("") }
    pub fn key(self) -> char { match self { Tool::Move => 'v', Tool::Marquee => 'm', Tool::Lasso => 'l', Tool::Wand => 'w', Tool::Crop => 'c', Tool::Eyedropper => 'i', Tool::Brush => 'b', Tool::Eraser => 'e', Tool::Heal => 'j', Tool::Clone => 's', Tool::Blur => 'r', Tool::Dodge => 'o', Tool::Gradient => 'g', Tool::Pen => 'p', Tool::Type => 't', Tool::Shape => 'u', Tool::Hand => 'h', Tool::Zoom => 'z' } }
    pub fn is_brush(self) -> bool { matches!(self, Tool::Brush | Tool::Eraser | Tool::Heal | Tool::Clone | Tool::Blur | Tool::Dodge) }
    pub fn is_selection(self) -> bool { matches!(self, Tool::Marquee | Tool::Lasso | Tool::Wand) }
}

pub struct ToolRail {
    pub widget: gtk::Box,
    buttons: Vec<(Tool, gtk::ToggleButton)>,
    foreground: Rc<super::color_wheel::ColorButton>,
    background: Rc<super::color_wheel::ColorButton>,
}

impl ToolRail {
    /// `changed` runs after the document's tool has been set.
    pub fn new(doc: DocRef, changed: Rc<dyn Fn()>) -> Rc<ToolRail> {
        let widget = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(1).width_request(34)
            .margin_top(6).margin_start(3).margin_end(3).css_classes(["tool-rail"]).build();
        let mut buttons = Vec::new();
        let current = doc.borrow().tool;
        let mut group: Option<gtk::ToggleButton> = None;
        for tool in Tool::ALL {
            let button = gtk::ToggleButton::builder().tooltip_text(tool.name()).width_request(28).height_request(26).active(tool == current).css_classes(["tool"]).build();
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
        // The palette: foreground over background, as Photoshop's rail has them (X swaps, D resets).
        let (fg, bg) = { let d = doc.borrow(); (d.brush.color, d.background) };
        let foreground = { let doc = doc.clone(); super::color_wheel::ColorButton::new(fg, Rc::new(move |c| { if let Ok(mut d) = doc.try_borrow_mut() { d.brush.color = c; } })) };
        let background = { let doc = doc.clone(); super::color_wheel::ColorButton::new(bg, Rc::new(move |c| { if let Ok(mut d) = doc.try_borrow_mut() { d.background = c; } })) };
        foreground.widget.set_tooltip_text(Some("Foreground color (X swaps with the background, D resets to black and white)"));
        background.widget.set_tooltip_text(Some("Background color"));
        // The squares overlap as Photoshop's do, with the swap and default marks beside them.
        foreground.set_compact();
        background.set_compact();
        let fixed = gtk::Fixed::builder().width_request(30).height_request(30).margin_top(8).halign(gtk::Align::Center).build();
        fixed.put(&background.widget, 9.0, 9.0);
        fixed.put(&foreground.widget, 0.0, 0.0);
        let marks = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).halign(gtk::Align::Center).spacing(2).margin_top(4).build();
        for (glyph, action, tip) in [("default-colors", "win.default-colors", "Default colors: black over white (D)"), ("swap", "win.swap-colors", "Swap the foreground and background colors (X)")] {
            let b = gtk::Button::builder().has_frame(false).action_name(action).tooltip_text(tip).css_classes(["tool", "mark"]).build();
            b.set_child(Some(&super::icons::glyph(glyph, 13)));
            marks.append(&b);
        }
        widget.append(&marks);
        widget.append(&fixed);
        Rc::new(ToolRail { widget, buttons, foreground, background })
    }

    /// Shows the palette as the document has it (after X, D or the eyedropper).
    pub fn sync_palette(&self, foreground: [f64; 3], background: [f64; 3]) { self.foreground.set_color(foreground); self.background.set_color(background); }

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
    color: Rc<super::color_wheel::ColorButton>,
    picker: Rc<super::brushes::BrushPicker>,
    spacing: gtk::SpinButton,
    angle: gtk::SpinButton,
    roundness: gtk::SpinButton,
    jitter: gtk::SpinButton,
    type_page: TypePage,
    gradient_preview: gtk::DrawingArea,
    pattern_list: gtk::DropDown,
    syncing: std::cell::Cell<bool>,
    /// Repaints the canvas after a field changed the document (set by the canvas that owns this bar).
    redraw: RedrawHook,
}

/// The Pattern Stamp's choices: none, then every saved pattern.
fn pattern_choices() -> Vec<String> {
    let mut names = vec!["None (sampled pixels)".to_string()];
    names.extend(crate::patterns::list());
    names
}

/// A gradient over a checkerboard, as the options bar and the editor show it.
pub fn draw_gradient_bar(cr: &cairo::Context, w: f64, h: f64, gradient: &crate::gradient::Gradient) {
    let tile = 6.0;
    for row in 0..(h / tile).ceil() as i32 { for col in 0..(w / tile).ceil() as i32 { let v = if (row + col) % 2 == 0 { 0.55 } else { 0.75 }; cr.set_source_rgb(v, v, v); cr.rectangle(col as f64 * tile, row as f64 * tile, tile, tile); let _ = cr.fill(); } }
    let p = cairo::LinearGradient::new(0.0, 0.0, w, 0.0);
    gradient.fill_pattern(&p, 0.0, 1.0);
    let _ = cr.set_source(&p);
    cr.rectangle(0.0, 0.0, w, h);
    let _ = cr.fill();
}

/// The Type tool's options: the font and its setting.
pub struct TypePage {
    families: gtk::StringList,
    family: gtk::DropDown,
    size: gtk::SpinButton,
    bold: gtk::ToggleButton,
    italic: gtk::ToggleButton,
    align: gtk::DropDown,
    leading: gtk::SpinButton,
    tracking: gtk::SpinButton,
    width: gtk::SpinButton,
    google: gtk::Entry,
    status: gtk::Label,
}

type RedrawHook = Rc<std::cell::RefCell<Option<Rc<dyn Fn()>>>>;
fn call_redraw(hook: &RedrawHook) { if let Some(f) = hook.borrow().as_ref() { f(); } }

impl TypePage {
    fn build(doc: &DocRef, redraw: &RedrawHook) -> (gtk::Box, TypePage) {
        let page = row();
        let names = crate::text::families();
        let families = gtk::StringList::new(&names.iter().map(String::as_str).collect::<Vec<_>>());
        let family = gtk::DropDown::builder().model(&families).enable_search(true).tooltip_text("Font family; type to search").build();
        family.set_expression(Some(&gtk::PropertyExpression::new(gtk::StringObject::static_type(), None::<gtk::Expression>, "string")));
        let current = doc.borrow().text_style.family.clone();
        if let Some(i) = names.iter().position(|n| *n == current) { family.set_selected(i as u32); }
        page.append(&family);
        page.append(&gtk::Label::new(Some("Size")));
        let size = gtk::SpinButton::with_range(1.0, 2000.0, 1.0);
        size.set_value(doc.borrow().text_style.size);
        size.set_tooltip_text(Some("Font size in document pixels"));
        page.append(&size);
        let styles = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).css_classes(["linked"]).build();
        let bold = gtk::ToggleButton::builder().label("B").tooltip_text("Bold").build();
        bold.add_css_class("type-bold");
        let italic = gtk::ToggleButton::builder().label("I").tooltip_text("Italic").build();
        italic.add_css_class("type-italic");
        styles.append(&bold);
        styles.append(&italic);
        page.append(&styles);
        let align = gtk::DropDown::from_strings(&["Left", "Center", "Right"]);
        align.set_tooltip_text(Some("Alignment of the lines"));
        page.append(&align);
        page.append(&gtk::Label::new(Some("Leading")));
        let leading = gtk::SpinButton::with_range(0.5, 5.0, 0.1);
        leading.set_digits(2);
        leading.set_value(doc.borrow().text_style.leading);
        leading.set_tooltip_text(Some("Line height as a multiple of the size"));
        page.append(&leading);
        page.append(&gtk::Label::new(Some("Tracking")));
        let tracking = gtk::SpinButton::with_range(-100.0, 500.0, 1.0);
        tracking.set_tooltip_text(Some("Letter spacing in pixels"));
        page.append(&tracking);
        page.append(&gtk::Label::new(Some("Width")));
        let width = gtk::SpinButton::with_range(0.0, 30_000.0, 10.0);
        width.set_value(doc.borrow().text_style.width.unwrap_or(0.0));
        width.set_tooltip_text(Some("Paragraph width in pixels: the lines wrap at it; 0 is point text that never wraps. Dragging with the Type tool sets it too"));
        page.append(&width);
        let google = gtk::Entry::builder().placeholder_text("Google font, e.g. Lobster").width_chars(18).tooltip_text("A family name from fonts.google.com; Get downloads it into your fonts").build();
        page.append(&google);
        let get = gtk::Button::builder().label("Get").tooltip_text("Download the family from Google Fonts").build();
        page.append(&get);
        let status = gtk::Label::builder().css_classes(["dim-label"]).build();
        page.append(&status);
        let this = TypePage { families, family, size, bold, italic, align, leading, tracking, width, google, status };
        this.connect(doc, redraw);
        { let google = this.google.clone(); get.connect_clicked(move |_| google.emit_activate()); }
        (page, this)
    }

    /// Each control changes the style for new text and, with a type layer active, that layer.
    fn connect(&self, doc: &DocRef, redraw: &RedrawHook) {
        let redraw = redraw.clone();
        let apply = move |doc: &DocRef, change: &dyn Fn(&mut crate::text::TextStyle)| {
            let changed = {
                let Ok(mut d) = doc.try_borrow_mut() else { return };
                if d.syncing_inspector { return; }
                change(&mut d.text_style);
                let mut changed = false;
                if let Some(id) = d.document.active {
                    if let Some(mut style) = d.document.text_style(id) {
                        change(&mut style);
                        if let Err(error) = d.document.set_text(id, &style) { eprintln!("type: {error:#}"); }
                        changed = true;
                    }
                }
                changed
            };
            if changed { call_redraw(&redraw); }
        };
        let apply = Rc::new(apply);
        { let (doc, apply) = (doc.clone(), apply.clone()); self.family.connect_selected_notify(move |f| { let Some(name) = f.selected_item().and_downcast::<gtk::StringObject>().map(|o| o.string().to_string()) else { return }; apply(&doc, &|s| s.family = name.clone()); }); }
        { let (doc, apply) = (doc.clone(), apply.clone()); self.size.connect_value_changed(move |s| { let v = s.value(); apply(&doc, &|st| st.size = v); }); }
        { let (doc, apply) = (doc.clone(), apply.clone()); self.bold.connect_toggled(move |b| { let v = b.is_active(); apply(&doc, &|st| st.bold = v); }); }
        { let (doc, apply) = (doc.clone(), apply.clone()); self.italic.connect_toggled(move |b| { let v = b.is_active(); apply(&doc, &|st| st.italic = v); }); }
        { let (doc, apply) = (doc.clone(), apply.clone()); self.align.connect_selected_notify(move |a| { let v = a.selected(); apply(&doc, &|st| st.align = v); }); }
        { let (doc, apply) = (doc.clone(), apply.clone()); self.leading.connect_value_changed(move |s| { let v = s.value(); apply(&doc, &|st| st.leading = v); }); }
        { let (doc, apply) = (doc.clone(), apply.clone()); self.tracking.connect_value_changed(move |s| { let v = s.value(); apply(&doc, &|st| st.tracking = v); }); }
        { let (doc, apply) = (doc.clone(), apply.clone()); self.width.connect_value_changed(move |s| { let v = s.value(); apply(&doc, &|st| st.width = if v >= 1.0 { Some(v) } else { None }); }); }
        {
            let (families, family, status) = (self.families.clone(), self.family.clone(), self.status.clone());
            let (doc, apply) = (doc.clone(), apply.clone());
            self.google.connect_activate(move |entry| {
                let name = entry.text().trim().to_string();
                if name.is_empty() { return; }
                status.set_label("Fetching…");
                entry.set_sensitive(false);
                let (tx, rx) = std::sync::mpsc::channel();
                { let name = name.clone(); std::thread::spawn(move || { let _ = tx.send(crate::text::fetch_google_family(&name)); }); }
                let (families, family, status, entry, doc, apply) = (families.clone(), family.clone(), status.clone(), entry.clone(), doc.clone(), apply.clone());
                glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
                    let Ok(result) = rx.try_recv() else { return glib::ControlFlow::Continue };
                    entry.set_sensitive(true);
                    match result {
                        Ok(n) => {
                            status.set_label(&format!("Got {name} ({n} files)"));
                            entry.set_text("");
                            let names = crate::text::families();
                            families.splice(0, families.n_items(), &names.iter().map(String::as_str).collect::<Vec<_>>());
                            if let Some(i) = names.iter().position(|f| f.eq_ignore_ascii_case(&name)) { family.set_selected(i as u32); }
                            else { apply(&doc, &|s| s.family = name.clone()); }
                        }
                        Err(error) => status.set_label(&format!("{error:#}")),
                    }
                    glib::ControlFlow::Break
                });
            });
        }
    }

    /// Shows a style in the controls without applying it back.
    fn show(&self, doc: &DocRef, style: &crate::text::TextStyle) {
        doc.borrow_mut().syncing_inspector = true;
        let n = self.families.n_items();
        if let Some(i) = (0..n).find(|i| self.families.string(*i).is_some_and(|s| s == style.family)) { self.family.set_selected(i); }
        self.size.set_value(style.size);
        self.bold.set_active(style.bold);
        self.italic.set_active(style.italic);
        self.align.set_selected(style.align.min(2));
        self.leading.set_value(style.leading);
        self.tracking.set_value(style.tracking);
        self.width.set_value(style.width.unwrap_or(0.0));
        doc.borrow_mut().syncing_inspector = false;
    }
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
        // Sized to the page showing, so a wide page (the brush controls) never forces the window wider.
        // The same height on every page, so the bar never jumps when the tool changes.
        let stack = gtk::Stack::builder().vhomogeneous(true).hhomogeneous(false).css_classes(["options"]).build();
        let hint = gtk::Label::builder().xalign(0.0).margin_start(12).margin_top(8).margin_bottom(8).css_classes(["dim-label"]).build();
        stack.add_named(&hint, Some("hint"));

        // Move: the transform inspector.
        let mv = row();
        let mut move_fields = Vec::new();
        for (label, low, high, tip) in [("X", -1.0e6, 1.0e6, "Left edge, document pixels"), ("Y", -1.0e6, 1.0e6, "Top edge, document pixels"), ("W", 1.0, 30_000.0, "Width, document pixels"), ("H", 1.0, 30_000.0, "Height, document pixels"), ("Angle", -3600.0, 3600.0, "Clockwise rotation in degrees")] {
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
        marquee.append(&gtk::Button::builder().label("Generative Fill…").action_name("win.generative-fill").tooltip_text("Paint the selection with a fal.ai model (Ctrl+Shift+G, or right-click the selection)").build());
        stack.add_named(&marquee, Some("marquee"));
        let lasso = row();
        lasso.append(&mode_buttons(&doc));
        let kind = gtk::DropDown::from_strings(&["Freehand", "Polygonal"]);
        { let doc = doc.clone(); kind.connect_selected_notify(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.lasso_polygonal = s.selected() == 1; } }); }
        lasso.append(&kind);
        lasso.append(&gtk::Button::builder().label("Generative Fill…").action_name("win.generative-fill").build());
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
        wand.append(&gtk::Button::builder().label("Generative Fill…").action_name("win.generative-fill").build());
        stack.add_named(&wand, Some("wand"));

        // Brush tools share the tip, size, hardness, spacing, angle, roundness and opacity; each adds its own
        // controls after them.
        let brushes = row();
        let settings = doc.borrow().brush.clone();
        let (hardness_cell, size_cell): (Rc<std::cell::RefCell<Option<gtk::SpinButton>>>, ()) = (Rc::new(std::cell::RefCell::new(None)), ());
        let _ = size_cell;
        let picker = { let (doc, cell) = (doc.clone(), hardness_cell.clone()); super::brushes::BrushPicker::new(doc.clone(), Rc::new(move || { if let Some(h) = cell.borrow().as_ref() { let v = doc.borrow().brush.hardness; h.set_value((v * 100.0).round()); } })) };
        brushes.append(&picker.widget);
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
        *hardness_cell.borrow_mut() = Some(hardness.clone());
        brushes.append(&hardness);
        brushes.append(&gtk::Label::new(Some("Spacing")));
        let spacing = gtk::SpinButton::with_range(0.0, 1000.0, 1.0);
        spacing.set_value(settings.spacing.map_or(0.0, |s| (s * 100.0).round()));
        spacing.set_tooltip_text(Some("Dab spacing as a percent of the size; 0 is automatic (dense, for a solid mark)"));
        { let doc = doc.clone(); spacing.connect_value_changed(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.brush.spacing = if s.value() <= 0.0 { None } else { Some(s.value() / 100.0) }; } }); }
        brushes.append(&spacing);
        brushes.append(&gtk::Label::new(Some("Angle")));
        let angle = gtk::SpinButton::with_range(-180.0, 180.0, 1.0);
        angle.set_value(settings.angle);
        angle.set_tooltip_text(Some("The tip's rotation in degrees"));
        { let doc = doc.clone(); angle.connect_value_changed(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.brush.angle = s.value(); } }); }
        brushes.append(&angle);
        brushes.append(&gtk::Label::new(Some("Roundness")));
        let roundness = gtk::SpinButton::with_range(5.0, 100.0, 1.0);
        roundness.set_value((settings.roundness * 100.0).round());
        roundness.set_tooltip_text(Some("Percent: 100 is the tip as it is, less squashes it across its angle"));
        { let doc = doc.clone(); roundness.connect_value_changed(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.brush.roundness = s.value() / 100.0; } }); }
        brushes.append(&roundness);
        brushes.append(&gtk::Label::new(Some("Jitter")));
        let jitter = gtk::SpinButton::with_range(0.0, 100.0, 5.0);
        jitter.set_value((settings.angle_jitter * 100.0).round());
        jitter.set_tooltip_text(Some("Angle jitter, percent: each dab turns by a random share of a full turn, so a textured tip does not repeat"));
        { let doc = doc.clone(); jitter.connect_value_changed(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.brush.angle_jitter = s.value() / 100.0; } }); }
        brushes.append(&jitter);
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
        let color = { let doc = doc.clone(); super::color_wheel::ColorButton::new(settings.color, Rc::new(move |c| { if let Ok(mut d) = doc.try_borrow_mut() { d.brush.color = c; } })) };
        paint.append(&color.widget);
        extra.add_named(&paint, Some("brush"));
        extra.add_named(&gtk::Box::new(gtk::Orientation::Horizontal, 0), Some("eraser"));
        let blur = row();
        blur.append(&gtk::Label::new(Some("Mode")));
        let blur_mode = gtk::DropDown::from_strings(&["Liquify", "Blur", "Smudge"]);
        blur_mode.set_tooltip_text(Some("Liquify pushes pixels along the drag, Blur softens under the tip, Smudge drags color along"));
        { let doc = doc.clone(); blur_mode.connect_selected_notify(move |m| { if let Ok(mut d) = doc.try_borrow_mut() { d.blur_mode = m.selected(); } }); }
        blur.append(&blur_mode);
        extra.add_named(&blur, Some("blur"));
        let dodge = row();
        dodge.append(&gtk::Label::new(Some("Mode")));
        let dodge_mode = gtk::DropDown::from_strings(&["Dodge", "Burn", "Saturate", "Desaturate"]);
        dodge_mode.set_tooltip_text(Some("Dodge lightens, Burn darkens; Saturate and Desaturate are the Sponge"));
        { let doc = doc.clone(); dodge_mode.connect_selected_notify(move |m| { if let Ok(mut d) = doc.try_borrow_mut() { d.dodge_mode = m.selected(); } }); }
        dodge.append(&dodge_mode);
        dodge.append(&gtk::Label::new(Some("Range")));
        let dodge_range = gtk::DropDown::from_strings(&["Shadows", "Midtones", "Highlights"]);
        dodge_range.set_selected(1);
        dodge_range.set_tooltip_text(Some("Which tones Dodge and Burn work on most"));
        { let doc = doc.clone(); dodge_range.connect_selected_notify(move |m| { if let Ok(mut d) = doc.try_borrow_mut() { d.dodge_range = m.selected(); } }); }
        dodge.append(&dodge_range);
        extra.add_named(&dodge, Some("dodge"));
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
        clone.append(&gtk::Label::new(Some("Pattern")));
        let pattern_model = gtk::StringList::new(&pattern_choices().iter().map(String::as_str).collect::<Vec<_>>());
        let pattern = gtk::DropDown::builder().model(&pattern_model).build();
        pattern.set_tooltip_text(Some("The Pattern Stamp: paint a saved pattern instead of sampled pixels (Edit > Define Pattern makes one)"));
        { let doc = doc.clone(); pattern.connect_selected_notify(move |p| { let name = p.selected_item().and_downcast::<gtk::StringObject>().map(|o| o.string().to_string()); if let Ok(mut d) = doc.try_borrow_mut() { d.clone_pattern = if p.selected() == 0 { None } else { name }; } }); }
        clone.append(&pattern);
        let pattern_list = pattern.clone();
        clone.append(&gtk::Label::builder().label("Alt-click to set the source").css_classes(["dim-label"]).build());
        extra.add_named(&clone, Some("clone"));
        brushes.append(&extra);
        stack.add_named(&brushes, Some("brushes"));

        // Gradient: a preview that opens the editor, the preset, the shape, reverse and opacity.
        let gradient = row();
        let preview = gtk::DrawingArea::builder().content_width(96).content_height(18).tooltip_text("The gradient; click to edit its colors and opacity stops").css_classes(["gradient-preview"]).build();
        { let doc = doc.clone(); preview.set_draw_func(move |_, cr, w, h| { if let Ok(d) = doc.try_borrow() { draw_gradient_bar(cr, w as f64, h as f64, &super::canvas::Canvas::current_gradient(&d)); } }); }
        let preset = gtk::DropDown::from_strings(&super::canvas::gradient_preset_names().iter().map(String::as_str).collect::<Vec<_>>());
        preset.set_tooltip_text(Some("Foreground and background presets follow the palette; Custom is what the editor made"));
        { let (doc, preview) = (doc.clone(), preview.clone()); preset.connect_selected_notify(move |p| { if let Ok(mut d) = doc.try_borrow_mut() { d.gradient_preset = p.selected(); } preview.queue_draw(); }); }
        {
            let (doc_c, preview_c, preset_c) = (doc.clone(), preview.clone(), preset.clone());
            let click = gtk::GestureClick::new();
            click.connect_released(move |g, _, _, _| {
                let Some(root) = g.widget().and_then(|w| w.root()).and_downcast::<gtk::Window>() else { return };
                let (initial, fg, bg) = { let d = doc_c.borrow(); (super::canvas::gradient_for(d.gradient_preset, d.brush.color, d.background, &d.gradient_custom), d.brush.color, d.background) };
                let (doc2, preview2, preset2) = (doc_c.clone(), preview_c.clone(), preset_c.clone());
                let custom_index = super::canvas::gradient_preset_names().len() as u32 - 1;
                super::gradient_editor::open(&root, initial, fg, bg, std::rc::Rc::new(move |g: crate::gradient::Gradient| {
                    if let Ok(mut d) = doc2.try_borrow_mut() { d.gradient_custom = g; d.gradient_preset = custom_index; }
                    preset2.set_selected(custom_index);
                    preview2.queue_draw();
                }));
            });
            preview.add_controller(click);
        }
        gradient.append(&preview);
        gradient.append(&preset);
        let gshape = gtk::DropDown::from_strings(&crate::gradient::Shape::ALL.map(|s| s.name()));
        gshape.set_tooltip_text(Some("Linear runs start to end; Radial spreads from the start; Angle sweeps around it; Reflected mirrors across it; Diamond grows a square from it"));
        { let doc = doc.clone(); gshape.connect_selected_notify(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.gradient_shape = crate::gradient::Shape::ALL[s.selected() as usize]; } }); }
        gradient.append(&gshape);
        let reverse = gtk::CheckButton::builder().label("Reverse").build();
        { let (doc, preview) = (doc.clone(), preview.clone()); reverse.connect_toggled(move |c| { if let Ok(mut d) = doc.try_borrow_mut() { d.gradient_reversed = c.is_active(); } preview.queue_draw(); }); }
        gradient.append(&reverse);
        gradient.append(&gtk::Label::new(Some("Opacity")));
        let gopacity = gtk::SpinButton::with_range(1.0, 100.0, 1.0);
        gopacity.set_value(100.0);
        { let doc = doc.clone(); gopacity.connect_value_changed(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.gradient_opacity = s.value() / 100.0; } }); }
        gradient.append(&gopacity);
        let gradient_preview = preview.clone();
        stack.add_named(&gradient, Some("gradient"));

        // Type.
        let redraw: RedrawHook = Rc::new(std::cell::RefCell::new(None));
        let (type_row, type_page) = TypePage::build(&doc, &redraw);
        stack.add_named(&type_row, Some("type"));

        // Pen: what to do with the path.
        let pen = row();
        pen.append(&gtk::Button::builder().label("Make Selection").action_name("win.path-select").tooltip_text("Marching ants from the path (Ctrl+Return); an open path closes itself").build());
        pen.append(&gtk::Button::builder().label("Fill Path").action_name("win.path-fill").tooltip_text("Fill the path on the active layer with the foreground color").build());
        pen.append(&gtk::Button::builder().label("Stroke with Brush").action_name("win.path-stroke").tooltip_text("Paint along the path with the current brush and foreground color").build());
        pen.append(&gtk::Button::builder().label("Make Shape").action_name("win.path-shape").tooltip_text("A vector shape layer from the path, in the foreground color; Layer > Edit Shape Points picks it up again").build());
        pen.append(&gtk::Button::builder().label("Apply to Shape").action_name("win.shape-apply").tooltip_text("Put the edited points back onto the active shape layer").build());
        pen.append(&gtk::Button::builder().label("Clear").action_name("win.path-clear").tooltip_text("Drop the path (Escape)").build());
        pen.append(&gtk::Label::builder().label("Click for corners, drag for curves, click the first point to close").css_classes(["dim-label"]).build());
        stack.add_named(&pen, Some("pen"));

        // Shape.
        let shape_row = row();
        let skind = gtk::DropDown::from_strings(&["Rectangle", "Ellipse"]);
        { let doc = doc.clone(); skind.connect_selected_notify(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.shape_ellipse = s.selected() == 1; } }); }
        shape_row.append(&skind);
        shape_row.append(&gtk::Label::new(Some("Corner radius")));
        let radius = gtk::SpinButton::with_range(0.0, 10_000.0, 1.0);
        radius.set_tooltip_text(Some("Document pixels; rectangles only, at most half the shorter side"));
        { let doc = doc.clone(); radius.connect_value_changed(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.shape_radius = s.value(); } }); }
        shape_row.append(&radius);
        stack.add_named(&shape_row, Some("shape"));

        // Crop.
        let crop = row();
        crop.append(&gtk::Label::new(Some("Ratio")));
        let ratio = gtk::DropDown::from_strings(&["Free", "Original", "1:1", "4:3", "16:9"]);
        { let doc = doc.clone(); ratio.connect_selected_notify(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.crop_ratio = s.selected(); } }); }
        crop.append(&ratio);
        crop.append(&gtk::Button::builder().label("Apply").action_name("win.crop-apply").css_classes(["suggested-action"]).tooltip_text("Crop the canvas to the frame (Return)").build());
        crop.append(&gtk::Button::builder().label("Cancel").action_name("win.crop-cancel").tooltip_text("Drop the frame (Escape)").build());
        stack.add_named(&crop, Some("crop"));

        // Eyedropper.
        let eye = row();
        eye.append(&gtk::Label::new(Some("Sample")));
        let sample = gtk::DropDown::from_strings(&["All Layers", "Current Layer"]);
        { let doc = doc.clone(); sample.connect_selected_notify(move |s| { if let Ok(mut d) = doc.try_borrow_mut() { d.eyedropper_all_layers = s.selected() == 0; } }); }
        eye.append(&sample);
        eye.append(&gtk::Label::builder().label("Click picks the foreground color; Alt-click the background").css_classes(["dim-label"]).build());
        stack.add_named(&eye, Some("eyedropper"));

        super::center_spins(&stack);
        let bar = OptionsBar { widget: stack, redraw, size, hardness, opacity, move_fields, mask_paint, color, picker, spacing, angle, roundness, jitter, type_page, gradient_preview, pattern_list, syncing: std::cell::Cell::new(false) };
        bar.connect_move_fields(&doc);
        bar.update(doc.borrow().tool);
        bar
    }

    /// Typing into X, Y, W, H or Angle places the active layer, as one undo step per change.
    fn connect_move_fields(&self, doc: &DocRef) {
        for (i, spin) in self.move_fields.iter().enumerate() {
            let doc = doc.clone();
            let fields: Vec<gtk::SpinButton> = self.move_fields.clone();
            let redraw = self.redraw.clone();
            spin.connect_value_changed(move |_| {
                {
                    let Ok(mut d) = doc.try_borrow_mut() else { return };
                    if d.syncing_inspector { return; }
                    let Some(id) = d.document.active else { return };
                    let mut t = d.document.renderer.layer(id).transform;
                    let v: Vec<f64> = fields.iter().map(|f| f.value()).collect();
                    match i { 0 => t.origin.0 = v[0], 1 => t.origin.1 = v[1], 2 => t.size.0 = v[2], 3 => t.size.1 = v[3], _ => t.rotation = v[4] }
                    d.document.set_transform(id, t, "Transform Layer");
                }
                call_redraw(&redraw);
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

    /// Shows the active type layer's style in the Type options, or the style for new text.
    pub fn sync_type(&self, doc: &DocRef) {
        let style = { let d = doc.borrow(); d.document.active.and_then(|id| d.document.text_style(id)).unwrap_or_else(|| d.text_style.clone()) };
        self.type_page.show(doc, &style);
    }

    pub fn show_mask_paint(&self, on: bool) { self.mask_paint.set_visible(on); }
    pub fn sync_mask_paint(&self, doc: &DocRef) { let white = doc.borrow().mask_paint_white; self.mask_paint.set_selected(if white { 1 } else { 0 }); }
    pub fn sync_shape_kind(&self, ellipse: bool) {
        if let Some(page) = self.widget.child_by_name("shape") { if let Some(dropdown) = page.first_child().and_downcast::<gtk::DropDown>() { dropdown.set_selected(if ellipse { 1 } else { 0 }); } }
    }

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
                        extra.set_visible_child_name(match t { Tool::Brush => "brush", Tool::Eraser => "eraser", Tool::Heal => "heal", Tool::Clone => "clone", Tool::Dodge => "dodge", _ => "blur" });
                        if t == Tool::Clone { self.refresh_patterns(); }
                    }
                }
            }
            Tool::Gradient => self.widget.set_visible_child_name("gradient"),
            Tool::Type => self.widget.set_visible_child_name("type"),
            Tool::Pen => self.widget.set_visible_child_name("pen"),
            Tool::Shape => self.widget.set_visible_child_name("shape"),
            Tool::Crop => self.widget.set_visible_child_name("crop"),
            Tool::Eyedropper => self.widget.set_visible_child_name("eyedropper"),
            other => { if let Some(hint) = hint { hint.set_label(other.help()); } self.widget.set_visible_child_name("hint"); }
        }
    }

    /// Reflects settings changed from the keyboard.
    /// Opens the color picker once the button is on screen (the options page may just have switched).
    pub fn set_redraw(&self, f: Option<Rc<dyn Fn()>>) { *self.redraw.borrow_mut() = f; }

    pub fn show_color_picker(&self) { let button = self.color.widget.clone(); gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || button.popup()); }

    /// The gradient preview follows the palette (the foreground presets are made from it).
    pub fn sync_gradient(&self) { self.gradient_preview.queue_draw(); }

    /// The Pattern Stamp's list follows the patterns folder, keeping the chosen one by name.
    pub fn refresh_patterns(&self) {
        let names = pattern_choices();
        let current = self.pattern_list.selected_item().and_downcast::<gtk::StringObject>().map(|o| o.string().to_string());
        let existing: Vec<String> = self.pattern_list.model().and_downcast::<gtk::StringList>().map(|m| (0..m.n_items()).filter_map(|i| m.string(i).map(|s| s.to_string())).collect()).unwrap_or_default();
        if existing == names { return; }
        let model = gtk::StringList::new(&names.iter().map(String::as_str).collect::<Vec<_>>());
        self.pattern_list.set_model(Some(&model));
        let index = current.and_then(|c| names.iter().position(|n| *n == c)).unwrap_or(0);
        self.pattern_list.set_selected(index as u32);
    }

    pub fn sync_brush(&self, settings: &crate::brush::BrushSettings) {
        self.size.set_value(settings.diameter);
        self.hardness.set_value((settings.hardness * 100.0).round());
        self.opacity.set_value((settings.opacity * 100.0).round());
        self.spacing.set_value(settings.spacing.map_or(0.0, |s| (s * 100.0).round()));
        self.angle.set_value(settings.angle);
        self.roundness.set_value((settings.roundness * 100.0).round());
        self.jitter.set_value((settings.angle_jitter * 100.0).round());
        self.picker.sync();
    }

    pub fn show_brush_picker(&self) { self.picker.popup(); }
}

fn row() -> gtk::Box {
    gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(10).margin_start(12).margin_end(12).margin_top(6).margin_bottom(6).build()
}
