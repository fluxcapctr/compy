//! The GTK4 application: a window of project tabs, each a canvas with its tool rail beside a layers panel,
//! and a menu of edits that run on the current tab's document.

pub mod brushes;
mod canvas;
pub mod color_wheel;
mod dialogs;
mod effects;
mod filter_dialog;
mod genfill;
mod icons;
mod layers;
pub mod theme;
mod tools;

use crate::brush::BrushSettings;
use crate::document::{Document, WandSettings};
use crate::filters::Kind;
use crate::format;
use crate::selection::Mode;
use crate::viewport::Viewport;
use anyhow::{Context as _, Result};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;
pub use tools::Tool;
use uuid::Uuid;

pub const APP_ID: &str = "co.ericstevens.compositor";

/// One open project and how it is being looked at and worked on.
pub struct Doc {
    pub title: String,
    pub document: Document,
    pub viewport: Viewport,
    pub collapsed: HashSet<Uuid>,
    pub tool: Tool,
    pub wand: WandSettings,
    /// The options bar's New / Add / Subtract choice.
    pub mode: Mode,
    pub ants_phase: f64,
    pub brush: BrushSettings,
    pub heal_mode: i32,
    pub clone_aligned: bool,
    pub clone_all_layers: bool,
    /// Where Clone Stamp copies from (Alt-click), and the offset the first aligned stroke fixed.
    pub clone_source: Option<(f64, f64)>,
    pub clone_offset: Option<(f64, f64)>,
    /// Where the last stroke ended, for Shift-click lines.
    pub last_brush_point: Option<(f64, f64)>,
    pub marquee_ellipse: bool,
    pub lasso_polygonal: bool,
    pub antialiased: bool,
    pub lock_ratio: bool,
    pub auto_select: bool,
    pub mask_paint_white: bool,
    /// The background color: fills with Ctrl+Backspace and the far end of gradients.
    pub background: [f64; 3],
    /// A free distortion in progress: the layer's corners on the document.
    pub distort: Option<crate::distort::Corners>,
    /// Gradient tool: radial rather than linear, foreground to transparent rather than to background, reversed, opacity.
    pub gradient_radial: bool,
    pub gradient_to_transparent: bool,
    pub gradient_reversed: bool,
    pub gradient_opacity: f64,
    /// A gradient being dragged: its two ends on the document.
    pub gradient_line: Option<((f64, f64), (f64, f64))>,
    /// Shape tool: ellipse rather than rectangle, the corner radius, and the shape being dragged (x, y, w, h).
    pub shape_ellipse: bool,
    pub shape_radius: f64,
    pub shape_draft: Option<(f64, f64, f64, f64)>,
    /// Type tool: the font and setting for new text (the color is the foreground color).
    pub text_style: crate::text::TextStyle,
    /// Crop tool: the frame (x, y, w, h) and the ratio choice (0 free, 1 original, 2 square, 3 4:3, 4 16:9).
    pub crop: Option<(f64, f64, f64, f64)>,
    pub crop_ratio: u32,
    /// Eyedropper reads every visible layer rather than the active one.
    pub eyedropper_all_layers: bool,
    /// The Blur tool's mode: 0 Liquify, 1 Blur, 2 Smudge, as the Mac orders them.
    pub blur_mode: u32,
    /// Guides drawn while a move is snapped: an x and a y across the whole canvas.
    pub snap_guides: (Option<f64>, Option<f64>),
    pub syncing_inspector: bool,
    pub needs_redraw: bool,
    /// Rulers along the top and left of the canvas in document pixels (Ctrl+R), as Photoshop's.
    pub rulers: bool,
    /// Ctrl+H: the selection edges and guides stay out of the way.
    pub hide_extras: bool,
    /// Ctrl+Shift+H: the Move tool's transform handles (Photoshop's Show Transform Controls).
    pub show_handles: bool,
    /// Preview mode (Ctrl+F): the picture alone on black, every panel hidden.
    pub preview: bool,
}

impl Doc {
    pub fn size(&self) -> (f64, f64) { (self.document.width() as f64, self.document.height() as f64) }
}

pub type DocRef = Rc<RefCell<Doc>>;

/// A project tab: its canvas and panel.
struct Page {
    root: gtk::Widget,
    canvas: Rc<canvas::Canvas>,
    panel: Rc<layers::LayersPanel>,
}

impl Page {
    /// After an edit that could have changed pixels, the selection or the layer list.
    fn refresh(&self) {
        self.panel.rebuild();
        self.canvas.sync_inspector();
        self.canvas.area.queue_draw();
    }
}

struct App {
    window: gtk::ApplicationWindow,
    header: gtk::HeaderBar,
    preview: Cell<bool>,
    panels_hidden: Cell<bool>,
    stack: gtk::Stack,
    notebook: gtk::Notebook,
    pages: RefCell<Vec<Page>>,
    space_held: Rc<Cell<bool>>,
}

impl Doc {
    pub fn from(document: Document, title: &str) -> Doc {
        Doc { title: title.to_string(), document, viewport: Viewport::default(), collapsed: HashSet::new(), tool: Tool::Move, wand: WandSettings::default(), mode: Mode::Replace, ants_phase: 0.0,
            brush: BrushSettings::default(), heal_mode: 0, clone_aligned: true, clone_all_layers: false, clone_source: None, clone_offset: None, last_brush_point: None,
            marquee_ellipse: false, lasso_polygonal: false, antialiased: true, lock_ratio: true, auto_select: false, mask_paint_white: false, background: [1.0; 3], distort: None, gradient_radial: false, gradient_to_transparent: true, gradient_reversed: false, gradient_opacity: 1.0, gradient_line: None, shape_ellipse: false, shape_radius: 0.0, shape_draft: None, text_style: crate::text::TextStyle::default(), crop: None, crop_ratio: 0, eyedropper_all_layers: true, blur_mode: 0, snap_guides: (None, None), syncing_inspector: false, needs_redraw: false, rulers: false, hide_extras: false, show_handles: true, preview: false }
    }
}

/// Opens a `.comp` package or a `.psd` file. A PSD opens as a new untitled-at-path document (it saves as
/// `.comp`); the notes say what the PSD had that was not carried over.
pub fn open_document(path: &Path) -> Result<(Doc, Vec<String>)> {
    let path = if path.file_name().is_some_and(|n| n == "manifest.json") { path.parent().unwrap_or(path) } else { path };
    if is_image(path) {
        let document = Document::open_image(path).with_context(|| format!("opening {}", path.display()))?;
        let title = path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Untitled".into());
        return Ok((Doc::from(document, &title), Vec::new()));
    }
    if is_psd(path) {
        let (document, notes) = Document::open_psd(path).with_context(|| format!("opening {}", path.display()))?;
        let title = path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Untitled".into());
        return Ok((Doc::from(document, &title), notes));
    }
    let project = format::load(path).with_context(|| format!("loading {}", path.display()))?;
    let document = Document::new(project)?;
    let title = path.file_name().map(|n| n.to_string_lossy().trim_end_matches(".comp").to_string()).unwrap_or_else(|| "Untitled".into());
    Ok((Doc { title, document, viewport: Viewport::default(), collapsed: HashSet::new(), tool: Tool::Move, wand: WandSettings::default(), mode: Mode::Replace, ants_phase: 0.0,
        brush: BrushSettings::default(), heal_mode: 0, clone_aligned: true, clone_all_layers: false, clone_source: None, clone_offset: None, last_brush_point: None,
        marquee_ellipse: false, lasso_polygonal: false, antialiased: true, lock_ratio: true, auto_select: false, mask_paint_white: false, background: [1.0; 3], distort: None, gradient_radial: false, gradient_to_transparent: true, gradient_reversed: false, gradient_opacity: 1.0, gradient_line: None, shape_ellipse: false, shape_radius: 0.0, shape_draft: None, text_style: crate::text::TextStyle::default(), crop: None, crop_ratio: 0, eyedropper_all_layers: true, blur_mode: 0, snap_guides: (None, None), syncing_inspector: false, needs_redraw: false, rulers: false, hide_extras: false, show_handles: true, preview: false }, Vec::new()))
}

pub fn is_psd(path: &Path) -> bool { path.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("psd")) }

pub const IMAGE_EXTENSIONS: [&str; 12] = ["png", "jpg", "jpeg", "tif", "tiff", "gif", "webp", "bmp", "jpe", "heic", "heif", "hif"];

