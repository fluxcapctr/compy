# Review results, round 9

Reviewed on 2026-09-20 at HEAD `3178239`, focusing on the renderer and history changes in `f3d0b7c`, as requested in `REVIEW.md`. Read `CLAUDE.md`, `README.md`, `AUDIT_FULL.md`, and the renderer and document audit reports. Application source and tests were not changed.

Found **9 issues: 8 P2 and 1 P3**. All have executed reproductions against the built library. The helper-contract issue is distinguished from defects reached through ordinary application operations. Earlier findings explicitly left unchanged by the whole-project audit are not repeated here.

## Validation

- `env -u COMPOSITOR_GPU cargo build`: passed, with five library warnings: two unused doc comments, deprecated GTK coordinate translation, unused `OP_COPY`, and unused `Assistant::present`.
- `env -u COMPOSITOR_GPU cargo test -q`: **188 passed, 0 failed, 2 ignored**. Tests also report unnecessary parentheses and an unused test variable. The ignored tests require network access or a downloaded model. GPU test success means its GPU execution was skipped.
- Standalone probe: `/tmp/round9_probe.rs`. Results: `/tmp/compositor-round9-probes.log`. Build and test logs: `/tmp/compositor-round9-build.log` and `/tmp/compositor-round9-tests.log`.
- No app, assistant, network request, or GPU execution was started. No writes were made to the user's compositor configuration or data directories. No maximum-memory allocation or interactive GTK test was performed.

## Findings

### 1. [P2] The HiDPI adjustment bounds check can skip the entire adjustment

**Locations:** `src/render/mod.rs:140-148`, `src/render/mod.rs:789-802`.

`Region::of` rounds the clip outward to whole logical units. `adjust` then multiplies that rounded extent by the surface scale and rejects the entire operation if the resulting physical rectangle exceeds the target by even one pixel. A real target need not contain an integral number of logical units.

**Executed reproduction:** render a 20 by 20 red document with an inverting Levels adjustment onto a 41 by 41 image surface with device scale 2 and context scale 1.025. The logical clip is 20.5 units, which becomes a 21-unit region and a 42-pixel readback. The bounds check returns early. The center remains red, BGRA `[0, 0, 255, 255]`, instead of cyan, `[255, 255, 0, 255]`.

This affects odd physical extents and fractional device scales, including offscreen and scaled render callers. Intersect the readback with actual physical bounds and preserve its exact placement rather than abandoning all adjustment work when outward rounding adds a fringe.

### 2. [P2] Adjustments read the original target while drawing inside a Cairo group

**Locations:** `src/render/mod.rs:670`, `src/render/mod.rs:789-797`; analogous underlying-pixel read at `src/render/mod.rs:947`.

Call `Renderer::draw` after `Context::push_group`. The current drawing destination is the group, but `adjust` reads `cr.target()`, the surface originally supplied to the context. Pixels drawn earlier in this render are in the group and are absent from that original surface. The adjustment then paints the wrong result into the group using `Operator::Source`.

**Executed reproduction:** render the same red document and inverting Levels adjustment on a scale-2 surface with translation `(3,4)` and context scale 0.75. Ordinary drawing produces cyan `[255,255,0,255]`. Wrapping the same draw in `push_group`, `pop_group_to_source`, and `paint` produces transparent `[0,0,0,0]` at the same point.

Use the current group target consistently for readback, readability checks, scale, and offset. The ordinary frame-cache caller does not itself wrap this draw in a Cairo group, so this is a confirmed renderer API failure rather than a reproduced normal-window failure. The prompt explicitly requests this group-surface case. Clipping stacks use a fresh context on their intermediate surface and are a different path.

### 3. [P2] Image Size preserves Path records with the wrong coordinate basis

**Locations:** `src/document.rs:3386-3392`; redraw consumer at `src/document.rs:1060-1065`, `src/document.rs:3828-3836`.

