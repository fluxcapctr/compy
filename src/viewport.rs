//! Where the document sits in the canvas view. A port of `CanvasViewport`: document coordinates are pixels
//! with a top-left origin, view coordinates are logical points, and zoom 1 means one device pixel per
//! document pixel.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub view_width: f64,
    pub view_height: f64,
    pub backing_scale: f64,
    zoom: f64,
    /// Offset of the document's center from the view's center, in points.
    pub pan: (f64, f64),
    /// Until the user zooms or pans, resizing the view keeps the document fitted.
    follows_fit: bool,
}

impl Default for Viewport {
    fn default() -> Self { Viewport { view_width: 0.0, view_height: 0.0, backing_scale: 1.0, zoom: 1.0, pan: (0.0, 0.0), follows_fit: true } }
}

impl Viewport {
    pub const MIN_ZOOM: f64 = 0.001;
    pub const MAX_ZOOM: f64 = 32.0;
    /// Space left around a fitted document, in points.
    pub const FIT_MARGIN: f64 = 96.0;

    pub fn zoom(&self) -> f64 { self.zoom }
    pub fn follows_fit(&self) -> bool { self.follows_fit }
    pub fn points_per_pixel(&self) -> f64 { self.zoom / self.backing_scale }
    pub fn center(&self) -> (f64, f64) { (self.view_width / 2.0, self.view_height / 2.0) }

    /// The document's rectangle in view points: (x, y, width, height).
    pub fn document_rect(&self, document: (f64, f64)) -> (f64, f64, f64, f64) {
        let ppp = self.points_per_pixel();
        let (w, h) = (document.0 * ppp, document.1 * ppp);
        let (cx, cy) = self.center();
        (cx - w / 2.0 + self.pan.0, cy - h / 2.0 + self.pan.1, w, h)
    }

    pub fn document_point(&self, view: (f64, f64), document: (f64, f64)) -> (f64, f64) {
        let (x, y, _, _) = self.document_rect(document);
        let ppp = self.points_per_pixel();
        ((view.0 - x) / ppp, (view.1 - y) / ppp)
    }

    pub fn view_point(&self, point: (f64, f64), document: (f64, f64)) -> (f64, f64) {
        let (x, y, _, _) = self.document_rect(document);
        let ppp = self.points_per_pixel();
        (x + point.0 * ppp, y + point.1 * ppp)
    }

    /// Zooms so the whole document shows with a margin, centered.
    pub fn fit(&mut self, document: (f64, f64)) {
        if self.view_width <= 0.0 || self.view_height <= 0.0 { self.follows_fit = true; return; }
        let zoom = ((self.view_width - Self::FIT_MARGIN).max(1.0) / document.0)
            .min((self.view_height - Self::FIT_MARGIN).max(1.0) / document.1) * self.backing_scale;
        self.zoom = Self::clamp(zoom);
        self.pan = (0.0, 0.0);
        self.follows_fit = true;
    }

    /// The view changed size or display; keeps the document fitted or its center point in place.
    pub fn resize(&mut self, size: (f64, f64), backing_scale: f64, document: Option<(f64, f64)>) {
        let old = self.points_per_pixel();
        self.view_width = size.0;
        self.view_height = size.1;
        self.backing_scale = backing_scale.max(1.0);
        match document {
            Some(document) if self.follows_fit => self.fit(document),
            _ => {
                let ratio = self.points_per_pixel() / old;
                self.pan = (self.pan.0 * ratio, self.pan.1 * ratio);
            }
        }
    }

    /// Sets the zoom, keeping the document point under `anchor` (view points) where it is.
    pub fn set_zoom(&mut self, value: f64, anchor: (f64, f64), document: (f64, f64)) {
        if !value.is_finite() { return; }
        let pixel = self.document_point(anchor, document);
        self.zoom = Self::clamp(value);
        let moved = self.view_point(pixel, document);
        self.pan.0 += anchor.0 - moved.0;
        self.pan.1 += anchor.1 - moved.1;
        self.follows_fit = false;
    }

    pub fn translate(&mut self, dx: f64, dy: f64) {
        self.pan.0 += dx;
        self.pan.1 += dy;
        self.follows_fit = false;
    }

    fn clamp(value: f64) -> f64 { value.clamp(Self::MIN_ZOOM, Self::MAX_ZOOM) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_centers_with_margin() {
        let mut v = Viewport::default();
        v.resize((1000.0, 800.0), 1.0, Some((2000.0, 1000.0)));
        assert!((v.zoom() - 0.452).abs() < 1e-9); // (1000 - 96) / 2000
        let (x, y, w, h) = v.document_rect((2000.0, 1000.0));
        assert!((w - 904.0).abs() < 1e-9 && (h - 452.0).abs() < 1e-9);
        assert!((x - 48.0).abs() < 1e-9 && (y - 174.0).abs() < 1e-9);
    }

    #[test]
    fn zoom_keeps_the_anchor_still() {
        let mut v = Viewport::default();
        v.resize((1000.0, 800.0), 1.0, Some((2000.0, 1000.0)));
        let anchor = (300.0, 250.0);
        let before = v.document_point(anchor, (2000.0, 1000.0));
        v.set_zoom(2.0, anchor, (2000.0, 1000.0));
        let after = v.document_point(anchor, (2000.0, 1000.0));
        assert!((before.0 - after.0).abs() < 1e-9 && (before.1 - after.1).abs() < 1e-9);
        assert!(!v.follows_fit());
    }

    #[test]
    fn zoom_is_clamped_and_hidpi_halves_points() {
        let mut v = Viewport::default();
        v.resize((100.0, 100.0), 2.0, None);
        v.set_zoom(1000.0, (0.0, 0.0), (10.0, 10.0));
        assert_eq!(v.zoom(), Viewport::MAX_ZOOM);
        assert_eq!(v.points_per_pixel(), 16.0);
    }
}