pub fn is_image(path: &Path) -> bool {
    path.is_file() && path.extension().is_some_and(|e| IMAGE_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Anything the app opens as a document: a `.comp` package (or its manifest), a PSD, or an image.
pub fn is_openable(path: &Path) -> bool { dialogs::is_project(path) || path.file_name().is_some_and(|n| n == "manifest.json") || is_psd(path) || is_image(path) }

/// Scripted checks: a zoom to set, a wand click to make (document pixels), and a PNG to save the window to
/// before quitting.
#[derive(Clone, Default)]
pub struct Script {
    pub screenshot: Option<PathBuf>,
    pub zoom: Option<f64>,
    pub wand: Option<(f64, f64)>,
    /// A filter dialog to open (its window is saved beside the screenshot with a `-1` suffix).
    pub filter: Option<Kind>,
    /// A tool to select and a stroke to paint with it (document points), timing each step.
    pub tool: Option<Tool>,
    pub brush_size: Option<f64>,
    pub blur_mode: Option<u32>,
    /// A layer to make active, by name.
    pub layer: Option<String>,
    pub stroke: Vec<(f64, f64)>,
    pub ellipse: bool,
    /// Opens the brush color picker before the screenshot.
    pub pick_color: bool,
    /// Opens the brush preset picker before the screenshot.
    pub pick_brush: bool,
    /// A brush preset to paint the scripted stroke with, by name.
    pub brush: Option<String>,
    pub rulers: bool,
    /// Opens the Generative Fill panel (after the wand selection) for a screenshot.
    pub genfill: bool,
    /// Opens the brush popover at the canvas center for a screenshot.
    pub brush_popover: bool,
    pub preview: bool,
    /// Guides to place: vertical xs and horizontal ys.
    pub guides: (Vec<f64>, Vec<f64>),
    /// Adds a type layer with this text at (40, 40) in the current style.
    pub text: Option<String>,
    /// Puts a drop shadow, stroke and bevel on the active layer; opens the Layer Style dialog.
    pub effects: bool,
    pub layer_style: bool,
    /// Shows the grid; opens the shortcuts window.
    pub grid: bool,
    pub shortcuts: bool,
    /// Starts typing on the canvas into the active type layer.
    pub type_edit: bool,
    /// A window size to ask for (tiling compositors may override it).
    pub window: Option<(i32, i32)>,
    /// An adjustment layer to add and open for editing.
    pub adjustment: Option<String>,
}

pub fn run(paths: Vec<PathBuf>, script: Script) -> glib::ExitCode {
    let app = gtk::Application::new(Some(APP_ID), gio::ApplicationFlags::NON_UNIQUE);
    app.connect_activate(move |app| {
        let state = build_window(app);
        for path in &paths { state.open_path(path); }
        state.window.present();
        if let Some((w, h)) = script.window { state.window.set_default_size(w, h); }
        if script.zoom.is_some() || script.wand.is_some() || script.filter.is_some() || script.tool.is_some() || script.adjustment.is_some() || script.layer.is_some() || script.pick_color || script.pick_brush || script.rulers || script.genfill || script.brush_popover || script.preview || script.text.is_some() || script.effects || script.layer_style || script.grid || script.shortcuts || script.type_edit || !script.guides.0.is_empty() || !script.guides.1.is_empty() {
            let (state, script) = (state.clone(), script.clone());
            // After the first layout and frame, so the fit has happened and the canvas has its size.
            glib::timeout_add_local_once(Duration::from_millis(1000), move || {
                state.with_current(|p| {
                    if let Some(name) = &script.layer { let mut d = p.canvas.doc().borrow_mut(); if let Some(id) = d.document.renderer.layers().iter().find(|l| l.name == *name).map(|l| l.id) { d.document.select_layer(Some(id)); } }
                    if let Some(zoom) = script.zoom { p.canvas.zoom_to(zoom); }
                    if let Some((x, y)) = script.wand { p.canvas.wand_at(x, y); }
                    if let Some(tool) = script.tool { p.canvas.set_tool(tool); }
                    if script.rulers { p.canvas.doc().borrow_mut().rulers = true; p.canvas.area.queue_draw(); }
                    if let Some(text) = &script.text { let mut d = p.canvas.doc().borrow_mut(); let mut style = d.text_style.clone(); style.text = text.clone(); style.size = 72.0; if let Err(e) = d.document.add_text_layer(&style, 40.0, 40.0) { eprintln!("text: {e:#}"); } drop(d); p.refresh(); }
                    if script.effects { let mut d = p.canvas.doc().borrow_mut(); if let Some(id) = d.document.active { let e = crate::effects::Effects { drop_shadow: Some(crate::effects::Shadow::drop_default()), stroke: Some(crate::effects::Stroke::default()), bevel: Some(crate::effects::Bevel::default()), ..Default::default() }; if let Err(e) = d.document.set_effects(id, Some(&e)) { eprintln!("effects: {e:#}"); } } drop(d); p.refresh(); }
                    if script.layer_style { state.open_layer_style(); }
                    if script.grid { p.canvas.doc().borrow_mut().document.grid = Some((100.0, 4)); p.canvas.area.queue_draw(); }
                    if script.shortcuts { state.show_shortcuts(); }
                    if script.type_edit { let id = p.canvas.doc().borrow().document.active; if let Some(id) = id { p.canvas.edit_text(id); } }
                    if !script.guides.0.is_empty() || !script.guides.1.is_empty() { let mut d = p.canvas.doc().borrow_mut(); d.document.guides_v = script.guides.0.clone(); d.document.guides_h = script.guides.1.clone(); p.canvas.area.queue_draw(); }
                    if script.pick_color { p.canvas.options.show_color_picker(); }
                    if script.pick_brush { p.canvas.options.show_brush_picker(); }
                    if script.brush_popover { let (w, h) = (p.canvas.area.width() as f64, p.canvas.area.height() as f64); p.canvas.brush_popover(w / 2.0, h / 2.0); }
                    if script.ellipse { p.canvas.doc().borrow_mut().marquee_ellipse = true; }
                    if let Some(size) = script.brush_size { p.canvas.doc().borrow_mut().brush.diameter = size; p.canvas.sync_brush_options(); }
                    if let Some(name) = &script.brush { let preset = brushes::presets().into_iter().find(|b| b.name.eq_ignore_ascii_case(name)); let mut d = p.canvas.doc().borrow_mut(); match preset { Some(pr) => { eprintln!("script: brush {} ({}x{}, spacing {})", pr.name, pr.width, pr.height, pr.spacing); d.brush.spacing = Some(pr.spacing / 100.0); d.brush.angle_jitter = pr.jitter; d.brush.preset = Some(pr); } None => eprintln!("script: no brush named {name}") } }
                    if let Some(mode) = script.blur_mode { p.canvas.doc().borrow_mut().blur_mode = mode; }
                    if !script.stroke.is_empty() { p.canvas.scripted_stroke(&script.stroke); }
                });
                if let Some(kind) = script.filter { state.open_filter(kind); }
                if script.genfill { state.open_genfill(false); }
                if script.preview { state.toggle_preview(); }
                if let Some(kind) = script.adjustment.clone() { state.edit(|d| { d.add_adjustment(&kind); Ok(()) }); state.edit_adjustment(); }
            });
        }
        if let Some(target) = script.screenshot.clone() {
            let main = state.window.clone();
            let app = app.clone();
            // Once the scripted steps have had time to run, wait for a frame to finish painting (so no child
            // is left with a redraw pending, which a snapshot would skip), then save every window.
            glib::timeout_add_local_once(Duration::from_millis(2500), move || {
                let Some(clock) = main.frame_clock() else { app.quit(); return };
                // Two paints: the first hands a fresh frame to the picture, the second shows it.
                let paints = Rc::new(Cell::new(0));
                let (main2, app2, paints2) = (main.clone(), app.clone(), paints.clone());
                clock.connect_after_paint(move |_| {
                    let n = paints2.get() + 1;
                    paints2.set(n);
                    if n == 1 { main2.queue_draw(); return; }
                    if n != 2 { return; }
                    let (main, app, target) = (main2.clone(), app2.clone(), target.clone());
                    glib::idle_add_local_once(move || {
                        for (i, window) in std::iter::once(main.clone().upcast::<gtk::Window>()).chain(app.windows().into_iter().filter(|w| *w != main)).enumerate() {
                            let path = if i == 0 { target.clone() } else { target.with_file_name(format!("{}-{i}.png", target.file_stem().unwrap_or_default().to_string_lossy())) };
                            if let Err(error) = snapshot_window(&window, &path) { eprintln!("screenshot failed: {error:#}"); }
                        }
                        app.quit();
                    });
                });
                main.queue_draw();
            });
        }
    });
    app.run_with_args::<&str>(&[])
}

fn build_window(app: &gtk::Application) -> Rc<App> {
    // Tool buttons are smaller than GTK's default minimum.
    let css = gtk::CssProvider::new();
    css.load_from_string("button.tool, .layers-panel row button, .layers-footer button, .layers-panel menubutton > button { background-image: none; background-color: transparent; border: none; box-shadow: none; outline: none; } button.tool:hover, .layers-panel row button:hover, .layers-footer button:hover { background-color: alpha(currentColor, 0.12); } button.tool { min-width: 0; min-height: 0; padding: 3px; border-radius: 3px; } button.tool.mark { padding: 1px; } button.swatch { min-width: 0; min-height: 0; padding: 0; border-radius: 0; border: 1px solid alpha(currentColor, 0.5); } .panel-tab { padding: 5px 12px; } .panel-tab.current { background-color: alpha(@window_bg_color, 1); border-bottom: 2px solid @accent_bg_color; } .layers-footer button { min-width: 0; min-height: 0; padding: 3px 5px; } .type-bold { font-weight: bold; } .type-italic { font-style: italic; } list.navigation-sidebar > row.multi { background-color: alpha(@accent_bg_color, 0.22); } list.navigation-sidebar > row.drop-above { box-shadow: inset 0 3px @accent_bg_color; } list.navigation-sidebar > row.drop-below { box-shadow: inset 0 -3px @accent_bg_color; } list.navigation-sidebar > row.drop-into { box-shadow: inset 0 0 0 2px @accent_bg_color; }");
    if let Some(display) = gdk::Display::default() { gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION); }

    let window = gtk::ApplicationWindow::builder().application(app).title("Compositor").default_width(1280).default_height(820).build();
    let header = gtk::HeaderBar::new();
    let open = gtk::Button::builder().label("Open").tooltip_text("Open a .comp project (Ctrl+O)").action_name("win.open").build();
    header.pack_start(&open);
    header.pack_end(&gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu()).tooltip_text("Edit, Select, Image and Filter").build());
    window.set_titlebar(Some(&header));

    let notebook = gtk::Notebook::builder().scrollable(true).show_border(false).build();
    let empty = start_page();
    let stack = gtk::Stack::new();
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&notebook, Some("tabs"));
    window.set_child(Some(&stack));

    let state = Rc::new(App { window: window.clone(), header: header.clone(), preview: Cell::new(false), panels_hidden: Cell::new(false), stack, notebook: notebook.clone(), pages: RefCell::new(Vec::new()), space_held: Rc::new(Cell::new(false)) });
    // Follow the Omarchy theme; every canvas rebuilds its frame when the palette changes.
    {
        let weak = Rc::downgrade(&state);
        theme::start(Rc::new(move || { if let Some(state) = weak.upgrade() { for page in state.pages.borrow().iter() { page.canvas.drop_cache(); } } }));
    }

    // Space held turns a drag into a pan; single letters pick tools, unless a text field has focus.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let pressed = state.clone();
        let released = state.clone();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let state = &pressed;
            if gtk::prelude::GtkWindowExt::focus(&state.window).is_some_and(|w| w.is::<gtk::Editable>() || w.is::<gtk::Text>()) { return glib::Propagation::Proceed; }
            // Text being typed on the canvas takes every key first.
            let mut typed = false;
            state.with_current(|p| { if p.canvas.text_editing() { typed = p.canvas.text_key(key, modifiers); } });
            if typed { return glib::Propagation::Stop; }
            if key == gdk::Key::space {
                if !state.space_held.get() { state.space_held.set(true); state.current_canvas_cursor(); }
                return glib::Propagation::Stop;
            }
            if modifiers.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) { return glib::Propagation::Proceed; }
            if key == gdk::Key::Tab && !modifiers.contains(gdk::ModifierType::SHIFT_MASK) && state.notebook.current_page().is_some() { state.toggle_panels(); return glib::Propagation::Stop; }
            if matches!(key, gdk::Key::f | gdk::Key::F) && !modifiers.contains(gdk::ModifierType::SHIFT_MASK) && state.notebook.current_page().is_some() { state.toggle_preview(); return glib::Propagation::Stop; }
            let mut handled = false;
            state.with_current(|p| {
                handled = p.canvas.special_key(key, modifiers);
                if !handled { if let Some(c) = key.to_unicode() { handled = p.canvas.brush_key(c); } }
            });
            if handled { return glib::Propagation::Stop; }
            if let Some(tool) = key.to_unicode().and_then(|c| Tool::ALL.into_iter().find(|t| t.key() == c.to_ascii_lowercase())) {
                state.with_current(|p| p.canvas.set_tool(tool));
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        keys.connect_key_released(move |_, key, _, _| {
            if key == gdk::Key::space { released.space_held.set(false); released.current_canvas_cursor(); }
        });
    }
    window.add_controller(keys);

    let actions: [(&str, &[&str], fn(&Rc<App>)); 92] = [
        ("toggle-preview", &["<Control>f"], |s| s.toggle_preview()),
        ("toggle-guides", &["<Control>semicolon"], |s| s.with_current(|p| { { let mut d = p.canvas.doc().borrow_mut(); d.document.show_guides = !d.document.show_guides; } p.canvas.area.queue_draw(); })),
        ("new-guide", &[], |s| s.new_guide()),
        ("toggle-snap", &["<Control><Shift>semicolon"], |s| s.with_current(|p| { { let mut d = p.canvas.doc().borrow_mut(); d.document.snap = !d.document.snap; } p.canvas.area.queue_draw(); })),
        ("toggle-grid", &["<Control>apostrophe"], |s| s.with_current(|p| { { let mut d = p.canvas.doc().borrow_mut(); d.document.grid = if d.document.grid.is_some() { None } else { Some((100.0, 4)) }; } p.canvas.area.queue_draw(); })),
        ("clear-guides", &[], |s| s.with_current(|p| { { let mut d = p.canvas.doc().borrow_mut(); d.document.guides_v.clear(); d.document.guides_h.clear(); } p.canvas.area.queue_draw(); })),
        ("copy", &["<Control>c"], |s| s.copy_layer()),
        ("paste", &["<Control>v"], |s| s.paste()),
        ("generative-fill", &["<Control><Shift>g"], |s| s.open_genfill(false)),
        ("generative-expand", &[], |s| s.generative_expand()),
        ("toggle-rulers", &["<Control>r"], |s| s.with_current(|p| { { let mut d = p.canvas.doc().borrow_mut(); d.rulers = !d.rulers; } p.canvas.area.queue_draw(); })),
        ("swap-colors", &[], |s| s.with_current(|p| { p.canvas.brush_key('x'); })),
        ("default-colors", &[], |s| s.with_current(|p| { p.canvas.brush_key('d'); })),
        ("crop-apply", &[], |s| s.with_current(|p| p.canvas.apply_crop())),
        ("crop-cancel", &[], |s| s.with_current(|p| p.canvas.cancel_crop())),
        ("nudge-pixels-left", &["<Control>Left"], |s| s.edit(|d| d.nudge_pixels(-1.0, 0.0))),
        ("nudge-pixels-right", &["<Control>Right"], |s| s.edit(|d| d.nudge_pixels(1.0, 0.0))),
        ("nudge-pixels-up", &["<Control>Up"], |s| s.edit(|d| d.nudge_pixels(0.0, -1.0))),
        ("nudge-pixels-down", &["<Control>Down"], |s| s.edit(|d| d.nudge_pixels(0.0, 1.0))),
        ("merge", &["<Control>e"], |s| s.edit(|d| d.merge_layers())),
        ("layer-style", &[], |s| s.open_layer_style()),
        ("clear-layer-style", &[], |s| s.edit(|d| { let Some(id) = d.active else { return Ok(()) }; d.set_effects(id, None) })),
        ("invert", &["<Control>i"], |s| s.edit(|d| d.invert())),
        ("flip-canvas-horizontal", &[], |s| s.edit(|d| d.flip_canvas(true))),
        ("flip-canvas-vertical", &[], |s| s.edit(|d| d.flip_canvas(false))),
        ("fill-foreground", &["<Alt>BackSpace", "<Alt>Delete"], |s| s.with_doc(|doc| { let color = if doc.document.mask_target() { if doc.mask_paint_white { [1.0; 3] } else { [0.0; 3] } } else { doc.brush.color }; doc.document.fill(color) })),
        ("fill-background", &["<Control>BackSpace", "<Control>Delete"], |s| s.with_doc(|doc| { let color = if doc.document.mask_target() { if doc.mask_paint_white { [0.0; 3] } else { [1.0; 3] } } else { doc.background }; doc.document.fill(color) })),
        ("clear", &[], |s| s.with_doc(|doc| { let white = !doc.mask_paint_white; doc.document.clear_selection(white) })),
        ("open", &["<Control>o"], |s| { let state = s.clone(); dialogs::open_file(s.window.upcast_ref(), move |path| state.open_path(&path)); }),
        ("open-project", &["<Control><Shift>o"], |s| s.choose_and_open()),
        ("export-psd", &[], |s| s.export_psd()),
        ("close-tab", &["<Control>w"], |s| s.close_current()),
        ("zoom-in", &["<Control>equal", "<Control>plus", "<Control>KP_Add"], |s| s.with_current(|p| p.canvas.zoom_by(2.0))),
        ("zoom-out", &["<Control>minus", "<Control>KP_Subtract"], |s| s.with_current(|p| p.canvas.zoom_by(0.5))),
        ("zoom-fit", &["<Control>0"], |s| s.with_current(|p| p.canvas.fit())),
        ("zoom-actual", &["<Control>1"], |s| s.with_current(|p| p.canvas.zoom_to(1.0))),
        ("undo", &["<Control>z", "<Control><Alt>z"], |s| s.with_current(|p| { p.canvas.doc().borrow_mut().document.undo(); p.refresh(); })),
        ("redo", &["<Control><Shift>z", "<Control>y"], |s| s.with_current(|p| { p.canvas.doc().borrow_mut().document.redo(); p.refresh(); })),
        ("select-all", &["<Control>a"], |s| s.edit(|d| d.select_all())),
        ("deselect", &["<Control>d"], |s| s.edit(|d| { d.deselect(); Ok(()) })),
        ("invert-selection", &["<Control><Shift>i"], |s| s.edit(|d| d.invert_selection())),
        ("content-aware-fill", &["<Shift>F5", "<Shift>BackSpace"], |s| s.edit(|d| d.apply_filter(Kind::ContentAwareFill, &Default::default()))),
        ("heal-selection", &[], |s| s.edit(|d| d.apply_filter(Kind::SpotHeal, &Default::default()))),
        ("flip-horizontal", &[], |s| s.edit(|d| { d.flip_layer(true); Ok(()) })),
        ("flip-vertical", &[], |s| s.edit(|d| { d.flip_layer(false); Ok(()) })),
        ("mask-reveal", &[], |s| s.edit(|d| d.add_mask(true))),
        ("mask-hide", &[], |s| s.edit(|d| d.add_mask(false))),
        ("mask-delete", &[], |s| s.edit(|d| { d.delete_mask(); Ok(()) })),
        ("mask-toggle", &[], |s| s.edit(|d| { d.toggle_mask_enabled(); Ok(()) })),
        ("mask-invert", &[], |s| s.edit(|d| d.invert_mask())),
        ("toggle-clipping", &["<Alt>g", "<Control><Alt>g"], |s| s.edit(|d| { if let Some(id) = d.active { d.toggle_clipping(id); } Ok(()) })),
        ("select-layer-pixels", &[], |s| s.edit(|d| { match d.active { Some(id) => d.select_layer_pixels(id, Mode::Replace), None => Ok(()) } })),
        ("new", &["<Control>n"], |s| { let state = s.clone(); dialogs::new_canvas(s.window.upcast_ref(), move |w, h, r| match Document::blank(w, h, r) { Ok(document) => state.add_page(Doc::from(document, "Untitled")), Err(e) => state.alert("Could not create the canvas", &format!("{e:#}")) }); }),
        ("save", &["<Control>s"], |s| s.save_current(false)),
        ("save-as", &["<Control><Shift>s"], |s| s.save_current(true)),
        ("import", &["<Control><Shift>p"], |s| { let state = s.clone(); dialogs::open_image(s.window.upcast_ref(), move |path| state.edit(|d| d.import_image(&path).map(|_| ()))); }),
        ("export-png", &["<Control><Alt><Shift>w"], |s| { let state = s.clone(); let title = s.current_title(); dialogs::save_as(s.window.upcast_ref(), "Export PNG", &title, "png", move |path| state.edit(|d| d.export_png(&path))); }),
        ("export-jpeg", &["<Control><Alt><Shift>s"], |s| s.export_jpeg()),
        ("copy-merged", &["<Control><Shift>c"], |s| s.copy_merged()),
        ("new-layer", &["<Control><Shift>n", "<Control><Alt><Shift>n"], |s| s.edit(|d| { d.add_blank_layer(); Ok(()) })),
        ("new-folder", &["<Control>g"], |s| s.edit(|d| { d.add_folder(); Ok(()) })),
        // Ctrl+J with a selection is Layer via Copy, as in Photoshop; without one it duplicates the layer.
        ("duplicate-layer", &["<Control>j"], |s| s.edit(|d| { if d.selection.as_ref().is_some_and(|sel| !sel.is_empty()) { d.layer_via(false).map(|_| ()) } else { d.duplicate_layer(); Ok(()) } })),
        ("free-transform", &["<Control>t"], |s| s.free_transform()),
        ("layer-via-cut", &["<Control><Shift>j"], |s| s.edit(|d| d.layer_via(true).map(|_| ()))),
        ("merge-visible", &["<Control><Shift>e"], |s| s.edit(|d| d.merge_visible().map(|_| ()))),
        ("stamp-visible", &["<Control><Alt><Shift>e"], |s| s.edit(|d| d.stamp_visible().map(|_| ()))),
        ("layer-top", &["<Control><Shift>bracketright"], |s| s.edit(|d| { d.move_layer_to_end(true); Ok(()) })),
        ("layer-bottom", &["<Control><Shift>bracketleft"], |s| s.edit(|d| { d.move_layer_to_end(false); Ok(()) })),
        ("select-all-layers", &["<Control><Alt>a"], |s| s.edit(|d| { d.select_all_layers(); Ok(()) })),
        ("toggle-layer-visibility", &["<Control>comma"], |s| s.edit(|d| { d.toggle_visible(); Ok(()) })),
        ("reselect", &["<Control><Shift>d"], |s| s.edit(|d| { d.reselect(); Ok(()) })),
        ("feather-selection", &["<Shift>F6", "<Control><Alt>d"], |s| { let state = s.clone(); dialogs::amount(s.window.upcast_ref(), "Feather Selection", "Feather radius (px)", move |n| state.edit(|d| d.feather_selection(n as f64))); }),
        ("desaturate", &["<Control><Shift>u"], |s| s.edit(|d| d.desaturate())),
        ("auto-tone", &["<Control><Shift>l"], |s| s.edit(|d| d.auto_levels(crate::document::AutoLevels::Tone))),
        ("auto-contrast", &["<Control><Alt><Shift>l"], |s| s.edit(|d| d.auto_levels(crate::document::AutoLevels::Contrast))),
        ("auto-color", &["<Control><Shift>b"], |s| s.edit(|d| d.auto_levels(crate::document::AutoLevels::Color))),
        ("liquify", &["<Control><Shift>x"], |s| s.with_current(|p| { p.canvas.doc().borrow_mut().blur_mode = 0; p.canvas.set_tool(Tool::Blur); })),
        ("toggle-extras", &["<Control>h"], |s| s.with_current(|p| { { let mut d = p.canvas.doc().borrow_mut(); d.hide_extras = !d.hide_extras; } p.canvas.area.queue_draw(); })),
        ("toggle-panels", &[], |s| s.toggle_panels()),
        ("toggle-handles", &["<Control><Shift>h"], |s| s.with_current(|p| { { let mut d = p.canvas.doc().borrow_mut(); d.show_handles = !d.show_handles; } p.canvas.update_cursor(); p.canvas.area.queue_draw(); })),
        ("edit-text", &[], |s| s.with_current(|p| { let id = p.canvas.doc().borrow().document.active; if let Some(id) = id { if p.canvas.doc().borrow().document.text_style(id).is_some() { p.canvas.edit_text(id); } } })),
        ("shortcuts", &["F1", "<Control><Alt><Shift>k"], |s| s.show_shortcuts()),
        ("delete-layer", &[], |s| s.edit(|d| { d.delete_layer(); Ok(()) })),
        ("layer-up", &["<Control>bracketright"], |s| s.edit(|d| { d.move_layer(true); Ok(()) })),
        ("layer-down", &["<Control>bracketleft"], |s| s.edit(|d| { d.move_layer(false); Ok(()) })),
        ("rename-layer", &[], |s| s.rename_layer()),
        ("edit-adjustment", &[], |s| s.edit_adjustment()),
        ("canvas-size", &["<Control><Alt>c"], |s| { let state = s.clone(); let Some((w, h)) = s.current_size() else { return }; dialogs::canvas_size(s.window.upcast_ref(), (w, h), move |nw, nh, anchor, fill| state.edit(|d| d.canvas_size(nw, nh, anchor, fill, None, "Canvas Size"))); }),
        ("image-size", &["<Control><Alt>i"], |s| { let state = s.clone(); let Some((w, h)) = s.current_size() else { return }; let res = s.current_resolution(); dialogs::image_size(s.window.upcast_ref(), (w, h, res), move |nw, nh, r, sampling| { state.edit(|d| d.image_size(nw, nh, r, sampling)); state.with_current(|p| p.canvas.fit()); }); }),
        ("crop", &[], |s| s.edit(|d| d.crop_to_selection())),
        ("expand-selection", &[], |s| { let state = s.clone(); dialogs::amount(s.window.upcast_ref(), "Expand Selection", "Expand by (px)", move |n| state.edit(|d| d.resize_selection(n))); }),
        ("contract-selection", &[], |s| { let state = s.clone(); dialogs::amount(s.window.upcast_ref(), "Contract Selection", "Contract by (px)", move |n| state.edit(|d| d.resize_selection(-n))); }),
    ];
    for (name, accels, handler) in actions {
        let action = gio::SimpleAction::new(name, None);
        let state = state.clone();
        action.connect_activate(move |_, _| handler(&state));
        window.add_action(&action);
        if !accels.is_empty() { app.set_accels_for_action(&format!("win.{name}"), accels); }
    }
    {
        let action = gio::SimpleAction::new("filter", Some(glib::VariantTy::STRING));
        let state = state.clone();
        action.connect_activate(move |_, parameter| {
            let Some(name) = parameter.and_then(|v| v.get::<String>()) else { return };
            let kind = match name.as_str() {
                "noise" => Kind::AddNoise, "grain" => Kind::Grain, "lens" => Kind::LensCorrection,
                "gradient" => Kind::GradientMap, "levels" => Kind::Levels, "curves" => Kind::Curves, "balance" => Kind::ColorBalance, "fade" => Kind::Fade, "hsv" => Kind::HueSaturation, "exposure" => Kind::Exposure, "gaussian" => Kind::GaussianBlur, "motion" => Kind::MotionBlur, "background" => Kind::RemoveBackground, _ => return,
            };
            state.open_filter(kind);
        });
        window.add_action(&action);
        app.set_accels_for_action("win.filter::levels", &["<Control>l"]);
        app.set_accels_for_action("win.filter::hsv", &["<Control>u"]);
        app.set_accels_for_action("win.filter::curves", &["<Control>m"]);
        app.set_accels_for_action("win.filter::balance", &["<Control>b"]);
        app.set_accels_for_action("win.filter::fade", &["<Control><Shift>f"]);
    }
    {
        // Parameterized: a layer by id (the canvas menu lists the layers under the pointer) and a guide.
        let action = gio::SimpleAction::new("select-layer-id", Some(glib::VariantTy::STRING));
        let state2 = state.clone();
        let state = state.clone();
        action.connect_activate(move |_, parameter| {
            let Some(id) = parameter.and_then(|v| v.get::<String>()).and_then(|t| uuid::Uuid::parse_str(&t).ok()) else { return };
            state.edit(|d| { if d.has_layer(id) { d.select_layer(Some(id)); } Ok(()) });
        });
        window.add_action(&action);
        let action = gio::SimpleAction::new("new-preset", Some(glib::VariantTy::STRING));
        let state3 = state2.clone();
        action.connect_activate(move |_, parameter| {
            let Some(spec) = parameter.and_then(|v| v.get::<String>()) else { return };
            let Some((w, h, r)) = parse_preset(&spec) else { return };
            match Document::blank(w, h, r) { Ok(document) => state3.add_page(Doc::from(document, "Untitled")), Err(e) => state3.alert("Could not create the canvas", &format!("{e:#}")) }
        });
        window.add_action(&action);
        let action = gio::SimpleAction::new("delete-guide", Some(glib::VariantTy::STRING));
        let state = state2.clone();
        action.connect_activate(move |_, parameter| {
            let Some(spec) = parameter.and_then(|v| v.get::<String>()) else { return };
            state.with_current(|p| {
                { let mut d = p.canvas.doc().borrow_mut(); let (kind, index) = spec.split_at(1); if let Ok(i) = index.parse::<usize>() { if kind == "v" { if i < d.document.guides_v.len() { d.document.guides_v.remove(i); } } else if i < d.document.guides_h.len() { d.document.guides_h.remove(i); } } }
                p.canvas.area.queue_draw();
            });
        });
        window.add_action(&action);
    }
    {
        let action = gio::SimpleAction::new("new-adjustment", Some(glib::VariantTy::STRING));
        let state = state.clone();
        action.connect_activate(move |_, parameter| {
            let Some(kind) = parameter.and_then(|v| v.get::<String>()) else { return };
            state.edit(|d| { d.add_adjustment(&kind); Ok(()) });
            state.edit_adjustment();
        });
        window.add_action(&action);
    }
    // Files dropped on the window open as projects or import as layers.
    {
        let target = gtk::DropTarget::new(gio::File::static_type(), gdk::DragAction::COPY);
        target.set_types(&[gio::File::static_type(), gdk::Texture::static_type()]);
        let state = state.clone();
        target.connect_drop(move |_, value, _, _| {
            // Pixels dragged out of another app (a browser, a screenshot tool) come as a texture.
            if let Ok(texture) = value.get::<gdk::Texture>() {
                let bytes = texture.save_to_png_bytes();
                if state.notebook.current_page().is_some() { state.edit(|d| d.import_image_bytes(&bytes, "Dropped Image").map(|_| ())); }
                else {
                    match Document::open_image_bytes(&bytes) { Ok(document) => state.add_page(Doc::from(document, "Dropped Image")), Err(e) => state.alert("Could not open the dropped image", &format!("{e:#}")) }
                }
                return true;
            }
            let Ok(file) = value.get::<gio::File>() else { return false };
            let Some(path) = file.path() else { return false };
            // Images import as a layer when a document is open, and open as their own document otherwise.
            if is_image(&path) && state.notebook.current_page().is_some() { state.edit(|d| d.import_image(&path).map(|_| ())); }
            else if is_openable(&path) { state.open_path(&path); }
            else { state.alert("Cannot open this file", "Compositor opens .comp projects, Photoshop files, and PNG, JPEG, TIFF, GIF, WebP and BMP images."); }
            true
        });
        window.add_controller(target);
    }
    {
        let action = gio::SimpleAction::new("open-path", Some(glib::VariantTy::STRING));
        let state = state.clone();
        action.connect_activate(move |_, parameter| {
            if let Some(path) = parameter.and_then(|v| v.get::<String>()) { state.open_path(Path::new(&path)); }
        });
        window.add_action(&action);
    }
    {
        let state = state.clone();
        notebook.connect_switch_page(move |_, _, _| state.current_canvas_cursor());
    }
    {
        let state = state.clone();
        notebook.connect_page_removed(move |_, _, _| state.prune());
    }
    {
        let state = state.clone();
        window.connect_close_request(move |_| {
            let any = state.pages.borrow().iter().any(|p| p.canvas.doc().borrow().document.is_modified());
            if any { state.close_window(); glib::Propagation::Stop } else { glib::Propagation::Proceed }
        });
    }
    state
}