Image Size multiplies a Path shape's `baseWidth` and `baseHeight` but leaves its local anchors and handles unchanged. The immediate raster is correctly resized. On the next shape redraw, `path_shape_image` divides the new image size by the enlarged base dimensions and applies that factor to the old anchors, shrinking the path within its layer.

**Executed reproduction:** create a square Path from `(10,10)` to `(50,50)` on a 100 by 100 document, resize the document to 200 by 200, then increase the shape's width by one pixel through `set_transform`. Pixel `(80,80)` is opaque red before this small edit and completely transparent afterward. The redrawn path has reverted toward its original local extent instead of remaining the resized square.

Preserving editability requires retaining a consistent coordinate system for both the base dimensions and the path coordinates. This is a regression in the audit's fix for dropped shape records, not a repeat of that original issue.

### 4. [P2] Image Size bakes text orientation into pixels but discards it from the editable state

**Locations:** `src/document.rs:3373-3388`, `src/document.rs:3413`; later editing at `src/document.rs:1105-1118`.

Resize a document containing rotated or flipped text, then edit that text. `resample` paints the transformed text into an upright bounding box and resets its transform to rotation zero and no flips. The retained text record only gets size, tracking, and width multipliers. It cannot reproduce the baked orientation. Nonuniform scaling similarly loses horizontal glyph scaling that cannot be represented by changing font size alone.

**Executed reproduction:** create `Hello` at size 20, rotate its layer 90 degrees, resize the document from 100 by 100 to 200 by 200, then call `set_text` with the unchanged retained style. The image changes from 52 by 93 to 93 by 48 and the text becomes horizontal. No text content or style change was needed to trigger the appearance change.

The retained editable representation must describe the post-resize picture, including orientation and scale, rather than relying on a raster that the next edit replaces.

### 5. [P2] Image Size can commit transforms that the new validator refuses to save

**Locations:** `src/document.rs:3373-3379`, `src/document.rs:3413`; validation boundary at `src/format/mod.rs:106-108`, `src/format/mod.rs:328-330`.

The new transform limit is 30,000 pixels per side, but `resample` accepts any raster size permitted by `new_argb`, whose side limit is 32,767. It then installs the resulting transform directly, without checking `Transform::is_valid`. The canvas size can remain very small while an off-canvas layer exceeds the transform limit.

**Executed reproduction:** on a 100 by 100 canvas, add a 1 by 1 image displayed at 20,000 by 1. Resize the canvas through Image Size to 155 by 100. The operation succeeds and commits a layer transform of 31,000 by 1. `validate::manifest` rejects the resulting document. Save invokes that validation, so the successful edit leaves a document that cannot be saved until corrected or undone.

Check every proposed layer and mask transform against the persistence limits before committing the resize. The problem is an internally generated invalid state, not merely rejection of an oversized legacy file.

### 6. [P2] Geometry undo and rollback do not restore the newly moved guides

**Locations:** `src/document.rs:75-83`, `src/document.rs:124-130`, `src/document.rs:3310-3312`, `src/document.rs:3415-3417`.

The audit added guide movement to Canvas Size, Image Size, Flip, and Rotate, and made guides persistent. However, `State` still contains only renderer state, selection, and active layer. Guide vectors remain outside the before/after snapshots used by undo and `abort_edit`.

**Executed reproduction:** put a vertical guide at 50 on a 100 by 100 document, increase Canvas Size to 120 by 120 using the bottom-right anchor, then undo. The canvas returns to width 100 while the guide stays at 70. The same omission means an error after guides move cannot fully restore geometry, even when `edited` correctly restores the layers and canvas.

This is the remaining rollback portion of the audit's guide fix. Forward movement is now implemented, but the new movement must also participate in the operation's history snapshot.

### 7. [P2] Repeated Image Size operations change artboard geometry through integer rounding

**Locations:** `src/document.rs:3337-3342`, `src/document.rs:3415`.

`map_artboards` rounds the origin and dimensions after every mapping and clamps both dimensions to at least one. Layer placement and guide scaling do not use the same integer-only mapping. Frames therefore drift relative to content, particularly for small boards and repeated reductions followed by enlargement.

