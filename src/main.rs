//! `compositor [project.comp | file.psd | image ...]` opens the app; `compositor info <project.comp>` lists the layer tree;
//! `compositor render <project.comp> <out.png>` flattens the project to a PNG.

use anyhow::{Context as _, Result, bail};
use compositor::format::{self, entries_ordered};
use compositor::{png_io, render, ui};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn main() {
    // Paths need not be UTF-8; only the options are matched as text.
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let result = match args.get(1).and_then(|a| a.to_str()) {
        Some("info") if args.len() == 3 => info(Path::new(&args[2])),
        Some("render") if args.len() == 4 => render_to(Path::new(&args[2]), Path::new(&args[3])),
        Some("psd") if args.len() == 4 => convert_psd(Path::new(&args[2]), Path::new(&args[3])),
        Some("convert-brushes") if args.len() >= 4 => {
            // Every .gbr, .gih and .abr given, written as one Photoshop brush file.
            let mut set = Vec::new();
            for path in &args[3..] {
                let p = Path::new(path);
                let gimp = p.extension().is_some_and(|e| e.eq_ignore_ascii_case("gbr") || e.eq_ignore_ascii_case("gih"));
                match if gimp { compositor::gbr::load(p) } else { compositor::abr::load(p) } { Ok(tips) => set.extend(tips), Err(e) => eprintln!("skipped {}: {e:#}", p.display()) }
            }
            std::fs::write(&args[2], compositor::brush_set::abr_bytes(&set)).map_err(|e| anyhow::anyhow!("{e}")).map(|_| eprintln!("wrote {} brushes to {}", set.len(), Path::new(&args[2]).display()))
        }
        Some("brushes") if args.len() == 3 => {
            let set = compositor::brush_set::presets();
            std::fs::write(&args[2], compositor::brush_set::abr_bytes(&set)).map_err(|e| anyhow::anyhow!("{e}")).map(|_| eprintln!("wrote {} brushes", set.len()))
        }
        Some("info") | Some("render") | Some("psd") | Some("--help") | Some("-h") => Err(anyhow::anyhow!(USAGE)),
        _ => {
            let mut paths = Vec::new();
            let mut script = ui::Script::default();
            let mut rest = args[1..].iter();
            while let Some(arg) = rest.next() {
                // Option values are text; anything else is a path.
                let mut text = || rest.next().and_then(|a| a.to_str()).map(str::to_string);
                match arg.to_str().unwrap_or("") {
                    "--screenshot" => script.screenshot = rest.next().map(PathBuf::from),
                    "--zoom" => script.zoom = text().and_then(|z| z.parse().ok()),
                    "--wand" => script.wand = text().and_then(|p| { let (x, y) = p.split_once(',')?; Some((x.parse().ok()?, y.parse().ok()?)) }),
                    "--tool" => script.tool = text().and_then(|t| ui::Tool::ALL.into_iter().find(|tool| format!("{tool:?}").to_lowercase() == *t)),
                    "--ellipse" => script.ellipse = true,
                    "--pick-color" => script.pick_color = true,
                    "--pick-brush" => script.pick_brush = true,
                    "--rulers" => script.rulers = true,
                    "--genfill" => script.genfill = true,
                    "--brush-popover" => script.brush_popover = true,
                    "--preview" => script.preview = true,
                    "--text" => script.text = text(),
                    "--effects" => script.effects = true,
                    "--grid" => script.grid = true,
                    "--shortcuts" => script.shortcuts = true,
                    "--type-edit" => script.type_edit = true,
                    "--layer-style" => script.layer_style = true,
                    "--guides" => { if let Some(spec) = text() { for part in spec.split(',') { if let Some(v) = part.strip_prefix('x').and_then(|v| v.parse().ok()) { script.guides.0.push(v); } else if let Some(v) = part.strip_prefix('y').and_then(|v| v.parse().ok()) { script.guides.1.push(v); } } } }
                    "--brush" => script.brush = text(),
                    "--window" => script.window = text().and_then(|v| { let (w, h) = v.split_once('x')?; Some((w.parse().ok()?, h.parse().ok()?)) }),
                    "--blur-mode" => script.blur_mode = text().and_then(|m| m.parse().ok()),
                    "--layer" => script.layer = text(),
                    "--adjustment" => script.adjustment = text(),
                    "--size" => { if let Some(size) = text().and_then(|v| v.parse::<f64>().ok()) { script.brush_size = Some(size); } }
                    "--stroke" => script.stroke = text().map(|s| s.split(';').filter_map(|p| { let (x, y) = p.split_once(',')?; Some((x.parse().ok()?, y.parse().ok()?)) }).collect()).unwrap_or_default(),
                    "--filter" => script.filter = text().and_then(|f| match f.as_str() {
                        "noise" => Some(compositor::filters::Kind::AddNoise), "grain" => Some(compositor::filters::Kind::Grain),
                        "lens" => Some(compositor::filters::Kind::LensCorrection), "gradient" => Some(compositor::filters::Kind::GradientMap),
                        "levels" => Some(compositor::filters::Kind::Levels), "gaussian" => Some(compositor::filters::Kind::GaussianBlur), "background" => Some(compositor::filters::Kind::RemoveBackground),
                        "motion" => Some(compositor::filters::Kind::MotionBlur), _ => None }),
                    _ => paths.push(PathBuf::from(arg)),
                }
            }
            std::process::exit(i32::from(ui::run(paths, script)));
        }
    };
    if let Err(error) = result {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

const USAGE: &str = "usage:\n  compositor [project.comp | file.psd | image.png ...]  open the app\n  compositor info <project.comp>           list the layer tree\n  compositor render <project.comp> <out.png>\n  compositor psd <in.comp|in.psd> <out.psd|out.comp>   convert either way\n  compositor brushes <out.abr>             write the bundled brush set as a Photoshop brush file\n  compositor convert-brushes <out.abr> <tips...>   GIMP .gbr/.gih (or .abr) tips as one Photoshop brush file";

fn info(path: &Path) -> Result<()> {
    let project = format::load(path).with_context(|| format!("loading {}", path.display()))?;
    let m = &project.manifest;
    println!("{}  {}x{} px  {} ppi  format v{}  {} layers", path.display(), m.width, m.height, m.resolution(), m.version, m.layers.len());
    for entry in entries_ordered(&m.layers, true) {
        let l = entry.layer;
        let mut notes = Vec::new();
        if l.is_group() { notes.push("folder".to_string()); }
        if let Some(a) = &l.adjustment { notes.push(format!("adjustment: {}", a.kind)); }
        if l.opacity() != 1.0 { notes.push(format!("{:.0}%", l.opacity() * 100.0)); }
        if l.blend_mode() != format::BlendMode::Normal { notes.push(l.blend_mode().name().to_string()); }
        if l.mask_file.is_some() { notes.push(if l.mask_enabled() { "mask".into() } else { "mask off".into() }); }
        if l.mask_source_id.is_some() { notes.push("clipped".into()); }
        if let Some(img) = project.images.get(&l.id) { notes.push(format!("{}x{}", img.width(), img.height())); }
        let t = &l.transform;
        let placed = format!("at ({:.0}, {:.0}) size {:.0}x{:.0}{}{}", t.origin.0, t.origin.1, t.size.0, t.size.1,
            if t.rotation != 0.0 { format!(" rot {:.1}", t.rotation) } else { String::new() },
            match (t.flip_x, t.flip_y) { (true, true) => " flipped xy", (true, false) => " flipped x", (false, true) => " flipped y", _ => "" });
        println!("{}{} {}  {}{}", "  ".repeat(entry.depth), if entry.visible { "●" } else { "○" }, l.name, placed,
            if notes.is_empty() { String::new() } else { format!("  [{}]", notes.join(", ")) });
    }
    Ok(())
}

fn render_to(input: &Path, output: &Path) -> Result<()> {
    let start = Instant::now();
    let project = format::load(input).with_context(|| format!("loading {}", input.display()))?;
    let loaded = start.elapsed();
    let rendered = render::render(project)?;
    let drawn = start.elapsed();
    for warning in &rendered.warnings { eprintln!("warning: {warning}"); }
    png_io::encode(&rendered.image, output, rendered.resolution).with_context(|| format!("writing {}", output.display()))?;
    println!("{} -> {}  {}x{}  load {:.2?}  render {:.2?}  write {:.2?}", input.display(), output.display(),
        rendered.image.width(), rendered.image.height(), loaded, drawn - loaded, start.elapsed() - drawn);
    if output.extension().is_none() { bail!("output has no extension"); }
    Ok(())
}

/// Converts between the two formats by extension; notes about what was not carried over go to stderr.
fn convert_psd(input: &Path, output: &Path) -> anyhow::Result<()> {
    let started = Instant::now();
    let to_psd = output.extension().is_some_and(|e| e.eq_ignore_ascii_case("psd"));
    let notes = if to_psd {
        let mut document = ui::open_document(input)?.0.document;
        document.export_psd(output)?
    } else {
        let (mut document, notes) = compositor::document::Document::open_psd(input)?;
        document.save(output)?;
        notes
    };
    for note in notes { eprintln!("note: {note}"); }
    eprintln!("wrote {} in {:.2}s", output.display(), started.elapsed().as_secs_f64());
    Ok(())
}