fn menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let file = gio::Menu::new();
    file.append(Some("New Canvas…"), Some("win.new"));
    file.append(Some("Open…"), Some("win.open"));
    file.append(Some("Open Project Folder…"), Some("win.open-project"));
    file.append(Some("Save"), Some("win.save"));
    file.append(Some("Save As…"), Some("win.save-as"));
    file.append(Some("Import Image…"), Some("win.import"));
    file.append(Some("Export PNG…"), Some("win.export-png"));
    file.append(Some("Export JPEG…"), Some("win.export-jpeg"));
    file.append(Some("Export PSD…"), Some("win.export-psd"));
    file.append(Some("Close"), Some("win.close-tab"));
    menu.append_submenu(Some("File"), &file);
    let edit = gio::Menu::new();
    edit.append(Some("Undo"), Some("win.undo"));
    edit.append(Some("Redo"), Some("win.redo"));
    edit.append(Some("Free Transform"), Some("win.free-transform"));
    edit.append(Some("Copy"), Some("win.copy"));
    edit.append(Some("Paste as New Layer"), Some("win.paste"));
    edit.append(Some("Copy Merged"), Some("win.copy-merged"));
    edit.append(Some("Fill with Foreground"), Some("win.fill-foreground"));
    edit.append(Some("Fill with Background"), Some("win.fill-background"));
    edit.append(Some("Clear"), Some("win.clear"));
    edit.append(Some("Generative Fill…"), Some("win.generative-fill"));
    menu.append_submenu(Some("Edit"), &edit);
    let select = gio::Menu::new();
    select.append(Some("All"), Some("win.select-all"));
    select.append(Some("Deselect"), Some("win.deselect"));
    select.append(Some("Reselect"), Some("win.reselect"));
    select.append(Some("Inverse"), Some("win.invert-selection"));
    select.append(Some("All Layers"), Some("win.select-all-layers"));
    select.append(Some("Feather…"), Some("win.feather-selection"));
    select.append(Some("Load Layer Pixels"), Some("win.select-layer-pixels"));
    select.append(Some("Expand…"), Some("win.expand-selection"));
    select.append(Some("Contract…"), Some("win.contract-selection"));
    menu.append_submenu(Some("Select"), &select);
    let layer = gio::Menu::new();
    layer.append(Some("New Layer"), Some("win.new-layer"));
    layer.append(Some("New Folder"), Some("win.new-folder"));
    let adjustments = gio::Menu::new();
    for kind in ["Hue/Saturation", "Levels", "Curves", "Exposure", "Gradient Map", "Grain"] { adjustments.append(Some(kind), Some(&format!("win.new-adjustment::{kind}"))); }
    layer.append_submenu(Some("New Adjustment Layer"), &adjustments);
    layer.append(Some("Edit Adjustment…"), Some("win.edit-adjustment"));
    layer.append(Some("Duplicate Layer / Layer via Copy"), Some("win.duplicate-layer"));
    layer.append(Some("Layer via Cut"), Some("win.layer-via-cut"));
    layer.append(Some("Layer Style…"), Some("win.layer-style"));
    layer.append(Some("Clear Layer Style"), Some("win.clear-layer-style"));
    layer.append(Some("Delete Layer"), Some("win.delete-layer"));
    layer.append(Some("Rename Layer…"), Some("win.rename-layer"));
    layer.append(Some("Move Up"), Some("win.layer-up"));
    layer.append(Some("Move Down"), Some("win.layer-down"));
    let mask = gio::Menu::new();
    mask.append(Some("Reveal All"), Some("win.mask-reveal"));
    mask.append(Some("Hide All"), Some("win.mask-hide"));
    layer.append_submenu(Some("Add Mask (from the selection, if any)"), &mask);
    layer.append(Some("Enable / Disable Mask"), Some("win.mask-toggle"));
    layer.append(Some("Invert Mask"), Some("win.mask-invert"));
    layer.append(Some("Delete Mask"), Some("win.mask-delete"));
    layer.append(Some("Create / Release Clipping Mask"), Some("win.toggle-clipping"));
    layer.append(Some("Flip Horizontal"), Some("win.flip-horizontal"));
    layer.append(Some("Flip Vertical"), Some("win.flip-vertical"));
    layer.append(Some("Merge Down / Group"), Some("win.merge"));
    layer.append(Some("Merge Visible"), Some("win.merge-visible"));
    layer.append(Some("Stamp Visible"), Some("win.stamp-visible"));
    layer.append(Some("Bring to Front"), Some("win.layer-top"));
    layer.append(Some("Send to Back"), Some("win.layer-bottom"));
    layer.append(Some("Hide / Show Layer"), Some("win.toggle-layer-visibility"));
    layer.append(Some("Edit Text…"), Some("win.edit-text"));
    menu.append_submenu(Some("Layer"), &layer);
    let image = gio::Menu::new();
    image.append(Some("Canvas Size…"), Some("win.canvas-size"));
    image.append(Some("Image Size…"), Some("win.image-size"));
    image.append(Some("Crop to Selection"), Some("win.crop"));
    image.append(Some("Hue/Saturation…"), Some("win.filter::hsv"));
    image.append(Some("Desaturate"), Some("win.desaturate"));
    image.append(Some("Exposure…"), Some("win.filter::exposure"));
    image.append(Some("Levels…"), Some("win.filter::levels"));
    image.append(Some("Curves…"), Some("win.filter::curves"));
    image.append(Some("Color Balance…"), Some("win.filter::balance"));
    image.append(Some("Auto Tone"), Some("win.auto-tone"));
    image.append(Some("Auto Contrast"), Some("win.auto-contrast"));
    image.append(Some("Auto Color"), Some("win.auto-color"));
    image.append(Some("Gradient Map…"), Some("win.filter::gradient"));
    image.append(Some("Grain…"), Some("win.filter::grain"));
    image.append(Some("Invert"), Some("win.invert"));
    image.append(Some("Flip Canvas Horizontal"), Some("win.flip-canvas-horizontal"));
    image.append(Some("Flip Canvas Vertical"), Some("win.flip-canvas-vertical"));
    image.append(Some("Generative Expand…"), Some("win.generative-expand"));
    image.append(Some("Rulers"), Some("win.toggle-rulers"));
    let view = gio::Menu::new();
    view.append(Some("Preview (picture only)"), Some("win.toggle-preview"));
    view.append(Some("Rulers"), Some("win.toggle-rulers"));
    view.append(Some("Show Guides"), Some("win.toggle-guides"));
    view.append(Some("New Guide…"), Some("win.new-guide"));
    view.append(Some("Clear Guides"), Some("win.clear-guides"));
    view.append(Some("Snap"), Some("win.toggle-snap"));
    view.append(Some("Show Grid"), Some("win.toggle-grid"));
    view.append(Some("Extras (selection edges, guides)"), Some("win.toggle-extras"));
    view.append(Some("Panels"), Some("win.toggle-panels"));
    view.append(Some("Transform Controls"), Some("win.toggle-handles"));
    menu.append_submenu(Some("View"), &view);
    menu.append_submenu(Some("Image"), &image);
    let filter = gio::Menu::new();
    filter.append(Some("Remove Background…"), Some("win.filter::background"));
    filter.append(Some("Gaussian Blur…"), Some("win.filter::gaussian"));
    filter.append(Some("Motion Blur…"), Some("win.filter::motion"));
    filter.append(Some("Add Noise…"), Some("win.filter::noise"));
    filter.append(Some("Lens Correction…"), Some("win.filter::lens"));
    filter.append(Some("Content-Aware Fill"), Some("win.content-aware-fill"));
    filter.append(Some("Fade…"), Some("win.filter::fade"));
    filter.append(Some("Heal Selection"), Some("win.heal-selection"));
    menu.append_submenu(Some("Filter"), &filter);
    let help = gio::Menu::new();
    help.append(Some("Keyboard Shortcuts (F1)"), Some("win.shortcuts"));
    menu.append_submenu(Some("Help"), &help);
    menu
}

