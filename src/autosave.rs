//! Autosave: every couple of minutes a modified document's state is copied (its pixels as plain bytes,
//! which takes milliseconds) and written as a .comp package on another thread, into
//! `~/.local/share/compositor/autosave/<document id>.comp`. Saving or closing a document cleanly removes
//! its autosave; what remains at the next start is offered on the start page as recoverable.

use crate::format::Manifest;
use anyhow::{Context as _, Result};
use cairo::{Format, ImageSurface};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const INTERVAL_SECONDS: u64 = 120;

pub fn dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".local/share"));
    base.join("compositor/autosave")
}

/// A surface's pixels, free of Cairo so they can cross to another thread.
pub struct Packed { pub width: i32, pub height: i32, pub stride: i32, pub format: Format, pub bytes: Vec<u8> }

pub fn pack(surface: &ImageSurface) -> Result<Packed> {
    let (width, height, stride, format) = (surface.width(), surface.height(), surface.stride(), surface.format());
    let bytes = crate::raster::with_bytes(surface, |d, _| d[..stride as usize * height as usize].to_vec())?;
    Ok(Packed { width, height, stride, format, bytes })
}

fn unpack(p: Packed) -> Result<ImageSurface> {
    ImageSurface::create_for_data(p.bytes, p.format, p.width, p.height, p.stride).context("rebuilding a surface")
}

/// Everything an autosave needs, taken on the main thread.
pub struct Snapshot { pub id: Uuid, pub title: String, pub manifest: Manifest, pub images: HashMap<Uuid, Packed>, pub masks: HashMap<Uuid, Packed>, discards: u64 }

/// How many times each document's autosave has been discarded, so a write that finishes after a
/// discard (the user saved or closed while the worker ran) removes what it wrote.
static DISCARDS: std::sync::Mutex<Option<HashMap<Uuid, u64>>> = std::sync::Mutex::new(None);

fn discards(id: Uuid) -> u64 { DISCARDS.lock().ok().and_then(|d| d.as_ref().and_then(|m| m.get(&id).copied())).unwrap_or(0) }

impl Snapshot {
    pub fn new(id: Uuid, title: String, manifest: Manifest, images: HashMap<Uuid, Packed>, masks: HashMap<Uuid, Packed>) -> Snapshot {
        Snapshot { id, title, manifest, images, masks, discards: discards(id) }
    }

    pub fn path(&self) -> PathBuf { path_for(self.id) }

    /// Writes the package; meant for a worker thread. The package is staged and renamed into place, so a
    /// reader never sees a half-written one.
    pub fn write(self) -> Result<PathBuf> {
        let target = self.path();
        std::fs::create_dir_all(dir())?;
        let mut images = HashMap::new();
        for (id, p) in self.images { images.insert(id, unpack(p)?); }
        let mut masks = HashMap::new();
        for (id, p) in self.masks { masks.insert(id, unpack(p)?); }
        crate::format::save(&target, &self.manifest, &images, &masks)?;
        std::fs::write(title_path(self.id), self.title)?;
        if discards(self.id) != self.discards {
            // Discarded while this was being written: it must not come back.
            discard(self.id);
            anyhow::bail!("discarded while writing");
        }
        Ok(target)
    }
}

pub fn path_for(id: Uuid) -> PathBuf { dir().join(format!("{}.comp", crate::format::upper(id))) }
fn title_path(id: Uuid) -> PathBuf { dir().join(format!("{}.title", crate::format::upper(id))) }

/// Removes a document's autosave (after a save, or a close the user chose).
pub fn discard(id: Uuid) {
    if let Ok(mut d) = DISCARDS.lock() { *d.get_or_insert_with(HashMap::new).entry(id).or_insert(0) += 1; }
    let _ = std::fs::remove_dir_all(path_for(id));
    let _ = std::fs::remove_file(title_path(id));
}

pub fn discard_all() { for r in recoverable() { discard(r.id); } }

/// An autosave left behind: where it is, what it was called, and when it was written.
pub struct Recoverable { pub id: Uuid, pub path: PathBuf, pub title: String, pub written: std::time::SystemTime }

pub fn recoverable() -> Vec<Recoverable> {
    let Ok(entries) = std::fs::read_dir(dir()) else { return Vec::new() };
    let mut out: Vec<Recoverable> = entries.filter_map(|e| {
        let path = e.ok()?.path();
        if path.extension().is_none_or(|x| x != "comp") || !path.join("manifest.json").exists() { return None; }
        let stem = path.file_stem()?.to_string_lossy().to_string();
        let id = Uuid::parse_str(&stem).ok()?;
        let title = std::fs::read_to_string(title_path(id)).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).unwrap_or_else(|| "Untitled".into());
        let written = std::fs::metadata(path.join("manifest.json")).and_then(|m| m.modified()).unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        Some(Recoverable { id, path, title, written })
    }).collect();
    out.sort_by(|a, b| b.written.cmp(&a.written));
    out
}

/// Whether a path is inside the autosave directory (a recovered document is untitled again).
pub fn is_autosave(path: &Path) -> bool { path.starts_with(dir()) }
