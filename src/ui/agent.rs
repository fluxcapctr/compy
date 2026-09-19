//! The agent inside the app: a socket that answers tool calls against the open document on the GTK
//! thread, and the assistant panel, a small chat that drives Claude Code with the document's state and a
//! snapshot attached to every message, so "this" means what is selected.

use super::{App, Doc};
use crate::genfill::Backend as _;
use crate::agent::{self, Request, Response, parse_color};
use crate::document::Document;
use anyhow::{Result, bail};
use gtk::prelude::*;
use gtk::glib;
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader, Write};
use std::rc::Rc;
use std::sync::mpsc;

/// A tool call waiting for the GTK thread, with where to send its answer.
struct Pending { request: Request, reply: mpsc::Sender<Response> }

/// A long job (generation) running on a thread; the socket thread polls until it is done.
struct Job { status: std::sync::Arc<std::sync::Mutex<(String, Option<Result<Value, String>>)>> }

thread_local! {
    static JOBS: RefCell<std::collections::HashMap<u64, Job>> = RefCell::new(std::collections::HashMap::new());
    static NEXT_JOB: Cell<u64> = const { Cell::new(1) };
}

/// Starts listening. Requests arrive on a thread and are answered on the GTK thread in turn.
pub fn serve(app: Rc<App>) {
    let path = agent::socket_path();
    let _ = std::fs::remove_file(&path);
    let listener = match std::os::unix::net::UnixListener::bind(&path) { Ok(l) => l, Err(e) => { eprintln!("agent: cannot listen on {}: {e}", path.display()); return; } };
    let (tx, rx) = mpsc::channel::<Pending>();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().expect("socket clone"));
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
                    let response = match serde_json::from_str::<Request>(line.trim()) {
                        Ok(request) => {
                            let id = request.id;
                            let (reply_tx, reply_rx) = mpsc::channel();
                            if tx.send(Pending { request, reply: reply_tx }).is_err() { break; }
                            let mut response = reply_rx.recv().unwrap_or(Response { id, ok: false, result: json!("the app went away") });
                            // A job id means the work continues on a thread: wait for it here, off the GTK thread.
                            while response.ok && response.result.get("job").is_some() {
                                std::thread::sleep(std::time::Duration::from_millis(300));
                                let job = response.result["job"].as_u64().unwrap_or(0);
                                let (poll_tx, poll_rx) = mpsc::channel();
                                if tx.send(Pending { request: Request { id, tool: "_poll".into(), args: json!({"job": job}) }, reply: poll_tx }).is_err() { break; }
                                response = poll_rx.recv().unwrap_or(Response { id, ok: false, result: json!("the app went away") });
                            }
                            response
                        }
                        Err(e) => Response { id: 0, ok: false, result: json!(format!("bad request: {e}")) },
                    };
                    let mut s = stream.try_clone().expect("socket clone");
                    let _ = writeln!(s, "{}", serde_json::to_string(&response).unwrap_or_default());
                    line.clear();
                }
            });
        }
    });
    glib::timeout_add_local(std::time::Duration::from_millis(30), move || {
        while let Ok(pending) = rx.try_recv() {
            let id = pending.request.id;
            let response = match app.agent_tool(&pending.request.tool, &pending.request.args) {
                Ok(result) => Response { id, ok: true, result },
                Err(e) => Response { id, ok: false, result: json!(format!("{e:#}")) },
            };
            let _ = pending.reply.send(response);
        }
        glib::ControlFlow::Continue
    });
}

fn num(args: &Value, key: &str) -> Option<f64> { args.get(key).and_then(Value::as_f64) }
fn text(args: &Value, key: &str) -> Option<String> { args.get(key).and_then(Value::as_str).map(str::to_string) }
fn flag(args: &Value, key: &str) -> Option<bool> { args.get(key).and_then(Value::as_bool) }

fn blend_from(name: &str) -> Option<crate::format::BlendMode> {
    use crate::format::BlendMode::*;
    Some(match name.to_lowercase().replace(' ', "").as_str() {
        "normal" => Normal, "multiply" => Multiply, "screen" => Screen, "overlay" => Overlay, "darken" => Darken, "lighten" => Lighten, "difference" => Difference,
        "colordodge" => ColorDodge, "colorburn" => ColorBurn, "hue" => Hue, "saturation" => Saturation, "color" => Color, "luminosity" => Luminosity, _ => return None,
    })
}

/// The document's state as the model reads it.
fn state_of(doc: &Doc) -> Value {
    let d = &doc.document;
    let layers: Vec<Value> = crate::format::entries_ordered(d.renderer.layers(), true).into_iter().map(|e| {
        let l = e.layer;
        let kind = if l.is_group() { "folder" } else if l.adjustment.is_some() { "adjustment" } else if l.text.is_some() { "text" } else if l.shape.is_some() { "shape" } else { "pixels" };
        let t = l.transform;
        json!({
            "id": crate::format::upper(l.id), "name": l.name, "kind": kind, "depth": e.depth, "visible": l.is_visible, "shown": e.visible,
            "opacity": l.opacity(), "blend": l.blend_mode().name(), "active": d.active == Some(l.id), "selected": d.selected.contains(&l.id),
            "x": t.origin.0, "y": t.origin.1, "width": t.size.0, "height": t.size.1, "rotation": t.rotation,
            "mask": l.mask_file.is_some(), "clipped_to_below": l.mask_source_id.is_some(), "effects": l.effects.is_some(),
            "text": l.text.as_ref().and_then(crate::text::TextStyle::from_record).map(|s| s.text),
        })
    }).collect();
    let selection = d.selection.as_ref().and_then(|s| s.bounds).map(|(x0, y0, x1, y1)| json!({"x": x0, "y": y0, "width": x1 - x0, "height": y1 - y0}));
    json!({
        "title": doc.title, "width": d.width(), "height": d.height(), "resolution": d.renderer.resolution(), "modified": d.is_modified(),
        "tool": format!("{:?}", doc.tool), "zoom_percent": (doc.viewport.zoom() * 100.0).round(),
        "foreground_color": format!("#{:02x}{:02x}{:02x}", (doc.brush.color[0] * 255.0) as u8, (doc.brush.color[1] * 255.0) as u8, (doc.brush.color[2] * 255.0) as u8),
        "selection": selection, "layers": layers,
    })
}