/// "1920x1080@72" as width, height and resolution.
fn parse_preset(spec: &str) -> Option<(i32, i32, f64)> {
    let (size, ppi) = spec.split_once('@')?;
    let (w, h) = size.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?, ppi.parse().ok()?))
}

/// Photoshop's New Document presets: (group, name, width, height, ppi).
const PRESETS: &[(&str, &str, i32, i32, i32)] = &[
    ("Photo", "Default Photoshop Size", 2100, 1500, 300), ("Photo", "Landscape 4 x 6 in", 1800, 1200, 300), ("Photo", "Portrait 4 x 6 in", 1200, 1800, 300), ("Photo", "8 x 10 in", 3000, 2400, 300),
    ("Print", "Letter", 2550, 3300, 300), ("Print", "Legal", 2550, 4200, 300), ("Print", "Tabloid", 3300, 5100, 300), ("Print", "A4", 2480, 3508, 300), ("Print", "A3", 3508, 4961, 300),
    ("Web", "Web Large 1920 x 1080", 1920, 1080, 72), ("Web", "Web Medium 1366 x 768", 1366, 768, 72), ("Web", "Web Small 1280 x 720", 1280, 720, 72), ("Web", "Common 1440 x 900", 1440, 900, 72),
    ("Mobile", "iPhone 15 Pro", 1179, 2556, 72), ("Mobile", "iPhone SE", 750, 1334, 72), ("Mobile", "iPad Pro 13", 2064, 2752, 72), ("Mobile", "Android 1080 x 1920", 1080, 1920, 72),
    ("Film and Video", "HDTV 1080p", 1920, 1080, 72), ("Film and Video", "UHD 4K", 3840, 2160, 72), ("Film and Video", "DCI 4K", 4096, 2160, 72), ("Film and Video", "Square 1080", 1080, 1080, 72),
    ("Social", "Instagram Post", 1080, 1080, 72), ("Social", "Instagram Story", 1080, 1920, 72), ("Social", "YouTube Thumbnail", 1280, 720, 72), ("Social", "X Header", 1500, 500, 72), ("Social", "Icon 1024", 1024, 1024, 72),
];

