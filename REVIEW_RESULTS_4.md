# Review results, round 4

## Status (2026-09-19)

All 13 findings fixed; regression tests in tests/features_f.rs, tests/features_g.rs and src/effects.rs.

| # | Finding | Fix |
|---|---|---|
| 1 | Click inside edited text panics | The editing state is copied out of the RefCell before the branch |
| 2 | Stale editor after undo or delete panics | Existence is checked first; the editor clears whenever the page refreshes without its layer |
| 3 | Floating transform closes without a prompt, saves the float | is_modified counts a float or an open edit; save lands the float first |
| 4 | Merge Visible deletes hidden children | Only what shows is merged; hidden children move up to the nearest surviving folder |
| 5 | Commit discards off-canvas pixels | A dedicated merge draws the float onto the source's own grid, growing it as the Mac does |
| 6 | Reordered float merges into the wrong layer | The merge targets the recorded source |
| 7 | Center stroke fills everything | Distance to the edge is read from the field of the other side |
| 8 | Color Overlay thickens alpha | Interior effects render without alpha and composite Atop inside a group of the layer alone |
| 9 | Bounds maxima treated as sizes | Effects padding and the text frame use the maxima; the frame follows rotation |
| 10 | Whole layer selected after commit | The float's pixels are selected before it merges |
| 11 | Caret ignores rotation and flips | Caret and click go through the layer's full placement matrix |
| 12 | Layer Style Cancel leaves steps | The dialog session is one open edit: OK ends it, Cancel aborts it |
| 13 | Cut test undid the wrong step | The test undoes visibility and the cut separately and checks both |


Reviewed on 2026-09-19 at HEAD `5bdef7c` (review instructions). Code scope: commits `52f83ad` through `904e58f`. Earlier review findings are excluded. Report only; no application, test, Swift reference, or C source files were changed.

Found 13 issues: five P1, seven P2, and one P3. P1 means a crash or a substantial data-loss risk; P2 means incorrect behavior; P3 is a test that passes for the wrong reason.

`cargo build` succeeded. `cargo test` succeeded: **141 passed, 0 failed, 2 ignored**. The ignored tests were `google_font_fetch_installs_a_family` and `remove_background_finds_a_subject`.

Small Rust probes linked against the built library reproduced the document and rendering failures below. The caret-click finding uses an isolated reproduction of the exact RefCell borrowing pattern. GTK interaction paths were traced in source, not exercised in a running GUI. Local probe source and output are `/tmp/compositor-review-4/probes.rs` and `/tmp/compositor-review-4/probes.log`; build and test logs are alongside them. Each finding also describes its reproduction without requiring those temporary files.

## Findings, ranked by severity

### 1. [P1] Clicking inside text already being edited panics

**Location:** `src/ui/canvas.rs:573-577`, `Canvas::type_at`.

Choose the Type tool, create or start editing a text layer, then click inside that same layer to reposition the caret. The `if let Some((id, _)) = *self.text_edit.borrow()` condition retains its immutable RefCell borrow throughout the matching body. Line 577 calls `self.text_edit.borrow_mut()` while that borrow is alive, causing a runtime borrow panic in the GTK callback.

An isolated Rust 2024 probe with the same condition and mutation confirmed the panic. Copy the editing state into a local variable and release the borrow before entering the conditional body.

### 2. [P1] Undoing creation of the edited text layer leaves a panic on the next key

**Locations:** `src/ui/canvas.rs:648-651`; `src/document.rs:940-942`; Undo action at `src/ui/mod.rs:393`.

Start a new on-canvas text layer and undo until its creation is undone. Undo refreshes the page but leaves `Canvas::text_edit` pointing to the deleted layer. The next canvas key calls `text_style(id)` before checking `has_layer(id)`. `text_style` calls the renderer's unchecked layer lookup, which panics for that stale UUID. The same issue follows deleting the layer through the menu while its text editor is active.

A core probe using `add_text_layer`, `undo`, and `text_style(old_id)` confirmed the panic. Check existence before reading the style, and clear or reconcile the canvas editing state after undo and deletion. The overlay drawing code already performs its existence check in the correct order.

### 3. [P1] An unfinished Free Transform can be closed without a save prompt

**Locations:** `src/document.rs:670-675`, `src/document.rs:2113-2120`; close checks at `src/ui/mod.rs:542` and `src/ui/mod.rs:768`; saving at `src/ui/mod.rs:986`.

Save a document with a selection, press Ctrl+T, and move the floating pixels. The outer history edit stays open, so its revision does not advance. `is_modified()` still returns false. Both tab close and application close use that flag, allowing the modified document to be discarded without prompting.