/// The canvas composite, at most `max` pixels on the long side, as PNG bytes; the selection outlined in red.
fn snapshot_png(doc: &mut Doc, max: i32, outline: bool) -> Result<Vec<u8>> {
    let flat = doc.document.renderer.render_flat()?;
    let (w, h) = (flat.width(), flat.height());
    let scale = (max as f64 / w.max(h).max(1) as f64).min(1.0);
    let (sw, sh) = (((w as f64 * scale).round() as i32).max(1), ((h as f64 * scale).round() as i32).max(1));
    let out = crate::raster::new_argb(sw, sh)?;
    {
        let cr = cairo::Context::new(&out)?;
        // A checkerboard behind transparency, so the model sees what the user sees.
        cr.set_source_rgb(0.30, 0.30, 0.30);
        cr.paint()?;
        cr.set_source_rgb(0.35, 0.35, 0.35);
        let tile = 8.0;
        for row in 0..(sh as f64 / tile).ceil() as i32 { for col in 0..(sw as f64 / tile).ceil() as i32 { if (row + col) % 2 == 0 { cr.rectangle(col as f64 * tile, row as f64 * tile, tile, tile); } } }
        cr.fill()?;
        cr.scale(scale, scale);
        cr.set_source_surface(&flat, 0.0, 0.0)?;
        cr.paint()?;
        if outline {
            if let Some(sel) = &doc.document.selection {
                cr.set_source_rgb(1.0, 0.1, 0.1);
                cr.set_line_width(2.0 / scale);
                for loop_ in sel.outline.iter() {
                    for (i, p) in loop_.iter().enumerate() { if i == 0 { cr.move_to(p.0 as f64, p.1 as f64); } else { cr.line_to(p.0 as f64, p.1 as f64); } }
                    cr.close_path();
                }
                cr.stroke()?;
            }
        }
    }
    crate::png_io::png_bytes(&out)
}

