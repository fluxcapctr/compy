# Review prompt

Paste everything below the line into a code review assistant, pointing it at this folder:
`/home/estevens/code/compositor-linux`.

---

Review the Rust project at /home/estevens/code/compositor-linux for bugs.

Context: it is a Linux rebuild of a macOS image editor (Swift source in reference/, read-only, treated
as the spec). The original C pixel core is compiled unchanged from csrc/ (do not edit it). Everything
else is Rust on GTK4 and Cairo. CLAUDE.md and README.md explain the layout and conventions; read both
first.

Key facts you need:

- Cairo ARGB32 is premultiplied BGRA in memory. Look for places that treat it as RGBA or as straight
  alpha.
- Cairo ImageSurface::data() requires refcount 1; raw pointer reads via cairo_image_surface_get_data
  are used elsewhere. Check for aliasing or use-after-free around those.
- Undo is value snapshots sharing surfaces by refcount (src/history.rs, src/render/mod.rs State).
  Check that no edit mutates a surface still referenced by a snapshot.
- Layers and masks render from Lanczos halvings (src/raster.rs) keyed by (id, source, level); check
  the cache is invalidated on every mutation.
- The PSD reader (src/psd.rs) parses untrusted files: look for panics, integer overflow, unbounded
  allocation, out-of-bounds indexing on malformed input.
- The .comp loader (src/format/) enforces the validation rules in
  reference/Compositor/Persistence/ProjectStore.swift; check for rules that are missing or wrong.

What I want:

1. Concrete bugs with file:line, what input triggers them, and what goes wrong. Rank by severity.
2. Places where the Rust behavior differs from the Swift reference for the same feature (blend
   modes, mask coverage, clipping, transform math, brush accumulation, selection ops).
3. Panics reachable from user input (unwrap, expect, slicing, arithmetic) in src/.
4. Anything in tests/ that asserts the wrong value or is tautological.

Run cargo build and cargo test (needs gtk4 and cairo dev packages; 69 tests should pass) and report
anything that fails. If you can only read files and not run commands, skip this step. Do not refactor
or restyle; report only. No em dashes in your output.
