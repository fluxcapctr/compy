//! Pen tool paths: anchors with Bezier handles, traced into Cairo, flattened into points for a brush, and
//! rasterized into a selection.

use anyhow::Result;
use cairo::Context;

pub type Point = (f64, f64);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    pub point: Point,
    /// The handle coming into this anchor from the previous segment, and the one leaving it.
    pub handle_in: Option<Point>,
    pub handle_out: Option<Point>,
}

impl Anchor {
    pub fn corner(point: Point) -> Anchor { Anchor { point, handle_in: None, handle_out: None } }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Path {
    pub anchors: Vec<Anchor>,
    pub closed: bool,
}

/// What a press with the Pen tool landed on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit { Anchor(usize), HandleIn(usize), HandleOut(usize) }

impl Path {
    pub fn is_empty(&self) -> bool { self.anchors.is_empty() }

    /// One cubic segment from anchor `i` to the next (wrapping when closed): the four control points.
    fn segment(&self, i: usize) -> Option<[Point; 4]> {
        let a = self.anchors.get(i)?;
        let b = if i + 1 < self.anchors.len() { &self.anchors[i + 1] } else if self.closed && self.anchors.len() >= 2 { &self.anchors[0] } else { return None };
        Some([a.point, a.handle_out.unwrap_or(a.point), b.handle_in.unwrap_or(b.point), b.point])
    }

    fn segments(&self) -> usize {
        match self.anchors.len() { 0 | 1 => 0, n if self.closed => n, n => n - 1 }
    }

    /// Traces the path into `cr`'s current path, in whatever space `cr` is in; `map` places each point.
    pub fn trace(&self, cr: &Context, map: impl Fn(Point) -> Point) {
        let Some(first) = self.anchors.first() else { return };
        let p = map(first.point);
        cr.move_to(p.0, p.1);
        for i in 0..self.segments() {
            let Some([a, c1, c2, b]) = self.segment(i) else { break };
            if c1 == a && c2 == b { let p = map(b); cr.line_to(p.0, p.1); }
            else { let (m1, m2, mb) = (map(c1), map(c2), map(b)); cr.curve_to(m1.0, m1.1, m2.0, m2.1, mb.0, mb.1); }
        }
        if self.closed { cr.close_path(); }
    }

    /// Points along the path about `step` apart (document pixels), for a brush to follow.
    pub fn flatten(&self, step: f64) -> Vec<Point> {
        let step = step.max(0.25);
        let mut out: Vec<Point> = Vec::new();
        let Some(first) = self.anchors.first() else { return out };
        out.push(first.point);
        for i in 0..self.segments() {
            let Some([a, c1, c2, b]) = self.segment(i) else { break };
            let rough = (c1.0 - a.0).hypot(c1.1 - a.1) + (c2.0 - c1.0).hypot(c2.1 - c1.1) + (b.0 - c2.0).hypot(b.1 - c2.1);
            let n = ((rough / step).ceil() as usize).clamp(1, 20_000);
            for k in 1..=n {
                let t = k as f64 / n as f64;
                let u = 1.0 - t;
                let x = u * u * u * a.0 + 3.0 * u * u * t * c1.0 + 3.0 * u * t * t * c2.0 + t * t * t * b.0;
                let y = u * u * u * a.1 + 3.0 * u * u * t * c1.1 + 3.0 * u * t * t * c2.1 + t * t * t * b.1;
                out.push((x, y));
            }
        }
        out
    }

    /// The anchor or handle within `tolerance` of `p`, handles first so they can be pulled off an anchor.
    pub fn hit(&self, p: Point, tolerance: f64) -> Option<Hit> {
        let near = |q: Point| (q.0 - p.0).hypot(q.1 - p.1) <= tolerance;
        for (i, a) in self.anchors.iter().enumerate() {
            if a.handle_out.is_some_and(near) { return Some(Hit::HandleOut(i)); }
            if a.handle_in.is_some_and(near) { return Some(Hit::HandleIn(i)); }
        }
        self.anchors.iter().position(|a| near(a.point)).map(Hit::Anchor)
    }

    /// Rasterizes the path (closed for the purpose) as a selection mask of the document's size.
    pub fn selection(&self, width: i32, height: i32) -> Result<crate::selection::Selection> {
        let closed = Path { anchors: self.anchors.clone(), closed: true };
        crate::selection::Selection::from_shape(width, height, true, |cr| { closed.trace(cr, |p| p); cr.fill()?; Ok(()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_curves_hits_handles_and_fills_a_selection() {
        let mut p = Path::default();
        p.anchors.push(Anchor::corner((10.0, 10.0)));
        p.anchors.push(Anchor { point: (50.0, 10.0), handle_in: Some((30.0, 40.0)), handle_out: None });
        let pts = p.flatten(2.0);
        assert!(pts.len() > 20 && pts[0] == (10.0, 10.0) && *pts.last().unwrap() == (50.0, 10.0));
        assert!(pts.iter().any(|q| q.1 > 15.0), "the curve bows toward its handle");
        assert_eq!(p.hit((31.0, 40.0), 3.0), Some(Hit::HandleIn(1)));
        assert_eq!(p.hit((10.5, 10.5), 3.0), Some(Hit::Anchor(0)));
        assert_eq!(p.hit((0.0, 0.0), 3.0), None);
        p.anchors.push(Anchor::corner((50.0, 50.0)));
        p.anchors.push(Anchor::corner((10.0, 50.0)));
        p.closed = true;
        let sel = p.selection(60, 60).unwrap();
        assert!(sel.contains(30.0, 30.0));
        assert!(!sel.contains(55.0, 5.0));
        assert_eq!(p.flatten(5.0).len(), p.flatten(5.0).len());
    }
}
