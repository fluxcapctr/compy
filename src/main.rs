//! `compositor [project.comp ...]` opens the app; `compositor info <project.comp>` lists the layer tree;
//! `compositor render <project.comp> <out.png>` flattens the project to a PNG.

use anyhow::{Context as _, Result, bail};
use compositor::format::{self, entries_ordered};
use compositor::{png_io, render, ui};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("info") if args.len() == 3 => info(Path::new(&args[2])),
        Some("render") if args.len() == 4 => render_to(Path::new(&args[2]), Path::new(&args[3])),
        Some("info") | Some("render") | Some("--help") | Some("-h") => Err(anyhow::anyhow!(USAGE)),
        _ => {
            let mut paths = Vec::new();
            let mut script = ui::Script::default();
            let mut rest = args[1..].iter();
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "--screenshot" => script.screenshot = rest.next().map(PathBuf::from),
                    "--zoom" => script.zoom = rest.next().and_then(|z| z.parse().ok()),
                    "--wand" => script.wand = rest.next().and_then(|p| { let (x, y) = p.split_once(',')?; Some((x.parse().ok()?, y.parse().ok()?)) }),
                    "--tool" => script.tool = rest.next().and_then(|t| ui::Tool::ALL.into_iter().find(|tool| format!("{tool:?}").to_lowercase() == *t)),
                    "--ellipse" => script.ellipse = true,
                    "--blur-mode" => script.blur_mode = rest.next().and_then(|m| m.parse().ok()),
                    "--layer" => script.layer = rest.next().cloned(),
                    "--adjustment" => script.adjustment = rest.next().cloned(),
                    "--size" => { if let Some(size) = rest.next().and_then(|v| v.parse::<f64>().ok()) { script.brush_size = Some(size); } }
                    "--stroke" => script.stroke = rest.next().map(|s| s.split(';').filter_map(|p| { let (x, y) = p.split_once(',')?; Some((x.parse().ok()?, y.parse().ok()?)) }).collect()).unwrap_or_default(),
                    "--filter" => script.filter = rest.next().and_then(|f| match f.as_str() {
                        "noise" => Some(compositor::filters::Kind::AddNoise), "grain" => Some(compositor::filters::Kind::Grain),
                        "lens" => Some(compositor::filters::Kind::LensCorrection), "gradient" => Some(compositor::filters::Kind::GradientMap),
                        "levels" => Some(compositor::filters::Kind::Levels), "gaussian" => Some(compositor::filters::Kind::GaussianBlur),
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

const USAGE: &str = "usage:\n  compositor [project.comp ...]            open the app\n  compositor info <project.comp>           list the layer tree\n  compositor render <project.comp> <out.png>";

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
