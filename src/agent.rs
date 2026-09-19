//! The agent surface. Compy, while running, answers tool calls on a local socket; `compositor mcp` is a
//! Model Context Protocol server over stdio that forwards those calls, so Claude Code (and the in-app
//! assistant panel, which drives Claude Code) can see and edit the open document. One core, three
//! surfaces: the GUI, this socket, and the MCP server all run the same document operations.

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

/// Where the running app listens.
pub fn socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join(format!("compositor-agent-{}.sock", std::env::var("USER").unwrap_or_else(|_| "user".into())))
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Request { pub id: u64, pub tool: String, pub args: Value }

#[derive(Serialize, Deserialize, Debug)]
pub struct Response { pub id: u64, pub ok: bool, pub result: Value }

/// One tool the app offers: its name, what it does, and its JSON Schema for arguments.
pub struct ToolSpec { pub name: &'static str, pub description: &'static str, pub schema: Value }

fn obj(props: Value, required: &[&str]) -> Value { json!({"type": "object", "properties": props, "required": required}) }

/// The catalog, in the order a reader should learn them. Descriptions are written for the model.
pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec { name: "state", description: "What is open in Compy: document size, every layer (id, name, kind, visible, opacity, blend, active, selected), the selection's bounds, the tool and zoom. Call this first.", schema: obj(json!({}), &[]) },
        ToolSpec { name: "snapshot", description: "A picture of the canvas as it looks now (the composite of visible layers, at most 1024 px on the long side). The selection, if any, is drawn as a red outline.", schema: obj(json!({"selection_outline": {"type": "boolean", "description": "Draw the selection outline (default true)"}}), &[]) },
        ToolSpec { name: "select_rectangle", description: "Select a rectangle in document pixels (replaces the selection).", schema: obj(json!({"x": {"type": "number"}, "y": {"type": "number"}, "width": {"type": "number"}, "height": {"type": "number"}, "ellipse": {"type": "boolean"}}), &["x", "y", "width", "height"]) },
        ToolSpec { name: "select_all", description: "Select the whole canvas.", schema: obj(json!({}), &[]) },
        ToolSpec { name: "deselect", description: "Drop the selection.", schema: obj(json!({}), &[]) },
        ToolSpec { name: "invert_selection", description: "Invert the selection.", schema: obj(json!({}), &[]) },
        ToolSpec { name: "select_layer_pixels", description: "Select the active layer's opaque pixels (its shape).", schema: obj(json!({}), &[]) },
        ToolSpec { name: "feather_selection", description: "Soften the selection edge by a radius in pixels.", schema: obj(json!({"radius": {"type": "number"}}), &["radius"]) },
        ToolSpec { name: "select_layer", description: "Make a layer active, by id or name.", schema: obj(json!({"layer": {"type": "string", "description": "Layer id or exact name"}}), &["layer"]) },
        ToolSpec { name: "new_layer", description: "Add an empty layer above the active one.", schema: obj(json!({"name": {"type": "string"}}), &[]) },
        ToolSpec { name: "duplicate_layer", description: "Duplicate the active layer; with a selection, only the selected pixels (Layer via Copy).", schema: obj(json!({}), &[]) },
        ToolSpec { name: "delete_layer", description: "Delete the active layer.", schema: obj(json!({}), &[]) },
        ToolSpec { name: "rename_layer", description: "Rename the active layer.", schema: obj(json!({"name": {"type": "string"}}), &["name"]) },
        ToolSpec { name: "set_layer", description: "Change the active layer's visibility, opacity (0 to 1) or blend mode (Normal, Multiply, Screen, Overlay, Darken, Lighten, Difference, Color Dodge, Color Burn, Hue, Saturation, Color, Luminosity).", schema: obj(json!({"visible": {"type": "boolean"}, "opacity": {"type": "number"}, "blend": {"type": "string"}}), &[]) },
        ToolSpec { name: "place_layer", description: "Move, resize or rotate the active layer: its top-left corner, size and rotation in document pixels and degrees. Omitted fields keep their values.", schema: obj(json!({"x": {"type": "number"}, "y": {"type": "number"}, "width": {"type": "number"}, "height": {"type": "number"}, "rotation": {"type": "number"}}), &[]) },
        ToolSpec { name: "reorder_layer", description: "Move the active layer up, down, to the top or to the bottom of its siblings.", schema: obj(json!({"direction": {"type": "string", "enum": ["up", "down", "top", "bottom"]}}), &["direction"]) },
        ToolSpec { name: "merge_visible", description: "Merge every visible layer into one.", schema: obj(json!({}), &[]) },
        ToolSpec { name: "stamp_visible", description: "A new layer holding everything visible, keeping the layers.", schema: obj(json!({}), &[]) },
        ToolSpec { name: "fill", description: "Fill the selection (or the whole active layer) with a color such as #ff0000.", schema: obj(json!({"color": {"type": "string"}}), &["color"]) },
        ToolSpec { name: "clear", description: "Make the selected pixels transparent.", schema: obj(json!({}), &[]) },
        ToolSpec { name: "remove_background", description: "Cut the subject of the active layer out of its background with a mask (runs a local model).", schema: obj(json!({}), &[]) },
        ToolSpec { name: "invert", description: "Invert the active layer's colors.", schema: obj(json!({}), &[]) },
        ToolSpec { name: "desaturate", description: "Turn the active layer gray.", schema: obj(json!({}), &[]) },
        ToolSpec { name: "auto_levels", description: "Auto Tone, Auto Contrast or Auto Color on the active layer.", schema: obj(json!({"mode": {"type": "string", "enum": ["tone", "contrast", "color"]}}), &["mode"]) },
        ToolSpec { name: "filter", description: "Run a filter on the active layer: gaussian_blur (radius), motion_blur (angle, distance), add_noise (amount), levels (black, white, gamma), exposure (exposure), hue_saturation (hue, saturation, lightness), color_balance (shadows, midtones, highlights: each [cyan_red, magenta_green, yellow_blue] from -100 to 100).", schema: obj(json!({"kind": {"type": "string"}, "radius": {"type": "number"}, "angle": {"type": "number"}, "distance": {"type": "number"}, "amount": {"type": "number"}, "black": {"type": "number"}, "white": {"type": "number"}, "gamma": {"type": "number"}, "exposure": {"type": "number"}, "hue": {"type": "number"}, "saturation": {"type": "number"}, "lightness": {"type": "number"}, "shadows": {"type": "array"}, "midtones": {"type": "array"}, "highlights": {"type": "array"}}), &["kind"]) },
        ToolSpec { name: "adjustment_layer", description: "Add a non-destructive adjustment layer above the active layer: levels (black, white, gamma), exposure (exposure), hue_saturation (hue, saturation, lightness), gradient_map, curves. It affects every layer below unless clip is true (then only the layer directly below).", schema: obj(json!({"kind": {"type": "string"}, "clip": {"type": "boolean"}, "black": {"type": "number"}, "white": {"type": "number"}, "gamma": {"type": "number"}, "exposure": {"type": "number"}, "hue": {"type": "number"}, "saturation": {"type": "number"}, "lightness": {"type": "number"}}), &["kind"]) },
        ToolSpec { name: "text_layer", description: "Add a type layer. Position is the top-left corner in document pixels; size in pixels; color such as #ffffff; align left, center or right.", schema: obj(json!({"text": {"type": "string"}, "x": {"type": "number"}, "y": {"type": "number"}, "size": {"type": "number"}, "family": {"type": "string"}, "color": {"type": "string"}, "bold": {"type": "boolean"}, "italic": {"type": "boolean"}, "align": {"type": "string"}}), &["text"]) },
        ToolSpec { name: "set_text", description: "Change the active type layer's text or style (same fields as text_layer).", schema: obj(json!({"text": {"type": "string"}, "size": {"type": "number"}, "family": {"type": "string"}, "color": {"type": "string"}, "bold": {"type": "boolean"}, "italic": {"type": "boolean"}, "align": {"type": "string"}}), &[]) },
        ToolSpec { name: "shape_layer", description: "Add a rectangle or ellipse layer in a color.", schema: obj(json!({"kind": {"type": "string", "enum": ["rectangle", "ellipse"]}, "x": {"type": "number"}, "y": {"type": "number"}, "width": {"type": "number"}, "height": {"type": "number"}, "color": {"type": "string"}, "corner_radius": {"type": "number"}}), &["x", "y", "width", "height"]) },
        ToolSpec { name: "layer_style", description: "Set the active layer's effects. Pass any of: drop_shadow {color, opacity, angle, distance, size}, inner_shadow {same}, outer_glow {color, opacity, size}, inner_glow {same}, bevel {style 0 inner 1 outer 2 emboss, depth, size, angle, altitude}, stroke {size, position 0 outside 1 inside 2 center, color, opacity}, color_overlay {color, opacity}. Colors as #rrggbb, opacities 0 to 1. An empty object clears the style.", schema: obj(json!({"drop_shadow": {"type": "object"}, "inner_shadow": {"type": "object"}, "outer_glow": {"type": "object"}, "inner_glow": {"type": "object"}, "bevel": {"type": "object"}, "stroke": {"type": "object"}, "color_overlay": {"type": "object"}}), &[]) },
        ToolSpec { name: "canvas_size", description: "Change the canvas size without scaling the layers; anchor 0 to 8 reading left to right, top to bottom (4 is the center).", schema: obj(json!({"width": {"type": "integer"}, "height": {"type": "integer"}, "anchor": {"type": "integer"}}), &["width", "height"]) },
        ToolSpec { name: "image_size", description: "Scale the whole image to a new size.", schema: obj(json!({"width": {"type": "integer"}, "height": {"type": "integer"}}), &["width", "height"]) },
        ToolSpec { name: "flip", description: "Flip the active layer, or the whole canvas.", schema: obj(json!({"axis": {"type": "string", "enum": ["horizontal", "vertical"]}, "canvas": {"type": "boolean"}}), &["axis"]) },
        ToolSpec { name: "crop_to_selection", description: "Crop the canvas to the selection.", schema: obj(json!({}), &[]) },
        ToolSpec { name: "generative_fill", description: "Fill the selection with generated content from a text prompt (fal.ai, costs money; the result comes back as a masked layer). Returns the estimated cost when estimate_only is true.", schema: obj(json!({"prompt": {"type": "string"}, "count": {"type": "integer"}, "estimate_only": {"type": "boolean"}}), &["prompt"]) },
        ToolSpec { name: "generative_expand", description: "Grow the canvas by pixels on each side and fill the new area from a text prompt (fal.ai, costs money).", schema: obj(json!({"left": {"type": "integer"}, "right": {"type": "integer"}, "top": {"type": "integer"}, "bottom": {"type": "integer"}, "prompt": {"type": "string"}}), &["prompt"]) },
        ToolSpec { name: "export", description: "Write the visible composite to a PNG or JPEG file (path with .png or .jpg).", schema: obj(json!({"path": {"type": "string"}, "quality": {"type": "number"}}), &["path"]) },
        ToolSpec { name: "save", description: "Save the document (as a .comp project); a path is needed the first time.", schema: obj(json!({"path": {"type": "string"}}), &[]) },
        ToolSpec { name: "open", description: "Open an image, Photoshop file or .comp project in a new tab.", schema: obj(json!({"path": {"type": "string"}}), &["path"]) },
        ToolSpec { name: "new_document", description: "A new blank document in a new tab.", schema: obj(json!({"width": {"type": "integer"}, "height": {"type": "integer"}}), &["width", "height"]) },
        ToolSpec { name: "undo", description: "Undo the last step.", schema: obj(json!({}), &[]) },
        ToolSpec { name: "redo", description: "Redo.", schema: obj(json!({}), &[]) },
        ToolSpec { name: "zoom", description: "Fit the picture, or zoom to a percentage.", schema: obj(json!({"percent": {"type": "number"}}), &[]) },
    ]
}