**Executed reproduction:** an artboard at `(1,1)` with size 1 by 1 on a 100 by 100 canvas becomes `(2,2,2,2)` after Image Size to 50 by 50 and back to 100 by 100. Its original frame was `(1,1,1,1)`. This changes the board's background, clipping, and exported dimensions.

Keep sufficient geometric precision across resizes and define consistent handling for frames that would fall below the minimum size. The current whole-project test only doubles integral coordinates and cannot expose this drift.

### 8. [P2] Coverage sampling can classify a visible thin element as absent

**Locations:** `src/document.rs:1350-1365`; Reframe consumer at `src/export_sizes.rs:110-116`.

`layer_coverage` shrinks the longest canvas side to 256 pixels, then discards every sample with alpha at or below 8. A fully opaque hairline on a large transparent layer can disappear under this reduction. Returning `None` does more than approximate its bounds: Reframe skips the layer completely when deciding which elements to reposition.

**Executed reproduction:** a 20,000 by 2 transparent image with an opaque red 1 by 2 mark at x=100 returns `None` from `layer_coverage`. It is visibly nonempty at document resolution. Such a mark follows the background's Fill crop rather than the element placement rule and can be cropped out.

Use a conservative coverage reduction or a fallback for apparently empty samples. Thresholded low-resolution coverage is useful for estimating area, but cannot establish that the original layer is empty. This case is in the round 9 requested scope although the initial coverage implementation predates `f3d0b7c`.

### 9. [P3] `edited` can consume its caller's transaction when the closure already closes its edit

**Location:** `src/document.rs:179-182`.

The new wrapper always calls exactly one `end_edit` or `abort_edit` after `f`, without checking ownership or entry depth. If the closure cancels the wrapper's edit itself and returns an error, the wrapper cancels the next outer edit. The equivalent successful-close case ends the outer edit prematurely. Conversely, if the closure leaves another nested edit open, the wrapper closes only that nested edit and leaks its own.

**Executed reproduction:** open `Outer`, add a layer, then call `edited("Inner", |d| { d.abort_edit(); bail!("already aborted") })`. After the error, history depth is zero and the layer added by `Outer` is gone. The outer transaction should still be open with its work intact.

This is a confirmed public-helper contract weakness, not an established ordinary menu trigger among the currently converted callers. Those callers generally balance their own nested edits. Either enforce and document the closure contract or track the wrapper's transaction identity/depth so cleanup cannot affect its caller. `unwind_edits_to` itself correctly does nothing when the current depth is already lower, but it cannot recover a transaction that was consumed earlier.

## Fix verification and coverage

All eight numbered areas received source review, with focused executed probes as described above. This is not a claim of exhaustive dynamic testing.

