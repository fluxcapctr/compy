# Review Results (Gemini Code Review)

Reviewed at HEAD after nine earlier review rounds and the whole-project audit. Application source and tests were not changed.

All 205 existing tests pass in release mode (`cargo test --release`: 205 passed, 0 failed, 2 ignored).

Below is the numbered list of findings ranked by severity, detailing the file and line, trigger, consequence, and suggested fix.

## Findings

1. [P1] `src/ui/genfill.rs:135`, `src/ui/agent.rs:706,834,842`, `src/document.rs:678-692`
   - Trigger: Running Generative Expand via fal.ai and the Bria outpainting model.
   - Consequence: Bria outpainting generates an image covering the entire expanded canvas (`cw` by `ch`). However, `window_rect` is set to `window` from `genfill_inputs()`, which is tightly cropped to the margin selection (`genfill_window`). When `apply_genfill` lands the result, it scales the full-canvas image down into this cropped margin rectangle and offsets it, severely distorting and displacing generated pixels.
   - Fix: When `request.expand` is active, set the landing window rectangle to the entire canvas `(0, 0, cw, ch)` instead of the margin sub-window.

2. [P1] `src/document.rs:59,655,841`, `src/ui/genfill.rs:134`
   - Trigger: Resizing canvas with Generative Expand, and later performing a normal Generative Fill on a selection that does not contain the original image center.
   - Consequence: `self.expand_source` is stored on `Document` when expanding the canvas but is never cleared upon landing, undoing, resizing the canvas again, or altering the selection, and is omitted from `history::State`. Because `expand_inputs` only checks canvas dimensions and that the selection does not contain the old image center, subsequent standard inpainting fills silently divert to Bria Expand instead of the user's selected fill model.
   - Fix: Clear `expand_source` inside `apply_genfill`, `canvas_size`, `deselect`, `set_selection`, and undo/redo state restoration.

3. [P1] `src/document.rs:3578-3580`, `src/render/mod.rs:851,863,868`
   - Trigger: Resizing canvas size on a document containing an adjustment layer with a mask.
   - Consequence: Commit `8de0735` expanded canvas-spanning adjustment layers to the new canvas size, but lines 3579 to 3580 freeze `mask_placement` to the old canvas bounds `(ow, oh)`. In `Renderer::adjust()`, line 863 clips coverage to the mask placement rectangle and zeroes coverage outside it via `Operator::In`. Consequently, the adjustment effect abruptly cuts off at the old canvas boundary despite the layer transform expanding.
   - Fix: In `canvas_size_body`, resize the adjustment mask surface to match the new canvas size (filling extended areas with white reveal), or in `adjust()`, do not zero out coverage outside the mask placement rectangle for canvas-spanning adjustments.

4. [P2] `src/document.rs:766,780`
   - Trigger: Tone matching in `match_tone` when estimating tone drift across an inpainting hole.
   - Consequence: The 2D Gaussian kernel in `blur` is unnormalized, giving a kernel sum of approximately 1.5 * r^2 (up to 2,500x). While this cancels in `bn / bw`, line 780 computes `t = (bw[c] / (0.25 * full)).min(1.0)`. Because `bw[c]` is inflated by the unnormalized kernel sum, `t` clamps to 1.0 even deep inside a hole with near-zero support, preventing smooth fallback to `global` tone drift and yielding noisy local corrections.
   - Fix: Normalize the 1D Gaussian kernel in `blur` so its elements sum to 1.0 before running convolution passes.

5. [P2] `src/document.rs:822,851-856`
   - Trigger: Picking an inpainting variation in `replace_genfill` when `self.active` points to a layer other than the genfill layer being replaced.
   - Consequence: `draw_under_insertion` determines cutoff index from `self.insertion()`, which relies on `self.active`. If the active layer shifted above or below the target layer, intermediate layers are incorrectly drawn or hidden during tone-drift estimation, corrupting tone calculations.
   - Fix: In `draw_under_insertion`, take an explicit target layer index rather than defaulting to `self.insertion()` when `also_hide` is provided.

6. [P2] `src/export_sizes.rs:128`
   - Trigger: Running Export Sizes reframing on a document with small masked pixel layers, such as cutout logos, stickers, watermarks, or badges.
   - Consequence: Line 128 excludes any pixel layer where `document.renderer.mask(id).is_some() || fraction >= 0.25` from element processing. As a result, small masked graphic assets are misclassified as part of the background photo and get cropped away during aspect ratio reframing instead of being repositioned within safe margins.
   - Fix: Only exclude masked pixel layers from elements if their canvas coverage or span exceeds a substantial threshold (e.g. `fraction >= 0.25 || span >= 0.5`).

