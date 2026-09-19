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
        headerbar {{ background-color: {dbg}; color: {fg}; box-shadow: none; border-bottom: 1px solid {ddbg}; }}
        headerbar button, .options button, .layers-footer button {{ background-color: {lbg}; color: {fg}; border: 1px solid {ddbg}; box-shadow: none; text-shadow: none; }}
        headerbar button:hover, .options button:hover, .layers-footer button:hover {{ background-color: {sel}; }}
        button.suggested-action {{ background-color: {accent}; color: {on_accent}; border-color: {accent}; }}
        notebook > header {{ background-color: {dbg}; border-color: {ddbg}; }}
        notebook > header tab {{ color: {dfg}; }}
        notebook > header tab:checked {{ color: {bfg}; box-shadow: inset 0 -2px {accent}; }}
        notebook > header tab button {{ background: none; border: none; color: {dfg}; }}
        .tool-rail {{ background-color: {dbg}; border-right: 1px solid {ddbg}; }}
        button.tool {{ background-color: transparent; color: {fg}; border: none; box-shadow: none; }}
        button.tool:hover {{ background-color: {sel}; }}
        button.tool:checked {{ background-color: {accent}; color: {on_accent}; }}
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
        entry, spinbutton, spinbutton text, dropdown > button {{ background-color: {dbg}; color: {fg}; border: 1px solid {lbg}; box-shadow: none; }}
        spinbutton button {{ background-color: {lbg}; color: {fg}; border: none; }}
        entry:focus-within, spinbutton:focus-within {{ border-color: {accent}; outline-color: {accent}; }}
        scale trough {{ background-color: {lbg}; }}
        scale highlight {{ background-color: {accent}; }}
        scale slider {{ background-color: {bfg}; border: 1px solid {muted}; box-shadow: none; }}
        check, radio {{ background-color: {dbg}; border: 1px solid {muted}; color: {on_accent}; }}
        check:checked, radio:checked {{ background-color: {accent}; border-color: {accent}; color: {on_accent}; }}
        popover > contents, popover.menu > contents {{ background-color: {lbg}; color: {fg}; border: 1px solid {ddbg}; }}
        popover.menu modelbutton:hover, popover listview > row:hover {{ background-color: {sel}; }}
        popover listview > row:selected {{ background-color: {accent}; color: {on_accent}; }}
        paned > separator {{ background-color: {ddbg}; }}
        tooltip {{ background-color: {ddbg}; color: {fg}; border: 1px solid {lbg}; }}
        *:focus-visible {{ outline-color: {accent}; }}
        selection {{ background-color: {accent}; color: {on_accent}; }}
    ")
}

/// Applies the palette (or clears it when `None`) to every window on the default display.
fn apply(palette: Option<Palette>) {
    let Some(display) = gdk::Display::default() else { return };
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
