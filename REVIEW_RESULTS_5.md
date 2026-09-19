# Review results, round 5

Reviewed on 2026-09-19 at HEAD `0b00ae4`. Scope was commits `272d01e^..ac49b7f`. Earlier findings are not repeated. No source files were changed.

`cargo build` succeeded. `cargo test` succeeded: **147 passed, 0 failed, 2 ignored**. The ignored tests are the Google Fonts network test and the Remove Background model test.

## Findings

### 1. [P1] Free Transform commit fails permanently when the float is entirely off canvas

**Locations:** `src/document.rs:735-757`, `src/document.rs:711-716`.

Select pixels, press Ctrl+T, and drag the floating layer so its complete bounds are outside the document. `merge_floating` correctly builds a grown source grid, but then calls `select_layer_pixels(float)`. That helper clamps the float bounds to the canvas at lines 653-659 and returns `None` when the intersection is empty. The function returns an error before removing the float or restoring the selection. `commit_free_transform` has already taken `self.floating`, calls `end_edit()` even on that error, and leaves the `Floating Selection` layer in the renderer with the source pixels already cut out.

The next Return or Escape sees no active float, so the orphan cannot be completed or cancelled through the normal command. Either carry the saved selection geometry through the merge or handle an empty canvas intersection as a valid commit. This is a data-loss and stuck-document path explicitly covered by the round 5 prompt.

### 2. [P1] Layer Via Copy and Cut ignore the source layer's mask

**Locations:** `src/document.rs:650-672`, especially line 666; `src/render/mod.rs:592-596`.

Add a Hide-All or partially black mask to a pixel layer, select an area, and invoke Layer Via Copy or Layer Via Cut. `selected_pixels` calls `draw_layer_plain`, whose contract is to draw only the layer's own image and no mask. Hidden pixels are therefore copied into the new layer. Cut also clears those hidden source pixels, changing the document even though the user could not see them.

The same unmasked extraction feeds Free Transform at line 703, so a masked source loses mask semantics before the floating merge. Render the selected pixels through the source mask, including placed masks and folder/clipping coverage, before creating the copy.

### 3. [P1] Auto Tone, Auto Contrast, and Auto Color use the image histogram while a mask is targeted

**Locations:** `src/document.rs:1297-1301`, `src/document.rs:477-479`, `src/document.rs:418-424`.

Select a layer mask and invoke any Auto Levels command. The command calls `histogram()`, which always reads `active_image()` and the image selection coverage. It then calls `apply_filter`; only afterward does `apply_filter` notice `active_mask()` and apply the generated Levels to the mask. The Levels are derived from RGB image pixels rather than the mask's grayscale values, so a mask with a narrow range is usually stretched with unrelated endpoints.

Compute the histogram from the active mask when `mask_target` is true, using the mask's placement and selection coverage. The ordinary Levels dialog has the same mismatch because it also initializes its histogram through `Document::histogram`.

### 4. [P1] Fade remains valid after unrelated edits to the layer

**Locations:** `src/document.rs:47`, `src/document.rs:328-332`, `src/document.rs:426-430`.

Apply a filter, then move or rotate the layer, change its opacity or visibility, or change its mask without replacing the image surface. `last_filter` stores only the layer UUID and two surfaces. Fade checks the UUID and compares the current image's raw pointer with the stored `after` surface. Those edits leave the pointer unchanged, so Fade still blends the old pre-filter image with the now differently placed or otherwise changed layer.

The pointer check is also vulnerable to allocator reuse after a surface is freed. Invalidate `last_filter` on every document or layer mutation that can change what the user sees, and use an edit/image revision rather than a raw Cairo pointer as the validity token. The field also retains two full surfaces until another filter or a Fade replaces it.

### 5. [P2] The 100 megapixel Free Transform fallback silently clips the transformed pixels

**Location:** `src/document.rs:732-747`.

Transform a selection whose source-grid union would exceed 30,000 pixels on a side or 100 megapixels. Instead of returning an error, the code resets all growth offsets to zero and renders the float into the original source grid. Pixels outside that grid are clipped while the command still returns success and completes the edit.