impl App {
    /// Runs one tool against the current document. Errors are messages for the model.
    pub fn agent_tool(self: &Rc<Self>, tool: &str, args: &Value) -> Result<Value> {
        if tool == "_poll" {
            let job = args.get("job").and_then(Value::as_u64).unwrap_or(0);
            let outcome = JOBS.with(|jobs| jobs.borrow().get(&job).and_then(|j| j.status.lock().ok().map(|s| (s.0.clone(), s.1.clone()))));
            return match outcome {
                None => bail!("no such job"),
                Some((_, None)) => Ok(json!({"job": job})),
                Some((_, Some(result))) => { JOBS.with(|jobs| jobs.borrow_mut().remove(&job)); result.map_err(|e| anyhow::anyhow!("{e}")) }
            };
        }
        match tool {
            "open" => { let path = text(args, "path").ok_or_else(|| anyhow::anyhow!("path needed"))?; self.open_path(std::path::Path::new(&path)); return Ok(json!("opened")); }
            "new_document" => { let (w, h) = (num(args, "width").unwrap_or(1920.0) as i32, num(args, "height").unwrap_or(1080.0) as i32); let document = Document::blank(w, h, 72.0)?; self.add_page(Doc::from(document, "Untitled")); return Ok(json!("created")); }
            _ => {}
        }
        if self.notebook.current_page().is_none() { bail!("Nothing is open. Use open or new_document first."); }
        let mut outcome: Result<Value> = Ok(Value::Null);
        let mut refresh = true;
        self.with_current(|p| {
            let doc = p.canvas.doc();
            let r: Result<Value> = (|| {
                let mut d = doc.borrow_mut();
                let dd = &mut d.document;
                Ok(match tool {
                    "state" => { refresh = false; state_of(&d) }
                    "snapshot" => { refresh = false; let png = snapshot_png(&mut d, 1024, flag(args, "selection_outline").unwrap_or(true))?; json!({"text": "The canvas now.", "png_base64": crate::genfill::base64_encode(&png)}) }
                    "select_rectangle" => { let (x, y, w, h) = (num(args, "x").unwrap_or(0.0), num(args, "y").unwrap_or(0.0), num(args, "width").unwrap_or(1.0), num(args, "height").unwrap_or(1.0)); dd.select_box(x, y, w, h, flag(args, "ellipse").unwrap_or(false), crate::selection::Mode::Replace, true)?; json!("selected") }
                    "select_all" => { dd.select_all()?; json!("selected") }
                    "deselect" => { dd.deselect(); json!("deselected") }
                    "invert_selection" => { dd.invert_selection()?; json!("inverted") }
                    "select_layer_pixels" => { let id = dd.active.ok_or_else(|| anyhow::anyhow!("no active layer"))?; dd.select_layer_pixels(id, crate::selection::Mode::Replace)?; json!("selected") }
                    "feather_selection" => { dd.feather_selection(num(args, "radius").unwrap_or(4.0))?; json!("feathered") }
                    "select_layer" => {
                        let key = text(args, "layer").unwrap_or_default();
                        let id = dd.renderer.layers().iter().find(|l| crate::format::upper(l.id).eq_ignore_ascii_case(&key) || l.id.to_string().eq_ignore_ascii_case(&key) || l.name == key).map(|l| l.id).ok_or_else(|| anyhow::anyhow!("no layer called {key}"))?;
                        dd.select_layer(Some(id)); json!("active")
                    }
                    "new_layer" => { let id = dd.add_blank_layer(); if let Some(n) = text(args, "name") { dd.rename_layer(id, &n); } json!({"id": crate::format::upper(id)}) }
                    "duplicate_layer" => { if dd.selection.as_ref().is_some_and(|s| !s.is_empty()) { dd.layer_via(false)?; } else { dd.duplicate_layer(); } json!("duplicated") }
                    "delete_layer" => { dd.delete_layer(); json!("deleted") }
                    "rename_layer" => { let id = dd.active.ok_or_else(|| anyhow::anyhow!("no active layer"))?; dd.rename_layer(id, &text(args, "name").unwrap_or_default()); json!("renamed") }
                    "set_layer" => {
                        let id = dd.active.ok_or_else(|| anyhow::anyhow!("no active layer"))?;
                        if let Some(v) = flag(args, "visible") { dd.set_visible(id, v); }
                        if let Some(o) = num(args, "opacity") { dd.set_opacity(id, o.clamp(0.0, 1.0)); }
                        if let Some(b) = text(args, "blend") { let mode = blend_from(&b).ok_or_else(|| anyhow::anyhow!("unknown blend mode {b}"))?; dd.set_blend_mode(id, mode); }
                        json!("set")
                    }
                    "place_layer" => {
                        let id = dd.active.ok_or_else(|| anyhow::anyhow!("no active layer"))?;
                        let mut t = dd.renderer.layer(id).transform;
                        if let Some(x) = num(args, "x") { t.origin.0 = x; }
                        if let Some(y) = num(args, "y") { t.origin.1 = y; }
                        if let Some(w) = num(args, "width") { t.size.0 = w.max(1.0); }
                        if let Some(h) = num(args, "height") { t.size.1 = h.max(1.0); }
                        if let Some(r) = num(args, "rotation") { t.rotation = r; }
                        dd.set_transform(id, t, "Transform Layer"); json!("placed")
                    }
                    "reorder_layer" => { match text(args, "direction").unwrap_or_default().as_str() { "up" => dd.move_layer(true), "down" => dd.move_layer(false), "top" => dd.move_layer_to_end(true), "bottom" => dd.move_layer_to_end(false), other => bail!("unknown direction {other}") } json!("moved") }
                    "merge_visible" => { dd.merge_visible()?; json!("merged") }
                    "stamp_visible" => { dd.stamp_visible()?; json!("stamped") }
                    "fill" => { let c = parse_color(&text(args, "color").unwrap_or_default()).ok_or_else(|| anyhow::anyhow!("color must look like #rrggbb"))?; dd.fill(c)?; json!("filled") }
                    "clear" => { dd.clear_selection(false)?; json!("cleared") }
                    "remove_background" => { dd.remove_background(&crate::matte::MatteSettings::default(), true)?; json!("background removed") }
                    "invert" => { dd.invert()?; json!("inverted") }
                    "desaturate" => { dd.desaturate()?; json!("desaturated") }
                    "auto_levels" => { let mode = match text(args, "mode").unwrap_or_default().as_str() { "contrast" => crate::document::AutoLevels::Contrast, "color" => crate::document::AutoLevels::Color, _ => crate::document::AutoLevels::Tone }; dd.auto_levels(mode)?; json!("done") }
                    "filter" => {
                        use crate::filters::{Kind, Settings};
                        let mut s = Settings::default();
                        let kind = match text(args, "kind").unwrap_or_default().as_str() {
                            "gaussian_blur" => { s.radius = num(args, "radius").unwrap_or(4.0); Kind::GaussianBlur }
                            "motion_blur" => { s.angle = num(args, "angle").unwrap_or(0.0); s.distance = num(args, "distance").unwrap_or(20.0); Kind::MotionBlur }
                            "add_noise" => { s.amount = num(args, "amount").unwrap_or(10.0); Kind::AddNoise }
                            "levels" => { if let Some(b) = num(args, "black") { s.levels.ranges[0].black = b; } if let Some(w) = num(args, "white") { s.levels.ranges[0].white = w; } if let Some(g) = num(args, "gamma") { s.levels.ranges[0].gamma = g; } Kind::Levels }
                            "exposure" => { s.exposure.exposure = num(args, "exposure").unwrap_or(0.5); Kind::Exposure }
                            "hue_saturation" => { s.hue_saturation.adjustments = vec![("Master".into(), [num(args, "hue").unwrap_or(0.0), num(args, "saturation").unwrap_or(0.0), num(args, "lightness").unwrap_or(0.0)])]; Kind::HueSaturation }
                            "color_balance" => { let arr = |k: &str| args.get(k).and_then(Value::as_array).map(|a| [a.first().and_then(Value::as_f64).unwrap_or(0.0), a.get(1).and_then(Value::as_f64).unwrap_or(0.0), a.get(2).and_then(Value::as_f64).unwrap_or(0.0)]); if let Some(v) = arr("shadows") { s.balance.shadows = v; } if let Some(v) = arr("midtones") { s.balance.midtones = v; } if let Some(v) = arr("highlights") { s.balance.highlights = v; } Kind::ColorBalance }
                            other => bail!("unknown filter {other}"),
                        };
                        dd.apply_filter(kind, &s)?; json!("applied")
                    }
                    "adjustment_layer" => {
                        let kind = text(args, "kind").unwrap_or_default();
                        let name = match kind.as_str() { "levels" => "Levels", "exposure" => "Exposure", "hue_saturation" => "Hue/Saturation", "gradient_map" => "Gradient Map", "curves" => "Curves", other => bail!("unknown adjustment {other}") };
                        let id = dd.add_adjustment(name).ok_or_else(|| anyhow::anyhow!("could not add {name}"))?;
                        if let Some(mut a) = dd.adjustment(id) {
                            match &mut a {
                                crate::filters::Adjustment::Levels(l) => { if let Some(b) = num(args, "black") { l.ranges[0].black = b; } if let Some(w) = num(args, "white") { l.ranges[0].white = w; } if let Some(g) = num(args, "gamma") { l.ranges[0].gamma = g; } }
                                crate::filters::Adjustment::Exposure(e) => { if let Some(v) = num(args, "exposure") { e.exposure = v; } }
                                crate::filters::Adjustment::HueSaturation(h) => { h.adjustments = vec![("Master".into(), [num(args, "hue").unwrap_or(0.0), num(args, "saturation").unwrap_or(0.0), num(args, "lightness").unwrap_or(0.0)])]; }
                                _ => {}
                            }
                            dd.set_adjustment(id, &a, true);
                        }
                        if flag(args, "clip").unwrap_or(false) { dd.toggle_clipping(id); }
                        json!({"id": crate::format::upper(id)})
                    }
                    "text_layer" | "set_text" => {
                        let mut style = if tool == "set_text" { let id = dd.active.ok_or_else(|| anyhow::anyhow!("no active layer"))?; dd.text_style(id).ok_or_else(|| anyhow::anyhow!("the active layer is not a type layer"))? } else { let mut s = d.text_style.clone(); s.color = d.brush.color; s };
                        if let Some(t) = text(args, "text") { style.text = t; }
                        if let Some(v) = num(args, "size") { style.size = v; }
                        if let Some(f) = text(args, "family") { style.family = f; }
                        if let Some(c) = text(args, "color").and_then(|c| parse_color(&c)) { style.color = c; }
                        if let Some(b) = flag(args, "bold") { style.bold = b; }
                        if let Some(i) = flag(args, "italic") { style.italic = i; }
                        if let Some(a) = text(args, "align") { style.align = match a.as_str() { "center" => 1, "right" => 2, _ => 0 }; }
                        let dd = &mut d.document;
                        if tool == "set_text" { let id = dd.active.unwrap(); dd.set_text(id, &style)?; json!("text set") } else { let id = dd.add_text_layer(&style, num(args, "x").unwrap_or(40.0), num(args, "y").unwrap_or(40.0))?; json!({"id": crate::format::upper(id)}) }
                    }
                    "shape_layer" => { let ellipse = text(args, "kind").unwrap_or_default() == "ellipse"; let color = text(args, "color").and_then(|c| parse_color(&c)).unwrap_or(d.brush.color); let dd = &mut d.document; let id = dd.add_shape_layer(ellipse, (num(args, "x").unwrap_or(0.0), num(args, "y").unwrap_or(0.0), num(args, "width").unwrap_or(100.0), num(args, "height").unwrap_or(100.0)), color, num(args, "corner_radius").unwrap_or(0.0))?; json!({"id": crate::format::upper(id)}) }
                    "layer_style" => {
                        let id = dd.active.ok_or_else(|| anyhow::anyhow!("no active layer"))?;
                        let mut e = crate::effects::Effects::default();
                        let color = |v: &Value, key: &str, fallback: [f64; 3]| v.get(key).and_then(Value::as_str).and_then(parse_color).unwrap_or(fallback);
                        let f = |v: &Value, key: &str, fallback: f64| v.get(key).and_then(Value::as_f64).unwrap_or(fallback);
                        if let Some(v) = args.get("drop_shadow") { let mut s = crate::effects::Shadow::drop_default(); s.color = color(v, "color", s.color); s.opacity = f(v, "opacity", s.opacity); s.angle = f(v, "angle", s.angle); s.distance = f(v, "distance", s.distance); s.size = f(v, "size", s.size); e.drop_shadow = Some(s); }
                        if let Some(v) = args.get("inner_shadow") { let mut s = crate::effects::Shadow::inner_default(); s.color = color(v, "color", s.color); s.opacity = f(v, "opacity", s.opacity); s.angle = f(v, "angle", s.angle); s.distance = f(v, "distance", s.distance); s.size = f(v, "size", s.size); e.inner_shadow = Some(s); }
                        if let Some(v) = args.get("outer_glow") { let mut g = crate::effects::Glow::outer_default(); g.color = color(v, "color", g.color); g.opacity = f(v, "opacity", g.opacity); g.size = f(v, "size", g.size); e.outer_glow = Some(g); }
                        if let Some(v) = args.get("inner_glow") { let mut g = crate::effects::Glow::inner_default(); g.color = color(v, "color", g.color); g.opacity = f(v, "opacity", g.opacity); g.size = f(v, "size", g.size); e.inner_glow = Some(g); }
                        if let Some(v) = args.get("bevel") { let mut b = crate::effects::Bevel::default(); b.style = f(v, "style", 0.0) as u32; b.depth = f(v, "depth", b.depth); b.size = f(v, "size", b.size); b.angle = f(v, "angle", b.angle); b.altitude = f(v, "altitude", b.altitude); e.bevel = Some(b); }
                        if let Some(v) = args.get("stroke") { let mut s = crate::effects::Stroke::default(); s.size = f(v, "size", s.size); s.position = f(v, "position", 0.0) as u32; s.color = color(v, "color", s.color); s.opacity = f(v, "opacity", s.opacity); e.stroke = Some(s); }
                        if let Some(v) = args.get("color_overlay") { let mut o = crate::effects::Overlay::default(); o.color = color(v, "color", o.color); o.opacity = f(v, "opacity", o.opacity); e.color_overlay = Some(o); }
                        dd.set_effects(id, Some(&e))?; json!("styled")
                    }
                    "canvas_size" => { dd.canvas_size(num(args, "width").unwrap_or(0.0) as i32, num(args, "height").unwrap_or(0.0) as i32, num(args, "anchor").unwrap_or(4.0) as usize, None, None, "Canvas Size")?; json!("resized") }
                    "image_size" => { let res = dd.renderer.resolution(); dd.image_size(num(args, "width").unwrap_or(0.0) as i32, num(args, "height").unwrap_or(0.0) as i32, res, crate::format::Sampling::High)?; json!("resized") }
                    "flip" => { let horizontal = text(args, "axis").unwrap_or_default() != "vertical"; if flag(args, "canvas").unwrap_or(false) { dd.flip_canvas(horizontal)?; } else { dd.flip_layer(horizontal); } json!("flipped") }
                    "crop_to_selection" => { dd.crop_to_selection()?; json!("cropped") }
                    "export" => {
                        let path = std::path::PathBuf::from(text(args, "path").unwrap_or_default());
                        if path.as_os_str().is_empty() { bail!("path needed"); }
                        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg")) { dd.export_jpeg(&path, num(args, "quality").unwrap_or(90.0), [1.0; 3])?; } else { dd.export_png(&path)?; }
                        refresh = false; json!(format!("wrote {}", path.display()))
                    }
                    "save" => {
                        let path = text(args, "path").map(std::path::PathBuf::from).or_else(|| dd.path.clone()).ok_or_else(|| anyhow::anyhow!("the document has no path yet: give one ending in .comp"))?;
                        dd.save(&path)?; dd.path = Some(path.clone()); json!(format!("saved {}", path.display()))
                    }
                    "undo" => { dd.undo(); json!("undone") }
                    "redo" => { dd.redo(); json!("redone") }
                    "zoom" => { refresh = false; json!({"zoom": num(args, "percent")}) }
                    "generative_fill" | "generative_expand" => {
                        let Some(key) = crate::genfill::key() else { bail!("No fal.ai key is set. File > Generative Fill lets the user enter one.") };
                        let models = crate::genfill::models();
                        let model = models.first().cloned().ok_or_else(|| anyhow::anyhow!("no generation model configured"))?;
                        let prompt = text(args, "prompt").unwrap_or_default();
                        if tool == "generative_expand" {
                            let (l, r, t, b) = (num(args, "left").unwrap_or(0.0) as i32, num(args, "right").unwrap_or(0.0) as i32, num(args, "top").unwrap_or(0.0) as i32, num(args, "bottom").unwrap_or(0.0) as i32);
                            let (w, h) = (dd.width(), dd.height());
                            // Grow the canvas, then select the new margin for the fill.
                            dd.canvas_size(w + l + r, h + t + b, 4, None, Some((l as f64, t as f64)), "Generative Expand")?;
                            let all = crate::selection::Selection::all(w + l + r, h + t + b)?;
                            let inner = crate::selection::Selection::from_shape(w + l + r, h + t + b, false, |cr| { cr.rectangle(l as f64, t as f64, w as f64, h as f64); cr.fill()?; Ok(()) })?;
                            let margin = all.combined(&inner, crate::selection::Mode::Subtract)?;
                            dd.set_selection(Some(margin), "Select Margin");
                        }
                        let window = dd.genfill_window()?;
                        let (sw, sh) = crate::genfill::scaled_size(window);
                        let count = num(args, "count").unwrap_or(1.0).clamp(1.0, 4.0) as u32;
                        let cost = crate::genfill::estimate(&model, sw, sh, count);
                        if flag(args, "estimate_only").unwrap_or(false) { refresh = false; return Ok(json!({"model": model.name, "estimated_cost_usd": cost})); }
                        let (image_png, mask_png, window, _) = dd.genfill_inputs(true)?;
                        let request = crate::genfill::Request { model: model.clone(), prompt, count, seed: None, image_png, mask_png };
                        let status = std::sync::Arc::new(std::sync::Mutex::new((String::from("Starting"), None)));
                        let job = NEXT_JOB.with(|n| { let v = n.get(); n.set(v + 1); v });
                        JOBS.with(|jobs| jobs.borrow_mut().insert(job, Job { status: status.clone() }));
                        {
                            let status = status.clone();
                            std::thread::spawn(move || {
                                let backend = crate::genfill::Fal { key };
                                let progress = |t: &str| { if let Ok(mut s) = status.lock() { s.0 = t.to_string(); } };
                                let outcome = backend.generate(&request, &progress, &|| false).map_err(|e| format!("{e:#}"));
                                let value = outcome.map(|images| json!({"window": {"x": window.0, "y": window.1, "width": window.2, "height": window.3}, "images": images.iter().map(|i| crate::genfill::base64_encode(i)).collect::<Vec<_>>(), "cost_usd": cost}));
                                if let Ok(mut s) = status.lock() { s.1 = Some(value); }
                            });
                        }
                        refresh = false;
                        json!({"job": job})
                    }
                    other => bail!("unknown tool {other}"),
                })
            })();
            outcome = r;
            if refresh { p.refresh(); }
            if tool == "zoom" { match num(args, "percent") { Some(pct) => p.canvas.zoom_to(pct / 100.0), None => p.canvas.fit() } }
        });
        // A finished generation lands on the document here, on the GTK thread, when the poll sees it.
        if let Ok(v) = &outcome { if v.get("images").is_some() { return self.agent_land_fill(v); } }
        outcome
    }