Saving during the float has another observable failure: it writes the temporary floating layer and marks the old history revision saved. Press Escape afterward. The in-memory document reverts, but still reports unmodified even though it differs from the saved file. In the probe, pixel `(20,20)` was transparent in memory and opaque red on disk, with `is_modified() == false`.

Resolve floating transforms before saving or closing, and ensure pending edits participate in dirty-state decisions. The Swift implementation explicitly supports synchronous resolution before Save in `reference/Compositor/Document/FloatingSelection.swift:66-68`.

### 4. [P1] Merge Visible deletes hidden children of visible folders

**Location:** `src/document.rs:720-723`, `Document::merge_visible`.

Create a visible folder containing one visible image and one hidden image, then run Merge Visible. The flattened result excludes the hidden image, but the recursive expansion of `going` adds every child of the visible folder without checking visibility. The removal loop deletes the hidden image too. Its pixels are neither retained as a layer nor included in the merge.

A fixture with a visible red child and a hidden blue child confirmed `has_layer(hidden_id) == false` afterward. Preserve hidden descendants and valid parent relationships when removing merged folders. The existing test covers only a hidden top-level layer.

### 5. [P1] Committing Free Transform discards untouched pixels outside the canvas

**Location:** `src/document.rs:683`, calling `merge_layers`; canvas-sized flattening occurs at `src/document.rs:1146-1147`.

On a 40 by 40 canvas, place a solid 20 by 20 layer at `(-10,0)`. Select a 2 by 2 area inside the canvas, press Ctrl+T, then commit without moving anything. The original layer spans x = -10 through 10. The result spans only x = 0 through 10: all of the untouched off-canvas pixels have been discarded.

The probe confirmed that the layer changes from origin `(-10,0)`, size `(20,20)`, to origin `(0,0)`, size `(10,20)`. Free Transform uses the ordinary canvas-sized Merge Down operation, which also flattens source properties rather than preserving the source layer's editable grid and mask.

The Swift behavior differs: `FloatingMerge.merge` in `reference/Compositor/Document/FloatingSelection.swift:119-151` unions the full source raster extent with the floating pixels and grows the source grid. Use a dedicated floating-pixel merge instead of flattening through the canvas.

### 6. [P2] Reordering a floating selection makes it merge into the wrong layer

**Location:** `src/document.rs:679-684`, `Document::commit_free_transform`.

Create a red source layer and an unrelated blue layer above it. Select part of the red layer, begin Free Transform, then use Bring to Front on the floating layer before committing. The implementation checks that the recorded source exists, but does not use its ID as the merge destination. `merge_layers()` merges with the current neighbor below the floating layer.

The probe confirmed that the original source remains with its cut-out, while the unrelated blue layer is removed into the merge. Merge directly into the recorded source or resolve the transform before allowing reordering. Swift looks up the destination using `floating.sourceID` at `reference/Compositor/Document/FloatingSelection.swift:73-79`.

### 7. [P2] Center stroke fills the whole effects rectangle

**Location:** `src/effects.rs:171`.

Enable a stroke with Position = Center and positive size. One of `outside_d[i]` and `inside_d[i]` is zero at every pixel, because each distance field starts at zero on its own class. Taking their minimum makes the computed coverage one everywhere, regardless of distance from the shape.

A 21 by 21 alpha image containing only a centered 5 by 5 opaque square, with a 2-pixel red center stroke, produced BGRA `[0,0,255,255]` at the transparent far corner `(0,0)`. That corner should remain transparent. Measure the distance to the opposite class on each side of the boundary. Existing stroke assertions exercise only the Outside position.

### 8. [P2] Color Overlay increases opacity and retains the original color

**Locations:** `src/effects.rs:143-145`; overlay compositing at `src/render/mod.rs:773-774`.

Apply a 100% red Color Overlay to a blue pixel with alpha 128. The overlay coverage includes the original alpha, but the resulting image is then painted source-over the original pixel. This applies two overlapping alpha contributions instead of recoloring the existing coverage.

The renderer probe produced premultiplied BGRA `[64,0,128,192]`. A full red overlay preserving the layer's alpha should produce `[0,0,128,128]`. The current behavior makes partially transparent artwork more opaque and leaves blue in the output. Soft mask edges are subject to the same compositing problem. Composite interior color effects within the original coverage rather than source-over as another independent layer.

### 9. [P2] Effects bounds treat maximum coordinates as widths

**Location:** `src/render/mod.rs:447-452`, `Renderer::styled`.

