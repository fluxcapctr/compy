//! The GTK4 application: a window of project tabs, each a canvas with its tool rail beside a layers panel,
//! and a menu of edits that run on the current tab's document.

mod canvas;
mod dialogs;
mod filter_dialog;
mod icons;
mod layers;
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
    /// The Blur tool's mode: 0 Liquify, 1 Blur, 2 Smudge, as the Mac orders them.
    pub blur_mode: u32,
    /// Guides drawn while a move is snapped: an x and a y across the whole canvas.
    pub snap_guides: (Option<f64>, Option<f64>),
    pub syncing_inspector: bool,
    pub needs_redraw: bool,
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
    stack: gtk::Stack,
    notebook: gtk::Notebook,
    pages: RefCell<Vec<Page>>,
    space_held: Rc<Cell<bool>>,
}

impl Doc {
    pub fn from(document: Document, title: &str) -> Doc {
        Doc { title: title.to_string(), document, viewport: Viewport::default(), collapsed: HashSet::new(), tool: Tool::Move, wand: WandSettings::default(), mode: Mode::Replace, ants_phase: 0.0,
            brush: BrushSettings::default(), heal_mode: 0, clone_aligned: true, clone_all_layers: false, clone_source: None, clone_offset: None, last_brush_point: None,
            marquee_ellipse: false, lasso_polygonal: false, antialiased: true, lock_ratio: true, auto_select: false, mask_paint_white: false, blur_mode: 0, snap_guides: (None, None), syncing_inspector: false, needs_redraw: false }
    }
}

pub fn open_document(path: &Path) -> Result<Doc> {
    let project = format::load(path).with_context(|| format!("loading {}", path.display()))?;
    let document = Document::new(project)?;
    let title = path.file_name().map(|n| n.to_string_lossy().trim_end_matches(".comp").to_string()).unwrap_or_else(|| "Untitled".into());
    Ok(Doc { title, document, viewport: Viewport::default(), collapsed: HashSet::new(), tool: Tool::Move, wand: WandSettings::default(), mode: Mode::Replace, ants_phase: 0.0,
        brush: BrushSettings::default(), heal_mode: 0, clone_aligned: true, clone_all_layers: false, clone_source: None, clone_offset: None, last_brush_point: None,
        marquee_ellipse: false, lasso_polygonal: false, antialiased: true, lock_ratio: true, auto_select: false, mask_paint_white: false, blur_mode: 0, snap_guides: (None, None), syncing_inspector: false, needs_redraw: false })
}

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
    /// An adjustment layer to add and open for editing.
    pub adjustment: Option<String>,
}