| Area | Result and limits |
|---|---|
| 1. History stack | `begin`, inner `cancel`, nested `end`, and outermost recording use the intended snapshots. Trimming runs only after an outer commit. No incorrect popped-inner-before state was found. `step_history` calls the guarded `undo`/`redo`, so it does not bypass preview refusal. The wrapper ownership gap is finding 9. |
| 2. Geometry | The new single-shift arrangement in `make_room_for`, `add_artboard`, and `set_artboard_frame` is consistent. Quarter-turn guide mapping and rotation cleanup depth arithmetic are sound on inspection. Findings 3 through 7 cover editable resampling, bounds, guide rollback, and frame precision. Legacy files with transforms above the new limit are refused by validation rather than normalized on open. |
| 3. Painting rasterization | Uneven and vector layers use an undoable rasterization step; plain uniformly scaled layers remain unchanged. Explicit mask placement remains in document coordinates; implicit nontrivial masks are rasterized with the image. Hidden-ancestor visibility is checked through `visible_layers`; collapse does not hide descendants from rendering. Existing painting tests pass. No additional confirmed defect in this function was established; rotated/clipped/masked pixel equivalence was not exhaustively tested. |
| 4. Coverage | Alpha comes from rendered layers, including effects and opacity; shadow coverage can influence classification. Bounds are quantized to the sample grid, which can represent many document pixels per sample, not a universal one-pixel error. Finding 8 establishes a false-empty case. |
| 5. Device scale | Existing scale-2 clipping-stack and adjustment tests pass. The offscreen buffers and A8 coverage carry device scale, and mask placement under identity is consistent. No double application of scale in the ordinary Blend If path was established. Findings 1 and 2 cover rounded physical bounds and drawing into groups. |
| 6. Filter math | Both calls to `ffi::levels` now receive premultiplied pixels directly. The remaining unpremultiply/premultiply pair belongs to Color Balance and does not wrap Levels. High Pass compares straight colors. Shadows/Highlights blurs coverage and premultiplied luma together; radius zero is normalized to one, and opaque coverage remains 255. No additional confirmed defect in these changes. |
| 7. Raster limits | The 400-million-pixel product check is inclusive. The independent 32,767 side cap still applies, so a long 100 MP document cannot necessarily become one 2x full-document surface. A normal 4K viewport at 2x and a 30,000 by 3,333 scale-1 surface satisfy these numeric checks. Actual maximum-size allocations were not attempted. `a8_from_data` and `argb_from_packed` wrap already allocated data and do not themselves invoke the new guard, so the audit's claim about every constructor is broader than the implementation. No new exploitable allocation path was established here. |
| 8. Tests | Both integration fix tests and the history unit test pass. Missing assertions are listed below. |

The tests leave these important gaps:

- The guide test checks forward movement and serialization, never undo, redo, or rollback after failure.
- No retained text or Path shape is edited again after Image Size.
- The HiDPI test uses an even physical surface size and does not draw into an existing Cairo group.
- The artboard resize test uses integral doubling, not subpixel downscaling or a round trip.
- The coverage test uses a substantial logo, not a mark that disappears at the 256-pixel sampling scale.
- The history unit test balances its edit calls; it does not exercise wrapper closures that close or leak edits.
- The oversized-transform test directly rejects 300,000 pixels, but never checks that Image Size cannot generate a transform in the 30,001 to 32,767 gap.
- The `round_eight_fixes` comment describes a partly off-canvas export, but its `add_artboard` and `set_artboard_frame` calls grow the canvas to contain that board. That test does not actually establish off-canvas export behavior.

Review complete for all eight numbered areas. Only this report was added to the repository.

## Status after fixes

| # | Finding | Status |
|---|---------|--------|
| 1 | The HiDPI adjustment bounds check skips the adjustment | Fixed. The readback is clipped to the pixels the target has, and the result and its coverage are placed at that clipped origin. Test on a 41-pixel scale-2 surface. |
| 2 | Adjustments read the original target inside a Cairo group | Fixed. `group_target` is used for the readback, the readability check, the scale and Blend If's underlying pixels. Test inside a pushed group. |
| 3 | Image Size leaves Path anchors on the old basis | Fixed. A Path's points scale with its base; other shapes scale their corner radius. Test: resized, then nudged. |
| 4 | Image Size bakes text orientation into pixels | Fixed. Type layers are set again from the scaled style in their own turn and flips, the mask carried along. Test: rotated type resized, then edited. |
| 5 | Image Size can commit a transform the file refuses | Fixed. Every layer's new box is checked against 30,000 pixels before anything changes. Test. |
| 6 | Guides do not undo | Fixed. Guides are part of the history snapshot. Test: undo and redo. |
| 7 | Frames drift through rounding | Fixed. Frames keep their precision; only the one-pixel floor remains. Test: a halving and a doubling return exactly. |
| 8 | A hairline counts as empty | Fixed. Any alpha counts, and an empty 256-pixel sample is checked again at 2,048 before the layer is called empty. Test: a one-pixel mark on a 20,000-pixel layer. |
| 9 | `edited` can take its caller's edit | Fixed. It closes exactly the edit it opened: one the closure left open is abandoned first, one the closure closed is not closed again. Test for both. |

Tests: 189 passed, 2 ignored.
