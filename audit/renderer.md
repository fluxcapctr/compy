# CPU renderer audit (Compy / compositor-linux)

Scope: `src/render/mod.rs`, `src/render/live.rs`, `src/raster.rs`, `src/ffi.rs`, `src/viewport.rs`,
`csrc/*.c`, and the parts of `src/document.rs` that hand surfaces to the renderer. Nothing was edited,
no cargo, no app run. Two small standalone C programs were compiled against the system Cairo to pin down
`cairo_user_to_device` and group-surface semantics (results quoted below); they touch nothing in the project.

Severity: P1 crash or memory corruption, P2 wrong pixels, P3 minor.

**No P1 was found.** Every indexing path I traced in the renderer is guarded, the C calls all respect
their strides, and the region and size caps keep allocations inside Cairo's own limits.

---

## The Cairo fact three findings depend on

`cairo_surface_set_device_scale` is **not** folded into the CTM. Verified:

```
surface 80x80, device scale 2, fresh context
  ctm = 1 0 0 1 0 0 ; user_to_device(1,1) = 1 1 ; after cairo_identity_matrix: still 1 1
  push_group -> group surface 80x80 pixels, device scale 2, offset 0
```

So `Region::of` (`src/render/mod.rs:135-151`) returns **CTM-device units (logical points)**, while
`ImageSurface::width()/stride()` are **real pixels**. `apply_blend_if` knows this (it multiplies by
`own.device_scale()`, the round 7 fix). `adjust`, `Region::offscreen` and `device_scale()` do not.

A second run reproducing the canvas at zoom 1 on a 2x screen (`translate(48,30)`, `scale(0.5,0.5)`,
clip = a 1000x800 document, target 1600x1200 pixels):

```
clip user 0 0 1000 800 -> device 48 30 548 430   (surface 1600 x 1200)
```

Region = (48, 30, 500, 400) logical; the frame it must address is 1600x1200 pixels.

---

## Findings

### 1. [P2] Clipping-stack children are clipped in document units instead of device pixels

`src/render/live.rs:68-71` (in `draw_composite`)

```rust
gcr.set_matrix(Matrix_for(cr, region));
gcr.rectangle(0.0, 0.0, region.width as f64, region.height as f64);
gcr.clip();
```

`Matrix_for` makes `gcr`'s user space the same as `cr`'s, which is **document pixels**. `region.width`
and `region.height` are **device pixels of the clip**. The rectangle is therefore a document-space box
whose numbers happen to be a device size, and it is intersected with everything the clipped children draw.

Trigger: any clipping mask (Alt-click a row, or Alt+G) while the canvas is not at exactly one document
pixel per point with the whole document visible.

* Fit zoom, 4000x3000 document in an 800x600 view: children are clipped to document (0..800, 0..600),
  so every clipped layer outside the document's top-left corner disappears.
* Zoom 1 on a 2x screen (numbers above): children survive only inside document (0..500, 0..400).
* Zoom 1, document larger than the window and panned: the clip is shifted by the pan, so a band of
  `|rx|` pixels of every clipped layer is cut on the right and bottom.
* At zoom >= `CRISP_ZOOM` the other canvas path (`src/ui/canvas.rs:1893-1899`) uses
  `translate(-x0,-y0)`, so the clip is shifted by the scroll offset in the same way.
* `render_layer` (`src/render/mod.rs:743-757`) and `render_artboard` (`:718`) hit it too whenever the
  exported origin is not (0, 0) or `scale != 1`, so Export Layers and Export Artboards of a clipping
  stack cut the same way.

It is exactly right only in `render_flat` (identity matrix, region at the origin), which is what every
test uses, so the suite cannot see it. The reference (`reference/Compositor/Rendering/LiveMaskRenderer.swift`,
`drawComposite`) adds no clip at all here; the group surface bounds the children by itself. The intent
in the comment ("A clip, not a path") is fine, but the rectangle has to be set under
`gcr.identity_matrix()` (or `save`/`identity_matrix`/`rectangle`/`clip`/`restore`).

Confidence: confirmed by tracing the full path plus the Cairo numbers above.

### 2. [P2] `adjust` mixes logical units and surface pixels, so adjustment layers are wrong on a HiDPI canvas

`src/render/mod.rs:774-848`, specifically the readback at `:783-787` and the write-back at `:843-847`.