pub fn run(paths: Vec<PathBuf>, script: Script) -> glib::ExitCode {
    let app = gtk::Application::new(Some(APP_ID), gio::ApplicationFlags::NON_UNIQUE);
    app.connect_activate(move |app| {
        let state = build_window(app);
        for path in &paths { state.open_path(path); }
        state.window.present();
        if script.zoom.is_some() || script.wand.is_some() || script.filter.is_some() || script.tool.is_some() || script.adjustment.is_some() || script.layer.is_some() {
            let (state, script) = (state.clone(), script.clone());
            // After the first layout, so the fit has happened.
            glib::timeout_add_local_once(Duration::from_millis(300), move || {
                state.with_current(|p| {
                    if let Some(name) = &script.layer { let mut d = p.canvas.doc().borrow_mut(); if let Some(id) = d.document.renderer.layers().iter().find(|l| l.name == *name).map(|l| l.id) { d.document.active = Some(id); } }
                    if let Some(zoom) = script.zoom { p.canvas.zoom_to(zoom); }
                    if let Some((x, y)) = script.wand { p.canvas.wand_at(x, y); }
                    if let Some(tool) = script.tool { p.canvas.set_tool(tool); }
                    if script.ellipse { p.canvas.doc().borrow_mut().marquee_ellipse = true; }
                    if let Some(size) = script.brush_size { p.canvas.doc().borrow_mut().brush.diameter = size; p.canvas.sync_brush_options(); }
                    if let Some(mode) = script.blur_mode { p.canvas.doc().borrow_mut().blur_mode = mode; }
                    if !script.stroke.is_empty() { p.canvas.scripted_stroke(&script.stroke); }
                });
                if let Some(kind) = script.filter { state.open_filter(kind); }
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
                let done = Rc::new(Cell::new(false));
                let (main2, app2, done2) = (main.clone(), app.clone(), done.clone());
                clock.connect_after_paint(move |_| {
                    if done2.replace(true) { return; }
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
    let window = gtk::ApplicationWindow::builder().application(app).title("Compositor").default_width(1280).default_height(820).build();
    let header = gtk::HeaderBar::new();
    let open = gtk::Button::builder().label("Open").tooltip_text("Open a .comp project (Ctrl+O)").action_name("win.open").build();
    header.pack_start(&open);
    header.pack_end(&gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu()).tooltip_text("Edit, Select, Image and Filter").build());
    window.set_titlebar(Some(&header));

    let notebook = gtk::Notebook::builder().scrollable(true).show_border(false).build();
    let empty = gtk::Label::builder().label("Open a .comp project to begin (Ctrl+O).")
        .justify(gtk::Justification::Center).css_classes(["dim-label"]).vexpand(true).hexpand(true).build();
    let stack = gtk::Stack::new();
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&notebook, Some("tabs"));
    window.set_child(Some(&stack));

    let state = Rc::new(App { window: window.clone(), stack, notebook: notebook.clone(), pages: RefCell::new(Vec::new()), space_held: Rc::new(Cell::new(false)) });

    // Space held turns a drag into a pan; single letters pick tools, unless a text field has focus.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let pressed = state.clone();
        let released = state.clone();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let state = &pressed;
            if gtk::prelude::GtkWindowExt::focus(&state.window).is_some_and(|w| w.is::<gtk::Editable>() || w.is::<gtk::Text>()) { return glib::Propagation::Proceed; }
            if key == gdk::Key::space {
                if !state.space_held.get() { state.space_held.set(true); state.current_canvas_cursor(); }
                return glib::Propagation::Stop;
            }
            if modifiers.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) { return glib::Propagation::Proceed; }
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

    let actions: [(&str, &[&str], fn(&Rc<App>)); 42] = [
        ("open", &["<Control>o"], |s| s.choose_and_open()),
        ("close-tab", &["<Control>w"], |s| s.close_current()),
        ("zoom-in", &["<Control>equal", "<Control>plus", "<Control>KP_Add"], |s| s.with_current(|p| p.canvas.zoom_by(2.0))),
        ("zoom-out", &["<Control>minus", "<Control>KP_Subtract"], |s| s.with_current(|p| p.canvas.zoom_by(0.5))),
        ("zoom-fit", &["<Control>0"], |s| s.with_current(|p| p.canvas.fit())),
        ("zoom-actual", &["<Control>1"], |s| s.with_current(|p| p.canvas.zoom_to(1.0))),
        ("undo", &["<Control>z"], |s| s.with_current(|p| { p.canvas.doc().borrow_mut().document.undo(); p.refresh(); })),
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
        ("toggle-clipping", &["<Alt>g"], |s| s.edit(|d| { if let Some(id) = d.active { d.toggle_clipping(id); } Ok(()) })),
        ("select-layer-pixels", &[], |s| s.edit(|d| { match d.active { Some(id) => d.select_layer_pixels(id, Mode::Replace), None => Ok(()) } })),
        ("new", &["<Control>n"], |s| { let state = s.clone(); dialogs::new_canvas(s.window.upcast_ref(), move |w, h, r| match Document::blank(w, h, r) { Ok(document) => state.add_page(Doc::from(document, "Untitled")), Err(e) => state.alert("Could not create the canvas", &format!("{e:#}")) }); }),
        ("save", &["<Control>s"], |s| s.save_current(false)),
        ("save-as", &["<Control><Shift>s"], |s| s.save_current(true)),
        ("import", &["<Control><Shift>i"], |s| { let state = s.clone(); dialogs::open_image(s.window.upcast_ref(), move |path| state.edit(|d| d.import_image(&path).map(|_| ()))); }),
        ("export-png", &["<Control><Shift>e"], |s| { let state = s.clone(); let title = s.current_title(); dialogs::save_as(s.window.upcast_ref(), "Export PNG", &title, "png", move |path| state.edit(|d| d.export_png(&path))); }),
        ("export-jpeg", &["<Control><Alt><Shift>s"], |s| s.export_jpeg()),
        ("copy-merged", &["<Control><Shift>c"], |s| s.copy_merged()),
        ("new-layer", &["<Control><Shift>n"], |s| s.edit(|d| { d.add_blank_layer(); Ok(()) })),
        ("new-folder", &["<Control>g"], |s| s.edit(|d| { d.add_folder(); Ok(()) })),
        ("duplicate-layer", &["<Control>j"], |s| s.edit(|d| { d.duplicate_layer(); Ok(()) })),
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
                "gradient" => Kind::GradientMap, "levels" => Kind::Levels, "hsv" => Kind::HueSaturation, "exposure" => Kind::Exposure, "gaussian" => Kind::GaussianBlur, "motion" => Kind::MotionBlur, _ => return,
            };
            state.open_filter(kind);
        });
        window.add_action(&action);
        app.set_accels_for_action("win.filter::levels", &["<Control>l"]);
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
        let state = state.clone();
        target.connect_drop(move |_, value, _, _| {
            let Ok(file) = value.get::<gio::File>() else { return false };
            let Some(path) = file.path() else { return false };
            if dialogs::is_project(&path) { state.open_path(&path); }
            else if state.notebook.current_page().is_some() { state.edit(|d| d.import_image(&path).map(|_| ())); }
            else { state.alert("Not a Compositor project", "Open a project first, then drop images to import them as layers."); }
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
    state
}

fn menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let file = gio::Menu::new();
    file.append(Some("New Canvas…"), Some("win.new"));
    file.append(Some("Open…"), Some("win.open"));
    file.append(Some("Save"), Some("win.save"));
    file.append(Some("Save As…"), Some("win.save-as"));
    file.append(Some("Import Image…"), Some("win.import"));
    file.append(Some("Export PNG…"), Some("win.export-png"));
    file.append(Some("Export JPEG…"), Some("win.export-jpeg"));
    file.append(Some("Close"), Some("win.close-tab"));
    menu.append_submenu(Some("File"), &file);
    let edit = gio::Menu::new();
    edit.append(Some("Undo"), Some("win.undo"));
    edit.append(Some("Redo"), Some("win.redo"));
    edit.append(Some("Copy Merged"), Some("win.copy-merged"));
    menu.append_submenu(Some("Edit"), &edit);
    let select = gio::Menu::new();
    select.append(Some("All"), Some("win.select-all"));
    select.append(Some("Deselect"), Some("win.deselect"));
    select.append(Some("Inverse"), Some("win.invert-selection"));
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
    layer.append(Some("Duplicate Layer"), Some("win.duplicate-layer"));
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
    menu.append_submenu(Some("Layer"), &layer);
    let image = gio::Menu::new();
    image.append(Some("Canvas Size…"), Some("win.canvas-size"));
    image.append(Some("Image Size…"), Some("win.image-size"));
    image.append(Some("Crop to Selection"), Some("win.crop"));
    image.append(Some("Hue/Saturation…"), Some("win.filter::hsv"));
    image.append(Some("Exposure…"), Some("win.filter::exposure"));
    image.append(Some("Levels…"), Some("win.filter::levels"));
    image.append(Some("Gradient Map…"), Some("win.filter::gradient"));
    image.append(Some("Grain…"), Some("win.filter::grain"));
    menu.append_submenu(Some("Image"), &image);
    let filter = gio::Menu::new();
    filter.append(Some("Gaussian Blur…"), Some("win.filter::gaussian"));
    filter.append(Some("Motion Blur…"), Some("win.filter::motion"));
    filter.append(Some("Add Noise…"), Some("win.filter::noise"));
    filter.append(Some("Lens Correction…"), Some("win.filter::lens"));
    filter.append(Some("Content-Aware Fill"), Some("win.content-aware-fill"));
    filter.append(Some("Heal Selection"), Some("win.heal-selection"));
    menu.append_submenu(Some("Filter"), &filter);
    menu
}

impl App {
    fn open_path(&self, path: &Path) {
        match open_document(path) {
            Ok(doc) => self.add_page(doc),
            Err(error) => self.alert("Could not open project", &format!("{error:#}")),
        }
    }

    fn add_page(&self, doc: Doc) {
        let title = doc.title.clone();
        let doc: DocRef = Rc::new(RefCell::new(doc));
        let canvas = canvas::Canvas::new(doc.clone(), self.space_held.clone());
        let panel = layers::LayersPanel::new(doc.clone(), canvas.area.clone());
        let paned = gtk::Paned::builder().orientation(gtk::Orientation::Horizontal)
            .shrink_end_child(false).resize_end_child(false).shrink_start_child(false).build();
        paned.set_start_child(Some(&canvas.widget));
        paned.set_end_child(Some(&panel.widget));
        paned.set_position(1280 - layers::WIDTH);

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
        }
        self.pages.borrow_mut().push(Page { root: root.clone(), canvas, panel });
        self.notebook.set_current_page(Some(index));
        self.show_tabs(true);
        let notebook = self.notebook.clone();
        close.connect_clicked(move |_| { if let Some(n) = notebook.page_num(&root) { notebook.remove_page(Some(n)); } });
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
        let (modified, title) = { let mut m = false; let mut t = String::new(); self.with_current(|p| { let d = p.canvas.doc().borrow(); m = d.document.is_modified(); t = d.title.clone(); }); (m, t) };
        if !modified { self.notebook.remove_page(Some(index)); return; }
        let state = self.clone();
        dialogs::unsaved(self.window.upcast_ref(), &title, move |choice| {
            if choice == 0 { state.save_current(false); if state.current_modified() { return; } }
            if let Some(index) = state.notebook.current_page() { state.notebook.remove_page(Some(index)); }
        });
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
    Ok(())
}
