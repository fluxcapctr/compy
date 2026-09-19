//! Type layers: text set in a font with Pango, rendered to the layer's pixels and kept as a record on the
//! layer so it can be edited again, plus Google Fonts fetched by family name into the user's fonts.

use anyhow::{Context as _, Result, bail};
use cairo::ImageSurface;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use pangocairo::pango;
use pangocairo::prelude::*;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    pub text: String,
    pub family: String,
    /// Font size in document pixels.
    pub size: f64,
    pub bold: bool,
    pub italic: bool,
    pub color: [f64; 3],
    /// 0 left, 1 center, 2 right.
    pub align: u32,
    /// Line height as a multiple of the size.
    pub leading: f64,
    /// Letter spacing in pixels.
    pub tracking: f64,
}

impl Default for TextStyle {
    fn default() -> Self { TextStyle { text: "Type here".into(), family: "Sans".into(), size: 48.0, bold: false, italic: false, color: [0.0; 3], align: 0, leading: 1.2, tracking: 0.0 } }
}

impl TextStyle {
    pub fn to_record(&self) -> serde_json::Value { serde_json::to_value(self).unwrap_or(serde_json::Value::Null) }
    pub fn from_record(value: &serde_json::Value) -> Option<TextStyle> { serde_json::from_value(value.clone()).ok() }
    fn description(&self) -> pango::FontDescription {
        let mut d = pango::FontDescription::new();
        d.set_family(&self.family);
        d.set_weight(if self.bold { pango::Weight::Bold } else { pango::Weight::Normal });
        d.set_style(if self.italic { pango::Style::Italic } else { pango::Style::Normal });
        d.set_absolute_size(self.size.max(1.0) * pango::SCALE as f64);
        d
    }
}

/// The families the system knows, sorted.
pub fn families() -> Vec<String> {
    let map = pangocairo::FontMap::default();
    let mut names: Vec<String> = map.list_families().iter().map(|f| f.name().to_string()).collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names
}

fn layout_for(style: &TextStyle) -> pango::Layout {
    let map = pangocairo::FontMap::default();
    let context = map.create_context();
    let layout = pango::Layout::new(&context);
    layout.set_font_description(Some(&style.description()));
    layout.set_text(&style.text);
    layout.set_alignment(match style.align { 1 => pango::Alignment::Center, 2 => pango::Alignment::Right, _ => pango::Alignment::Left });
    let attrs = pango::AttrList::new();
    attrs.insert(pango::AttrInt::new_letter_spacing((style.tracking * pango::SCALE as f64) as i32));
    layout.set_attributes(Some(&attrs));
    layout.set_line_spacing(style.leading as f32);
    layout
}

/// Where the layout's origin sits on the rendered surface: one pixel in from the ink or logical box,
/// whichever reaches further, so glyph overhangs are kept.
fn origin_offset(layout: &pango::Layout) -> (f64, f64) {
    let (ink, logical) = layout.pixel_extents();
    (-(ink.x().min(logical.x()) as f64) + 1.0, -(ink.y().min(logical.y()) as f64) + 1.0)
}

/// The text rendered on a transparent surface just its size, and where the ink's top-left sits relative to
/// the layout's origin (so the layer lands where the click was). Empty text renders one line's worth of
/// nothing, so a type layer being typed into has a size before the first letter.
pub fn render(style: &TextStyle) -> Result<(ImageSurface, i32, i32)> {
    let layout = layout_for(style);
    if style.text.is_empty() { layout.set_text(" "); }
    let (ink, logical) = layout.pixel_extents();
    let (w, h) = ((ink.width().max(logical.width()) + 2).max(1), (ink.height().max(logical.height()) + 2).max(1));
    if w > 30_000 || h > 30_000 || w as i64 * h as i64 > 100_000_000 { bail!("The text is larger than the 30,000-pixel side or 100-megapixel limit."); }
    let surface = crate::raster::new_argb(w, h)?;
    if !style.text.is_empty() {
        let cr = cairo::Context::new(&surface)?;
        let (ox, oy) = origin_offset(&layout);
        cr.translate(ox, oy);
        cr.set_source_rgb(style.color[0], style.color[1], style.color[2]);
        pangocairo::functions::show_layout(&cr, &layout);
    }
    Ok((surface, ink.x().min(logical.x()) - 1, ink.y().min(logical.y()) - 1))
}