7. [P2] `src/psd.rs:400,418`, `src/document.rs:1369-1374`
   - Trigger: Opening a PSD file containing center- or right-aligned paragraph text and editing its text.
   - Consequence: Photoshop saves PSD pixel channels cropped to the ink bounding box, initializing `transform.origin` to the ink bounds. In Compy, `crate::text::render` now returns the full paragraph width. When edited, `set_text` updates `transform.size` to the full width but retains `transform.origin` at the ink-cropped position, shifting centered and right-aligned text far to the right.
   - Fix: Adjust `transform.origin` on PSD text import or within `set_text` by accounting for paragraph alignment and box bounds so the layout origin remains fixed.

8. [P2] `src/ui/agent.rs:459`
   - Trigger: An MCP client or assistant calling the `set_text` tool when no layer is active (`dd.active` is `None`).
   - Consequence: Line 459 executes `let id = dd.active.unwrap()`, triggering a panic on the GTK main UI thread and crashing the application process with unsaved user data lost.
   - Fix: Check `dd.active` before unwrapping, and return an error JSON response when no layer is active.

9. [P2] `src/render/mod.rs:1199`
   - Trigger: Calling the public method `Renderer::mask_background(id)` on a layer without a mask.
   - Consequence: Line 1199 calls `self.masks.get_mut(&id).unwrap()`, causing a thread panic instead of returning an error result.
   - Fix: Replace `.unwrap()` with `.ok_or_else(|| anyhow::anyhow!("layer has no mask"))?`.

10. [P2] `src/ui/agent.rs:45-55`
    - Trigger: An MCP agent tool accessing a file path containing a symbolic link pointing to a hidden file or outside the home folder (e.g. `~/Pictures/symlink -> ~/.ssh/id_rsa`).
    - Consequence: `agent_path` checks path components lexically without resolving symbolic links via `std::fs::canonicalize()`. An agent can read or overwrite sensitive files outside confinement by following symlinks located inside `$HOME`.
    - Fix: Resolve symlinks using `std::fs::canonicalize` on the target path or its parent directory, and verify that the canonical target resides within `$HOME` and outside hidden directories.

11. [P3] `src/document.rs:1387`, `src/ui/canvas.rs:689`
    - Trigger: Clicking with the Type tool in the transparent, empty region of a wide paragraph text box.
    - Consequence: `text_layer_at` tests `e.layer.transform.contains(point)`. Because paragraph text layers retain their full paragraph width, clicks in empty, un-inked space are intercepted by the paragraph layer, preventing selection of underlying layers or placement of new text.
    - Fix: Hit-test paragraph type layers against their rendered text glyph extents rather than the full paragraph bounding box.

12. [P3] `src/genfill.rs:220-222`
    - Trigger: Saving a fal.ai API key via `save_key`.
    - Consequence: `std::fs::write(&path, ...)` creates `fal.key` with default umask permissions (e.g. 0644) before `set_permissions(0o600)` is invoked on line 222. This creates a time-of-check to time-of-use window where other local users can read the secret key.
    - Fix: On Unix, create the file using `std::fs::OpenOptions` with `std::os::unix::fs::OpenOptionsExt::mode(0o600)`.

13. [P3] `install.sh:14`, `get.sh:25`
    - Trigger: Installing Compy from a release tarball via `install.sh` on a machine lacking system-wide dependencies like `libgtk-4` or `libheif`.
    - Consequence: Shared libraries are copied into `~/.local/lib/compy`, but the binary `~/.local/bin/compositor` does not have an `RPATH` or `RUNPATH` pointing to that directory, and no launcher script sets `LD_LIBRARY_PATH`. Running Compy directly or from the desktop launcher fails with missing library errors.
    - Fix: Build release binaries with `RPATH` set to `$ORIGIN/../lib/compy`, or install a shell launcher wrapper that sets `LD_LIBRARY_PATH`.

14. [P3] `src/ui/agent.rs:81-85`
    - Trigger: A local process connecting to the agent Unix domain socket.
    - Consequence: The socket server accepts connections without verifying peer credentials. If file permissions on parent directories are permissive, unauthorized local processes can connect and execute document modifications.
    - Fix: Check peer credentials via `getsockopt` with `SO_PEERCRED` on Linux to verify that the connecting process UID matches the running user.

15. [P3] `src/psd.rs:247-253`
    - Trigger: Opening a crafted PSD file specifying a large number of layers and channels with lengths up to the file size.
    - Consequence: Memory for `channel_specs` and raw layer metadata is allocated in loops before the overall sample decoding budget (`samples_budget`) is evaluated, allowing memory pressure from malformed inputs.
    - Fix: Validate that cumulative channel header sizes do not exceed the remaining file data length before allocating vector capacity.

