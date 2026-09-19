//! Every rule `ProjectStore.validate`, `LayerHierarchy.validate` and `LiveMaskGraph.validate` apply before a
//! file is allowed to replace the live document.

use super::{ADJUSTMENT_KINDS, BlendMode, Layer, MAX_LAYERS, MAX_SIDE, Manifest, ProjectError, VERSIONS, FORMAT};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub fn manifest(m: &Manifest) -> Result<(), ProjectError> {
    if m.format != FORMAT { return Err(ProjectError::Invalid); }
    if !VERSIONS.contains(&m.version) { return Err(ProjectError::Version(m.version)); }
    if m.color_space != "sRGB" { return Err(ProjectError::Invalid); }
    if let Some(r) = m.resolution {
        if !r.is_finite() || !(1.0..=9600.0).contains(&r) { return Err(ProjectError::Invalid); }
    }
    if !(1..=MAX_SIDE).contains(&m.width) || !(1..=MAX_SIDE).contains(&m.height) || m.layers.len() > MAX_LAYERS {
        return Err(ProjectError::TooLarge);
    }
    for layer in &m.layers {
        if let Some(adjustment) = &layer.adjustment {
            if m.version < 7 || layer.is_group() || layer.image_file.is_some()
                || !ADJUSTMENT_KINDS.contains(&adjustment.kind.as_str())
                || !crate::filters::Adjustment::record_is_valid(adjustment) {
                return Err(ProjectError::Invalid);
            }
        }
        // Layer masks arrived in version 4, folder masks in version 6.
        if let Some(mask_file) = &layer.mask_file {
            let needed = if layer.is_group() { 6 } else { 4 };
            if m.version < needed || *mask_file != format!("{}.mask.png", upper(layer.id)) { return Err(ProjectError::Invalid); }
        }
        if layer.mask_enabled.is_some() && layer.mask_file.is_none() { return Err(ProjectError::Invalid); }
        if let Some(placement) = &layer.mask_placement {
            if !placement.is_valid() || layer.mask_file.is_none() { return Err(ProjectError::Invalid); }
        }
        let opacity = layer.opacity();
        let blend = layer.blend_mode();
        let plain = opacity == 1.0 && blend == BlendMode::Normal;
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) { return Err(ProjectError::Invalid); }
        if m.version < 3 && !plain { return Err(ProjectError::Invalid); }
        if layer.is_group() && !plain { return Err(ProjectError::Invalid); }
    }
    hierarchy(&m.layers)?;
    live_mask_graph(&m.layers)?;
    if m.version < 5 && m.layers.iter().any(|l| l.mask_source_id.is_some()) { return Err(ProjectError::Invalid); }
    if m.version == 1 && m.layers.iter().any(|l| l.parent_id.is_some() || l.is_group()) { return Err(ProjectError::Invalid); }
    let mut ids = HashSet::new();
    for layer in &m.layers {
        if !ids.insert(layer.id) || !layer.transform.is_valid() || layer.name.trim().is_empty() || layer.name.len() > 16_384 {
            return Err(ProjectError::Invalid);
        }
        if let Some(image_file) = &layer.image_file {
            if *image_file != format!("{}.png", upper(layer.id)) { return Err(ProjectError::Invalid); }
        }
    }
    if let Some(active) = m.active_layer_id {
        if !ids.contains(&active) { return Err(ProjectError::Invalid); }
    }
    Ok(())
}

/// Swift's `UUID.uuidString` is uppercase, and asset filenames are compared against it exactly.
pub fn upper(id: Uuid) -> String { id.hyphenated().to_string().to_uppercase() }

/// Folders form a tree: parents exist and are folders, no cycles, no folder carries an image, and nesting
/// stays within 64 ancestors.
pub fn hierarchy(layers: &[Layer]) -> Result<(), ProjectError> {
    let mut by_id: HashMap<Uuid, &Layer> = HashMap::new();
    for layer in layers {
        if by_id.insert(layer.id, layer).is_some() || (layer.is_group() && layer.image_file.is_some()) {
            return Err(ProjectError::Invalid);
        }
    }
    for layer in layers {
        let mut seen = HashSet::from([layer.id]);
        let mut parent = layer.parent_id;
        while let Some(id) = parent {
            if seen.len() > 64 || !seen.insert(id) { return Err(ProjectError::Invalid); }
            let Some(node) = by_id.get(&id) else { return Err(ProjectError::Invalid) };
            if !node.is_group() { return Err(ProjectError::Invalid); }
            parent = node.parent_id;
        }
        if layer.is_group() && seen.len() > 64 { return Err(ProjectError::Invalid); }
    }
    Ok(())
}

/// Clipping links (`maskSourceID`) point at existing, non-folder, non-adjustment layers, never from a folder,
/// with no cycles and no chain over 256 layers.
pub fn live_mask_graph(layers: &[Layer]) -> Result<(), ProjectError> {
    let mut records: HashMap<Uuid, &Layer> = HashMap::new();
    for layer in layers {
        if records.insert(layer.id, layer).is_some() { return Err(ProjectError::Invalid); }
    }
    for layer in layers {
        let mut path = HashSet::new();
        let mut current = Some(layer.id);
        while let Some(id) = current {
            if path.len() >= 256 || !path.insert(id) { return Err(ProjectError::Invalid); }
            let Some(record) = records.get(&id) else { return Err(ProjectError::Invalid) };
            if let Some(source) = record.mask_source_id {
                let Some(target) = records.get(&source) else { return Err(ProjectError::Invalid) };
                if record.is_group() || target.is_group() || target.adjustment.is_some() { return Err(ProjectError::Invalid); }
            }
            current = record.mask_source_id;
        }
    }
    Ok(())
}