```rust
let (tw, th) = (target.width() as i32, target.height() as i32);      // pixels
if region.x < 0 || ... || region.x + region.width > tw { return Ok(()); }  // logical vs pixels
... data[(region.y + r) * stride + region.x * 4 ..]                   // pixels, indexed with logical
cr.identity_matrix();                                                 // does NOT drop the device scale
cr.set_source_surface(&result, region.x, region.y);                   // painted back at 2x
```

The frame cache is created at `w*scale x h*scale` with `set_device_scale(scale, scale)`
(`src/ui/canvas.rs:176-177`), and `Viewport` treats `backing_scale` as a first-class quantity, so
`scale == 2` is a normal state, not a corner case.

On a scale-2 display an adjustment layer therefore: reads the pixel rectangle
`(region.x, region.y, region.width, region.height)`, which is the **top-left quarter of the view**;
adjusts that; and paints it back over the **whole view magnified 2x**. The coverage mask is built
correctly in shape (its matrix comes from `cr.matrix()`), so the result is masked to the right region,
but the pixels inside it are the wrong ones, doubled. The bounds check passes because it compares
logical numbers against pixel dimensions, so nothing is caught and nothing reads out of bounds.

At scale 1 everything lines up and the existing tests (all scale 1) pass. The same class of bug was
found and fixed for Blend If in round 7 (`apply_blend_if` at `:913-965` now goes through
`device_scale()`/`device_offset()` of both surfaces and has a 2x test); `adjust` was not given the
same treatment. `units = 1.0 / device_scale(cr)` at `:791` is wrong by the same factor, so Grain's
pattern is also the wrong size on HiDPI.

Confidence: confirmed by tracing plus the measured Cairo behavior.

### 3. [P2] Levels, Curves, Exposure and Brightness/Contrast unpremultiply twice, darkening every soft edge

`src/filters.rs:953-959` (`apply_tables`), reached from the renderer at `src/render/mod.rs:792`
(`adjustment.apply(...)` inside `adjust`); the same pattern is in the Filter-menu path at
`src/filters.rs:617-620`.

```rust
unpremultiply_partial(pixels);   // p = straight
ffi::levels(pixels, count, &tables);
premultiply_partial(pixels);
```

`levels_apply` in `csrc/LevelsPixels.c` already does the un- and re-premultiplication itself:

```c
float x = fminf(255, p[channel]*255.0f/alpha);
...
p[channel] = (uint8_t)fminf(alpha, fmaxf(0, roundf(result*alpha)));
```

So for any pixel with `0 < alpha < 255` the lookup input becomes `s*255/a` instead of `s`, and the
result is multiplied by alpha a second time. Net: `straight_out = a/255 * T(min(255, s*255/a))` where
the correct value is `255 * T(s)`.

Worked example: a 50 percent white pixel (premultiplied 128,128,128,128; straight 255) under a Levels
that leaves white alone comes out premultiplied 64 at alpha 128, that is mid gray. Fully opaque and
fully transparent pixels are untouched (both helpers skip `a == 0` and `a == 255`), which is why the
analytic tests, all on opaque layers, do not see it.

Trigger: any Levels/Curves/Exposure/Brightness-Contrast adjustment layer over antialiased layer edges,
a feathered mask, or a layer at less than full opacity; also Image > Levels on a cutout.

Confidence: confirmed by reading both sides of the boundary.

### 4. [P2, latent] `draw` leaves a stray path on the offscreen context

`src/render/mod.rs:660-667`

```rust
let (surface, inner) = region.offscreen(cr)?;
inner.rectangle(region.x as f64, region.y as f64, region.width as f64, region.height as f64);
self.draw_artboard_backgrounds(&inner)?;
```

The rectangle is never clipped or filled, so it stays as `inner`'s current path. Cairo does not clear
the path on `save`/`restore`, so the first `fill()` or `clip()` that follows consumes it as part of its
own path: an artboard background would be filled over a second bogus rectangle, and the first
`cr.rectangle(); cr.clip()` inside `paint_own` would widen that layer's clip to the union, smearing the
`Extend::Pad` edge pixels across the region. This is the failure mode that the comment in `live.rs:68`
describes. The rectangle is also in the wrong space (device numbers as user coordinates), so it is only
the intended region when the matrix is the identity.

