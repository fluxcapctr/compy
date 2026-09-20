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
        ToolSpec { name: "filter", description: "Run a filter on the active layer: gaussian_blur (radius), motion_blur (angle, distance), add_noise (amount), unsharp_mask (amount %, radius, threshold), smart_sharpen (amount %, radius, noise %; sharpens brightness only), levels (black, white, gamma), exposure (exposure), hue_saturation (hue, saturation, lightness), color_balance (shadows, midtones, highlights: each [cyan_red, magenta_green, yellow_blue] from -100 to 100), brightness_contrast (brightness -150 to 150, contrast -50 to 100), vibrance (vibrance, saturation -100 to 100), black_white (reds, yellows, greens, cyans, blues, magentas in percent), photo_filter (color #rrggbb, density 1 to 100, preserve_luminosity), threshold (level 1 to 255), posterize (levels 2 to 255), shadows_highlights (shadow_amount, highlight_amount 0 to 100, radius), selective_color (range Reds/Yellows/Greens/Cyans/Blues/Magentas/Whites/Neutrals/Blacks, cyan, magenta, yellow, black -100 to 100, relative), channel_mixer (red, green, blue: each [from_red, from_green, from_blue, constant] in percent; monochrome), high_pass (radius), radial_blur (amount, method spin or zoom).", schema: obj(json!({"kind": {"type": "string"}, "range": {"type": "string"}, "cyan": {"type": "number"}, "magenta": {"type": "number"}, "yellow": {"type": "number"}, "black_amount": {"type": "number"}, "shadow_amount": {"type": "number"}, "highlight_amount": {"type": "number"}, "relative": {"type": "boolean"}, "red": {"type": "array"}, "green": {"type": "array"}, "blue": {"type": "array"}, "monochrome": {"type": "boolean"}, "method": {"type": "string"}, "radius": {"type": "number"}, "angle": {"type": "number"}, "distance": {"type": "number"}, "amount": {"type": "number"}, "threshold": {"type": "number"}, "noise": {"type": "number"}, "black": {"type": "number"}, "white": {"type": "number"}, "gamma": {"type": "number"}, "exposure": {"type": "number"}, "hue": {"type": "number"}, "saturation": {"type": "number"}, "lightness": {"type": "number"}, "shadows": {"type": "array"}, "midtones": {"type": "array"}, "highlights": {"type": "array"}, "brightness": {"type": "number"}, "contrast": {"type": "number"}, "vibrance": {"type": "number"}, "reds": {"type": "number"}, "yellows": {"type": "number"}, "greens": {"type": "number"}, "cyans": {"type": "number"}, "blues": {"type": "number"}, "magentas": {"type": "number"}, "color": {"type": "string"}, "density": {"type": "number"}, "preserve_luminosity": {"type": "boolean"}, "level": {"type": "number"}, "levels": {"type": "number"}}), &["kind"]) },
        ToolSpec { name: "adjustment_layer", description: "Add a non-destructive adjustment layer above the active layer: levels (black, white, gamma), exposure (exposure), hue_saturation (hue, saturation, lightness), gradient_map, curves, brightness_contrast (brightness, contrast), vibrance (vibrance, saturation), black_white (reds, yellows, greens, cyans, blues, magentas), photo_filter (color, density, preserve_luminosity), threshold (level), posterize (levels), shadows_highlights (shadows, highlights, radius), selective_color (range, cyan, magenta, yellow, black, relative), channel_mixer (red, green, blue rows, monochrome). It affects every layer below unless clip is true (then only the layer directly below).", schema: obj(json!({"kind": {"type": "string"}, "clip": {"type": "boolean"}, "range": {"type": "string"}, "cyan": {"type": "number"}, "magenta": {"type": "number"}, "yellow": {"type": "number"}, "relative": {"type": "boolean"}, "red": {"type": "array"}, "green": {"type": "array"}, "blue": {"type": "array"}, "monochrome": {"type": "boolean"}, "shadows": {"type": "number"}, "highlights": {"type": "number"}, "radius": {"type": "number"}, "black": {"type": "number"}, "white": {"type": "number"}, "gamma": {"type": "number"}, "exposure": {"type": "number"}, "hue": {"type": "number"}, "saturation": {"type": "number"}, "lightness": {"type": "number"}, "brightness": {"type": "number"}, "contrast": {"type": "number"}, "vibrance": {"type": "number"}, "reds": {"type": "number"}, "yellows": {"type": "number"}, "greens": {"type": "number"}, "cyans": {"type": "number"}, "blues": {"type": "number"}, "magentas": {"type": "number"}, "color": {"type": "string"}, "density": {"type": "number"}, "preserve_luminosity": {"type": "boolean"}, "level": {"type": "number"}, "levels": {"type": "number"}}), &["kind"]) },
        ToolSpec { name: "text_layer", description: "Add a type layer. Position is the top-left corner in document pixels; size in pixels; color such as #ffffff; align left, center or right; width makes paragraph text that wraps at that many pixels (omit for a single line).", schema: obj(json!({"text": {"type": "string"}, "x": {"type": "number"}, "y": {"type": "number"}, "size": {"type": "number"}, "family": {"type": "string"}, "color": {"type": "string"}, "bold": {"type": "boolean"}, "italic": {"type": "boolean"}, "align": {"type": "string"}, "width": {"type": "number"}}), &["text"]) },
        ToolSpec { name: "set_text", description: "Change the active type layer's text or style (same fields as text_layer; width 0 turns paragraph text back into a single line).", schema: obj(json!({"text": {"type": "string"}, "size": {"type": "number"}, "family": {"type": "string"}, "color": {"type": "string"}, "bold": {"type": "boolean"}, "italic": {"type": "boolean"}, "align": {"type": "string"}, "width": {"type": "number"}}), &[]) },
        ToolSpec { name: "shape_layer", description: "Add a rectangle or ellipse layer in a color.", schema: obj(json!({"kind": {"type": "string", "enum": ["rectangle", "ellipse"]}, "x": {"type": "number"}, "y": {"type": "number"}, "width": {"type": "number"}, "height": {"type": "number"}, "color": {"type": "string"}, "corner_radius": {"type": "number"}}), &["x", "y", "width", "height"]) },
        ToolSpec { name: "layer_style", description: "Set the active layer's effects. Pass any of: drop_shadow {color, opacity, angle, distance, size}, inner_shadow {same}, outer_glow {color, opacity, size}, inner_glow {same}, bevel {style 0 inner 1 outer 2 emboss, depth, size, angle, altitude}, stroke {size, position 0 outside 1 inside 2 center, color, opacity}, color_overlay {color, opacity}, gradient_overlay {stops [{position, color, alpha}], angle, opacity, radial, reverse}, pattern_overlay {pattern (a saved pattern's name), scale, opacity}, blend_if {this_black, this_white, under_black, under_white (0 to 255: the layer shows only where its own tones and the tones beneath fall in range), feather}. Colors as #rrggbb, opacities 0 to 1. An empty object clears the style.", schema: obj(json!({"drop_shadow": {"type": "object"}, "inner_shadow": {"type": "object"}, "outer_glow": {"type": "object"}, "inner_glow": {"type": "object"}, "bevel": {"type": "object"}, "stroke": {"type": "object"}, "color_overlay": {"type": "object"}, "gradient_overlay": {"type": "object"}, "pattern_overlay": {"type": "object"}, "blend_if": {"type": "object"}}), &[]) },
        ToolSpec { name: "gradient_fill", description: "Draw a gradient on the active layer (inside the selection if any) from start to end in document pixels. shape linear, radial, angle, reflected or diamond. stops: a list of {position 0 to 1, color #rrggbb, alpha 0 to 1 (optional)}; two or more. opacity 0 to 1.", schema: obj(json!({"start": {"type": "array"}, "end": {"type": "array"}, "shape": {"type": "string"}, "stops": {"type": "array"}, "opacity": {"type": "number"}}), &["start", "end", "stops"]) },
        ToolSpec { name: "brush_stroke", description: "Paint one brush stroke on the active layer along points in document pixels ([[x, y], ...], two or more; a curve needs several). kind: paint (default), erase, dodge, burn, saturate, desaturate, blur, heal, or pattern (stamps a saved pattern named in pattern). diameter in pixels, hardness 0 to 1, opacity 0 to 1 (the exposure for dodge and burn), color #rrggbb (paint only), tip: a textured tip by name from the state's brush_tips (Chalk, Charcoal, Grain, Sponge, Spatter, Watercolor, Stipple, Splat and more; omit for a round tip), spacing 0.02 to 2 as a fraction of the diameter, range shadows, midtones or highlights for dodge and burn. Textures read best as several short strokes at low opacity with a textured tip. One undo step.", schema: obj(json!({"points": {"type": "array"}, "kind": {"type": "string"}, "diameter": {"type": "number"}, "hardness": {"type": "number"}, "opacity": {"type": "number"}, "color": {"type": "string"}, "tip": {"type": "string"}, "spacing": {"type": "number"}, "range": {"type": "string"}, "pattern": {"type": "string"}}), &["points"]) },
        ToolSpec { name: "define_pattern", description: "Save the selection's part of the picture (or the whole active layer) as a named pattern for fill_pattern.", schema: obj(json!({"name": {"type": "string"}}), &["name"]) },
        ToolSpec { name: "fill_pattern", description: "Fill the selection (or the whole active layer) with a saved pattern, tiled; scale 1 is the pattern's own size; opacity 0 to 1. The state lists the pattern names.", schema: obj(json!({"name": {"type": "string"}, "scale": {"type": "number"}, "opacity": {"type": "number"}}), &["name"]) },
        ToolSpec { name: "stroke_selection", description: "Outline the selection with a line on the active layer (Edit > Stroke): width in pixels, color such as #ffffff, position inside, center or outside, opacity 0 to 1.", schema: obj(json!({"width": {"type": "number"}, "color": {"type": "string"}, "position": {"type": "string", "enum": ["inside", "center", "outside"]}, "opacity": {"type": "number"}}), &["width", "color"]) },
        ToolSpec { name: "select_color_range", description: "Select every pixel near a color (Select > Color Range): color such as #3a7bd5, fuzziness 0 to 200 (how far off still counts, 40 is typical), all_layers true samples the composite, false the active layer.", schema: obj(json!({"color": {"type": "string"}, "fuzziness": {"type": "number"}, "all_layers": {"type": "boolean"}}), &["color"]) },
        ToolSpec { name: "align_layers", description: "Align the selected layers (or the active one) to the selection's bounds, or to the canvas when nothing is selected: edge left, center, right, top, middle or bottom. Use select_layer with several names first to align a group.", schema: obj(json!({"edge": {"type": "string", "enum": ["left", "center", "right", "top", "middle", "bottom"]}}), &["edge"]) },
        ToolSpec { name: "distribute_layers", description: "Space three or more selected layers evenly by their centers, horizontal or vertical; the outer two stay put.", schema: obj(json!({"axis": {"type": "string", "enum": ["horizontal", "vertical"]}}), &["axis"]) },
        ToolSpec { name: "canvas_size", description: "Change the canvas size without scaling the layers; anchor 0 to 8 reading left to right, top to bottom (4 is the center).", schema: obj(json!({"width": {"type": "integer"}, "height": {"type": "integer"}, "anchor": {"type": "integer"}}), &["width", "height"]) },
        ToolSpec { name: "image_size", description: "Scale the whole image to a new size.", schema: obj(json!({"width": {"type": "integer"}, "height": {"type": "integer"}}), &["width", "height"]) },
        ToolSpec { name: "flip", description: "Flip the active layer, or the whole canvas.", schema: obj(json!({"axis": {"type": "string", "enum": ["horizontal", "vertical"]}, "canvas": {"type": "boolean"}}), &["axis"]) },
        ToolSpec { name: "new_artboard", description: "Add an artboard: a named frame on the canvas whose layers clip to it, placed to the right of the last board unless x and y are given. Size by preset name (the export_sizes presets, such as 'story or reel') or width and height. background #rrggbb (default white), or transparent true. The canvas grows to hold it. Select a layer inside a board before adding layers to it.", schema: obj(json!({"name": {"type": "string"}, "preset": {"type": "string"}, "width": {"type": "number"}, "height": {"type": "number"}, "x": {"type": "number"}, "y": {"type": "number"}, "background": {"type": "string"}, "transparent": {"type": "boolean"}}), &[]) },
        ToolSpec { name: "artboard_from_layers", description: "Wrap the selected layers (or the active one) in a new artboard drawn around their bounds.", schema: obj(json!({"name": {"type": "string"}}), &[]) },
        ToolSpec { name: "move_artboard", description: "Move or resize an artboard by name or id: its layers come along.", schema: obj(json!({"artboard": {"type": "string"}, "x": {"type": "number"}, "y": {"type": "number"}, "width": {"type": "number"}, "height": {"type": "number"}}), &["artboard"]) },
        ToolSpec { name: "export_artboards", description: "Write every artboard's picture into a folder, one file each named after the board; PNG, or JPEG at a quality.", schema: obj(json!({"folder": {"type": "string"}, "format": {"type": "string"}, "quality": {"type": "number"}}), &["folder"]) },
        ToolSpec { name: "rotate_canvas", description: "Turn the whole canvas by degrees (clockwise positive; 90, -90, 180 or any angle, the canvas growing to hold the turned picture).", schema: obj(json!({"degrees": {"type": "number"}}), &["degrees"]) },
        ToolSpec { name: "straighten", description: "Level the picture: give two points along a horizon or an edge that should be horizontal (or vertical) and the canvas turns and crops to make it so.", schema: obj(json!({"a": {"type": "array"}, "b": {"type": "array"}}), &["a", "b"]) },
        ToolSpec { name: "export_layers", description: "Write every visible layer as its own PNG into a folder, trimmed to its bounds (trim true, default) or on the full canvas.", schema: obj(json!({"folder": {"type": "string"}, "trim": {"type": "boolean"}}), &["folder"]) },
        ToolSpec { name: "crop_to_selection", description: "Crop the canvas to the selection.", schema: obj(json!({}), &[]) },
        ToolSpec { name: "generative_fill", description: "Fill the selection with generated content from a text prompt (fal.ai, costs money; the result comes back as a masked layer). Returns the estimated cost when estimate_only is true.", schema: obj(json!({"prompt": {"type": "string"}, "count": {"type": "integer"}, "estimate_only": {"type": "boolean"}}), &["prompt"]) },
        ToolSpec { name: "generative_expand", description: "Grow the canvas by pixels on each side and fill the new area from a text prompt (fal.ai, costs money).", schema: obj(json!({"left": {"type": "integer"}, "right": {"type": "integer"}, "top": {"type": "integer"}, "bottom": {"type": "integer"}, "prompt": {"type": "string"}}), &["prompt"]) },
        ToolSpec { name: "generative_edit", description: "Change the active layer's pixels (or just the selected part of them) by instruction, such as 'make the shirt red' or 'give her a moustache', through an image edit model on fal.ai. The result lands as a new layer over the original, so both stay. model: 'nano banana' (Google, the default), 'gpt image' (OpenAI), 'flux' (Kontext), or a full fal id. transparent: true asks for a cutout on a transparent background (GPT Image does this; it is used automatically).", schema: obj(json!({"prompt": {"type": "string"}, "count": {"type": "integer"}, "model": {"type": "string"}, "transparent": {"type": "boolean"}}), &["prompt"]) },
        ToolSpec { name: "generate_image", description: "Make a new picture from a text prompt on fal.ai as a new layer; size defaults to the canvas. model: 'nano banana' (Google, the default), 'gpt image' (OpenAI), 'flux', or a full fal id. transparent: true makes a cutout (an object, a logo, a person) on a transparent background, the right choice for anything that will sit over other layers; GPT Image is used automatically for it.", schema: obj(json!({"prompt": {"type": "string"}, "width": {"type": "integer"}, "height": {"type": "integer"}, "model": {"type": "string"}, "transparent": {"type": "boolean"}}), &["prompt"]) },
        ToolSpec { name: "upscale", description: "Four times the pixels of the active layer through an upscaler on fal.ai (about $0.02); the sharper result lands as a new layer at the same place.", schema: obj(json!({}), &[]) },
        ToolSpec { name: "relight", description: "Relight the active layer on fal.ai (about $0.05) with a lighting style: studio, golden_hour, blue_hour, dramatic, backlight, rim_light, side_light, candlelight, moonlight, spotlight or ambient. Lands as a new layer at the same place.", schema: obj(json!({"style": {"type": "string"}}), &["style"]) },
        ToolSpec { name: "export_sizes", description: "Write the document at several sizes into a folder (File > Export Sizes). sizes: a list of preset names (instagram post, instagram portrait, story or reel, facebook post, x post, linkedin post, youtube thumbnail, pinterest pin, leaderboard, medium rectangle, wide skyscraper, billboard, half page, hd, 4k, square 2048, letter 300 ppi, a4 300 ppi, tabloid 300 ppi) or {name, width, height} objects. fit: reframe (default; the picture fills, type and elements keep their places inside a margin), fill (cover and crop) or pad (fit with a background color). format png (default) or jpeg with quality 0 to 1; background #rrggbb for pad and jpeg. Returns the files written. With as_artboards true, no files: each size becomes an artboard in the document instead, to the right of the last board, for fixing by hand before export_artboards.", schema: obj(json!({"folder": {"type": "string"}, "sizes": {"type": "array"}, "fit": {"type": "string"}, "format": {"type": "string"}, "quality": {"type": "number"}, "background": {"type": "string"}, "as_artboards": {"type": "boolean"}}), &["sizes"]) },
        ToolSpec { name: "export", description: "Write the visible composite to a PNG, JPEG, WebP, GIF or AVIF file (by the path's extension; quality applies to JPEG and AVIF).", schema: obj(json!({"path": {"type": "string"}, "quality": {"type": "number"}}), &["path"]) },
        ToolSpec { name: "save", description: "Save the document (as a .comp project); a path is needed the first time.", schema: obj(json!({"path": {"type": "string"}}), &[]) },
        ToolSpec { name: "open", description: "Open an image, Photoshop file or .comp project in a new tab.", schema: obj(json!({"path": {"type": "string"}}), &["path"]) },
        ToolSpec { name: "new_document", description: "A new blank document in a new tab.", schema: obj(json!({"width": {"type": "integer"}, "height": {"type": "integer"}}), &["width", "height"]) },
        ToolSpec { name: "undo", description: "Undo the last step.", schema: obj(json!({}), &[]) },
        ToolSpec { name: "redo", description: "Redo.", schema: obj(json!({}), &[]) },
        ToolSpec { name: "zoom", description: "Fit the picture, or zoom to a percentage.", schema: obj(json!({"percent": {"type": "number"}}), &[]) },
    ]
}

