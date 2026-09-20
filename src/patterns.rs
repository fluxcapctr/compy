//! Patterns: small images tiled by Fill with Pattern and the Pattern Stamp, kept as PNGs in
//! `~/.local/share/compositor/patterns/<name>.png`. Define Pattern writes one from the selection.

use anyhow::{Context as _, Result, bail};
use cairo::ImageSurface;
use std::path::PathBuf;

pub fn dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".local/share"));
    base.join("compositor/patterns")
}

/// A name safe as a file name.
fn clean(name: &str) -> String { name.trim().chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' }).collect::<String>().trim().to_string() }

pub fn path_for(name: &str) -> PathBuf { dir().join(format!("{}.png", clean(name))) }

/// The pattern names on disk, sorted.
pub fn list() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir()) else { return Vec::new() };
    let mut names: Vec<String> = entries.filter_map(|e| { let p = e.ok()?.path(); if p.extension().is_some_and(|x| x == "png") { Some(p.file_stem()?.to_string_lossy().to_string()) } else { None } }).collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

pub fn load(name: &str) -> Result<ImageSurface> {
    let path = path_for(name);
    if !path.exists() { bail!("There is no pattern called {name}."); }
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let (surface, w, h) = crate::document::Document::decode_image_bytes(&bytes)?;
    if w == 0 || h == 0 { bail!("The pattern {name} is empty."); }
    Ok(surface)
}

pub fn save(name: &str, surface: &ImageSurface) -> Result<PathBuf> {
    let name = clean(name);
    if name.is_empty() { bail!("Give the pattern a name."); }
    std::fs::create_dir_all(dir())?;
    let path = path_for(&name);
    let bytes = crate::png_io::png_bytes(surface)?;
    // Staged beside the target and renamed into place, so a crash mid-write cannot leave half a tile.
    let staging = dir().join(format!(".{name}.saving-{}.png", std::process::id()));
    std::fs::write(&staging, bytes).with_context(|| format!("writing {}", staging.display()))?;
    if let Err(e) = std::fs::rename(&staging, &path) { let _ = std::fs::remove_file(&staging); return Err(e).with_context(|| format!("writing {}", path.display())); }
    Ok(path)
}

/// `save` under a name not already taken: "Brick", then "Brick 2", "Brick 3" (an import that must not
/// replace what the user has).
pub fn save_new(name: &str, surface: &ImageSurface) -> Result<PathBuf> {
    let base = clean(name);
    if base.is_empty() { bail!("Give the pattern a name."); }
    let mut n = 1;
    loop {
        let candidate = if n == 1 { base.clone() } else { format!("{base} {n}") };
        if !path_for(&candidate).exists() { return save(&candidate, surface); }
        n += 1;
        if n > 10_000 { bail!("too many patterns named {base}"); }
    }
}