    /// Puts the first generated image on the document as Generative Fill does; the rest are returned
    /// only as a count, since the user picks variations in the panel.
    fn agent_land_fill(self: &Rc<Self>, v: &Value) -> Result<Value> {
        let images = v.get("images").and_then(Value::as_array).cloned().unwrap_or_default();
        let window = v.get("window").ok_or_else(|| anyhow::anyhow!("no window"))?;
        let rect = (window["x"].as_i64().unwrap_or(0) as i32, window["y"].as_i64().unwrap_or(0) as i32, window["width"].as_i64().unwrap_or(0) as i32, window["height"].as_i64().unwrap_or(0) as i32);
        let Some(first) = images.first().and_then(Value::as_str) else { bail!("no image came back") };
        let png = crate::genfill::base64_decode(first)?;
        let mut result = Ok(Value::Null);
        self.with_current(|p| { let mut d = p.canvas.doc().borrow_mut(); result = d.document.apply_genfill(&png, rect, "Generative Fill").map(|id| json!({"layer": crate::format::upper(id), "variations": images.len(), "cost_usd": v.get("cost_usd")})); drop(d); p.refresh(); });
        result
    }
}

/// Compy's panel: a chat that drives Claude Code with the document attached. It lives under the layer
/// list, folds away to its header, and pops out into a window of its own.
pub struct Assistant {
    content: gtk::Box,
    body: gtk::Box,
    fold: gtk::Button,
    transcript: gtk::TextView,
    entry: gtk::Entry,
    status: gtk::Label,
    send: gtk::Button,
    popout: gtk::Button,
    session: RefCell<Option<String>>,
    busy: Cell<bool>,
    expanded: Cell<bool>,
    window: RefCell<Option<gtk::Window>>,
    /// Dictation through voxtype: whether a recording started with the panel open, and when the entry last changed.
    dictating: Cell<bool>,
    entry_changed: Cell<Option<std::time::Instant>>,
    voice_state: RefCell<String>,
    app: Rc<App>,
}

