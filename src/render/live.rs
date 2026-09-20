//! Clipping masks, as `LiveMaskRenderer` draws them. A layer with a `maskSourceID` takes its coverage from
//! that source: the source's alpha with its own masks, opacity and upstream clips, ignoring its visibility and
//! color. A contiguous run of layers clipped to the sibling below them is a clipping stack: the base's colors
//! are brought to full coverage, the clipped layers blend onto those colors, and the base's alpha is put back,
//! so soft edges stay soft instead of thickening. Every buffer here covers the context's clip in device pixels.

use super::{FolderMask, Region, Renderer, operator};
use crate::ffi;
use crate::raster::{a8_from_data, with_bytes, with_bytes_mut};
use anyhow::Result;
use cairo::{Context, Format, ImageSurface};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

#[derive(Default)]
pub(crate) struct LiveMasks {
    coverage: HashMap<Uuid, (ImageSurface, Region)>,
    visiting: HashSet<Uuid>,
    pub(crate) stacks: HashMap<Uuid, Vec<Uuid>>,
    pub(crate) stacked: HashSet<Uuid>,
}

impl LiveMasks {
    /// Forgets the previous draw's coverages and stacks (both depend on the view and on visibility).
    pub(crate) fn reset(&mut self) { *self = LiveMasks::default(); }
}

impl Renderer {
    /// Finds the clipping stacks among `ids` (visible layers in drawing order): a base with no clip of its own
    /// followed by siblings clipped to it.
    pub(crate) fn prepare_stacks(&mut self, ids: &[Uuid]) {
        for (index, &base) in ids.iter().enumerate() {
            if self.source(base).is_some() || self.layer(base).adjustment.is_some() { continue; }
            let mut children = Vec::new();
            for &child in &ids[index + 1..] {
                if self.source(child) != Some(base) || self.parent(child) != self.parent(base) { break; }
                children.push(child);
            }
            if children.is_empty() { continue; }
            self.live.stacked.extend(children.iter().copied());
            self.live.stacks.insert(base, children);
        }
    }

    /// Draws `id` onto the canvas: a whole clipping stack when `id` is its base, nothing when `id` is inside
    /// one (its base drew it), else the layer alone through its clip.
    pub(crate) fn draw_composite(&mut self, id: Uuid, cr: &Context, folders: &[FolderMask]) -> Result<()> {
        if self.live.stacked.contains(&id) { return Ok(()); }
        if self.layer(id).adjustment.is_some() {
            if self.source(id).is_none() { self.adjust(id, cr, folders)?; }
            return Ok(());
        }
        let Some(children) = self.live.stacks.get(&id).cloned() else { return self.draw_clipped(id, cr, folders) };
        let Some(region) = Region::of(cr)? else { return Ok(()) };
        let (mut group, gcr) = region.offscreen(cr)?;
        self.draw_own(id, &gcr, None, &[])?;
        drop(gcr);
        // The group's own pixels (more than the region's units on a HiDPI frame).
        let (w, h) = (group.width() as usize, group.height() as usize);
        let alpha_stride = Format::A8.stride_for_width(group.width() as u32)? as usize;
        let mut coverage = vec![0u8; alpha_stride * h];
        with_bytes_mut(&mut group, |data, stride| {
            ffi::extract_alpha(data, stride, &mut coverage, alpha_stride, w, h);
            ffi::unpremultiply_opaque(data, stride, w, h);
        })?;
        for child in children {
            let gcr = Context::new(&group)?;
            // A clip, not a path (a path left pending would widen the child's own clip to the whole region
            // and smear its edge pixels across the base), set in the group's own space: the region's size
            // is in device units, not document ones.
            gcr.identity_matrix();
            gcr.rectangle(0.0, 0.0, region.width as f64, region.height as f64);
            gcr.clip();
            gcr.set_matrix(Matrix_for(cr, region));
            if self.layer(child).adjustment.is_some() { self.adjust(child, &gcr, &[])?; } else { self.draw_own(child, &gcr, None, &[])?; }
        }
        with_bytes_mut(&mut group, |data, stride| {
            ffi::restore_alpha(data, stride, &coverage, alpha_stride, w, h);
        })?;
        cr.save()?;
        let matrix = cr.matrix();
        cr.identity_matrix();
        cr.push_group();
        cr.set_source_surface(&group, region.x as f64, region.y as f64)?;
        cr.paint()?;
        cr.pop_group_to_source()?;
        cr.set_matrix(matrix);
        self.through_masks(cr, folders, None)?;
        cr.set_operator(operator(self.layer(id).blend_mode()));
        cr.paint()?;
        cr.restore()?;
        Ok(())
    }

    /// One layer through its clipping source's coverage, if it has one.
    pub(crate) fn draw_clipped(&mut self, id: Uuid, cr: &Context, folders: &[FolderMask]) -> Result<()> {
        let clip = match self.source(id) {
            Some(source) => match self.coverage(source, cr)? { Some(c) => Some(c), None => return Ok(()) },
            None => None,
        };
        self.draw_own(id, cr, clip.as_ref().map(|(s, r)| (s, *r)), folders)
    }

    /// A layer's alpha over the context's clip region, drawn with its own masks and clips, cached per draw.
    fn coverage(&mut self, id: Uuid, cr: &Context) -> Result<Option<(ImageSurface, Region)>> {
        if let Some(existing) = self.live.coverage.get(&id) { return Ok(Some(existing.clone())); }
        if self.live.visiting.contains(&id) || self.live.visiting.len() >= 256 { return Ok(None); }
        let Some(region) = Region::of(cr)? else { return Ok(None) };
        self.live.visiting.insert(id);
        let (mut pixels, pcr) = region.offscreen(cr)?;
        let drawn = self.draw_clipped(id, &pcr, &[]);
        drop(pcr);
        self.live.visiting.remove(&id);
        drawn?;
        let (w, h) = (pixels.width() as usize, pixels.height() as usize);
        let alpha_stride = Format::A8.stride_for_width(pixels.width() as u32)? as usize;
        let mut alpha = vec![0u8; alpha_stride * h];
        with_bytes(&mut pixels, |data, stride| {
            ffi::extract_alpha(data, stride, &mut alpha, alpha_stride, w, h);
        })?;
        let surface = a8_from_data(pixels.width(), pixels.height(), alpha, alpha_stride as i32)?;
        let (sx, sy) = pixels.device_scale();
        surface.set_device_scale(sx, sy);
        self.live.coverage.insert(id, (surface.clone(), region));
        Ok(Some((surface, region)))
    }

}

/// The matrix an offscreen context over `region` needs to map user space as `cr` does.
#[allow(non_snake_case)]
fn Matrix_for(cr: &Context, region: Region) -> cairo::Matrix {
    cairo::Matrix::multiply(&cr.matrix(), &cairo::Matrix::new(1.0, 0.0, 0.0, 1.0, -region.x as f64, -region.y as f64))
}