/// Sends one call to the running app and waits for its answer.
pub fn call(tool: &str, args: Value) -> Result<Value> {
    let path = socket_path();
    let mut stream = std::os::unix::net::UnixStream::connect(&path).with_context(|| format!("Compy is not running (no socket at {})", path.display()))?;
    let request = Request { id: 1, tool: tool.to_string(), args };
    let mut line = serde_json::to_string(&request)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut reader = BufReader::new(stream);
    let mut reply = String::new();
    reader.read_line(&mut reply).context("reading the app's answer")?;
    let response: Response = serde_json::from_str(reply.trim()).context("parsing the app's answer")?;
    if response.ok { Ok(response.result) } else { bail!("{}", response.result.as_str().unwrap_or("the tool failed")) }
}

/// `compositor mcp`: a Model Context Protocol server on stdin and stdout that forwards to the app.
pub fn run_mcp() -> Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() { continue; }
        let message: Value = match serde_json::from_str(&line) { Ok(v) => v, Err(_) => continue };
        let id = message.get("id").cloned();
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let reply = match method {
            "initialize" => json!({"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}, "serverInfo": {"name": "compy", "version": env!("CARGO_PKG_VERSION")}}),
            "ping" => json!({}),
            "tools/list" => json!({"tools": tools().iter().map(|t| json!({"name": t.name, "description": t.description, "inputSchema": t.schema})).collect::<Vec<_>>()}),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match call(name, args) {
                    Ok(result) => {
                        // A snapshot comes back as a picture; everything else as text.
                        if let Some(png) = result.get("png_base64").and_then(Value::as_str) {
                            let text = result.get("text").and_then(Value::as_str).unwrap_or("The canvas.");
                            json!({"content": [{"type": "text", "text": text}, {"type": "image", "data": png, "mimeType": "image/png"}]})
                        } else {
                            let text = match &result { Value::String(s) => s.clone(), other => serde_json::to_string_pretty(other).unwrap_or_default() };
                            json!({"content": [{"type": "text", "text": text}]})
                        }
                    }
                    Err(e) => json!({"content": [{"type": "text", "text": format!("{e:#}")}], "isError": true}),
                }
            }
            _ => {
                // Notifications have no id and need no answer; unknown requests get an error.
                if id.is_none() { continue; }
                let error = json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("unknown method {method}")}});
                writeln!(out, "{error}")?;
                out.flush()?;
                continue;
            }
        };
        if id.is_none() { continue; }
        writeln!(out, "{}", json!({"jsonrpc": "2.0", "id": id, "result": reply}))?;
        out.flush()?;
    }
    Ok(())
}

/// A color like #ff8800 as red, green, blue from 0 to 1.
pub fn parse_color(text: &str) -> Option<[f64; 3]> {
    let hex = text.trim().trim_start_matches('#');
    if hex.len() != 6 { return None; }
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some([((v >> 16) & 255) as f64 / 255.0, ((v >> 8) & 255) as f64 / 255.0, (v & 255) as f64 / 255.0])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_and_colors() {
        let names: Vec<&str> = tools().iter().map(|t| t.name).collect();
        assert!(names.contains(&"state") && names.contains(&"snapshot") && names.contains(&"generative_fill"));
        assert_eq!(names.len(), names.iter().collect::<std::collections::HashSet<_>>().len(), "unique names");
        assert_eq!(parse_color("#ff8000"), Some([1.0, 128.0 / 255.0, 0.0]));
        assert_eq!(parse_color("nope"), None);
        assert!(matches!(call("state", json!({})), Err(_)), "no app running: a clear error");
    }
}