/// What shows with nothing open: Photoshop's New Document presets in their groups, a custom size, and Open.
fn start_page() -> gtk::Widget {
    let page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(18).margin_top(36).margin_bottom(36).margin_start(48).margin_end(48).halign(gtk::Align::Center).valign(gtk::Align::Start).build();
    page.append(&gtk::Label::builder().label("New document").xalign(0.0).css_classes(["heading"]).build());
    let mut group = "";
    let mut flow: Option<gtk::FlowBox> = None;
    for &(g, name, w, h, ppi) in PRESETS {
        if g != group {
            group = g;
            page.append(&gtk::Label::builder().label(g).xalign(0.0).css_classes(["dim-label", "caption"]).margin_top(6).build());
            let f = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).column_spacing(8).row_spacing(8).max_children_per_line(6).min_children_per_line(2).homogeneous(true).build();
            page.append(&f);
            flow = Some(f);
        }
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).build();
        content.append(&gtk::Label::builder().label(name).xalign(0.0).build());
        content.append(&gtk::Label::builder().label(format!("{w} × {h} px · {ppi} ppi")).xalign(0.0).css_classes(["dim-label", "caption"]).build());
        let button = gtk::Button::builder().child(&content).action_name("win.new-preset").width_request(190).build();
        button.set_action_target_value(Some(&format!("{w}x{h}@{ppi}").to_variant()));
        if let Some(f) = &flow { f.insert(&button, -1); }
    }
    let custom = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).margin_top(12).build();
    custom.append(&gtk::Label::builder().label("Custom").css_classes(["dim-label", "caption"]).build());
    let width = gtk::SpinButton::with_range(1.0, 30000.0, 1.0);
    width.set_value(1920.0);
    let height = gtk::SpinButton::with_range(1.0, 30000.0, 1.0);
    height.set_value(1080.0);
    let ppi = gtk::SpinButton::with_range(1.0, 9600.0, 1.0);
    ppi.set_value(72.0);
    for (label, spin) in [("W", &width), ("H", &height), ("ppi", &ppi)] { custom.append(&gtk::Label::new(Some(label))); custom.append(spin); }
    let create = gtk::Button::builder().label("Create").css_classes(["suggested-action"]).build();
    { let (w, h, r) = (width.clone(), height.clone(), ppi.clone()); create.connect_clicked(move |b| { if let Some(root) = b.root().and_downcast::<gtk::ApplicationWindow>() { gtk::prelude::WidgetExt::activate_action(&root, "win.new-preset", Some(&format!("{}x{}@{}", w.value_as_int(), h.value_as_int(), r.value_as_int()).to_variant())).ok(); } }); }
    custom.append(&create);
    custom.append(&gtk::Box::builder().hexpand(true).build());
    custom.append(&gtk::Button::builder().label("Open…").action_name("win.open").tooltip_text("An image, a Photoshop file or a .comp project (Ctrl+O); dropping one here works too").build());
    page.append(&custom);
    let scroller = gtk::ScrolledWindow::builder().child(&page).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).hexpand(true).css_classes(["start-page"]).build();
    scroller.upcast()
}

