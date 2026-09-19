//! Omarchy theme sync. Omarchy writes the active palette to
//! `~/.local/state/omarchy/current/theme/colors.toml` whenever a theme is set; this reads it into a
//! palette, styles the app's chrome from it, and watches the file so a theme change re-skins the app
//! while it runs. Without the file (another desktop) the stock look stays. The canvas checkerboard and
//! selection ants are never themed: they must stay neutral so image colors read true.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub accent: (f64, f64, f64),
    pub background: String,
    pub dark_background: String,
    pub darker_background: String,
    pub lighter_background: String,
    pub foreground: String,
    pub dark_foreground: String,
    pub bright_foreground: String,
    pub selection: String,
    pub muted: String,
    pub accent_hex: String,
}

thread_local! {
    static CURRENT: RefCell<Option<Palette>> = const { RefCell::new(None) };
    static PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
}

/// The palette in force, if Omarchy provides one.
pub fn current() -> Option<Palette> { CURRENT.with(|c| c.borrow().clone()) }

/// The accent as cairo components, for the canvas overlays; the stock blue otherwise.
pub fn accent() -> (f64, f64, f64) { current().map_or((0.21, 0.52, 0.89), |p| p.accent) }

/// The color around the document on the canvas.
pub fn surround() -> (f64, f64, f64) { current().and_then(|p| hex(&p.darker_background)).unwrap_or((0.105, 0.105, 0.105)) }

pub fn colors_path() -> PathBuf {
    if let Ok(path) = std::env::var("COMPOSITOR_THEME_COLORS") { return PathBuf::from(path); }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".local/state/omarchy/current/theme/colors.toml")
}

fn hex(text: &str) -> Option<(f64, f64, f64)> {
    let t = text.trim().trim_start_matches('#');
    if t.len() != 6 { return None; }
    let v = u32::from_str_radix(t, 16).ok()?;
    Some((((v >> 16) & 255) as f64 / 255.0, ((v >> 8) & 255) as f64 / 255.0, (v & 255) as f64 / 255.0))
}

/// The flat `key = "value"` lines of colors.toml; nothing else in the file matters here.
pub fn parse(text: &str) -> Option<Palette> {
    let mut map: HashMap<&str, &str> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') { continue; }
        let Some((key, value)) = line.split_once('=') else { continue };
        map.insert(key.trim(), value.trim().trim_matches('"'));
    }
    let get = |key: &str| map.get(key).filter(|v| hex(v).is_some()).map(|v| v.to_string());
    let background = get("background")?;
    let foreground = get("foreground")?;
    let accent_hex = get("accent").unwrap_or_else(|| foreground.clone());
    Some(Palette {
        dark: map.get("mode").is_none_or(|m| *m != "light"),
        accent: hex(&accent_hex)?,
        dark_background: get("dark_background").unwrap_or_else(|| background.clone()),
        darker_background: get("darker_background").or_else(|| get("dark_background")).unwrap_or_else(|| background.clone()),
        lighter_background: get("lighter_background").or_else(|| get("selection")).unwrap_or_else(|| background.clone()),
        dark_foreground: get("dark_foreground").or_else(|| get("muted")).unwrap_or_else(|| foreground.clone()),
        bright_foreground: get("bright_foreground").unwrap_or_else(|| foreground.clone()),
        selection: get("selection").unwrap_or_else(|| accent_hex.clone()),
        muted: get("muted").or_else(|| get("dark_foreground")).unwrap_or_else(|| foreground.clone()),
        background, foreground, accent_hex,
    })
}

