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

/// A long job (generation) running on a thread; the socket thread polls until it is done. The job
/// remembers the document it started on, the layer it came from and the selection it was given, so
/// the result lands there and not on whatever is current when it finishes.
struct Job {
    status: std::sync::Arc<std::sync::Mutex<(String, Option<Result<Value, String>>)>>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    document: uuid::Uuid,
    source: Option<uuid::Uuid>,
    selection: Option<crate::selection::Selection>,
}

thread_local! {
    static JOBS: RefCell<std::collections::HashMap<u64, Job>> = RefCell::new(std::collections::HashMap::new());
    static NEXT_JOB: Cell<u64> = const { Cell::new(1) };
}

/// Whether the client on `stream` has hung up (its end closed) without the server reading anything.
fn client_gone(stream: &std::os::unix::net::UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    let mut byte = [0u8; 1];
    // A peek that does not wait: 0 means the peer closed; an error other than "nothing yet" means gone.
    let n = unsafe { libc::recv(stream.as_raw_fd(), byte.as_mut_ptr() as *mut libc::c_void, 1, libc::MSG_PEEK | libc::MSG_DONTWAIT) };
    if n == 0 { return true; }
    if n > 0 { return false; }
    let e = std::io::Error::last_os_error();
    e.kind() != std::io::ErrorKind::WouldBlock && e.kind() != std::io::ErrorKind::Interrupted
}