/// Every shortcut, for the Help window: (group, key, what it does).
pub const SHORTCUTS: &[(&str, &str, &str)] = &[
    ("Tools", "V M L W C I B E J S R G T U H Z", "Move, Marquee, Lasso, Wand, Crop, Eyedropper, Brush, Eraser, Heal, Clone, Smear, Gradient, Type, Shape, Hand, Zoom"),
    ("Tools", "Shift+M, Shift+L, Shift+U", "Swap the marquee, lasso or shape kind"),
    ("Tools", "X, D", "Swap the colors, reset them"),
    ("Tools", "[ ], Shift+[ ], 0 to 9", "Brush size, hardness, opacity"),
    ("Tools", "Alt+click, Right-click", "Brush: pick a color; open the brush settings"),
    ("Tools", "Space+drag, Ctrl+scroll", "Pan, zoom around the pointer"),
    ("File", "Ctrl+N, Ctrl+O, Ctrl+W", "New, Open, Close"),
    ("File", "Ctrl+S, Ctrl+Shift+S", "Save, Save As"),
    ("File", "Ctrl+Shift+P", "Import an image as a layer"),
    ("File", "Ctrl+Alt+Shift+W, Ctrl+Alt+Shift+S", "Export PNG, Export JPEG"),
    ("Edit", "Ctrl+Z, Ctrl+Alt+Z", "Undo"),
    ("Edit", "Ctrl+Shift+Z, Ctrl+Y", "Redo"),
    ("Edit", "Ctrl+C, Ctrl+Shift+C, Ctrl+V", "Copy, Copy Merged, Paste as a new layer"),
    ("Edit", "Alt+Backspace, Ctrl+Backspace", "Fill with the foreground, the background"),
    ("Edit", "Shift+F5, Shift+Backspace", "Content-Aware Fill"),
    ("Edit", "Delete", "Clear the selection, or delete the mask or layer"),
    ("Edit", "Arrows, Shift+Arrows", "Nudge the layer or selection by 1 or 10 px"),
    ("Edit", "Ctrl+Arrows", "Move the selected pixels"),
    ("Edit", "Ctrl+Shift+G", "Generative Fill"),
    ("Edit", "Ctrl+T", "Free Transform the selection (Return commits, Escape cancels); without one, the Move handles"),
    ("Select", "Ctrl+A, Ctrl+D, Ctrl+Shift+D", "All, Deselect, Reselect"),
    ("Select", "Ctrl+Shift+I", "Inverse"),
    ("Select", "Ctrl+Alt+A", "All layers"),
    ("Select", "Shift+F6, Ctrl+Alt+D", "Feather"),
    ("Select", "Ctrl+click a layer row", "Load its pixels as a selection"),
    ("Select", "Shift, Alt while selecting", "Add to, subtract from the selection"),
    ("Image", "Ctrl+L, Ctrl+M, Ctrl+B", "Levels, Curves, Color Balance"),
    ("Image", "Ctrl+U, Ctrl+I, Ctrl+Shift+U", "Hue/Saturation, Invert, Desaturate"),
    ("Image", "Ctrl+Shift+L, Ctrl+Alt+Shift+L, Ctrl+Shift+B", "Auto Tone, Auto Contrast, Auto Color"),
    ("Image", "Ctrl+Shift+F", "Fade the last filter"),
    ("Image", "Ctrl+Alt+I, Ctrl+Alt+C", "Image Size, Canvas Size"),
    ("Image", "Ctrl+Shift+X", "Liquify (the Smear tool)"),
    ("Layer", "Ctrl+Shift+N, Ctrl+G", "New layer, new folder"),
    ("Layer", "Ctrl+J, Ctrl+Shift+J", "Duplicate (Layer via Copy with a selection), Layer via Cut"),
    ("Layer", "Ctrl+E, Ctrl+Shift+E, Ctrl+Alt+Shift+E", "Merge Down, Merge Visible, Stamp Visible"),
    ("Layer", "Ctrl+], Ctrl+[", "Move the layer up, down"),
    ("Layer", "Ctrl+Shift+], Ctrl+Shift+[", "Bring to Front, Send to Back"),
    ("Layer", "Alt+G, Ctrl+Alt+G", "Clip to the layer below"),
    ("Layer", "Ctrl+,", "Hide or show the layer"),
    ("Layer", "Double-click a row", "Rename; adjustment layers open their settings"),
    ("View", "Ctrl+0, Ctrl+1, Ctrl++, Ctrl+-", "Fit, 100%, zoom in, zoom out"),
    ("View", "Ctrl+F, F", "Preview: the picture alone on black"),
    ("View", "Tab", "Hide and show the panels"),
    ("View", "Ctrl+R, Ctrl+;, Ctrl+'", "Rulers, Guides, Grid"),
    ("View", "Ctrl+Shift+;", "Snap"),
    ("View", "Ctrl+H", "Extras: selection edges and guides"),
    ("View", "Ctrl+Shift+H, Return", "Transform handles on and off; Return puts them away until the next click"),
    ("View", "Double-click a ruler", "New guide there; drag guides with Move"),
    ("Help", "F1, Ctrl+Alt+Shift+K", "This list"),
];

impl App {
    fn open_path(self: &Rc<Self>, path: &Path) {
        match open_document(path) {
            Ok((doc, notes)) => {
                self.add_page(doc);
                if !notes.is_empty() { self.alert("Opened with changes", &format!("{}\n\nSave keeps it as a .comp project; use Export PSD to write a Photoshop file.", notes.join("\n"))); }
            }
            Err(error) => self.alert("Could not open", &format!("{error:#}")),
        }
    }

    fn add_page(self: &Rc<Self>, doc: Doc) {
        let title = doc.title.clone();
        let doc: DocRef = Rc::new(RefCell::new(doc));
        let canvas = canvas::Canvas::new(doc.clone(), self.space_held.clone());
        let panel = layers::LayersPanel::new(doc.clone(), canvas.area.clone());
        let paned = gtk::Paned::builder().orientation(gtk::Orientation::Horizontal)
            .shrink_end_child(false).resize_end_child(false).shrink_start_child(false).build();
        paned.set_start_child(Some(&canvas.widget));
        paned.set_end_child(Some(&panel.widget));
        {
            // The panel keeps its width; the canvas takes whatever the window has, at any size (Hyprland
            // hands out a whole tile, often far wider than 1280).
            let (p, panel_widget) = (paned.clone(), panel.widget.clone());
            paned.connect_map(move |_| { let (p, panel_widget) = (p.clone(), panel_widget.clone()); glib::idle_add_local_once(move || {
                let w = p.width();
                // The panel's own minimum (its widest control) wins over the nominal width.
                let need = panel_widget.measure(gtk::Orientation::Horizontal, -1).0.max(layers::WIDTH);
                if w > 0 { p.set_position(w - need); }
            }); });
        }

        let tab = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        tab.append(&gtk::Label::new(Some(&title)));
        let close = gtk::Button::builder().icon_name("window-close-symbolic").has_frame(false).tooltip_text("Close (Ctrl+W)").build();
        tab.append(&close);
        let index = self.notebook.append_page(&paned, Some(&tab));
        self.notebook.set_tab_reorderable(&paned, true);
        let root: gtk::Widget = paned.upcast();
        {
            let (panel_ref, area) = (panel.clone(), canvas.area.clone());
            canvas.set_refresh(Rc::new(move || { panel_ref.rebuild(); area.queue_draw(); }));
            let canvas2 = canvas.clone();
            panel.set_on_select(Rc::new(move || { canvas2.sync_inspector(); canvas2.area.queue_draw(); }));
            canvas.sync_inspector();
        }
        self.pages.borrow_mut().push(Page { root: root.clone(), canvas, panel });
        self.notebook.set_current_page(Some(index));
        self.show_tabs(true);
        let state = self.clone();
        close.connect_clicked(move |_| { if let Some(n) = state.notebook.page_num(&root) { state.close_page(n, Rc::new(|| {})); } });
    }

    /// Closes the page at `index`, asking about unsaved changes first; `then` runs once it is gone (not
    /// when the user cancels).
    fn close_page(self: &Rc<Self>, index: u32, then: Rc<dyn Fn()>) {
        self.notebook.set_current_page(Some(index));
        let (modified, title) = { let mut m = false; let mut t = String::new(); self.with_current(|p| { let d = p.canvas.doc().borrow(); m = d.document.is_modified(); t = d.title.clone(); }); (m, t) };
        if !modified { self.notebook.remove_page(Some(index)); then(); return; }
        let state = self.clone();
        dialogs::unsaved(self.window.upcast_ref(), &title, move |choice| {
            if choice == 0 { state.save_current(false); if state.current_modified() { return; } }
            if let Some(index) = state.notebook.current_page() { state.notebook.remove_page(Some(index)); }
            then();
        });
    }

