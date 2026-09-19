//! Compositor on Linux.
//!
//! The macOS app (in `reference/`) is the specification. Its C pixel core is compiled unchanged from `csrc/`;
//! everything else is rebuilt here on Cairo. Phase 1 reads a `.comp` package and renders it flat.

pub mod abr;
pub mod blur;
pub mod brush;
pub mod brush_set;
pub mod distort;
pub mod document;
pub mod ffi;
pub mod filters;
pub mod history;
pub mod format;
pub mod heic;
pub mod matte;
pub mod png_io;
pub mod psd;
pub mod raster;
pub mod render;
pub mod selection;
pub mod transform;
pub mod ui;
pub mod viewport;
pub mod warp;