/// Starts listening. Requests arrive on a thread and are answered on the GTK thread in turn.
pub fn serve(app: Rc<App>) {
    let path = agent::socket_path();
    // Another Compy already listening keeps its socket: unlinking it would cut that app off from its
    // own assistant. This one runs without an agent.
    if std::os::unix::net::UnixStream::connect(&path).is_ok() { eprintln!("agent: another Compy is already serving {}; this window has no assistant", path.display()); return; }
    let _ = std::fs::remove_file(&path);
    let listener = match std::os::unix::net::UnixListener::bind(&path) { Ok(l) => l, Err(e) => { eprintln!("agent: cannot listen on {}: {e}", path.display()); return; } };
    let (tx, rx) = mpsc::channel::<Pending>();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let tx = tx.clone();
            std::thread::spawn(move || {
                let Ok(clone) = stream.try_clone() else { return };
                let mut reader = BufReader::new(clone);
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
                    if !line.ends_with('\n') { break; }
                    let response = match serde_json::from_str::<Request>(line.trim()) {
                        Ok(request) => {
                            let id = request.id;
                            let (reply_tx, reply_rx) = mpsc::channel();
                            if tx.send(Pending { request, reply: reply_tx }).is_err() { break; }
                            let mut response = reply_rx.recv().unwrap_or(Response { id, ok: false, result: json!("the app went away") });
                            // A job id means the work continues on a thread: wait for it here, off the GTK thread.
                            let started = std::time::Instant::now();
                            while response.ok && response.result.get("job").is_some() {
                                let job = response.result["job"].as_u64().unwrap_or(0);
                                let gone = client_gone(&stream);
                                if gone || started.elapsed().as_secs() > agent::JOB_TIMEOUT_SECONDS {
                                    // Nobody is waiting (Claude was stopped, its MCP server with it), or it has
                                    // taken too long: cancel rather than pay for a result no one asked to keep.
                                    let (c_tx, c_rx) = mpsc::channel();
                                    if tx.send(Pending { request: Request { id, tool: "_cancel".into(), args: json!({"job": job}) }, reply: c_tx }).is_ok() { let _ = c_rx.recv(); }
                                    response = Response { id, ok: false, result: json!(if gone { "the client went away; the job was cancelled" } else { "the job took too long and was cancelled" }) };
                                    break;
                                }
                                std::thread::sleep(std::time::Duration::from_millis(300));
                                let (poll_tx, poll_rx) = mpsc::channel();
                                if tx.send(Pending { request: Request { id, tool: "_poll".into(), args: json!({"job": job}) }, reply: poll_tx }).is_err() { break; }
                                response = poll_rx.recv().unwrap_or(Response { id, ok: false, result: json!("the app went away") });
                            }
                            response
                        }
                        Err(e) => Response { id: 0, ok: false, result: json!(format!("bad request: {e}")) },
                    };
                    let Ok(mut s) = stream.try_clone() else { break };
                    if writeln!(s, "{}", serde_json::to_string(&response).unwrap_or_default()).is_err() { break; }
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

/// The undo step a tool's call makes: "new_layer" reads as "New Layer".
fn step_name(tool: &str) -> String {
    tool.split('_').map(|w| { let mut c = w.chars(); match c.next() { Some(f) => f.to_uppercase().collect::<String>() + c.as_str(), None => String::new() } }).collect::<Vec<_>>().join(" ")
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
                Some((_, Some(result))) => {
                    let Some(job) = JOBS.with(|jobs| jobs.borrow_mut().remove(&job)) else { bail!("no such job") };
                    // The finished generation lands on the document here, on the GTK thread.
                    let value = result.map_err(|e| anyhow::anyhow!("{e}"))?;
                    if value.get("images").is_some() { self.agent_land_fill(&value, &job) } else { Ok(value) }
                }
            };
        }
        if tool == "_cancel" {
            let job = args.get("job").and_then(Value::as_u64).unwrap_or(0);
            JOBS.with(|jobs| { if let Some(j) = jobs.borrow_mut().remove(&job) { j.cancelled.store(true, std::sync::atomic::Ordering::Relaxed); } });
            return Ok(json!("cancelled"));
        }
        match tool {
            "open" => {
                let path = text(args, "path").ok_or_else(|| anyhow::anyhow!("path needed"))?;
                if !self.open_path(std::path::Path::new(&path)) { bail!("Could not open {path}; the user has been shown why."); }
                return Ok(json!("opened"));
            }
            "new_document" => { let (w, h) = (num(args, "width").unwrap_or(1920.0) as i32, num(args, "height").unwrap_or(1080.0) as i32); let document = Document::blank(w, h, 72.0)?; self.add_page(Doc::from(document, "Untitled")); return Ok(json!("created")); }
            _ => {}
        }
        if self.notebook.current_page().is_none() { bail!("Nothing is open. Use open or new_document first."); }
        // Tools that only look, navigate or write files run as they are; every other call changes the
        // document as one undoable step, all of it or none.
        let reads = matches!(tool, "state" | "snapshot" | "select_layer" | "zoom" | "export" | "save" | "undo" | "redo");
        let mut outcome: Result<Value> = Ok(Value::Null);
        let mut refresh = true;
        self.with_current(|p| {
            // Typing on the canvas ends first, as a click elsewhere would.
            if !reads && p.canvas.text_editing() { p.canvas.finish_text_edit(); }
            let doc = p.canvas.doc();
            let r: Result<Value> = (|| {
                let mut d = doc.borrow_mut();
                if !reads {
                    // A floating Free Transform lands first, as Return would; anything else half done waits.
                    if d.document.floating.is_some() { d.document.commit_free_transform()?; }
                    if d.document.busy_editing() { bail!("The user is in the middle of an edit (a dialog or a stroke); try again when they are done."); }
                    d.document.begin_edit(&step_name(tool));
                }
                let result: Result<Value> = (|| {
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
                        let blend = text(args, "blend").map(|b| blend_from(&b).ok_or_else(|| anyhow::anyhow!("unknown blend mode {b}"))).transpose()?;
                        if let Some(v) = flag(args, "visible") { dd.set_visible(id, v); }
                        if let Some(o) = num(args, "opacity") { dd.set_opacity(id, o.clamp(0.0, 1.0)); }
                        if let Some(mode) = blend { dd.set_blend_mode(id, mode); }
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
                            "unsharp_mask" => { if let Some(v) = num(args, "amount") { s.sharpen.amount = v; } if let Some(v) = num(args, "radius") { s.sharpen.radius = v; } if let Some(v) = num(args, "threshold") { s.sharpen.threshold = v; } Kind::UnsharpMask }
                            "smart_sharpen" => { if let Some(v) = num(args, "amount") { s.sharpen.amount = v; } if let Some(v) = num(args, "radius") { s.sharpen.radius = v; } if let Some(v) = num(args, "noise") { s.sharpen.noise = v; } Kind::SmartSharpen }
                            "brightness_contrast" => { s.brightness.brightness = num(args, "brightness").unwrap_or(0.0); s.brightness.contrast = num(args, "contrast").unwrap_or(0.0); Kind::BrightnessContrast }
                            "vibrance" => { s.vibrance.vibrance = num(args, "vibrance").unwrap_or(0.0); s.vibrance.saturation = num(args, "saturation").unwrap_or(0.0); Kind::Vibrance }
                            "black_white" => { for (k, f) in [("reds", &mut s.black_white.reds), ("yellows", &mut s.black_white.yellows), ("greens", &mut s.black_white.greens), ("cyans", &mut s.black_white.cyans), ("blues", &mut s.black_white.blues), ("magentas", &mut s.black_white.magentas)] { if let Some(v) = num(args, k) { *f = v; } } Kind::BlackWhite }
                            "photo_filter" => { if let Some(c) = text(args, "color").and_then(|c| parse_color(&c)) { s.photo_filter.color = c; } if let Some(v) = num(args, "density") { s.photo_filter.density = v; } if let Some(b) = flag(args, "preserve_luminosity") { s.photo_filter.preserve_luminosity = b; } Kind::PhotoFilter }
                            "threshold" => { s.threshold.level = num(args, "level").unwrap_or(128.0); Kind::Threshold }
                            "posterize" => { s.posterize.levels = num(args, "levels").unwrap_or(4.0); Kind::Posterize }
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
                        let name = match kind.as_str() { "levels" => "Levels", "exposure" => "Exposure", "hue_saturation" => "Hue/Saturation", "gradient_map" => "Gradient Map", "curves" => "Curves", "brightness_contrast" => "Brightness/Contrast", "vibrance" => "Vibrance", "black_white" => "Black & White", "photo_filter" => "Photo Filter", "threshold" => "Threshold", "posterize" => "Posterize", other => bail!("unknown adjustment {other}") };
                        let id = dd.add_adjustment(name).ok_or_else(|| anyhow::anyhow!("could not add {name}"))?;
                        if let Some(mut a) = dd.adjustment(id) {
                            match &mut a {
                                crate::filters::Adjustment::Levels(l) => { if let Some(b) = num(args, "black") { l.ranges[0].black = b; } if let Some(w) = num(args, "white") { l.ranges[0].white = w; } if let Some(g) = num(args, "gamma") { l.ranges[0].gamma = g; } }
                                crate::filters::Adjustment::Exposure(e) => { if let Some(v) = num(args, "exposure") { e.exposure = v; } }
                                crate::filters::Adjustment::HueSaturation(h) => { h.adjustments = vec![("Master".into(), [num(args, "hue").unwrap_or(0.0), num(args, "saturation").unwrap_or(0.0), num(args, "lightness").unwrap_or(0.0)])]; }
                                crate::filters::Adjustment::BrightnessContrast(b) => { if let Some(v) = num(args, "brightness") { b.brightness = v; } if let Some(v) = num(args, "contrast") { b.contrast = v; } }
                                crate::filters::Adjustment::Vibrance(v) => { if let Some(x) = num(args, "vibrance") { v.vibrance = x; } if let Some(x) = num(args, "saturation") { v.saturation = x; } }
                                crate::filters::Adjustment::BlackWhite(b) => { for (k, f) in [("reds", &mut b.reds), ("yellows", &mut b.yellows), ("greens", &mut b.greens), ("cyans", &mut b.cyans), ("blues", &mut b.blues), ("magentas", &mut b.magentas)] { if let Some(v) = num(args, k) { *f = v; } } }
                                crate::filters::Adjustment::PhotoFilter(p) => { if let Some(c) = text(args, "color").and_then(|c| parse_color(&c)) { p.color = c; } if let Some(v) = num(args, "density") { p.density = v; } if let Some(b) = flag(args, "preserve_luminosity") { p.preserve_luminosity = b; } }
                                crate::filters::Adjustment::Threshold(t) => { if let Some(v) = num(args, "level") { t.level = v; } }
                                crate::filters::Adjustment::Posterize(p) => { if let Some(v) = num(args, "levels") { p.levels = v; } }
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
                        if style.text.trim().is_empty() { bail!("text needed: a type layer with nothing in it is dropped, as the Type tool does"); }
                        let dd = &mut d.document;
                        if tool == "set_text" { let id = dd.active.unwrap(); dd.set_text(id, &style)?; json!("text set") } else { let id = dd.add_text_layer(&style, num(args, "x").unwrap_or(40.0), num(args, "y").unwrap_or(40.0))?; json!({"id": crate::format::upper(id)}) }
                    }
                    "shape_layer" => { let ellipse = text(args, "kind").unwrap_or_default() == "ellipse"; let color = text(args, "color").and_then(|c| parse_color(&c)).unwrap_or(d.brush.color); let dd = &mut d.document; let id = dd.add_shape_layer(ellipse, (num(args, "x").unwrap_or(0.0), num(args, "y").unwrap_or(0.0), num(args, "width").unwrap_or(100.0), num(args, "height").unwrap_or(100.0)), color, num(args, "corner_radius").unwrap_or(0.0))?; json!({"id": crate::format::upper(id)}) }
                    "layer_style" => {
                        let id = dd.active.ok_or_else(|| anyhow::anyhow!("no active layer"))?;
                        const KEYS: [&str; 7] = ["drop_shadow", "inner_shadow", "outer_glow", "inner_glow", "bevel", "stroke", "color_overlay"];
                        let Some(map) = args.as_object() else { bail!("layer_style takes an object of effects") };
                        for (k, v) in map {
                            if !KEYS.contains(&k.as_str()) { bail!("unknown effect {k}; the effects are {}", KEYS.join(", ")); }
                            let Some(fields) = v.as_object() else { bail!("{k} must be an object of settings") };
                            for (f, fv) in fields {
                                match f.as_str() {
                                    "color" => { if fv.as_str().and_then(parse_color).is_none() { bail!("{k}.color must look like #rrggbb"); } }
                                    "opacity" | "angle" | "distance" | "size" | "depth" | "altitude" | "style" | "position" => { if !fv.is_number() { bail!("{k}.{f} must be a number"); } }
                                    other => bail!("{k}.{other} is not a setting"),
                                }
                            }
                        }
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
                    "stroke_selection" => {
                        let color = parse_color(&text(args, "color").unwrap_or_default()).ok_or_else(|| anyhow::anyhow!("color must look like #rrggbb"))?;
                        let position = match text(args, "position").unwrap_or_else(|| "center".into()).as_str() { "inside" => 0, "outside" => 2, _ => 1 };
                        dd.stroke_selection(num(args, "width").unwrap_or(3.0), color, position, num(args, "opacity").unwrap_or(1.0))?; json!("stroked")
                    }
                    "select_color_range" => {
                        let color = parse_color(&text(args, "color").unwrap_or_default()).ok_or_else(|| anyhow::anyhow!("color must look like #rrggbb"))?;
                        dd.select_color_range(color, num(args, "fuzziness").unwrap_or(40.0), flag(args, "all_layers").unwrap_or(true), crate::selection::Mode::Replace)?; json!("selected")
                    }
                    "align_layers" => { dd.align_layers(&text(args, "edge").unwrap_or_default())?; json!("aligned") }
                    "distribute_layers" => { dd.distribute_layers(text(args, "axis").unwrap_or_default() != "vertical")?; json!("distributed") }
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
                        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                        let job = NEXT_JOB.with(|n| { let v = n.get(); n.set(v + 1); v });
                        JOBS.with(|jobs| jobs.borrow_mut().insert(job, Job { status: status.clone(), cancelled: cancelled.clone(), document: dd.document_id, source: dd.active, selection: dd.selection.clone() }));
                        {
                            let status = status.clone();
                            std::thread::spawn(move || {
                                let backend = crate::genfill::Fal { key };
                                let progress = |t: &str| { if let Ok(mut s) = status.lock() { s.0 = t.to_string(); } };
                                let outcome = backend.generate(&request, &progress, &|| cancelled.load(std::sync::atomic::Ordering::Relaxed)).map_err(|e| format!("{e:#}"));
                                let value = outcome.map(|images| json!({"window": {"x": window.0, "y": window.1, "width": window.2, "height": window.3}, "images": images.iter().map(|i| crate::genfill::base64_encode(i)).collect::<Vec<_>>(), "cost_usd": cost}));
                                if let Ok(mut s) = status.lock() { s.1 = Some(value); }
                            });
                        }
                        refresh = false;
                        json!({"job": job})
                    }
                    "generative_edit" | "generate_image" | "upscale" | "relight" => {
                        let Some(key) = crate::genfill::key() else { bail!("No fal.ai key is set. File > Generative Fill lets the user enter one.") };
                        let (w, h) = (dd.width() as f64, dd.height() as f64);
                        // What goes to fal, and where the result lands.
                        let (model, body, place, name, cost): (String, Value, (f64, f64, f64, f64), String, f64) = match tool {
                            "generate_image" => {
                                let (gw, gh) = (num(args, "width").unwrap_or(w).clamp(64.0, 4096.0), num(args, "height").unwrap_or(h).clamp(64.0, 4096.0));
                                let transparent = args.get("transparent").and_then(Value::as_bool).unwrap_or(false);
                                // A transparent background needs GPT Image, whatever the default says.
                                let model = crate::genfill::resolve_model(&text(args, "model").unwrap_or_else(|| if transparent { "gpt image".into() } else { String::new() }), false);
                                let model = if transparent && !model.starts_with("openai/") { crate::genfill::resolve_model("gpt image", false) } else { model };
                                let body = crate::genfill::agent_body(&model, &text(args, "prompt").unwrap_or_default(), None, gw as usize, gh as usize, 1, transparent);
                                (model, body, ((w - gw) / 2.0, (h - gh) / 2.0, gw, gh), "Generated".into(), 0.05)
                            }
                            _ => {
                                let Some((png, rect)) = dd.copy_layer_pixels()? else { bail!("The active layer has no pixels there.") };
                                let layer_name = dd.active.map(|id| dd.renderer.layer(id).name.clone()).unwrap_or_default();
                                let uri = crate::genfill::data_uri(&png);
                                let place = (rect.0 as f64, rect.1 as f64, rect.2 as f64, rect.3 as f64);
                                match tool {
                                    "generative_edit" => {
                                        let count = num(args, "count").unwrap_or(1.0).clamp(1.0, 4.0) as i64;
                                        let transparent = args.get("transparent").and_then(Value::as_bool).unwrap_or(false);
                                        let model = crate::genfill::resolve_model(&text(args, "model").unwrap_or_else(|| if transparent { "gpt image".into() } else { String::new() }), true);
                                        let model = if transparent && !model.starts_with("openai/") { crate::genfill::resolve_model("gpt image", true) } else { model };
                                        let body = crate::genfill::agent_body(&model, &text(args, "prompt").unwrap_or_default(), Some(&uri), rect.2 as usize, rect.3 as usize, count, transparent);
                                        (model, body, place, format!("{layer_name} edited"), 0.05 * count as f64)
                                    }
                                    "upscale" => ("fal-ai/aura-sr".into(), json!({"image_url": uri, "upscaling_factor": 4}), place, format!("{layer_name} upscaled"), 0.02),
                                    _ => ("fal-ai/image-apps-v2/relighting".into(), json!({"image_url": uri, "lighting_style": text(args, "style").unwrap_or_else(|| "studio".into())}), place, format!("{layer_name} relit"), 0.05),
                                }
                            }
                        };
                        let status = std::sync::Arc::new(std::sync::Mutex::new((String::from("Starting"), None)));
                        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                        let job = NEXT_JOB.with(|n| { let v = n.get(); n.set(v + 1); v });
                        JOBS.with(|jobs| jobs.borrow_mut().insert(job, Job { status: status.clone(), cancelled: cancelled.clone(), document: dd.document_id, source: dd.active, selection: None }));
                        {
                            let status = status.clone();
                            std::thread::spawn(move || {
                                let backend = crate::genfill::Fal { key };
                                let progress = |t: &str| { if let Ok(mut s) = status.lock() { s.0 = t.to_string(); } };
                                let outcome = backend.run(&model, body, &progress, &|| cancelled.load(std::sync::atomic::Ordering::Relaxed)).map_err(|e| format!("{e:#}"));
                                let value = outcome.map(|images| json!({"mode": "layer", "place": {"x": place.0, "y": place.1, "width": place.2, "height": place.3}, "name": name, "images": images.iter().map(|i| crate::genfill::base64_encode(i)).collect::<Vec<_>>(), "cost_usd": cost}));
                                if let Ok(mut s) = status.lock() { s.1 = Some(value); }
                            });
                        }
                        refresh = false;
                        json!({"job": job})
                    }
                    other => bail!("unknown tool {other}"),
                })
                })();
                if !reads { match &result { Ok(_) => d.document.end_edit(), Err(_) => d.document.abort_edit() } }
                result
            })();
            outcome = r;
            if refresh { p.refresh(); }
            if tool == "zoom" { match num(args, "percent") { Some(pct) => p.canvas.zoom_to(pct / 100.0), None => p.canvas.fit() } }
        });
        outcome
    }

    /// Runs `f` on the page holding document `id`; false when it is no longer open.
    fn with_document(&self, id: uuid::Uuid, f: impl FnOnce(&super::Page)) -> bool {
        let pages = self.pages.borrow();
        match pages.iter().find(|p| p.canvas.doc().borrow().document.document_id == id) { Some(page) => { f(page); true } None => false }
    }

    /// Puts the first generated image on the document the job started on, as Generative Fill does; the
    /// rest are returned only as a count, since the user picks variations in the panel.
    fn agent_land_fill(self: &Rc<Self>, v: &Value, job: &Job) -> Result<Value> {
        let images = v.get("images").and_then(Value::as_array).cloned().unwrap_or_default();
        let Some(first) = images.first().and_then(Value::as_str) else { bail!("no image came back") };
        let bytes = crate::genfill::base64_decode(first)?;
        let mut result = Err(anyhow::anyhow!("The document this was made for has been closed; the picture was not kept."));
        if v.get("mode").and_then(Value::as_str) == Some("layer") {
            // A generated, edited, upscaled or relit picture: a new layer above the layer it came from.
            let (surface, iw, ih) = Document::decode_image_bytes(&bytes)?;
            let p = v.get("place").cloned().unwrap_or(json!({}));
            let name = v.get("name").and_then(Value::as_str).unwrap_or("Generated").to_string();
            // The picture fits inside the place, centered, keeping its own proportions: models that
            // pick from a list of aspect ratios return something close to, not exactly, the request.
            let (mut x, mut y, mut pw, mut ph) = (p["x"].as_f64().unwrap_or(0.0), p["y"].as_f64().unwrap_or(0.0), p["width"].as_f64().unwrap_or(0.0), p["height"].as_f64().unwrap_or(0.0));
            if !(pw >= 1.0 && ph >= 1.0) { pw = iw as f64; ph = ih as f64; }
            let scale = (pw / iw as f64).min(ph / ih as f64);
            let (fw, fh) = (iw as f64 * scale, ih as f64 * scale);
            x += (pw - fw) / 2.0;
            y += (ph - fh) / 2.0;
            pw = fw;
            ph = fh;
            self.with_document(job.document, |page| {
                let mut d = page.canvas.doc().borrow_mut();
                if let Some(src) = job.source { if d.document.has_layer(src) { d.document.select_layer(Some(src)); } }
                result = d.document.add_image_surface(surface, &name, (x, y), (pw, ph)).map(|id| json!({"layer": crate::format::upper(id), "variations": images.len(), "cost_usd": v.get("cost_usd")}));
                drop(d);
                page.refresh();
            });
            return result;
        }
        let window = v.get("window").ok_or_else(|| anyhow::anyhow!("no window"))?;
        let rect = (window["x"].as_i64().unwrap_or(0) as i32, window["y"].as_i64().unwrap_or(0) as i32, window["width"].as_i64().unwrap_or(0) as i32, window["height"].as_i64().unwrap_or(0) as i32);
        self.with_document(job.document, |page| {
            let mut d = page.canvas.doc().borrow_mut();
            // The layer's mask comes from the selection the model was given, whatever is selected now.
            let dd = &mut d.document;
            dd.begin_edit("Generative Fill");
            let current = std::mem::replace(&mut dd.selection, job.selection.clone());
            let landed = dd.apply_genfill(&bytes, rect, "Generative Fill");
            dd.selection = current;
            match &landed { Ok(_) => dd.end_edit(), Err(_) => dd.abort_edit() }
            result = landed.map(|id| json!({"layer": crate::format::upper(id), "variations": images.len(), "cost_usd": v.get("cost_usd")}));
            drop(d);
            page.refresh();
        });
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
    /// Messages typed while a turn was running; they go next, in order.
    queue: RefCell<Vec<String>>,
    /// The running Claude Code process, so a stuck turn can be stopped.
    child_pid: Cell<Option<u32>>,
    /// Counts turns; a turn's timer and process act only while this still names their turn.
    turn: Cell<u64>,
    replied: Cell<bool>,
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
        let fresh = gtk::Button::builder().label("\u{f0453}").has_frame(false).tooltip_text("Start a new conversation (Compy forgets this one)").build();
        header.append(&fresh);
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
        let this = Rc::new(Assistant { content: content.clone(), body, fold: fold.clone(), transcript, entry: entry.clone(), status, send: send.clone(), popout: popout.clone(), session: RefCell::new(None), busy: Cell::new(false), expanded: Cell::new(true), window: RefCell::new(None), dictating: Cell::new(false), entry_changed: Cell::new(None), voice_state: RefCell::new(String::new()), queue: RefCell::new(Vec::new()), child_pid: Cell::new(None), turn: Cell::new(0), replied: Cell::new(false), app });
        { let t = this.clone(); fresh.connect_clicked(move |_| t.reset()); }
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
        // Take the window out first and drop the borrow: closing it runs the close handler, which looks here.
        let taken = self.window.borrow_mut().take();
        if let Some(w) = taken {
            // The floating window holds the content inside a WindowHandle; ask that to let go, never unparent by hand.
            if let Some(handle) = self.content.parent().and_downcast::<gtk::WindowHandle>() { handle.set_child(None::<&gtk::Widget>); }
            w.close();
        }
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
        { let t = self.clone(); window.connect_close_request(move |_| { let open = t.window.borrow().is_some(); if open { let t2 = t.clone(); glib::idle_add_local_once(move || t2.dock()); } glib::Propagation::Proceed }); }
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
            let focused = this.app.window.is_active() || this.window.borrow().as_ref().is_some_and(|w| w.is_active());
            if state == "recording" && previous != "recording" && focused {
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
        // A long session stays light: the oldest lines go once the transcript passes a few hundred kilobytes.
        if buffer.char_count() > 300_000 {
            let (mut a, mut b) = (buffer.start_iter(), buffer.iter_at_offset(100_000));
            b.forward_line();
            buffer.delete(&mut a, &mut b);
        }
        let mark = buffer.create_mark(None, &buffer.end_iter(), false);
        self.transcript.scroll_to_mark(&mark, 0.0, true, 0.0, 1.0);
    }

    /// The system prompt for this turn: what Compy is, the tools, and the document as it is right now.
    fn context(&self) -> String {
        let mut state = json!(null);
        self.app.with_current(|p| state = state_of(&p.canvas.doc().borrow()));
        format!(concat!(
            "You are Compy, the assistant inside Compy, a layer-based image editor. The compy MCP tools are the only tools you have: no shell, no files, no web. ",
            "If a request needs something they cannot do, say so in a sentence instead of trying another way. Use the tools to look and act; every tool works on the document ",
            "that is open in front of the user, as an undoable step they watch happen. When the user says 'this' or 'the thing I selected', they mean ",
            "the current selection (its bounds are in the state; call snapshot to see the canvas, the selection is outlined in red). Prefer native tools ",
            "(select, layers, adjustments, styles, type) over generation; generative tools cost money, so state the estimated cost before running one ",
            "unless the user already asked for it plainly. You have taste: before any layout, type, color, effect or 'make it look good' decision, load the ",
            "compy-design skill and follow it; when the user asks for options, offer two or three, each in its own tab. Talk like a colleague at the next desk: short, plain sentences about the picture, never about tools, ",
            "JSON, ids or code. Say what changed in a line or two. Current state of the document:\n{}"),
            serde_json::to_string(&state).unwrap_or_default())
    }

    /// Forgets the conversation; the next message starts a new one. A running turn is stopped.
    pub fn reset(&self) {
        self.stop_turn();
        *self.session.borrow_mut() = None;
        self.queue.borrow_mut().clear();
        self.busy.set(false);
        self.send.set_sensitive(true);
        self.transcript.buffer().set_text("");
        self.status.set_label("New conversation.");
    }

    /// Stops the running turn, if any: its process is killed and its timer, seeing a new turn number,
    /// stops touching the shared state.
    fn stop_turn(&self) {
        self.turn.set(self.turn.get() + 1);
        if let Some(pid) = self.child_pid.take() { let _ = std::process::Command::new("kill").arg(pid.to_string()).status(); }
    }

    /// The window is closing: Claude Code must not keep running without it.
    pub fn shutdown(&self) { self.stop_turn(); self.busy.set(false); }

    fn submit(self: &Rc<Self>) {
        let message = self.entry.text().trim().to_string();
        if message.is_empty() { return; }
        self.entry.set_text("");
        // Sent by hand or by dictation, either way the recording is spent.
        self.dictating.set(false);
        self.append("you", &message);
        if self.busy.get() {
            // Compy is mid-turn: keep the message and send it as soon as the turn ends.
            self.queue.borrow_mut().push(message);
            self.status.set_label("Compy is still working; that goes next.");
            return;
        }
        self.send_now(message);
    }

    /// Runs one turn of Claude Code for `message`, which is already in the transcript.
    fn send_now(self: &Rc<Self>, message: String) {
        let Some(claude) = claude_binary() else { self.append("system", "Claude Code is not installed."); return };
        self.replied.set(false);
        self.busy.set(true);
        self.send.set_sensitive(false);
        self.status.set_label("Compy is thinking…");
        let fail = |this: &Self, what: String| { this.append("system", &what); this.busy.set(false); this.send.set_sensitive(true); };
        // The MCP server is this same binary, pointed at the running app.
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("compositor"));
        let config_dir = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".config")).join("compositor");
        let config = config_dir.join("mcp.json");
        // Compy's own design skill lives in a folder of its own, which the session runs in so Claude Code
        // finds it as a project skill.
        let agent_dir = config_dir.join("agent");
        let skill_dir = agent_dir.join(".claude/skills/compy-design");
        let prepared = std::fs::create_dir_all(&skill_dir)
            .and_then(|_| std::fs::write(&config, json!({"mcpServers": {"compy": {"command": exe.display().to_string(), "args": ["mcp"]}}}).to_string()))
            .and_then(|_| std::fs::write(skill_dir.join("SKILL.md"), include_str!("../../assets/compy-design.md")));
        if let Err(e) = prepared { fail(self, format!("Could not write Compy's files under {}: {e}", config_dir.display())); return; }
        let (resume, session_id) = match self.session.borrow().clone() { Some(id) => (true, id), None => (false, uuid::Uuid::new_v4().to_string()) };
        *self.session.borrow_mut() = Some(session_id.clone());
        let context = self.context();
        let mut command = std::process::Command::new(claude);
        command.arg("-p").arg(&message).arg("--output-format").arg("stream-json").arg("--verbose").arg("--mcp-config").arg(&config).arg("--strict-mcp-config").arg("--allowedTools").arg("mcp__compy").arg("Skill").arg("--append-system-prompt").arg(&context).arg("--max-turns").arg("30");
        if resume { command.arg("--resume").arg(&session_id); } else { command.arg("--session-id").arg(&session_id); }
        command.current_dir(&agent_dir);
        command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        let (tx, rx) = mpsc::channel::<(String, String)>();
        let turn = self.turn.get() + 1;
        self.turn.set(turn);
        let pid;
        match command.spawn() {
            Ok(mut child) => {
                pid = child.id();
                self.child_pid.set(Some(pid));
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
            Err(e) => { fail(self, format!("Could not start Claude Code: {e}")); return; }
        }
        let this = self.clone();
        let mut quiet_since = std::time::Instant::now();
        let last_error = Rc::new(RefCell::new(String::new()));
        glib::timeout_add_local(std::time::Duration::from_millis(80), move || {
            // A newer turn (or a reset) owns the panel now: this timer is done, its process already stopped.
            if this.turn.get() != turn { return glib::ControlFlow::Break; }
            // A turn that says nothing for five minutes is stuck: stop it and say so.
            if quiet_since.elapsed() > std::time::Duration::from_secs(300) && this.child_pid.get() == Some(pid) { let _ = std::process::Command::new("kill").arg(pid.to_string()).status(); this.child_pid.set(None); }
            while let Ok((kind, line)) = rx.try_recv() {
                quiet_since = std::time::Instant::now();
                match kind.as_str() {
                    "out" => this.handle_line(&line),
                    "err" => { if !line.trim().is_empty() { eprintln!("claude: {line}"); *last_error.borrow_mut() = line.clone(); } }
                    _ => {
                        if this.child_pid.get() == Some(pid) { this.child_pid.set(None); }
                        this.busy.set(false);
                        this.send.set_sensitive(true);
                        if line != "0" {
                            let detail = last_error.borrow().clone();
                            this.append("system", &format!("Compy stopped ({}). {}", if line == "-1" { "it was interrupted".to_string() } else { format!("status {line}") }, if detail.is_empty() { "Try again, or start a new conversation with the arrow button.".to_string() } else { detail.clone() }));
                            // A conversation that cannot be resumed starts over next time.
                            if detail.contains("session") || detail.contains("resume") { *this.session.borrow_mut() = None; }
                        } else if !this.replied.get() {
                            this.append("system", "Compy finished without saying anything. Ask again.");
                        }
                        this.app.with_current(|p| p.refresh());
                        // Anything typed meanwhile goes now.
                        let next = { let mut q = this.queue.borrow_mut(); if q.is_empty() { None } else { Some(q.remove(0)) } };
                        if let Some(message) = next { let t = this.clone(); glib::idle_add_local_once(move || t.send_now(message)); }
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
                        Some("text") => { if let Some(t) = block.get("text").and_then(Value::as_str) { if !t.trim().is_empty() { self.replied.set(true); self.append("claude", t.trim()); } } }
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