This branch runs only when the target is not an `ImageSurface` and an adjustment or a Blend If that
reads the underlying layers exists. Every current call site passes an image surface (the frame cache,
`render_flat`, `document.rs` sampling buffers), so it is latent today, and any new caller drawing onto a
recording, PDF or SVG surface would hit it. Either clip it (under `identity_matrix`) or drop it.

Confidence: confirmed by reading; not reachable from today's call sites.

### 5. [P3] The mip level is one step too deep on a HiDPI display

`src/render/mod.rs:1199-1204`

```rust
pub(crate) fn device_scale(cr: &Context) -> f64 { let m = cr.matrix(); (m.xx()*m.xx() + m.yx()*m.yx()).sqrt() }
```

This is the CTM scale only, and the canvas CTM carries `points_per_pixel() == zoom / backing_scale`
(`src/viewport.rs:29`). The real device pixels per document pixel is `zoom`. Every consumer
(`reduced` at `:1054`, `paint_own` at `:975`, `through_masks` at `:1013`, `draw_layer_plain` at `:617`,
`draw_mask_plain` at `:594`, `adjust`'s coverage at `:812`) therefore asks for half the pixels it needs
on a 2x screen, and `level_for` picks one halving too many whenever the effective factor drops below
0.5. The layer is then drawn from a copy half the size it should be and scaled back up with Bilinear,
so everything below zoom 1 is visibly soft on HiDPI. `interpolation()` also chooses Bilinear where it
should choose the layer's quality filter.

Related, same root cause: `Region::offscreen` (`:145-150`) creates `new_argb(region.width, region.height)`
with device scale 1, so every clipping stack, every clip coverage and the whole adjustment group are
composited at logical resolution and upscaled onto a 2x target. Clipped layers are noticeably softer
than unclipped ones on HiDPI.

Confidence: confirmed by tracing plus the measured Cairo behavior.

### 6. [P3] Overlapping artboard backgrounds are painted in the wrong order

`src/render/mod.rs:686-695`

```rust
let boards = entries_ordered(&self.layers, true) ... // top first
for (x, y, w, h, c) in boards { ... cr.rectangle(x, y, w, h); cr.fill()?; }
```

`entries_ordered(.., true)` is top first, and the fills are opaque, so the last board painted (the
bottom-most) covers the ones above it. With two overlapping boards the lower board's background colour
wins in the overlap while its layers still draw underneath. Layers are unaffected (they are drawn
bottom to top separately). Use `entries_ordered(.., false)` here.

Confidence: confirmed by reading; needs overlapping artboards to show.

### 7. [P3] `styled` drops the preview pixels but keeps the preview placement

`src/render/mod.rs:477-480` and `:968-972`

`styled()` temporarily removes `previews[id]` so effects render from committed pixels, but leaves
`preview_transforms[id]` in place, and `paint_own` reads
`self.preview_transforms.get(&id).copied().unwrap_or(layer.transform)` regardless of which buffer it
ended up using. If the effects buffer has to be built while a placed preview is live (Smudge, Liquify,
or a Gaussian Blur preview that grew the layer, all of which call `set_preview_placed` with a
canvas-sized grid), the layer's own pixels are stretched over the preview's rect and the shadow, glow
and stroke are computed from that. The cache key does not contain `preview_transforms`, so the usual
case (styled already built before the preview started) is unaffected; the bug needs a rebuild during
the preview.

Confidence: confirmed by reading, narrow trigger.

### 8. [P3] A clipped layer paints over its base's own drop shadow

`src/render/live.rs:57-64`

`draw_own(base, &gcr, ...)` draws the base's `below` and `above` effect buffers into the stack group,
and the coverage is extracted from the group **after** that, so the base's exterior effects become part
of the clipping coverage and the clipped children spill onto them. Photoshop clips to the base's own
transparency and keeps exterior effects under the whole group. The reference's `drawOwn` renders no
layer styles at all, so there is no spec answer here; this is a difference this port introduces by
adding styles to `draw_own`.

Confidence: plausible (behavior traced, expected result taken from Photoshop, not from the reference).

### 9. [P3] A separately placed mask is sampled with no mip level

`src/render/mod.rs:981-984`

```rust
Some(MaskSource::Placed(surface)) => Some((surface, 0, 1.0, 1.0)),
```

A mask moved apart from its layer (unlinked, or given its own `mask_placement`) is resampled once into
the layer's pixel grid by `place_mask` and then drawn at full size with the image's filter, that is
Bilinear from a full-resolution A8 when zoomed out, while every other mask path goes through
`reduced()`. The placed mask aliases at low zoom where the same mask, linked, does not. `place_mask`
itself already picks a halving for its own source (`:1148-1150`), so only the final draw is missing it.

Confidence: confirmed by reading.

### 10. [P3] An adjustment layer clipped to a hidden base vanishes; a pixel layer in the same place does not

`src/render/live.rs:47-52` with `coverage` at `:102-121`

`prepare_stacks` only sees visible layers, so with the base hidden the adjustment is not in a stack,
and `draw_composite` drops it (`if self.source(id).is_none()` fails). A non-adjustment layer clipped to
the same hidden base still draws, because `coverage()` deliberately ignores the source's visibility
(documented at the top of `live.rs`, and matching the reference). So the same arrangement behaves two
different ways depending on the clipped layer's kind. The reference has the identical structure, so
this is a faithful port of a reference quirk rather than a new defect; worth knowing about.

Confidence: confirmed by tracing, spec-faithful.

### 11. [P3] The manifest validator caps each side but not the canvas area

`src/format/validate.rs:15` checks `1..=MAX_SIDE` per side and the layer count, but never
`width * height <= MAX_PIXELS`, while `check_size` (`src/format/mod.rs:395-401`) charges the 100 MP
budget only against decoded layer images, and every runtime path (`document.rs:756, 1003, 1027, 1174,
3082`) enforces 100 MP. A crafted or hand-edited `.comp` can declare a 30,000 x 30,000 canvas. Then
`render_flat` fails with a Cairo error (no crash), and more quietly `Region::of` returns `None` for
anything over 100 MP, which makes `draw_composite` return without drawing the stack at all and
`coverage()` return `None` so clipped layers disappear. The reference falls back to drawing the base
unstacked when the group buffer cannot be made (`LiveMaskRenderer.drawComposite`); this port draws
nothing.

Confidence: confirmed by reading.

### 12. [P3] The per-draw coverage cache is keyed by layer only, but the clip changes per artboard

`src/render/live.rs:102-120`. `coverage` stores `(surface, region)` under the source's id for the whole
draw, while `draw_within_artboard` (`src/render/mod.rs:763-769`) narrows the clip, and therefore the
region, per artboard. Two layers in different artboards clipped to the same source would reuse the
first board's region and be masked to that board. The UI cannot build such a link (`toggle_clipping`
only links siblings, so the source shares the artboard) and `live_mask_graph` does not check the
parent, so it takes a hand-written file. Cheap fix: key the cache on `(id, region)`.