/// How long a client waits for one call, including a generation job (the app abandons jobs after
/// `JOB_TIMEOUT_SECONDS`).
pub const JOB_TIMEOUT_SECONDS: u64 = 600;
pub const CALL_TIMEOUT_SECONDS: u64 = JOB_TIMEOUT_SECONDS + 60;
static NEXT_CALL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Sends one call to the running app and waits for its answer.
pub fn call(tool: &str, args: Value) -> Result<Value> {
    let path = socket_path();
    let mut stream = std::os::unix::net::UnixStream::connect(&path).with_context(|| format!("Compy is not running (no socket at {})", path.display()))?;
    // A generation can take minutes in fal's queue; the app gives up on a job after ten, so wait a little longer.
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(CALL_TIMEOUT_SECONDS)));
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(30)));
    let id = std::process::id() as u64 * 1000 + NEXT_CALL.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 1000;
    let request = Request { id, tool: tool.to_string(), args };
    let mut line = serde_json::to_string(&request)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut reader = BufReader::new(stream);
    let mut reply = String::new();
    let n = reader.read_line(&mut reply).context("reading the app's answer (it may be busy or gone)")?;
    if n == 0 { bail!("Compy closed the connection without answering"); }
    let response: Response = serde_json::from_str(reply.trim()).context("parsing the app's answer")?;
    if response.id != id && response.id != 0 { bail!("Compy answered a different request ({} instead of {id})", response.id); }
    if response.ok { Ok(response.result) } else { bail!("{}", response.result.as_str().unwrap_or("the tool failed")) }
}

/// `compositor mcp`: a Model Context Protocol server on stdin and stdout that forwards to the app.
pub fn run_mcp() -> Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() { continue; }
        let message: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                // JSON-RPC: a line that does not parse gets a parse error with a null id.
                writeln!(out, "{}", json!({"jsonrpc": "2.0", "id": Value::Null, "error": {"code": -32700, "message": format!("parse error: {e}")}}))?;
                out.flush()?;
                continue;
            }
        };
        let id = message.get("id").cloned();
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        if method == "tools/call" && (params.get("name").and_then(Value::as_str).is_none_or(str::is_empty) || params.get("arguments").is_some_and(|a| !a.is_object() && !a.is_null())) {
            if id.is_none() { continue; }
            writeln!(out, "{}", json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": "tools/call needs a tool name and an arguments object"}}))?;
            out.flush()?;
            continue;
        }
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
        // Only meaningful when no Compy is running on this desktop; with one up the call succeeds.
        if !socket_path().exists() { assert!(matches!(call("state", json!({})), Err(_)), "no app running: a clear error"); }
    }
}