`Transform::bounds()` returns `(x0,y0,x1,y1)`, but this code binds the final values as `bw,bh` and adds the origin again. A 20 by 20 layer at `(-10,0)` therefore gets a right effects boundary near x = 0 instead of x = 10. Positive origins instead make unnecessarily oversized intermediate surfaces.

With that layer filled blue and a 100% red Color Overlay, the probe found pixel `(5,5)` still blue, BGRA `[255,0,0,255]`, because the overlay was clipped away. Use the returned maxima directly.

The same mistake appears in the new text-edit frame at `src/ui/canvas.rs:1858-1860`. For a layer at `(100,100)` with size `(50,20)`, the frame is calculated as 150 by 120 instead of 50 by 20.

### 10. [P2] Free Transform selects the whole resulting layer instead of the moved selection

**Location:** `src/document.rs:684`.

Make an opaque 10 by 10 source at `(0,0)`. Select only its top-left 2 by 2 pixels and transform them to `(20,20)`. After commit, the selection should describe the moved 2 by 2 area. Instead, `select_layer_pixels(merged, Replace)` selects both the moved pixels and all the untouched source pixels.

The probe returned selection bounds `(0,0,22,22)`, rather than `(20,20,22,22)`. The next fill, delete, or transformation therefore affects unrelated source pixels. Keep the original selection and transform its geometry or coverage, as Swift does in `reference/Compositor/Document/FloatingSelection.swift:59-63` and `:81-96`. The existing Free Transform test selects the whole source, hiding this distinction.

### 11. [P2] The on-canvas text caret does not follow rotation or flipping

**Locations:** `src/ui/canvas.rs:603-612` and `src/ui/canvas.rs:1868-1876`.

Rotate a text layer 90 degrees, then invoke Edit Text. The caret is drawn by scaling and translating its raster coordinates, with no rotation, so it remains vertical instead of following the rotated line. Flipping a text layer also leaves the drawn caret at the unflipped position. This is visible when editing an existing transformed layer even without triggering finding 1.

The click-to-raster helper handles flips but explicitly ignores rotation, so the initial click can choose the wrong insertion index on rotated text. Both directions should use the same complete raster/document transform used by rendering. This finding is established by the coordinate formulas; it was not visually exercised in GTK.

### 12. [P2] Layer Style Cancel leaves dirty history, and a dialog session can require multiple undos

**Locations:** `src/ui/effects.rs:15-21` and `:69`; `src/document.rs:978-986`.

Open a saved document's Layer Style dialog, change an effect, then Cancel. Every preview is a committed `set_effects` edit; Cancel commits another edit that restores the original settings. It does not restore the original history revision. The probe confirmed that effects return to `None`, but the document is still modified and Undo still says `Layer Style`.

Merging depends on the history's 1.5-second time window. Pausing longer between controls produces multiple undo steps, contrary to the dialog's stated one-step behavior. Cancel after such a pause also leaves an undo step that can reinstate the supposedly cancelled effect.

Treat the dialog session as one preview transaction, with a commit on OK and cancellation that restores the original state and history. The immediate Cancel dirty-state failure was reproduced through the same document calls used by the dialog; the timed behavior follows from `History::end_with` and its merge window.

### 13. [P3] The Layer Via Cut test undoes visibility, not the cut

**Location:** `tests/features_g.rs:48-54`, `layer_via_copy_and_cut_lift_the_selection_in_place`.

After cutting, the test calls `set_visible(cut, false)`, which adds a separate history entry. Its next `undo()` reverses that visibility edit. The final alpha assertion passes because the cut layer becomes visible again, even though the source remains cut and the floating copy still exists.

A probe of those exact calls confirmed that the cut layer still exists and is visible after this undo, and that the next undo action is still `Layer Via Cut`. Undo the visibility change separately, then undo the cut and assert both that the cut layer is gone and that the original source pixels are restored.

## Coverage and limits

The pass examined the new type/editor code, effects and renderer integration, guides and snapping, document commands, floating transforms, action and menu changes, theme installation, preview/picture changes, and new tests. The concrete Swift comparisons above use its floating-selection implementation. Passing tests do not cover the newly demonstrated cases.

No live GTK session, screenshot capture, font download, or maximum-size allocation stress run was performed. Consequently this report does not establish runtime correctness of GTK 4.22 CSS parsing, popover lifecycle, window focus interactions, the picture/overlay snapshot timing, or Google Fonts network and font-cache behavior. Those remain useful follow-up validation targets. No claim is made that the audit proves the remaining code free of faults.