/// The caret before byte `index` of the text: its left, top and height on the rendered surface.
pub fn caret(style: &TextStyle, index: usize) -> (f64, f64, f64) {
    let layout = layout_for(style);
    let empty = style.text.is_empty();
    if empty { layout.set_text(" "); }
    let (ox, oy) = origin_offset(&layout);
    let index = if empty { 0 } else { index.min(style.text.len()) as i32 };
    let pos = layout.index_to_pos(index as i32);
    let scale = pango::SCALE as f64;
    (pos.x() as f64 / scale + ox, pos.y() as f64 / scale + oy, (pos.height() as f64 / scale).max(1.0))
}

/// The byte index of the caret nearest a point on the rendered surface (after the character when the
/// point is past its middle).
pub fn index_at(style: &TextStyle, x: f64, y: f64) -> usize {
    if style.text.is_empty() { return 0; }
    let layout = layout_for(style);
    let (ox, oy) = origin_offset(&layout);
    let scale = pango::SCALE as f64;
    let (_, index, trailing) = layout.xy_to_index(((x - ox) * scale) as i32, ((y - oy) * scale) as i32);
    let mut i = index.max(0) as usize;
    for _ in 0..trailing.max(0) { if let Some(c) = style.text[i..].chars().next() { i += c.len_utf8(); } }
    i.min(style.text.len())
}

fn fonts_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".local/share"));
    base.join("fonts/compositor-google")
}

/// Fetches a Google Fonts family (regular, bold, italic and bold italic where they exist) into the user's
/// fonts and refreshes the font cache. Returns the files fetched.
pub fn fetch_google_family(family: &str) -> Result<usize> {
    let family = family.trim();
    if family.is_empty() { bail!("Type a family name, as it appears on fonts.google.com."); }
    let query = family.replace(' ', "+");
    let url = format!("https://fonts.googleapis.com/css2?family={query}:ital,wght@0,400;0,700;1,400;1,700&display=swap");
    // An old browser identity makes the service list TrueType files rather than WOFF2.
    let mut response = ureq::get(&url).header("User-Agent", "Mozilla/4.0").call().map_err(|e| match e { ureq::Error::StatusCode(400) => anyhow::anyhow!("Google Fonts has no family called \"{family}\"."), other => anyhow::anyhow!("fetching the font list: {other}") })?;
    let css = response.body_mut().read_to_string().context("reading the font list")?;
    let dir = fonts_dir().join(family.replace('/', "-"));
    std::fs::create_dir_all(&dir)?;
    let mut count = 0;
    let mut rest = css.as_str();
    let mut seen = std::collections::HashSet::new();
    while let Some(start) = rest.find("url(") {
        let after = &rest[start + 4..];
        let Some(end) = after.find(')') else { break };
        let file_url = after[..end].trim_matches(|c| c == '"' || c == '\'');
        rest = &after[end + 1..];
        if !file_url.ends_with(".ttf") || !seen.insert(file_url.to_string()) { continue; }
        let name = file_url.rsplit('/').next().unwrap_or("font.ttf");
        let mut r = ureq::get(file_url).call().map_err(|e| anyhow::anyhow!("downloading {name}: {e}"))?;
        let bytes = r.body_mut().with_config().limit(64 << 20).read_to_vec().context("downloading a font file")?;
        std::fs::write(dir.join(name), bytes)?;
        count += 1;
    }
    if count == 0 { bail!("Google Fonts listed no TrueType files for \"{family}\"."); }
    let _ = std::process::Command::new("fc-cache").arg("-f").arg(fonts_dir()).status();
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders_text_and_round_trips_the_record() {
        let style = TextStyle { text: "Hi".into(), size: 40.0, color: [1.0, 0.0, 0.0], ..Default::default() };
        let (surface, _, _) = render(&style).unwrap();
        assert!(surface.width() > 20 && surface.height() > 20);
        let painted = crate::raster::with_bytes(&surface, |d, _| d.chunks(4).filter(|p| p[3] > 0).count()).unwrap();
        assert!(painted > 100, "{painted} pixels painted");
        let back = TextStyle::from_record(&style.to_record()).unwrap();
        assert_eq!(back, style);
        let (empty, _, _) = render(&TextStyle { text: String::new(), ..Default::default() }).unwrap();
        assert!(empty.height() > 10, "empty text still has a line's height");
        let (x0, _, h) = caret(&style, 0);
        let (x1, _, _) = caret(&style, 2);
        assert!(x1 > x0 + 10.0 && h > 20.0, "caret moves along the text: {x0} {x1} {h}");
        assert_eq!(index_at(&style, x0 + 1.0, h / 2.0), 0);
        assert_eq!(index_at(&style, x1 + 5.0, h / 2.0), 2);
        assert!(!families().is_empty());
    }
}