/// GTK CSS for the chrome: window, header, tabs, tool rail, options, layers panel, fields and popovers.
pub fn css(p: &Palette) -> String {
    let (bg, dbg, ddbg, lbg) = (&p.background, &p.dark_background, &p.darker_background, &p.lighter_background);
    let (fg, dfg, bfg, sel, muted, accent) = (&p.foreground, &p.dark_foreground, &p.bright_foreground, &p.selection, &p.muted, &p.accent_hex);
    let on_accent = if p.dark { dbg } else { bfg };
    format!("
        @define-color accent_color {accent};
        @define-color accent_bg_color {accent};
        @define-color accent_fg_color {on_accent};
        @define-color window_bg_color {bg};
        @define-color window_fg_color {fg};
        @define-color view_bg_color {dbg};
        @define-color view_fg_color {fg};
        @define-color headerbar_bg_color {dbg};
        @define-color headerbar_fg_color {fg};
        @define-color popover_bg_color {lbg};
        @define-color popover_fg_color {fg};
        @define-color dialog_bg_color {bg};
        @define-color dialog_fg_color {fg};
        @define-color card_bg_color {lbg};
        window, .background {{ background-color: {bg}; color: {fg}; }}
        .start-page, .start-page viewport, .start-page flowbox, .start-page flowboxchild {{ background-color: {bg}; color: {fg}; }}
        .start-page button {{ color: {fg}; }}
        headerbar {{ background-color: {dbg}; color: {fg}; box-shadow: none; border-bottom: 1px solid {ddbg}; }}
        headerbar button, .options button {{ background-color: transparent; color: {fg}; border: 1px solid {lbg}; box-shadow: none; text-shadow: none; }}
        .main-menu > item {{ color: {fg}; background-color: transparent; }}
        .main-menu > item:hover, .main-menu > item:selected {{ background-color: {sel}; color: {bfg}; }}
        headerbar button:hover, .options button:hover {{ background-color: {lbg}; border-color: {muted}; }}
        button {{ background-color: transparent; color: {fg}; border-color: {lbg}; }}
        button:hover {{ background-color: {lbg}; border-color: {muted}; }}
        button:checked {{ background-color: {sel}; color: {bfg}; }}
        .layers-footer button, .layers-panel row button {{ background-color: transparent; background-image: none; border: none; box-shadow: none; }}
        button.suggested-action {{ background-color: {fg}; color: {dbg}; border-color: {fg}; }}
        button.suggested-action:hover {{ background-color: {bfg}; border-color: {bfg}; }}
        decoration {{ box-shadow: 0 0 0 1px {muted}; }}
        notebook > header {{ background-color: {dbg}; border-color: {ddbg}; }}
        notebook > header tab {{ color: {dfg}; }}
        notebook > header tab:checked {{ color: {bfg}; box-shadow: inset 0 -2px {accent}; }}
        notebook > header tab button {{ background: none; border: none; color: {dfg}; }}
        .tool-rail {{ background-color: {dbg}; border-right: 1px solid {ddbg}; }}
        button.tool {{ background-color: transparent; color: {fg}; border: none; box-shadow: none; }}
        button.tool:hover {{ background-color: {sel}; }}
        button.tool:checked {{ background-color: {lbg}; color: {bfg}; box-shadow: inset 0 0 0 1px {muted}; }}
        .panel-tabs {{ background-color: {ddbg}; }}
        .panel-tab {{ color: {dfg}; }}
        .panel-tab.current {{ background-color: {bg}; color: {bfg}; border-bottom: 1px solid {accent}; }}
        .options {{ background-color: {bg}; border-bottom: 1px solid {ddbg}; }}
        .layers-panel {{ background-color: {bg}; border-left: 1px solid {ddbg}; }}
        .layers-panel .heading {{ color: {bfg}; }}
        list.navigation-sidebar {{ background-color: {bg}; }}
        list.navigation-sidebar > row {{ color: {fg}; }}
        list.navigation-sidebar > row:hover {{ background-color: {lbg}; }}
        list.navigation-sidebar > row:selected {{ background-color: {sel}; color: {bfg}; }}
        list.navigation-sidebar > row:selected .dim-label {{ color: {dfg}; }}
        .dim-label, label.caption {{ color: {dfg}; }}
        .canvas-status {{ background-color: {dbg}; color: {dfg}; border-top: 1px solid {ddbg}; }}
        entry, spinbutton, dropdown > button {{ background-color: {dbg}; color: {fg}; border: 1px solid {lbg}; box-shadow: none; }}
        spinbutton text, entry > text {{ background-color: transparent; border: none; box-shadow: none; }}
        spinbutton button {{ background-color: transparent; color: {dfg}; border: none; border-left: 1px solid {lbg}; }}
        spinbutton button:hover {{ background-color: {lbg}; color: {fg}; }}
        entry:focus-within, spinbutton:focus-within {{ border-color: {accent}; outline-color: {accent}; }}
        scale trough {{ background-color: {lbg}; }}
        scale highlight {{ background-color: {accent}; }}
        scale slider {{ background-color: {bfg}; border: 1px solid {dbg}; box-shadow: none; }}
        check, radio {{ background-color: {dbg}; border: 1px solid {muted}; color: {on_accent}; }}
        check:checked, radio:checked {{ background-color: {accent}; border-color: {accent}; color: {on_accent}; }}
        popover > contents, popover.menu > contents {{ background-color: {dbg}; color: {fg}; border: 1px solid {muted}; }}
        popover.menu modelbutton:hover, popover listview > row:hover {{ background-color: {sel}; }}
        popover listview > row:selected {{ background-color: {accent}; color: {on_accent}; }}
        paned > separator {{ background-color: {ddbg}; }}
        tooltip {{ background-color: {ddbg}; color: {fg}; border: 1px solid {muted}; }}
        *:focus-visible {{ outline-color: {accent}; }}
        selection {{ background-color: {accent}; color: {on_accent}; }}
    ")
}

/// The system's monospace font as Omarchy sets it (`omarchy font current`), else JetBrains Mono.
pub fn system_font() -> String {
    if let Ok(name) = std::env::var("COMPOSITOR_FONT") { if !name.trim().is_empty() { return name; } }
    let out = std::process::Command::new("omarchy").args(["font", "current"]).output().ok();
    let name = out.filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    if name.is_empty() { "JetBrains Mono".into() } else { name }
}

/// The look shared with omarchy.org, independent of the palette: the system monospace font throughout,
/// square corners, one-pixel borders, flat controls, small uppercase labels with wide tracking.
pub fn look_css(font: &str) -> String {
    let font = font.replace('"', "");
    format!(r#"
        window, .background, popover, tooltip {{ font-family: "{font}", "JetBrains Mono", monospace; font-size: 12.5px; }}
        button, entry, spinbutton, spinbutton text, spinbutton button, dropdown > button, menubutton > button, check, radio, popover > contents, popover > arrow, tooltip, tooltip > contents,
        notebook > header tab, list row, scale slider, scale trough, scale highlight, scrollbar slider, switch, switch slider, textview, scrolledwindow, frame, .frame, window.csd, decoration, .card, headerbar, entry > text, searchbar, listview > row, treeview {{ border-radius: 0; }}
        decoration {{ box-shadow: 0 0 0 1px alpha(currentColor, 0.28); margin: 0; }}
        window.csd {{ box-shadow: none; }}
        headerbar {{ min-height: 34px; padding: 0 6px; }}
        .main-menu {{ padding: 0; }}
        .main-menu > item {{ padding: 4px 10px; border-radius: 0; border: none; }}
        headerbar .dialog-title, .panel-tab, .heading, .layers-panel .heading {{ text-transform: uppercase; letter-spacing: 1.4px; font-size: 10.5px; font-weight: 600; }}
        headerbar .title {{ letter-spacing: 0; font-weight: 600; }}
        .panel-tab.current {{ border-bottom: 1px solid currentColor; }}
        button {{ background-image: none; box-shadow: none; text-shadow: none; padding: 3px 10px; min-height: 22px; border: 1px solid alpha(currentColor, 0.22); }}
        button:hover {{ border-color: alpha(currentColor, 0.5); }}
        button:active, button:checked {{ box-shadow: none; }}
        button.flat, button.tool, .layers-footer button, .layers-panel row button, notebook > header tab button, spinbutton button, button.swatch, menubutton > button.flat {{ border: none; }}
        button.tool {{ padding: 3px; min-width: 0; min-height: 0; }}
        button.tool.mark {{ padding: 1px; }}
        button.swatch {{ padding: 0; min-width: 0; min-height: 0; }}
        .layers-footer button, .layers-panel row button {{ padding: 3px 5px; min-width: 0; min-height: 0; }}
        spinbutton button, notebook > header tab button, menubutton > button.flat {{ min-height: 0; }}
        button.suggested-action {{ border: 1px solid transparent; }}
        button.suggested-action label {{ font-weight: 600; }}
        entry, spinbutton, dropdown > button {{ min-height: 24px; padding: 0 6px; font-size: 11.5px; }}
        .options label {{ font-size: 11.5px; }}
        spinbutton {{ padding: 0; }}
        spinbutton text {{ padding: 1px 6px; min-height: 22px; border: none; background: none; box-shadow: none; }}
        spinbutton button {{ padding: 0 5px; min-width: 16px; border: none; border-left: 1px solid alpha(currentColor, 0.15); background-image: none; }}
        scale {{ min-height: 16px; }}
        scale trough {{ min-height: 2px; }}
        scale highlight {{ min-height: 2px; }}
        scale slider {{ min-width: 8px; min-height: 16px; margin: -7px; box-shadow: none; }}
        check, radio {{ min-width: 13px; min-height: 13px; -gtk-icon-size: 11px; }}
        popover > contents {{ box-shadow: none; padding: 4px; }}
        popover.menu modelbutton {{ min-height: 24px; padding: 2px 10px; }}
        tooltip {{ box-shadow: none; padding: 4px 6px; }}
        notebook > header {{ padding: 0; }}
        notebook > header tab {{ min-height: 24px; padding: 2px 10px; }}
        scrollbar {{ background: transparent; }}
        scrollbar slider {{ min-width: 4px; min-height: 4px; margin: 2px; }}
        .canvas-status {{ font-size: 11px; }}
        .dim-label, label.caption {{ font-size: 11px; }}
        .layers-panel row label.caption {{ font-size: 10px; letter-spacing: 0.3px; }}
        .monospace {{ letter-spacing: 0; }}
        paned > separator {{ min-width: 1px; min-height: 1px; }}
        .layers-panel list.navigation-sidebar > row {{ border-bottom: 1px solid alpha(currentColor, 0.12); border-radius: 0; margin: 0; }}
    "#)
}

thread_local! {
    static LOOK: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
}

/// Installs the shared look once; the palette provider sits above it.
fn install_look(display: &gdk::Display) {
    LOOK.with(|slot| {
        if slot.borrow().is_some() { return; }
        // Light hinting keeps the tops of small capitals round instead of snapping them flat, which
        // read as clipped in a monospace face at 12 px.
        if let Some(settings) = gtk::Settings::default() {
            settings.set_gtk_xft_hintstyle(Some("hintslight"));
            settings.set_gtk_xft_antialias(1);
        }
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&look_css(&system_font()));
        gtk::style_context_add_provider_for_display(display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        *slot.borrow_mut() = Some(provider);
    });
}

/// Applies the palette (or clears it when `None`) to every window on the default display.
fn apply(palette: Option<Palette>) {
    let Some(display) = gdk::Display::default() else { return };
    install_look(&display);
    PROVIDER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(old) = slot.take() { gtk::style_context_remove_provider_for_display(&display, &old); }
        if let Some(p) = &palette {
            let provider = gtk::CssProvider::new();
            provider.load_from_string(&css(p));
            gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);
            *slot = Some(provider);
        }
    });
    if let Some(p) = &palette {
        if let Some(settings) = gtk::Settings::default() { settings.set_gtk_application_prefer_dark_theme(p.dark); }
    }
    CURRENT.with(|c| *c.borrow_mut() = palette);
}

