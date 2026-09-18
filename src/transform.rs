//! Dragging a layer's transform box: move, the eight resize handles, and rotation, with the modifier rules
//! of `TransformDrag.updated`, and snapping a move to the canvas and the other layers (`TransformSnap`).

use crate::format::{Point, Size, Transform};

/// Unit-square positions of the eight handles: corners and edge midpoints, clockwise from top left.
pub const HANDLES: [(f64, f64); 8] = [(0.0, 0.0), (0.5, 0.0), (1.0, 0.0), (1.0, 0.5), (1.0, 1.0), (0.5, 1.0), (0.0, 1.0), (0.0, 0.5)];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode { Move, Resize(usize), Rotate }

#[derive(Clone, Copy, Debug)]
pub struct Drag {
    pub original: Transform,
    pub start: (f64, f64),
    pub mode: Mode,
}

impl Drag {
    /// The transform for the pointer at `point` (document pixels). Resizing: Shift (or the ratio lock, but not
    /// both) keeps the proportions, Alt resizes around the center. Rotating: Shift snaps to 15 degrees.
    /// Moving: Shift keeps to one axis.
    pub fn updated(&self, point: (f64, f64), lock_ratio: bool, shift: bool, alt: bool) -> Transform {
        let o = self.original;
        let mut result = o;
        match self.mode {
            Mode::Move => {
                let (mut dx, mut dy) = (point.0 - self.start.0, point.1 - self.start.1);
                if shift { if dx.abs() >= dy.abs() { dy = 0.0; } else { dx = 0.0; } }
                result.origin = Point(o.origin.0 + dx, o.origin.1 + dy);
            }
            Mode::Rotate => {
                let center = o.center();
                let delta = (point.1 - center.1).atan2(point.0 - center.0) - (self.start.1 - center.1).atan2(self.start.0 - center.0);
                result.rotation = o.rotation + delta.to_degrees();
                if shift { result.rotation = (result.rotation / 15.0).round() * 15.0; }
            }
            Mode::Resize(index) => {
                let handle = HANDLES[index];
                let anchor_unit = if alt { (0.5, 0.5) } else { (1.0 - handle.0, 1.0 - handle.1) };
                let anchor = o.point(anchor_unit);
                // Use the initial handle plus pointer delta to avoid a jump on grab.
                let initial = o.point(handle);
                let dx = initial.0 + point.0 - self.start.0 - anchor.0;
                let dy = initial.1 + point.1 - self.start.1 - anchor.1;
                let span = if alt { 2.0 } else { 1.0 };
                let (c, s) = (o.radians().cos(), o.radians().sin());
                let local_x = (dx * c + dy * s) * span;
                let local_y = (-dx * s + dy * c) * span;
                let (sx, sy) = (handle.0 * 2.0 - 1.0, handle.1 * 2.0 - 1.0);
                let mut width = if sx == 0.0 { o.size.0 } else { (local_x * sx).max(1.0) };
                let mut height = if sy == 0.0 { o.size.1 } else { (local_y * sy).max(1.0) };
                if lock_ratio != shift {
                    let factor = if sx == 0.0 { height / o.size.1 } else if sy == 0.0 { width / o.size.0 } else {
                        ((local_x * sx * o.size.0 + local_y * sy * o.size.1) / (o.size.0 * o.size.0 + o.size.1 * o.size.1)).max(1.0 / o.size.0.min(o.size.1))
                    };
                    width = o.size.0 * factor;
                    height = o.size.1 * factor;
                }
                result.size = Size(width, height);
                let offset_x = (0.5 - anchor_unit.0) * width;
                let offset_y = (0.5 - anchor_unit.1) * height;
                let center = (anchor.0 + offset_x * c - offset_y * s, anchor.1 + offset_x * s + offset_y * c);
                result.origin = Point(center.0 - width / 2.0, center.1 - height / 2.0);
            }
        }
        if result.is_valid() { result } else { o }
    }
}

/// `box` (min x, min y, max x, max y) moved so whichever of its left, center or right lands nearest an `xs`
/// target does, and the same vertically, each axis on its own and only within `tolerance` document pixels.
/// Returns the move and the targets met, to draw guides along.
pub fn snap_offset(bx: (f64, f64, f64, f64), xs: &[f64], ys: &[f64], tolerance: f64) -> ((f64, f64), Option<f64>, Option<f64>) {
    fn shift(guides: [f64; 3], targets: &[f64], tolerance: f64) -> (f64, Option<f64>) {
        let mut best: Option<(f64, f64)> = None;
        for g in guides {
            for &t in targets {
                let m = t - g;
                if m.abs() > tolerance { continue; }
                if best.is_some_and(|(bm, _)| bm.abs() <= m.abs()) { continue; }
                best = Some((m, t));
            }
        }
        (best.map_or(0.0, |b| b.0), best.map(|b| b.1))
    }
    let h = shift([bx.0, (bx.0 + bx.2) / 2.0, bx.2], xs, tolerance);
    let v = shift([bx.1, (bx.1 + bx.3) / 2.0, bx.3], ys, tolerance);
    ((h.0, v.0), h.1, v.1)
}

/// How close, in screen points, a guide comes before it snaps.
pub const SNAP_DISTANCE: f64 = 10.0;

/// The geometry of the transform box on screen: handle points, the rotation handle, and hit testing.
pub struct Geometry {
    pub handles: [(f64, f64); 8],
    pub rotation: (f64, f64),
}

impl Geometry {
    pub fn new(transform: &Transform, view: impl Fn((f64, f64)) -> (f64, f64)) -> Geometry {
        let handles = HANDLES.map(|u| view(transform.point(u)));
        let r = transform.radians();
        Geometry { handles, rotation: (handles[1].0 + r.sin() * 28.0, handles[1].1 - r.cos() * 28.0) }
    }

    pub fn hit(&self, point: (f64, f64)) -> Option<Mode> {
        let near = |o: (f64, f64)| (point.0 - o.0).hypot(point.1 - o.1) <= 10.0;
        if near(self.rotation) { return Some(Mode::Rotate); }
        if let Some(i) = self.handles.iter().position(|h| near(*h)) { return Some(Mode::Resize(i)); }
        for (start, end, handle) in [(0, 2, 1), (2, 4, 3), (4, 6, 5), (6, 0, 7)] {
            let (a, b) = (self.handles[start], self.handles[end]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let l2 = dx * dx + dy * dy;
            if l2 <= 0.0 { continue; }
            let t = ((point.0 - a.0) * dx + (point.1 - a.1) * dy) / l2;
            if (0.0..=1.0).contains(&t) && (point.0 - a.0 - t * dx).hypot(point.1 - a.1 - t * dy) <= 10.0 { return Some(Mode::Resize(handle)); }
        }
        None
    }
}