Confidence: plausible, not reachable through the UI.

### 13. [P3] Peak memory of `styled`

`src/render/mod.rs:465-489`. The padded rectangle is allowed up to 120 megapixels, above the 100 MP
document limit, and a build allocates the ARGB buffer (480 MB), the packed alpha (120 MB) and then up
to three packed RGBA results from `effects.render_at` (480 MB each) before any of them are turned into
surfaces. A large layer with a wide-reach effect can transiently need about 1.5 GB. Nothing overflows;
the cap is just generous.

---

## Checked and found sound

* **Compositing order.** `draw_own` matches the reference: effects `below`, then the layer (interior
  effects `Atop` inside their own group, the round 4 fix), then `above`, then Blend If, then folder
  masks and the clip coverage, then the blend mode and opacity on the group. The `direct` fast path
  has exactly the conditions that make it equivalent.
* **Clipping stacks.** Opacity applied to the base inside the group, `extract_alpha` /
  `unpremultiply_opaque` / children / `restore_alpha`, then folder masks and the base's blend mode on
  the whole group, matching `LiveMaskRenderer.drawComposite` step for step (apart from findings 1 and 8).
* **Group opacity and blend.** Folders are flattened by `visible_layers` and only their masks are
  applied. The reference does the same (`ImageExporter.render`), and `validate::manifest` refuses a
  folder with a non-default opacity or blend, so there is nothing to lose.