fn reload(on_change: &dyn Fn()) {
    let palette = std::fs::read_to_string(colors_path()).ok().and_then(|t| parse(&t));
    if palette == current() { return; }
    apply(palette);
    on_change();
}

/// Reads the theme now and keeps following it. `on_change` runs after every re-skin (canvases redraw).
pub fn start(on_change: std::rc::Rc<dyn Fn()>) {
    reload(&*on_change);
    // Omarchy replaces the whole theme directory, so watch the directory above it as well as the file.
    let path = colors_path();
    let targets = [gio::File::for_path(&path), gio::File::for_path(path.parent().unwrap_or(&path)), gio::File::for_path(path.parent().and_then(|p| p.parent()).unwrap_or(&path))];
    let pending = std::rc::Rc::new(std::cell::Cell::new(false));
    for target in targets {
        let Ok(monitor) = target.monitor(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) else { continue };
        let (on_change, pending) = (on_change.clone(), pending.clone());
        monitor.connect_changed(move |_, _, _, _| {
            // Several events arrive per theme switch; one reload after they settle.
            if pending.replace(true) { return; }
            let (on_change, pending) = (on_change.clone(), pending.clone());
            glib::timeout_add_local_once(Duration::from_millis(250), move || { pending.set(false); reload(&*on_change); });
        });
        // The monitor lives as long as the app.
        std::mem::forget(monitor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_omarchy_colors() {
        let p = parse("mode = \"dark\"\naccent = \"#2ad4f0\"\nbackground = \"#08131c\"\nforeground = \"#c2d8e6\"\n# comment\nselection = \"#173247\"\n").unwrap();
        assert!(p.dark);
        assert_eq!(p.accent_hex, "#2ad4f0");
        assert_eq!(p.selection, "#173247");
        assert_eq!(p.dark_background, "#08131c", "missing shades fall back to the background");
        assert!(parse("nonsense").is_none());
        assert!(parse("background = \"red\"\nforeground = \"#ffffff\"").is_none(), "only hex colors");
        assert!(css(&p).contains("#2ad4f0"));
    }
}
