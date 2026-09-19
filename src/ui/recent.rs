//! Recently opened and saved files, most recent first, kept in `~/.config/compositor/recent.list` for the
//! start page.

use std::path::{Path, PathBuf};

const LIMIT: usize = 12;

fn list_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".config"));
    base.join("compositor/recent.list")
}

/// The remembered files that still exist, most recent first.
pub fn list() -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(list_path()) else { return Vec::new() };
    text.lines().map(str::trim).filter(|l| !l.is_empty()).map(PathBuf::from).filter(|p| p.exists()).take(LIMIT).collect()
}

/// Puts `path` at the front of the list.
pub fn remember(path: &Path) {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut entries: Vec<PathBuf> = list().into_iter().filter(|p| *p != path).collect();
    entries.insert(0, path);
    entries.truncate(LIMIT);
    let text: String = entries.iter().map(|p| format!("{}\n", p.display())).collect();
    let target = list_path();
    if let Some(dir) = target.parent() { let _ = std::fs::create_dir_all(dir); }
    let _ = std::fs::write(target, text);
}