    /// Closing the window closes each modified page in turn, with its prompt, and then the window.
    fn close_window(self: &Rc<Self>) {
        let modified = self.pages.borrow().iter().position(|p| p.canvas.doc().borrow().document.is_modified()).and_then(|i| { let pages = self.pages.borrow(); self.notebook.page_num(&pages[i].root) });
        match modified {
            None => self.window.destroy(),
            Some(index) => { let state = self.clone(); self.close_page(index, Rc::new(move || state.close_window())); }
        }
    }

    fn show_tabs(&self, tabs: bool) {
        self.stack.set_visible_child_name(if tabs { "tabs" } else { "empty" });
    }

    fn with_current(&self, f: impl FnOnce(&Page)) {
        let Some(index) = self.notebook.current_page() else { return };
        let child = self.notebook.nth_page(Some(index));
        let pages = self.pages.borrow();
        if let Some(page) = pages.iter().find(|p| Some(&p.root) == child.as_ref()) { f(page); }
    }

    /// Like `edit`, with the whole view state (tool settings, palette) in reach.
    fn with_doc(self: &Rc<Self>, f: impl FnOnce(&mut Doc) -> Result<()>) {
        let mut failure = None;
        self.with_current(|p| {
            let result = f(&mut p.canvas.doc().borrow_mut());
            p.refresh();
            if let Err(error) = result { failure = Some(format!("{error:#}")); }
        });
        if let Some(detail) = failure { self.alert("Could not do that", &detail); }
    }

    /// Runs an edit on the current document, reporting a failure in an alert.
    fn edit(self: &Rc<Self>, f: impl FnOnce(&mut Document) -> Result<()>) {
        let mut failure = None;
        self.with_current(|p| {
            let result = f(&mut p.canvas.doc().borrow_mut().document);
            p.refresh();
            if let Err(error) = result { failure = Some(format!("{error:#}")); }
        });
        self.update_tab_titles();
        if let Some(detail) = failure { self.alert("Could not apply", &detail); }
    }

    /// Ctrl+F: the picture alone on black, full screen; again brings everything back.
    /// Tab: the tool rail, options and layers panel come and go; the picture stays where it is.
    /// Ctrl+T: with a selection, floats it for the Move tool's handles; without one, the handles already on
    /// the active layer are the transform.
    fn free_transform(self: &Rc<Self>) {
        self.with_current(|p| {
            let has_selection = p.canvas.doc().borrow().document.selection.as_ref().is_some_and(|s| !s.is_empty());
            let result = if has_selection { p.canvas.doc().borrow_mut().document.begin_free_transform() } else { Ok(()) };
            match result {
                Ok(()) => {
                    p.canvas.set_tool(Tool::Move);
                    p.refresh();
                    p.canvas.notify(if has_selection { "Free Transform: drag the handles to move, scale or rotate; Return commits, Escape cancels." } else { "Drag the handles to move, scale or rotate the layer (Shift keeps the ratio, Alt scales from the center)." });
                }
                Err(error) => p.canvas.notify(&format!("{error:#}")),
            }
        });
    }

    fn toggle_panels(self: &Rc<Self>) {
        let on = !self.panels_hidden.get();
        self.panels_hidden.set(on);
        for page in self.pages.borrow().iter() {
            page.canvas.set_panels_hidden(on);
            if let Some(paned) = page.root.downcast_ref::<gtk::Paned>() { if let Some(end) = paned.end_child() { end.set_visible(!on); } }
        }
    }

