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

---

# Round 2 prompt

Paste everything below the line into the review assistant. It assumes the round 1 findings in
`REVIEW_RESULTS.md` were fixed (commit 53e1fac).

---

Review the Rust project at /home/estevens/code/compositor-linux for bugs, second pass.

Context: a Linux rebuild of a macOS image editor. The Swift source in reference/ is the spec (read-only);
the C pixel core in csrc/ is compiled unchanged (do not edit it). Read CLAUDE.md and README.md first.
REVIEW_RESULTS.md holds the first review and a status table of what was fixed; do not re-report those
eleven findings unless a fix is wrong or incomplete.

Cover what the first pass did not:

1. Unsafe code and surface lifetimes. The first review said it did not check these. Audit every use of
   cairo_image_surface_get_data, with_bytes, with_bytes_mut and with_bytes_raw_mut in src/raster.rs and
   their callers. Look for reads while a Context or pattern still references the surface, writes without
   mark_dirty, surfaces shared between an undo snapshot and a live edit, and stride assumptions.
2. The round 1 fixes themselves: src/psd.rs validation (rect_dims, the channel length and budget checks,
   the merged image path), History::cancel and Document::abort_edit, Renderer::restore's placed-mask
   invalidation, the Clone Stamp source-over, the adjustment coverage clip, and
   Adjustment::record_is_valid against reference/Compositor/Document/LayerAdjustment.swift.
3. The GTK layer in src/ui/: RefCell borrow_mut while another borrow is live (a panic at runtime),
   callbacks that run after the tab was closed, dialogs that can be opened twice, actions that assume a
   current document, and the screenshot/script path.
4. The brush and warp engines (src/brush.rs, src/warp.rs) against reference/Compositor/Document/
   BrushStroke.swift and SmudgeLiquify.swift: dab spacing, opacity capping, selection respect, layer
   growth, the 64-pixel commit alignment, and heal/blur modes.
5. Opening odd inputs through every path (command line, drop, Open dialog): a .comp folder missing an
   image file, a manifest that is valid JSON but the wrong shape, a 0-byte file with a .psd or .png
   extension, a symlink, a path with no extension, a file that is not UTF-8 named, a 30,000 x 30,000
   image. Each should produce an error dialog or message, never a panic.
6. Anything in tests/ that asserts the wrong value or passes for the wrong reason.

What I want: concrete bugs with file:line, the input that triggers them, and what goes wrong, ranked by
severity. Run cargo build and cargo test (78 tests should pass) and report anything that fails. If you can
only read files and not run commands, skip that. Report only; do not refactor or restyle. Write the
report to /home/estevens/code/compositor-linux/REVIEW_RESULTS_2.md. No em dashes in your output.
