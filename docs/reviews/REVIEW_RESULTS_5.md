# Review results, round 5

## Status (2026-09-19)

All 9 findings fixed; regression tests in tests/features_f.rs and tests/features_g.rs.

| # | Finding | Fix |
|---|---|---|
| 1 | Oversized Free Transform merge clips silently | The merge refuses with an error and the transform aborts back to before Ctrl+T |
| 2 | Save during Layer Style persists a cancelled effect | Previews live on the renderer outside the history; save writes the original record |
| 3 | Layer Style used a document-wide nested transaction | One session per document, scoped to its layer; OK makes one step, Cancel restores the record, other edits stand |
| 4 | Dark mask edge hides pixels landed on a grown grid | The mask grows white with the grid, as pixel moves already did |
| 5 | Auto Levels and the histogram read pixels under a mask target | Both refuse with a message when a mask is targeted |
| 6 | Fade stays offered after later edits | An edit serial counts every step, undo and redo; Fade needs it unchanged since the filter |
| 7 | Styled cache stale after canvas growth | The canvas size is part of the cache key |
| 8 | Color Balance luminosity drifts when a channel clips | The missing luma is redistributed to channels with room |
| 9 | Parked handles outlive their layer | Parking is tied to the layer and cleared by a new transform, another layer or Transform Controls |


Reviewed again on 2026-09-19 from checkout HEAD `c5d5b17`. The requested code scope remains commits `272d01e^..ac49b7f`. Changes after `ac49b7f` were not added to the audit scope. Earlier round findings are not repeated. No application source files were changed.

`cargo build` succeeded. `cargo test` succeeded: **147 passed, 0 failed, 2 ignored**. The ignored tests are the Google Fonts network test and the Remove Background model test. The full suite was rerun after the second source pass.

## Findings

### 1. [P1] Free Transform silently clips pixels when the merged grid exceeds the size limit

**Locations:** `src/document.rs:732-747`; reference behavior in `reference/Compositor/Document/FloatingSelection.swift:128-137`.

Select a small area, start Free Transform, and enter a width such as 300,000 pixels in the Move inspector. That value is explicitly allowed by the transform controls. If the union of the source grid and transformed float exceeds 30,000 pixels on a side or 100 megapixels, line 735 discards every growth offset and resets the destination to the original source dimensions. Cairo then draws the transformed float into that small grid, clipping everything outside it. The command returns success, removes the floating layer, and closes the undo transaction.

The Swift implementation rejects this merge with `ProjectError.tooLarge`, restores the complete pre-transform document, and leaves the user with an error. The Rust path should also return an error and abort the open edit. Silently accepting the operation loses transformed pixels.

### 2. [P1] Saving during Layer Style can persist an effect that Cancel removes from memory

**Locations:** `src/document.rs:1061-1064`, `src/document.rs:2202-2213`; dialog is nonmodal at `src/ui/effects.rs:69`.

Open Layer Style, change an effect, save the document while the nonmodal style window remains open, then press Cancel. `save` writes the live preview and calls `history.mark_saved()` while the Layer Style transaction is still pending. Cancel subsequently restores the state from before the dialog. The history revision still equals the saved revision, so `is_modified()` becomes false even though the in-memory document no longer matches the file on disk. Closing the document produces no save prompt, and reopening it brings back the effect the user cancelled.

This was reproduced with a core probe: the live document had no effects and reported unmodified after Cancel, while reopening the saved file restored the previewed stroke. Saving must first resolve the open Layer Style session, or it must refuse to mark the pending revision as saved.

### 3. [P1] Layer Style uses one global nested history transaction, so a second dialog and unrelated commands corrupt Cancel and Undo semantics

**Locations:** `src/ui/effects.rs:13-29`, `src/document.rs:1061-1074`, `src/history.rs:43-70`.

Open Layer Style twice for the same layer. Each window calls `begin_layer_style`, increasing the same history depth. Cancelling either window while the other is open only decrements the depth and restores nothing, so its preview remains despite Cancel. The window closed last can then commit or abort the combined state based on its own button, regardless of what the other window requested.

The window is nonmodal, so the same outer transaction also absorbs ordinary edits made elsewhere. For example, deleting another layer while Layer Style is open and then pressing Cancel restores the deleted layer. Pressing OK commits that deletion under the name `Layer Style`. Undo is unavailable while the depth is nonzero. Guard against a second style session and give the dialog ownership of only its target layer's effects instead of using a document-wide nested transaction.

### 4. [P2] A source mask with a dark edge hides pixels moved into a grown Free Transform grid

**Locations:** `src/document.rs:749-759`; correct mask growth already exists at `src/document.rs:1695-1712`; reference behavior at `reference/Compositor/Document/FloatingSelection.swift:148-155`.

Put a black-border, white-center mask on a layer, select visible pixels from the center, and Free Transform them beyond the old layer grid. When the source grid grows, line 755 converts the old mask into a placed mask. Pixels outside that placement receive `mask_background`, which is black when most edge pixels are black. The newly landed pixels are therefore present in the source image but completely hidden.

The Swift merge grows an attached mask to the new raster size, fills the new area white, and draws the old mask over its original position. Rust already implements that behavior in `grown_mask` for ordinary pixel moves. `merge_floating` should use the same white-growth operation. A core probe confirmed that a moved red region had alpha zero after the current merge.