    /// Help > Keyboard Shortcuts: every key, grouped, in a floating window.
    fn show_shortcuts(self: &Rc<Self>) {
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).margin_top(10).margin_bottom(12).margin_start(14).margin_end(14).build();
        let grid = gtk::Grid::builder().row_spacing(3).column_spacing(18).build();
        let mut row = 0;
        let mut group = "";
        for &(g, keys, what) in SHORTCUTS {
            if g != group {
                group = g;
                grid.attach(&gtk::Label::builder().label(g).xalign(0.0).margin_top(if row == 0 { 0 } else { 10 }).css_classes(["heading"]).build(), 0, row, 2, 1);
                row += 1;
            }
            grid.attach(&gtk::Label::builder().label(keys).xalign(0.0).css_classes(["monospace"]).build(), 0, row, 1, 1);
            grid.attach(&gtk::Label::builder().label(what).xalign(0.0).wrap(true).max_width_chars(48).css_classes(["dim-label"]).build(), 1, row, 1, 1);
            row += 1;
        }
        let scroller = gtk::ScrolledWindow::builder().child(&grid).min_content_height(520).max_content_height(760).propagate_natural_height(true).propagate_natural_width(true).hscrollbar_policy(gtk::PolicyType::Never).build();
        content.append(&scroller);
        let window = dialogs::floating(self.window.upcast_ref(), "Keyboard Shortcuts", false, 640, &content);
        let keys = gtk::EventControllerKey::new();
        { let window = window.clone(); keys.connect_key_pressed(move |_, key, _, _| { if key == gdk::Key::Escape { window.close(); glib::Propagation::Stop } else { glib::Propagation::Proceed } }); }
        window.add_controller(keys);
        window.present();
    }

    fn toggle_preview(self: &Rc<Self>) {
        let on = !self.preview.get();
        self.preview.set(on);
        self.header.set_visible(!on);
        self.notebook.set_show_tabs(!on);
        for page in self.pages.borrow().iter() {
            page.canvas.set_preview(on);
            if let Some(paned) = page.root.downcast_ref::<gtk::Paned>() { if let Some(end) = paned.end_child() { end.set_visible(!on); } }
            page.canvas.doc().borrow_mut().preview = on;
        }
        if on { self.window.fullscreen(); } else { self.window.unfullscreen(); }
        // Fit once the new layout has settled.
        let state = self.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || state.with_current(|p| { p.canvas.drop_cache(); p.canvas.fit(); }));
    }

    /// View > New Guide: a guide at a typed position.
    fn new_guide(self: &Rc<Self>) {
        let state = self.clone();
        dialogs::new_guide(self.window.upcast_ref(), move |vertical, position| state.with_current(|p| { { let mut d = p.canvas.doc().borrow_mut(); if vertical { d.document.guides_v.push(position); } else { d.document.guides_h.push(position); } d.document.show_guides = true; } p.canvas.area.queue_draw(); }));
    }

    /// The Generative Fill panel over the current selection.
    /// The Layer Style dialog for the active pixel layer.
    fn open_layer_style(self: &Rc<Self>) {
        let mut opened = false;
        self.with_current(|p| {
            let doc = p.canvas.doc().clone();
            let id = { let d = doc.borrow(); d.document.active.filter(|id| { let l = d.document.renderer.layer(*id); !l.is_group() && l.adjustment.is_none() }) };
            let Some(id) = id else { return };
            let (panel, area) = (p.panel.clone(), p.canvas.area.clone());
            let finished: Rc<dyn Fn()> = Rc::new(move || { panel.rebuild(); area.queue_draw(); });
            effects::open(self.window.upcast_ref(), doc, id, finished);
            opened = true;
        });
        if !opened && self.notebook.current_page().is_some() { self.alert("Select a pixel layer first", "Layer effects go on image, shape and type layers, not folders or adjustments."); }
    }

    fn open_genfill(self: &Rc<Self>, expand: bool) {
        let mut opened = false;
        self.with_current(|p| {
            let doc = p.canvas.doc().clone();
            if doc.borrow().document.genfill_window().is_err() { return; }
            let (panel, area) = (p.panel.clone(), p.canvas.area.clone());
            let finished: Rc<dyn Fn()> = Rc::new(move || { panel.rebuild(); area.queue_draw(); });
            genfill::GenFill::open(self.window.upcast_ref(), doc, finished, expand);
            opened = true;
        });
        if !opened && self.notebook.current_page().is_some() { self.alert("Select an area first", "Generative Fill paints inside a selection. Make one with the Marquee, Lasso or Wand, then try again."); }
    }

    /// Generative Expand: a Canvas Size dialog that grows the canvas, selects the new margin, then opens the
    /// fill panel on it.
    fn generative_expand(self: &Rc<Self>) {
        let state = self.clone();
        let Some((w, h)) = self.current_size() else { return };
        dialogs::canvas_size(self.window.upcast_ref(), (w, h), move |nw, nh, anchor, _| {
            let mut ok = true;
            let s2 = state.clone();
            state.edit(|d| { let r = d.expand_canvas_for_fill(nw, nh, anchor); ok = r.is_ok(); r });
            if ok { s2.with_current(|p| p.canvas.fit()); s2.open_genfill(true); }
        });
    }

    fn open_filter(self: &Rc<Self>, kind: Kind) {
        let mut opened = false;
        self.with_current(|p| {
            let doc = p.canvas.doc().clone();
            if doc.borrow().document.active_image().is_none() { return; }
            let (panel, area) = (p.panel.clone(), p.canvas.area.clone());
            let finished: Rc<dyn Fn()> = Rc::new(move || { panel.rebuild(); area.queue_draw(); });
            filter_dialog::FilterDialog::open(self.window.upcast_ref(), doc, kind, p.canvas.area.clone(), finished);
            opened = true;
        });
        if !opened && self.notebook.current_page().is_some() { self.alert("Select an image layer first", "Filters work on the active image layer. Click a layer with pixels in the Layers panel."); }
    }

    /// Drops records of pages the notebook no longer holds.
    fn prune(&self) {
        let any = {
            let mut pages = self.pages.borrow_mut();
            pages.retain(|p| self.notebook.page_num(&p.root).is_some());
            !pages.is_empty()
        };
        self.show_tabs(any);
    }

    /// Closes the current tab, asking first when it has unsaved changes.
    fn close_current(self: &Rc<Self>) {
        let Some(index) = self.notebook.current_page() else { return };
        self.close_page(index, Rc::new(|| {}));
    }

    fn current_modified(&self) -> bool { let mut m = false; self.with_current(|p| m = p.canvas.doc().borrow().document.is_modified()); m }
    fn current_title(&self) -> String { let mut t = "Untitled".to_string(); self.with_current(|p| t = p.canvas.doc().borrow().title.clone()); t }
    fn current_size(&self) -> Option<(i32, i32)> { let mut s = None; self.with_current(|p| { let d = p.canvas.doc().borrow(); s = Some((d.document.width(), d.document.height())); }); s }
    fn current_resolution(&self) -> f64 { let mut r = 72.0; self.with_current(|p| r = p.canvas.doc().borrow().document.renderer.resolution()); r }

    /// Save, or Save As when the document has no path yet (or `ask`).
    fn save_current(self: &Rc<Self>, ask: bool) {
        let mut path = None;
        self.with_current(|p| path = p.canvas.doc().borrow().document.path.clone());
        match path {
            Some(path) if !ask => self.save_to(&path),
            _ => {
                let state = self.clone();
                let title = self.current_title();
                dialogs::save_as(self.window.upcast_ref(), "Save Project", &title, "comp", move |path| state.save_to(&path));
            }
        }
    }

    fn save_to(&self, path: &Path) {
        let mut failure = None;
        self.with_current(|p| {
            let mut d = p.canvas.doc().borrow_mut();
            match d.document.save(path) {
                Ok(()) => {
                    d.document.path = Some(path.to_path_buf());
                    d.title = path.file_name().map(|n| n.to_string_lossy().trim_end_matches(".comp").to_string()).unwrap_or_else(|| "Untitled".into());
                }
                Err(error) => failure = Some(format!("{error:#}")),
            }
        });
        self.update_tab_titles();
        if let Some(detail) = failure { self.alert("Could not save", &detail); }
    }

    /// Tab labels carry the title and a dot while unsaved.
    fn update_tab_titles(&self) {
        for page in self.pages.borrow().iter() {
            let d = page.canvas.doc().borrow();
            let label = format!("{}{}", d.title, if d.document.is_modified() { " •" } else { "" });
            if let Some(tab) = self.notebook.tab_label(&page.root) { if let Some(l) = tab.first_child().and_downcast::<gtk::Label>() { l.set_label(&label); } }
        }
    }

    fn export_jpeg(self: &Rc<Self>) {
        let mut doc = None;
        self.with_current(|p| doc = Some(p.canvas.doc().clone()));
        let Some(doc) = doc else { return };
        let encode: Rc<dyn Fn(f64, [f64; 3]) -> Result<Vec<u8>>> = { let doc = doc.clone(); Rc::new(move |q, bg| doc.borrow_mut().document.jpeg_bytes(q, bg)) };
        let state = self.clone();
        let title = self.current_title();
        dialogs::jpeg_export(self.window.upcast_ref(), encode, move |quality, background| {
            let (state, doc) = (state.clone(), doc.clone());
            let window = state.window.clone();
            dialogs::save_as(window.upcast_ref(), "Export JPEG", &title, "jpg", move |path| {
                if let Err(error) = doc.borrow_mut().document.export_jpeg(&path, quality, background) { state.alert("Could not export", &format!("{error:#}")); }
            });
        });
    }

    fn export_psd(self: &Rc<Self>) {
        let state = self.clone();
        let title = self.current_title();
        dialogs::save_as(self.window.upcast_ref(), "Export PSD", &title, "psd", move |path| {
            let mut outcome = None;
            state.with_current(|p| outcome = Some(p.canvas.doc().borrow_mut().document.export_psd(&path)));
            match outcome {
                Some(Ok(notes)) if !notes.is_empty() => state.alert("Exported with changes", &notes.join("\n")),
                Some(Err(error)) => state.alert("Could not export", &format!("{error:#}")),
                _ => {}
            }
        });
    }

    /// Ctrl+C: the active layer's selected pixels to the clipboard as an image.
    fn copy_layer(&self) {
        let mut result: Result<Option<(Vec<u8>, (i32, i32, i32, i32))>> = Ok(None);
        self.with_current(|p| result = p.canvas.doc().borrow_mut().document.copy_layer_pixels());
        match result {
            Ok(Some((bytes, _))) => match gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)) {
                Ok(texture) => self.window.clipboard().set_texture(&texture),
                Err(error) => self.alert("Could not copy", &format!("{error}")),
            },
            Ok(None) => {}
            Err(error) => self.alert("Could not copy", &format!("{error:#}")),
        }
    }

    /// Ctrl+V: whatever image the clipboard holds (pixels from any app, or image files) as a new layer.
    fn paste(self: &Rc<Self>) {
        let clipboard = self.window.clipboard();
        let state = self.clone();
        clipboard.read_texture_async(gio::Cancellable::NONE, move |result| {
            match result {
                Ok(Some(texture)) => {
                    let bytes = texture.save_to_png_bytes();
                    if state.notebook.current_page().is_some() { state.edit(|d| d.import_image_bytes(&bytes, "Pasted").map(|_| ())); }
                    else { match Document::open_image_bytes(&bytes) { Ok(document) => state.add_page(Doc::from(document, "Pasted")), Err(e) => state.alert("Could not paste", &format!("{e:#}")) } }
                }
                _ => {
                    // Not pixels: perhaps files copied from a file manager.
                    let state = state.clone();
                    state.window.clipboard().read_value_async(gdk::FileList::static_type(), glib::Priority::DEFAULT, gio::Cancellable::NONE, move |result| {
                        let files = result.ok().and_then(|v| v.get::<gdk::FileList>().ok()).map(|l| l.files()).unwrap_or_default();
                        if files.is_empty() { state.alert("Nothing to paste", "The clipboard holds no image or image files."); return; }
                        for file in files { if let Some(path) = file.path() { if is_openable(&path) && state.notebook.current_page().is_none() { state.open_path(&path); } else { state.edit(|d| d.import_image(&path).map(|_| ())); } } }
                    });
                }
            }
        });
    }

    fn copy_merged(&self) {
        let mut result: Result<Option<(Vec<u8>, (i32, i32, i32, i32))>> = Ok(None);
        self.with_current(|p| result = p.canvas.doc().borrow_mut().document.copy_merged());
        match result {
            Ok(Some((bytes, _))) => {
                match gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)) {
                    Ok(texture) => self.window.clipboard().set_texture(&texture),
                    Err(error) => self.alert("Could not copy", &format!("{error}")),
                }
            }
            Ok(None) => {}
            Err(error) => self.alert("Could not copy", &format!("{error:#}")),
        }
    }

    fn rename_layer(self: &Rc<Self>) {
        let mut current = None;
        self.with_current(|p| { let d = p.canvas.doc().borrow(); current = d.document.active.map(|id| (id, d.document.renderer.layer(id).name.clone())); });
        let Some((id, name)) = current else { return };
        let state = self.clone();
        dialogs::text(self.window.upcast_ref(), "Rename Layer", "Name", &name, move |text| state.edit(|d| { d.rename_layer(id, &text); Ok(()) }));
    }

    /// Opens the settings of the active adjustment layer with a live preview and one undo step on OK.
    fn edit_adjustment(self: &Rc<Self>) {
        let mut target = None;
        self.with_current(|p| { let d = p.canvas.doc().borrow(); target = d.document.active.and_then(|id| d.document.adjustment(id).map(|a| (id, a, p.canvas.doc().clone(), p.canvas.area.clone(), p.panel.clone()))); });
        let Some((id, adjustment, doc, area, panel)) = target else { return };
        let finished: Rc<dyn Fn()> = Rc::new(move || { panel.rebuild(); area.queue_draw(); });
        filter_dialog::FilterDialog::open_adjustment(self.window.upcast_ref(), doc, id, adjustment, finished);
    }

    fn current_canvas_cursor(&self) {
        self.with_current(|p| p.canvas.update_cursor());
    }

    fn choose_and_open(&self) {
        let dialog = gtk::FileDialog::builder().title("Open Compositor project").modal(true).build();
        let window = self.window.clone();
        let target = window.clone();
        dialog.select_folder(Some(&window), gio::Cancellable::NONE, move |result| {
            let Ok(file) = result else { return };
            let Some(path) = file.path() else { return };
            if let Some(action) = target.lookup_action("open-path") { action.activate(Some(&path.to_string_lossy().to_variant())); }
        });
    }

    fn alert(&self, message: &str, detail: &str) {
        gtk::AlertDialog::builder().message(message).detail(detail).modal(true).build().show(Some(&self.window));
    }
}

fn snapshot_window(window: &gtk::Window, target: &Path) -> Result<()> {
    // Snapshotting the children (title bar and content) renders them fresh; a WidgetPaintable of the whole
    // window comes back empty whenever a redraw is queued, which the marching ants keep doing.
    // Every widget is asked to draw again, so a picture whose texture changed on the last frame is not
    // captured from the render node it cached before that.
    fn queue_all(widget: &gtk::Widget) { widget.queue_draw(); let mut c = widget.first_child(); while let Some(w) = c { queue_all(&w); c = w.next_sibling(); } }
    queue_all(window.upcast_ref());
    let snapshot = gtk::Snapshot::new();
    let mut child = window.first_child();
    while let Some(widget) = child {
        window.snapshot_child(&widget, &snapshot);
        child = widget.next_sibling();
    }
    let node = snapshot.to_node().context("nothing drawn")?;
    let renderer = window.renderer().context("no renderer")?;
    let texture = renderer.render_texture(&node, None);
    texture.save_to_png(target)?;
    // Open popovers draw on their own surfaces, so each one goes to a file of its own (`-popover`).
    let mut popovers = Vec::new();
    collect_popovers(window.upcast_ref(), &mut popovers);
    if std::env::var_os("COMPOSITOR_TRACE").is_some() { eprintln!("popovers open: {}", popovers.len()); }
    for (i, popover) in popovers.iter().enumerate() {
        // The user's child sits inside the popover's own contents widget; snapshot the direct children.
        let snapshot = gtk::Snapshot::new();
        let mut child = popover.first_child();
        while let Some(widget) = child { popover.snapshot_child(&widget, &snapshot); child = widget.next_sibling(); }
        let Some(node) = snapshot.to_node() else { continue };
        let texture = renderer.render_texture(&node, None);
        let stem = target.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        texture.save_to_png(target.with_file_name(format!("{stem}-popover{}.png", if i == 0 { String::new() } else { i.to_string() })))?;
    }
    Ok(())
}

fn collect_popovers(widget: &gtk::Widget, out: &mut Vec<gtk::Popover>) {
    if let Some(popover) = widget.downcast_ref::<gtk::Popover>() { if popover.is_visible() { out.push(popover.clone()); } }
    let mut child = widget.first_child();
    while let Some(c) = child { collect_popovers(&c, out); child = c.next_sibling(); }
}