The caller should receive the same explicit size-limit error used by filters and text rendering, or preserve the pre-transform state. Silent clipping is especially dangerous for a large off-canvas or rotated float.

### 6. [P2] A rotated Layer Via copy has an axis-aligned, incorrectly oriented result

**Locations:** `src/document.rs:652-669`, `src/document.rs:681-684`.

Copy or cut a selection from a layer with a nonzero rotation. The extraction rectangle is computed from `transform.bounds()`, and the drawn pixels are then stored in a new layer with an unrotated transform whose origin and size are that document rectangle. The copied raster is no longer represented by the source rotation, so its pixels are stretched or rotated relative to the original. Free Transform starts from this same result.

Use the source transform for the new pixel grid or rasterize the selected quadrilateral into the new layer's transform. The current implementation only works for unrotated layers.

### 7. [P2] Color Balance does not preserve luminosity when clamping is needed

**Location:** `src/filters.rs:208-219`.

Use a strong asymmetric Color Balance adjustment on a saturated pixel with Preserve Luminosity enabled, such as a full red pixel with positive red and green shifts. The code first clamps each adjusted channel, computes one luminance delta, then clamps each channel again. If the delta pushes a channel below zero or above one, the second clamp prevents the requested luminance from being restored. The output's luma changes even though the option is enabled.

Apply the luminance correction with a bounded scale or redistribute the correction after clipping, then premultiply. The existing test checks a mid-gray pixel where no channel reaches a bound and therefore misses this case.

### 8. [P2] Layer Style can be reopened on the same layer while an edit is already open

**Locations:** `src/ui/effects.rs:12-25`; dialog launch in `src/ui/mod.rs:1018-1040`.

Open Layer Style twice for the same layer before closing the first window. Each call invokes `begin_layer_style`, increasing the history nesting depth. Closing one dialog calls `end_layer_style`, but the other dialog still owns the same shared document edit. Its later close can commit or abort the combined session with the wrong original state, and a normal Undo cannot occur until both nesting levels have ended.

There is no per-layer dialog registry or guard in the new `begin_layer_style` path. Prevent a second dialog for an already edited layer, or make the session owner and nesting explicit. A similar stale-window case occurs when the layer is deleted while a style window remains: the callback calls `end_layer_style` and `finished`, but does not verify that its original layer still exists before committing the open edit.

### 9. [P2] Curves can become numerically invalid when loaded points are not strictly monotonic

**Locations:** `src/filters.rs:578-590`, adjustment parsing at `src/filters.rs:648-657`.

`Curves::value` divides each segment by the difference between adjacent x coordinates. The UI prevents duplicate x values, but `Adjustment::from_kind` accepts any point list with at least two points and does not normalize or reject non-increasing x values. A malformed or legacy adjustment record with duplicate x values reaches `value`, producing NaN or infinity and then a bad lookup table.

`record_is_valid` rejects such records when validating project input, but the public parser used by adjustment editing still accepts them and silently substitutes the points. Enforce the same strict ordering in `from_kind` or normalize points before every evaluation.

### 10. [P3] The new round 5 test suite still does not test the mask-target Auto Levels path

**Location:** `tests/features_g.rs:229-280`.

The new test exercises Curves, Color Balance, Fade, and Auto Levels only on ordinary image layers. It never adds or selects a mask before Auto Tone, Auto Contrast, or Auto Color. Consequently the mismatch in finding 3 passes all tests. Add a mask with known grayscale endpoints and assert that the generated Levels are based on those mask values.

## Coverage limits

The build and full test suite ran successfully. Source review covered the round 4 fixes, Curves, Color Balance, automatic Levels, Fade state, on-canvas typing, the start page and preset parser, transform handles, layer-row styling, and the added tests. No live GTK display session was run, so focus transitions, duplicate dialogs, point-hit geometry and CSS rendering were checked statically rather than through actual pointer events. The report stopped after numbered area 8's dialog/lifecycle checks and the filter and test areas; no further numbered area remains.
