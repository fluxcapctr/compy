# Working on this repo

- `reference/` is the macOS app and is the specification. Before implementing a feature, read the Swift file
  that implements it there and the matching test in `reference/CompositorTests/`, then port the behavior and
  the test's assertions. Do not edit anything under `reference/` or `csrc/`.
- `csrc/` is the original C pixel core, unchanged. Add new C beside it only for ports of CoreImage filters
  the Swift used; keep the same premultiplied-RGBA-plus-stride convention.
- Cairo's ARGB32 is B, G, R, A in memory. The C core's alpha-only routines work on it as is; anything that
  computes luminance from channel positions needs the boundary swap handled in `src/ffi.rs`.
- Cairo `ImageSurface::data()` needs the surface unreferenced by any live pattern or context. Drop them first.
- Tests build `.comp` packages in a temp dir (`tests/common/mod.rs`) with analytically known results. Every
  new rendering feature gets one. Run `cargo test` before committing.
- No em dashes in prose or comments.