### 5. [P2] Auto Levels and the Levels histogram read image pixels while a layer mask is targeted

**Locations:** `src/document.rs:474-478`, `src/document.rs:1297-1300`, `src/document.rs:418-424`; dialog initialization at `src/ui/filter_dialog.rs:35`.

Target a mask whose gray values occupy a different range from its layer image, then invoke Auto Tone, Auto Contrast, or Auto Color. `auto_levels` calls `histogram()`, which always reads `active_image()` and maps the selection through the image transform. `apply_filter` later notices the mask target and applies those image-derived Levels settings to the mask. The result uses unrelated black, white, and gamma values. A separately placed mask also gets the wrong selection mapping.

The ordinary Levels dialog has the same misleading histogram because it uses the same method. Either color adjustments should be disabled for masks, as the Swift reference does through `canAdjustColors`, or the Linux mask-filter feature must compute a grayscale histogram from the active mask using its placement and selection coverage.

### 6. [P2] Fade remains available after later edits to the filtered layer

**Locations:** `src/document.rs:328-342`, `src/document.rs:418-430`, and the `last_filter` field at `src/document.rs:46-47`.

Apply a filter, then move or rotate the layer, change its opacity, visibility, blend mode or mask, and invoke Fade. The stored state is considered valid whenever the active layer UUID and Cairo image pointer still match. All of those edits leave the image surface pointer unchanged, so Fade blends the old pre-filter pixels even though the filter is no longer the immediately preceding layer operation.

There is a second stale path: apply an image filter, target the mask, apply a mask filter, return to the image, and invoke Fade. The mask branch at lines 418-424 never clears the earlier image `last_filter`. Track a document or layer edit revision and invalidate Fade on later mutations, including mask filters. The current two-surface state also remains allocated until another image filter replaces it.

### 7. [P2] The styled-layer cache is stale after a top-left canvas expansion

**Locations:** `src/render/mod.rs:442-477`, `src/render/mod.rs:526`, `src/document.rs:2627-2634`.

Place a styled layer completely outside a small canvas and render once, then expand the canvas from the top-left so the layer becomes visible without changing its transform. `styled` clips its cached surface against the current canvas size, but the cache key at line 448 does not contain that size. `Renderer::set_size` also leaves the `styled` cache intact. The raw layer pixels appear in the expanded canvas, while its overlay, stroke, glow, bevel and shadows continue using the old empty or clipped cache.

This was reproduced with a blue layer and a red Color Overlay: after expanding a 40-pixel canvas to 120 pixels, the newly visible layer rendered blue. Changing its transform forced a cache rebuild and made it red. Include the canvas size in the cache key or clear styled surfaces in `set_size` and restore paths that change the size.

### 8. [P2] Color Balance does not preserve luminosity when its correction clips a channel

**Location:** `src/filters.rs:208-219`.

With Preserve Luminosity enabled, apply Shadows red `+100` to a black pixel. The first pass produces RGB `(0.25, 0, 0)`. The luma correction subtracts about `0.075` from all channels, but green and blue clamp at zero. The final pixel remains approximately `(0.175, 0, 0)`, with a positive luma even though the original luma was zero. Similar drift occurs at saturated highlights.

The implementation performs one additive luma correction and then independently clamps all three channels. Once a channel clips, the requested luma cannot be maintained by that single delta. Redistribute the residual after clipping or use a bounded luminance-preserving conversion. The current test covers mid-gray, where no channel reaches a bound.

### 9. [P2] Parked transform handles stay hidden when a new transform starts or Transform Controls is turned back on

**Locations:** `src/ui/canvas.rs:522-524`, `src/ui/canvas.rs:716-729`, `src/ui/tools.rs:61-64`, `src/ui/mod.rs:437`, and the layer-panel callback at `src/ui/mod.rs:878`.

Commit a Free Transform with Return. The canvas sets `handles_parked` to true. The moved region remains selected, so pressing Ctrl+T immediately can start another Free Transform. `free_transform` selects the Move tool, but Move is already active; `ToolRail::select` changes no toggle and `tool_changed` never runs. The new floating layer therefore has no visible handles.

The same stale flag survives selecting a different layer in the Layers panel. It also survives turning View > Transform Controls off and back on, so the menu says controls are enabled while `geometry()` still suppresses them. Reset `handles_parked` when a transform session starts, when the active layer changes, and when transform controls are enabled.

## Test coverage notes

The new round 5 tests do not exercise the Free Transform size limit, attached-mask growth, an entirely off-canvas float, duplicate Layer Style windows, saving or running another command during Layer Style, Fade after non-pixel edits, Auto Levels on a mask, effects after canvas expansion, Color Balance at clamped endpoints, or the handle reset sequences above. The existing Color Balance assertion uses mid-gray with a tolerance of four luma levels, so it cannot expose the clipping failure. No tautological or plainly incorrect assertion was found.

## Coverage limits

The second pass covered every numbered area in the round 5 prompt and compared Layer Via, Free Transform, Curves and adjustment parsing with the Swift implementation. No live GTK display session was available. UI lifecycle and focus paths were checked statically, while the Layer Style save mismatch, mask-growth result and styled-cache result were additionally reproduced with temporary core probes. The probes were removed after use.