16. [P3] `src/psd.rs:571,595-596`
    - Trigger: Exporting a document containing layer groups/folders to a PSD file and opening it in Adobe Photoshop.
    - Consequence: In Photoshop, layer folders default to "Pass Through" (`pass`), allowing adjustment layers and blend modes inside the group to affect layers below. Compy writes folders with "Normal" (`norm`), forcing isolated group blending and altering visual appearance in Photoshop.
    - Fix: Write `b"pass"` as the blend mode key for folder records unless an explicit blend mode is set on the folder.

17. [P3] `src/ui/mod.rs:552`
    - Trigger: Pressing Delete or Backspace on an active layer when not editing text.
    - Consequence: In Photoshop, Delete or Backspace deletes the active layer when there is no pixel selection. In Compy, `delete-layer` has no keyboard accelerator mapped, requiring users to use menus or mouse clicks.
    - Fix: Map Delete and Backspace keys to `delete-layer` when text edit mode is inactive and no pixel selection exists.

18. [P3] `tests/features_g.rs:1469-1478`
    - Trigger: Running the `adjustment_layers_cover_a_grown_canvas` test.
    - Consequence: The test creates an adjustment layer without a mask. Because the adjustment clipping defect specifically occurs when an adjustment layer has a mask, this test passes even though masked adjustment layers remain broken on canvas resize.
    - Fix: Add a test asserting that an adjustment layer with a mask continues to adjust extended margin areas.

19. [P3] `tests/genfill.rs:96-111`
    - Trigger: Running the `expand_grows_the_canvas_and_selects_the_margin` test.
    - Consequence: The test expands the canvas symmetrically from center anchor (4), causing the margin selection to touch all edges and `genfill_window()` to match `(0, 0, 80, 30)`. An asymmetric expansion (e.g. anchor 3) produces a cropped sub-window, which would reveal the Bria window coordinate mismatch.
    - Fix: Test asymmetric canvas expansions and assert that the coordinates passed to the outpainting model match the full expanded canvas.

## Status

| # | Verdict | What was done |
|---|---|---|
| 1 | Fixed | A Generative Expand that goes to Bria lands over the whole canvas, in the dialog and the assistant. A new test also caught the old picture's place being clamped to one pixel; fixed. |
| 2 | Fixed | The expand's inputs are used only while the selection is the expand's own margin (every grown side selected, the old picture clear); a later fill or an undone expand is an ordinary fill. |
| 3 | Fixed | A masked adjustment's mask is redrawn onto the new canvas, padded, with the mask's edge value beyond it. |
| 4 | Fixed | The drift blur's kernel is normalized. |
| 5 | Fixed | Refilling a variation measures under that layer, whatever layer is active. |
| 6 | Not changed | Masked pixel layers are almost always cutouts and fills of the photo; moving them apart splits the picture, which is the bug this rule fixed. A logo with alpha and no mask is still an element. |
| 7 | Fixed | The first edit of an ink-cropped paragraph (from a PSD, or older files) moves the layer left by where its ink sat, so aligned text stays put. |
| 8 | Not a bug | set_text returns an error earlier in the same branch when nothing is active; the unwrap cannot fail. |
| 9 | Fixed | mask_background returns an error instead of panicking. |
| 10 | Fixed | Tool paths are checked again after resolving symbolic links; a link into a hidden folder or outside home is refused. Test added. |
| 11 | Not changed | Clicking inside a paragraph box edits that paragraph, as in Photoshop. |
| 12 | Fixed | The key file is created with mode 0600 before the key is written. |
| 13 | Not a bug | Release binaries are linked with rpath $ORIGIN/../lib/compy (release.yml), which is ~/.local/lib/compy from ~/.local/bin. |
| 14 | Not changed | The socket lives in XDG_RUNTIME_DIR or a 0700 cache folder, so only the user can reach it. |
| 15 | Not a bug | Channels per layer are capped at 56, each claimed length is checked against the file, and records are reserved at most 1024. |
| 16 | Not changed | Compy composites groups isolated; writing Normal keeps the file looking as it did in Compy. |
| 17 | Not a bug | Delete and Backspace already clear a selection or delete the layer (canvas key handler). |
| 18 | Fixed | New test: a masked adjustment still covers a grown canvas, and undo restores it. |
| 19 | Fixed | New test: a one-sided expand's outpainting inputs, and a later fill not going to outpainting. |