* **Clipping to an adjustment layer.** Blocked in `can_clip` and in `live_mask_graph`, so the case that
  would produce an empty coverage and a vanished layer cannot be loaded or created.
* **Clipping chains.** `toggle_clipping` resolves to `below.mask_source_id.unwrap_or(below.id)`, so
  there is no A -> B -> C chain for `prepare_stacks` to break on.
* **Blend If.** Luminance is computed as `0.114*p[0] + 0.587*p[1] + 0.299*p[2]`, correct for Cairo's
  B, G, R, A order; the source pattern stays locked to the user space it was set in, so the
  `identity_matrix` juggling in `apply_blend_if` and `through_masks` is safe; the round 7 device
  scale and offset handling is right (I re-derived it at 2x).
* **`adjust` blend mode and opacity.** Alpha is extracted, both sides unpremultiplied, blended,
  alpha restored, then the premultiplied lerp for opacity, which is the reference's sequence; the
  coverage is the folder masks times the layer's own, each clipped to its own rectangle
  (the round 1 fix), and `Operator::Source` under a mask is Cairo's lerp, which is what is wanted.
* **Transparent pixels.** `levels_apply`, `map_straight`, `high_pass`, `ShadowsHighlights` and
  `layer_unpremultiply_opaque` all skip or clamp at `alpha == 0`, so no adjustment can write a
  colour above its own alpha into the canvas.
* **The C boundary.** Every `extract_alpha` / `restore_alpha` / `unpremultiply_opaque` call passes a
  matching stride (A8 stride from `stride_for_width` in `live.rs`, packed `w` in `styled` and `adjust`),
  and `ffi::check` would catch a short buffer before the call. `argb_from_packed`'s
  `stride == width * 4` assertion holds for ARGB32 at any width Cairo accepts. `swap_red_blue` is
  applied around, and only around, the two luminance routines (`gradient_map`, `grain`).
* **`halve_into`.** Tap indices stay inside `[sy0, sy1)` and `[0, w)` in both the padded and the clamped
  branch; the destination size is verified against the source; `halved_span` growth is clamped to the
  destination. The preview halvings built by `begin_preview` are a prefix, so `halved_copy` never
  rebuilds from a stale level.
* **Surface aliasing.** `with_bytes_mut` is only used on the exclusively owned stack group after its
  context is dropped; `with_bytes_raw_mut` is only used on buffers this crate owns and is not drawing
  with at that moment; `adopt_preview` hands the stroke's surface over only after `flush`/`heal`, and
  the next stroke allocates a fresh grid. Cached `placed`, `halved`, `styled` and `thumbnails` entries
  are outputs and are never written in place.
* **Pointer keys.** `State::eq` and `restore` compare `to_raw_none()` between two simultaneously live
  surfaces, where equal pointers do mean the same surface, and `styled`'s stored address cannot go
  stale because every path that changes `images[id]` or `masks[id]` calls `invalidate(id)` first
  (`restore` included, via its per-id pointer comparison). The address-reuse hazard is real in form
  but not reachable.
* **Cache invalidation.** I walked every `&mut self` method on `Renderer`: all of them call `touch()`,
  so the canvas frame cache cannot show a stale frame; `placed` is dropped by
  `set_layer_transform`, `set_mask_placement`, `set_artboard`, `replace_layers`, `invalidate`, and by
  all four mask-preview entry points; `styled`'s key covers transform, mask placement, mask enabled,
  both surface identities, the canvas size (the round 5 fix) and the effects JSON; `halved` and
  `thumbnails` are dropped by `invalidate` and moved correctly by `adopt_preview` and
  `adopt_preview_cropped`. The only gap is cosmetic: `set_mask_linked` does not drop `placed`, and
  `restore` does, but `mask_for` never reads `mask_linked`, so nothing depends on it.
* **Flips, rotation and the reduced-copy padding.** `place()` plus the `w*ws` by `h*hs` rectangle keeps
  the halving's round-up padding on the image's own right and bottom edge under flips, so the padded
  band lands where the source's edge pixels are.
* **Bounds.** `Region::of` rejects non-finite or oversized regions before any allocation;
  `stride_for_width` is always used with `?`, never unwrapped; `sync_mask_preview`'s rectangle comes
  from tile bounds already clipped to the grid; `render_layer` checks the id before indexing;
  `mask_background`'s `unwrap` is guarded by every caller.