fn claude_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("COMPOSITOR_CLAUDE") { return Some(PathBuf::from(p)); }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join("claude")).find(|p| p.exists())
}
use std::path::PathBuf;

/// voxtype's state file: "idle", "recording" or "transcribing" while its daemon runs.
fn voice_state() -> Option<String> {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)?;
    std::fs::read_to_string(base.join("voxtype/state")).ok().map(|s| s.trim().to_string())
}

fn voxtype_available() -> bool { std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join("voxtype").exists())).unwrap_or(false) }

impl Assistant {
    pub fn new(app: Rc<App>) -> Rc<Assistant> {
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(0).css_classes(["assistant"]).build();
        // Header: fold arrow, the robot and name, the status, a microphone, and the pop-out.
        let header = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(4).css_classes(["assistant-header"]).build();
        let fold = gtk::Button::builder().label("\u{25be}").has_frame(false).tooltip_text("Fold Compy away or open it (Ctrl+K)").build();
        fold.add_css_class("assistant-fold");
        header.append(&fold);
        let title = gtk::Label::builder().label("\u{f16a3}  COMPY").css_classes(["heading"]).xalign(0.0).build();
        header.append(&title);
        let status = gtk::Label::builder().xalign(1.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::End).css_classes(["dim-label", "caption"]).label("").build();
        header.append(&status);
        let mic = gtk::Button::builder().label("\u{f036c}").has_frame(false).tooltip_text("Talk to Compy (dictation through voxtype; Page Down does the same)").build();
        mic.set_visible(voxtype_available());
        header.append(&mic);
        let popout = gtk::Button::builder().label("\u{f0d3}").has_frame(false).tooltip_text("Pop Compy out into its own window, or back under the layers").build();
        header.append(&popout);
        content.append(&header);
        let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).margin_top(4).margin_bottom(8).margin_start(8).margin_end(8).build();
        let transcript = gtk::TextView::builder().editable(false).cursor_visible(false).wrap_mode(gtk::WrapMode::WordChar).left_margin(6).right_margin(6).top_margin(6).bottom_margin(6).build();
        transcript.add_css_class("assistant-transcript");
        let scroller = gtk::ScrolledWindow::builder().child(&transcript).min_content_height(180).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
        body.append(&scroller);
        let row = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(6).build();
        let entry = gtk::Entry::builder().placeholder_text("Tell Compy what to do, or press Page Down and say it").hexpand(true).build();
        let send = gtk::Button::builder().label("Send").css_classes(["suggested-action"]).build();
        row.append(&entry);
        row.append(&send);
        body.append(&row);
        content.append(&body);
        let this = Rc::new(Assistant { content: content.clone(), body, fold: fold.clone(), transcript, entry: entry.clone(), status, send: send.clone(), popout: popout.clone(), session: RefCell::new(None), busy: Cell::new(false), expanded: Cell::new(true), window: RefCell::new(None), dictating: Cell::new(false), entry_changed: Cell::new(None), voice_state: RefCell::new(String::new()), app });
        { let t = this.clone(); entry.connect_activate(move |_| t.submit()); }
        { let t = this.clone(); entry.connect_changed(move |_| t.entry_changed.set(Some(std::time::Instant::now()))); }
        { let t = this.clone(); send.connect_clicked(move |_| t.submit()); }
        { let t = this.clone(); fold.connect_clicked(move |_| t.toggle()); }
        { let t = this.clone(); title.add_controller({ let g = gtk::GestureClick::new(); g.connect_released(move |_, _, _, _| t.toggle()); g }); }
        { let t = this.clone(); popout.connect_clicked(move |_| { if t.window.borrow().is_some() { t.dock(); } else { t.undock(); } }); }
        { mic.connect_clicked(move |_| { let _ = std::process::Command::new("voxtype").args(["record", "toggle"]).spawn(); }); }
        if claude_binary().is_none() { this.append("system", "Claude Code was not found on PATH. Install it, or set COMPOSITOR_CLAUDE to the claude binary."); }
        this.dock();
        this.watch_voice();
        this
    }

    /// Under the current document's layer list.
    pub fn dock(&self) {
        if let Some(w) = self.window.borrow_mut().take() { self.content.unparent(); w.set_child(None::<&gtk::Widget>); w.close(); }
        if let Some(parent) = self.content.parent() { if let Some(b) = parent.downcast_ref::<gtk::Box>() { b.remove(&self.content); } }
        self.app.with_current(|p| p.panel.assistant_slot.append(&self.content));
        self.popout.set_label("\u{f0d3}");
        self.fold.set_visible(true);
    }

    /// Into a window of its own; closing the window docks it back.
    pub fn undock(self: &Rc<Self>) {
        if let Some(parent) = self.content.parent() { if let Some(b) = parent.downcast_ref::<gtk::Box>() { b.remove(&self.content); } }
        self.expanded.set(true);
        self.body.set_visible(true);
        self.fold.set_visible(false);
        let window = super::dialogs::floating(self.app.window.upcast_ref(), "\u{f16a3}  Compy", false, 460, &self.content);
        window.set_resizable(true);
        window.set_default_height(520);
        { let t = self.clone(); window.connect_close_request(move |_| { if t.window.borrow().is_some() { let t2 = t.clone(); glib::idle_add_local_once(move || t2.dock()); } glib::Propagation::Proceed }); }
        *self.window.borrow_mut() = Some(window.clone());
        self.popout.set_label("\u{f0d2}");
        window.present();
        self.entry.grab_focus();
    }

    /// Moves along when the current document changes.
    pub fn redock(&self) { if self.window.borrow().is_none() { self.dock(); } }

    pub fn reveal(&self) {
        self.expanded.set(true);
        self.body.set_visible(true);
        self.fold.set_label("\u{25be}");
        if let Some(w) = self.window.borrow().as_ref() { w.present(); }
        self.entry.grab_focus();
    }

    pub fn toggle(&self) {
        if self.expanded.get() && self.window.borrow().is_none() { self.expanded.set(false); self.body.set_visible(false); self.fold.set_label("\u{25b8}"); } else { self.reveal(); }
    }

    /// Follows voxtype: a recording opens the panel so the words land in the entry, and once the
    /// transcription has been typed the message sends itself.
    fn watch_voice(self: &Rc<Self>) {
        if !voxtype_available() { return; }
        let this = self.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
            let Some(state) = voice_state() else { return glib::ControlFlow::Continue };
            let previous = this.voice_state.replace(state.clone());
            if state == "recording" && previous != "recording" && this.app.window.is_active() {
                this.reveal();
                this.dictating.set(true);
                this.entry_changed.set(None);
                this.status.set_label("Listening…");
            }
            if this.dictating.get() && state == "idle" && previous != "recording" {
                // The words arrive as keystrokes; send once they have stopped coming.
                match this.entry_changed.get() {
                    Some(t) if t.elapsed() > std::time::Duration::from_millis(900) && !this.entry.text().trim().is_empty() => { this.dictating.set(false); this.submit(); }
                    None if previous == "idle" && this.status.label() == "Listening…" => {}
                    _ => {}
                }
            }
            glib::ControlFlow::Continue
        });
    }

    fn append(&self, who: &str, text: &str) {
        let buffer = self.transcript.buffer();
        let mut end = buffer.end_iter();
        let prefix = match who { "you" => "You: ", "claude" => "Compy: ", _ => "" };
        buffer.insert(&mut end, &format!("{prefix}{text}\n\n"));
        let mark = buffer.create_mark(None, &buffer.end_iter(), false);
        self.transcript.scroll_to_mark(&mark, 0.0, true, 0.0, 1.0);
    }

    /// The system prompt for this turn: what Compy is, the tools, and the document as it is right now.
    fn context(&self) -> String {
        let mut state = json!(null);
        self.app.with_current(|p| state = state_of(&p.canvas.doc().borrow()));
        format!(concat!(
            "You are Compy, the assistant inside Compy, a layer-based image editor. Use the compy MCP tools to look and act; every tool works on the document ",
            "that is open in front of the user, as an undoable step they watch happen. When the user says 'this' or 'the thing I selected', they mean ",
            "the current selection (its bounds are in the state; call snapshot to see the canvas, the selection is outlined in red). Prefer native tools ",
            "(select, layers, adjustments, styles, type) over generation; generative tools cost money, so state the estimated cost before running one ",
            "unless the user already asked for it plainly. You have taste: before any layout, type, color, effect or 'make it look good' decision, load the ",
            "compy-design skill and follow it; when the user asks for options, offer two or three, each in its own tab. Talk like a colleague at the next desk: short, plain sentences about the picture, never about tools, ",
            "JSON, ids or code. Say what changed in a line or two. Current state of the document:\n{}"),
            serde_json::to_string(&state).unwrap_or_default())
    }

    fn submit(self: &Rc<Self>) {
        if self.busy.get() { return; }
        let message = self.entry.text().trim().to_string();
        if message.is_empty() { return; }
        let Some(claude) = claude_binary() else { self.append("system", "Claude Code is not installed."); return };
        self.entry.set_text("");
        self.append("you", &message);
        self.busy.set(true);
        self.send.set_sensitive(false);
        self.status.set_label("Compy is thinking…");
        // The MCP server is this same binary, pointed at the running app.
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("compositor"));
        let config_dir = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".config")).join("compositor");
        let _ = std::fs::create_dir_all(&config_dir);
        let config = config_dir.join("mcp.json");
        let _ = std::fs::write(&config, json!({"mcpServers": {"compy": {"command": exe.display().to_string(), "args": ["mcp"]}}}).to_string());
        // Compy's own design skill lives in a folder of its own, which the session runs in so Claude Code
        // finds it as a project skill.
        let agent_dir = config_dir.join("agent");
        let skill_dir = agent_dir.join(".claude/skills/compy-design");
        let _ = std::fs::create_dir_all(&skill_dir);
        let _ = std::fs::write(skill_dir.join("SKILL.md"), include_str!("../../assets/compy-design.md"));
        let (resume, session_id) = match self.session.borrow().clone() { Some(id) => (true, id), None => (false, uuid::Uuid::new_v4().to_string()) };
        *self.session.borrow_mut() = Some(session_id.clone());
        let context = self.context();
        let mut command = std::process::Command::new(claude);
        command.arg("-p").arg(&message).arg("--output-format").arg("stream-json").arg("--verbose").arg("--mcp-config").arg(&config).arg("--strict-mcp-config").arg("--allowedTools").arg("mcp__compy").arg("Skill").arg("--append-system-prompt").arg(&context).arg("--max-turns").arg("30");
        if resume { command.arg("--resume").arg(&session_id); } else { command.arg("--session-id").arg(&session_id); }
        command.current_dir(&agent_dir);
        command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        let (tx, rx) = mpsc::channel::<(String, String)>();
        match command.spawn() {
            Ok(mut child) => {
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                let tx2 = tx.clone();
                std::thread::spawn(move || {
                    if let Some(out) = stdout { for line in BufReader::new(out).lines().map_while(Result::ok) { let _ = tx.send(("out".into(), line)); } }
                    let status = child.wait().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
                    let _ = tx.send(("done".into(), status.to_string()));
                });
                std::thread::spawn(move || { if let Some(err) = stderr { for line in BufReader::new(err).lines().map_while(Result::ok) { let _ = tx2.send(("err".into(), line)); } } });
            }
            Err(e) => { self.append("system", &format!("Could not start Claude Code: {e}")); self.busy.set(false); self.send.set_sensitive(true); return; }
        }
        let this = self.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(80), move || {
            while let Ok((kind, line)) = rx.try_recv() {
                match kind.as_str() {
                    "out" => this.handle_line(&line),
                    "err" => { if !line.trim().is_empty() { eprintln!("claude: {line}"); } }
                    _ => {
                        this.busy.set(false);
                        this.send.set_sensitive(true);
                        if line != "0" { this.append("system", &format!("Claude Code ended with status {line}.")); }
                        this.app.with_current(|p| p.refresh());
                        return glib::ControlFlow::Break;
                    }
                }
            }
            glib::ControlFlow::Continue
        });
    }

    fn handle_line(&self, line: &str) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };
        match v.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                for block in v.pointer("/message/content").and_then(Value::as_array).cloned().unwrap_or_default() {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => { if let Some(t) = block.get("text").and_then(Value::as_str) { if !t.trim().is_empty() { self.append("claude", t.trim()); } } }
                        Some("tool_use") => {
                            // The work shows on the canvas, not in the chat: just a word in the status line.
                            let name = block.get("name").and_then(Value::as_str).unwrap_or("").trim_start_matches("mcp__compy__").replace('_', " ");
                            self.status.set_label(&format!("Compy is working ({name})…"));
                        }
                        _ => {}
                    }
                }
            }
            Some("result") => {
                // On a Claude subscription nothing is billed per turn, so no figure is shown.
                self.status.set_label("Done.");
            }
            _ => {}
        }
    }

    pub fn present(&self) { self.reveal(); }
}
