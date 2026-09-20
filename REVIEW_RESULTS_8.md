# Review results, round 8

Reviewed on 2026-09-20 at HEAD `35e5b535762ecc970bf3121ae23c1d09dd127c8c`. The implementation scope was commits `663891e` through `2f2e459`, plus verification of the round 7 fixes. Application source was not changed.

Found **17 issues: 14 P2 and 3 P3**. Findings identify whether they were reproduced by an executed probe or established by source analysis. No application process, assistant process, socket request, network request, GPU path, or `avifenc` process was started. No user brush, pattern, or configuration file was written.

## Validation

- `env -u COMPOSITOR_GPU cargo build`: passed. The four existing warnings remain: an unused doc comment, deprecated GTK coordinate translation, unused `OP_COPY`, and unused `Assistant::present`.
- `env -u COMPOSITOR_GPU cargo test -q`: **178 passed, 0 failed, 2 ignored**. The ignored tests require network access or a downloaded model. The GPU test returns early because GPU mode is unset, as required by the prompt.
- A separate Rust probe compiled against the built library and ran only with temporary output. It reproduced opaque transparent artboards, unchanged artboard frames after Rotate Canvas, masks omitted from Export Layers, and separate undo steps for Export Sizes artboards. Probe output: `transparent board alpha 255`, `rotated board (0.0, 0.0, 10.0, 10.0)`, `hidden mask exported alpha 255`, and two boards becoming one after a single undo.
- The repository was clean before this report was created. Temporary audit inputs and outputs were kept under `/tmp`.

## Findings

### 1. [P2] Export Layers discards masks, effects, opacity, and blend mode

**Locations:** `src/document.rs:1274-1286`, `src/render/mod.rs:611-631`.

Export a visible pixel layer that has a hide-all mask, a Layer Style, opacity below 100 percent, or a non-Normal blend mode. `export_layers` calls `draw_layer_plain`, whose contract and implementation place only the raw image through its transform. None of those visible-layer attributes are applied.

**Executed reproduction:** a solid layer with a hide-all mask exported with an opaque center pixel, alpha 255, instead of an empty image. Effects, opacity, and blending follow the same bypass in `draw_layer_plain`. The test at `tests/features_g.rs:904-909` uses plain layers and checks only size and numbering, so it cannot detect this.

### 2. [P2] An artboard requested as transparent is rendered opaque white

**Locations:** `src/render/mod.rs:684-693`; transparent agent input at `src/ui/agent.rs:508-513`.

Create an artboard with `background: None`, including `new_artboard` with `transparent: true`. The renderer replaces `None` with `[1.0; 3]` and fills the entire frame white. This contradicts the stored optional background and the public transparent option.

**Executed reproduction:** a blank document with a transparent artboard produced alpha 255 at the center of the board. It should have remained alpha 0.

### 3. [P2] Export Artboards crops the global composite instead of isolating each board

**Locations:** `src/document.rs:2739-2764`, `src/render/mod.rs:676-693`.

Create two overlapping artboards with different backgrounds or content, then export them. `export_artboards` renders the whole document once and crops each frame from that shared surface. All board backgrounds are drawn first and all visible content is then drawn globally. An exported board can therefore include pixels belonging to an overlapping board.

The same crop clamps a partly off-canvas frame to the document at `src/document.rs:2746-2749`, so the output becomes smaller than the artboard's declared width and height instead of preserving its frame with transparent or background-filled padding. Valid loaded files can contain finite artboard coordinates outside the canvas because validation does not bound `x` or `y` to the manifest.

**Static rendering-path verification.** The current artboard test uses one fully on-canvas board and only checks that a file exists.

### 4. [P2] Rotate Canvas turns artboard children but leaves their frames behind

**Locations:** `src/document.rs:1159-1198`.

Rotate a document containing an artboard. The loop updates each layer's `Transform` and mask placement, but an artboard frame lives in `Layer.artboard` as separate `x`, `y`, `width`, and `height` fields. Those fields are never turned or resized. The children move while clipping and background remain at the old frame.

**Executed reproduction:** rotating a 30 by 20 document containing artboard `(0, 0, 10, 10)` by 90 degrees left its frame exactly `(0, 0, 10, 10)` after the canvas became 20 by 30.

### 5. [P2] Arbitrary Rotate Canvas angles can clip the outside edge

**Location:** `src/document.rs:1167-1173`.

Rotate a canvas by a non-quarter-turn angle whose exact bounding box has a fractional extent. The new width and height use `round()`. A containing box must round outward, so any fractional extent below `n + 0.5` is rounded down and the rotated image is clipped at the boundary. This also affects angles close to, but not exactly, 90 degrees.

For example, a 10 by 10 canvas at 45 degrees has an extent of about 14.142 pixels and is allocated as 14 rather than 15. **Static geometry verification.** The test covers exactly 90 degrees, where the extent is integral.

### 6. [P2] Reframe classifies transparent or masked full-size layers as pictures

**Locations:** `src/export_sizes.rs:67`, `src/export_sizes.rs:100-110`.

Use Reframe on a full-canvas layer whose actual visible coverage is under half of the canvas, such as a small logo stored in a full-size transparent image or a full-size layer reduced by a mask. The documented element rule is based on coverage, but `element_layers` divides the transformed rectangular bounds by canvas area. It never reads alpha, the layer mask, rotation-aware visible coverage, or clipping.

The layer is treated as the picture and remains on the Fill crop path instead of being repositioned within the margin. The current test represents its logo with a small layer surface, so transformed bounds and visible coverage happen to agree.

### 7. [P2] Importing a document that already has artboards creates stale nested artboards

**Locations:** `src/document.rs:2690-2703`.

Run Export Sizes as artboards when the source document already contains artboards, or call `import_as_artboard` with such a source. Every top-level source layer is copied into the new artboard, including source artboard folders and their `Artboard` records. The code offsets only copied layer transforms and mask placements. It does not offset the copied artboard records.

The result contains artboards nested inside another artboard whose backgrounds and clip frames still use the source document coordinates. Validation permits artboard folders beneath other folders, so saving and reopening does not reject this state. **Static copy-path verification.**

### 8. [P2] Dragging an artboard during another open edit joins the wrong transaction

**Locations:** `src/ui/canvas.rs:509`, `src/ui/canvas.rs:841-851`, `src/document.rs:1418-1420`.

Start Free Transform or another preview edit, then press an artboard name strip with the Move tool. `begin_board_drag` blocks only an active brush stroke. It does not consult `busy_editing`, so `begin_edit("Move Artboard")` nests inside the existing edit.

Finishing the drag does not create an independent history step. Applying the original transform commits both changes together, while canceling it can also revert the completed artboard drag. **Static transaction-path verification.**

### 9. [P2] Export Sizes creates one undo step per artboard in the dialog path

**Locations:** `src/export_sizes.rs:127-135`, `src/document.rs:2690-2706`, `src/ui/mod.rs:1430-1441`.

Choose multiple sizes and select the dialog option that creates artboards. `add_as_artboards` has no outer edit, while each `import_as_artboard` opens and ends its own edit. The UI calls the function directly, without the agent's general mutation wrapper.

**Executed reproduction:** creating two sizes produced two artboards; one undo removed only the second and left the first. The operation is presented as one Export Sizes action and should be one history step.

### 10. [P2] Sanitized export names can silently overwrite earlier results

**Locations:** `src/export_sizes.rs:52-56`, `src/export_sizes.rs:117-120`, `src/document.rs:2751-2764`.

Export two same-size presets named `A/B` and `A?B`, or two artboards with those names. Both sanitize to `A_B`. Export Sizes also includes dimensions, but identical dimensions still collide. The second write replaces the first, while the returned `made` or `written` list contains duplicate paths and reports both as successful.

Raw artboard name uniqueness does not prevent this because the names differ before sanitization. **Static path verification.**

### 11. [P2] Pattern Overlay anchors tiles to opaque coverage instead of the layer origin

**Locations:** `src/effects.rs:75`, `src/effects.rs:236-249`.

Apply the same Pattern Overlay to two equal-size layers with different transparent margins, or edit a mask so the first covered pixel changes. The type says the pattern is tiled from the layer's top-left corner, but painting subtracts `coverage_box(alpha).x/y`. The pattern phase therefore changes with the contents or mask instead of remaining fixed to the layer.

The existing overlay test uses an opaque rectangle whose coverage origin equals its layer origin, so the two anchors are indistinguishable. **Static indexing verification.**

### 12. [P2] PSD EngineData key scanners accept key-looking text inside strings

**Locations:** `src/psd.rs:806-839`.

Open a valid PSD type layer whose EngineData contains text such as `/FontSize 999` before the actual `/FontSize` property, or contains another `/Name (` after the FontSet. The scanners perform raw byte searches and return the first matching spelling without parsing PostScript strings, arrays, dictionaries, escapes, or scope.

The imported font size, fill, font index, paragraph bounds, leading, or font list can consequently come from user text or an unrelated dictionary entry. Repeated legitimate keys also select the first occurrence regardless of the active style run. **Static parser verification.**

### 13. [P2] PSD type transforms ignore rotation and shear when converting text metrics

**Locations:** `src/psd.rs:771-802`.

Open a PSD with a rotated or sheared editable type layer. `read_tysh` reads all six transform values but uses only `xx` and `yy`, selecting one absolute diagonal as a scalar. Off-diagonal `xy` and `yx` components never contribute to font size or paragraph width.

A 90-degree transform has diagonal values near zero and nonzero off-diagonal values, so this code falls back to scale 1. Text size and `BoxBounds` width are imported incorrectly. More general nonuniform and sheared transforms are also collapsed to one axis. **Static matrix verification.**

### 14. [P2] Saving a swatch can write into the working directory when HOME is absent

**Locations:** `src/ui/color_wheel.rs:83-96`.

Launch with both `XDG_CONFIG_HOME` and `HOME` unset, then add or remove a swatch. `unwrap_or_default()` supplies an empty path and appends `.config/compositor/swatches.json`, producing a relative path. The application creates that tree under its current working directory and writes there.

This can modify a project checkout or another arbitrary launch directory instead of a user configuration location. **Static environment-path verification.**

### 15. [P3] AVIF staging PNG is not cleaned up if staging itself fails

**Locations:** `src/document.rs:1253-1263`.

Export AVIF when writing or encoding the temporary PNG fails after creating it, such as a full filesystem or another mid-write error. The `?` on `export_png` returns before the cleanup at line 1262. A partial `*.avif-source-PID.png` can remain beside the destination.

Cleanup does run after attempting `avifenc`, including a failed encoder exit. The test checks only successful staging and the missing-encoder branch, so it misses the early-return path. **Static error-path verification.**

### 16. [P3] The 64-swatch cap removes only one item from an oversized file

**Location:** `src/ui/color_wheel.rs:88-97`.

Start with a valid manually edited `swatches.json` containing more than 64 entries, then add a swatch. The code removes only index 0 once when `len() > 64`. A 100-entry list remains at 100 entries after the new value is appended and one old value is removed.

The cap works only when every prior write was already at or below 64. **Static state verification.**

### 17. [P3] Rotate Canvas normalizes 180 degrees to the excluded endpoint

**Location:** `src/document.rs:1179-1183`.

Rotate a layer or canvas so its resulting inspector rotation is exactly 180 degrees. The documented interval is `(-180, 180]`, but the expression produces `[-180, 180)`, returning `-180`. The rendered result is equivalent, but the inspector and serialized transform use the wrong canonical endpoint. **Static boundary verification.**

## Round 7 fix verification

All round 7 status-table fixes remain present. The focused regression tests covering those fixes passed. GPU behavior was deliberately excluded.

| Round 7 item | Result |
|---|---|
| 1. Landing during a stroke | Fix present: completed jobs defer while the document is busy and strokes retain their originating layer. |
| 2. Pattern aggregate budget | Fix present: a shared decoded-output budget is charged before allocation. |
| 3. HiDPI Blend If | Fix present and its scale-2 regression test passed. |
| 4. Unwritten pattern channels | Fix present and tested. |
| 5. Pattern channels shrunk as brush tips | Separate plane handling remains present and tested. |
| 6. Pattern channel origins | Offset placement remains present and tested. |
| 7. PAT container | The `8BPT` branch remains present and tested. |
| 8. Type drag dispatch | Type is still included in drag dispatch. |
| 9. Coincident gradient stops | Both hard-edge sides remain emitted and tested. |
| 10. Reversed mask gradients | Reversal remains applied after mask-gradient substitution. |
| 11. Mask stroke opacity | `fill_with` opacity handling remains present and tested. |
| 12. Sharpen alpha edges | Straight-color comparison and premultiplication remain present and tested. |
| 13. Filter schema | Numeric shadow/highlight fields and `black_amount` remain aligned with handlers. |
| 14. Generation watchdog | A running job still refreshes the quiet clock. |
| 15. Socket request framing | The 60-second timeout and 16 MiB request cap remain present. No socket was exercised. |
| 16. Define Pattern layer sampling | The no-selection path still draws the active layer alone and its test passed. |
| 17. Pattern dropdown refresh | The list is still rebuilt when Clone options are shown. |
| Test environment restoration | Pattern tests still restore `XDG_DATA_HOME`. |

## Coverage and test gaps

All ten requested round 8 areas received source review. Menu action references were compared with registrations, including the parameterized filter, adjustment, alignment, and distribution actions; no missing action or unintended round 7 menu removal was established. The duplicate menu entries called out in the prompt resolve to the intended shared actions. GPU code was not reviewed.

The new tests provide useful happy-path coverage, but several assertions cannot detect the findings above:

- Export Layers uses only plain, unmasked, full-opacity layers.
- Reframe uses a logo whose image bounds equal its visible coverage.
- Rotate Canvas covers an exact 90-degree turn and no artboard.
- Pattern Overlay uses opaque coverage beginning at the layer origin.
- Artboard export uses one fully on-canvas board and does not compare output pixels or dimensions.
- AVIF accepts a missing encoder as the normal alternate outcome and tests cleanup only after successful PNG staging.
- PSD layer-style round trips are read back by the same descriptor implementation; they do not establish Photoshop interoperability. No test places EngineData key spellings inside text or exercises a rotated TySh matrix.
- Export naming has no sanitization-collision case, and Export Sizes artboards have no one-undo assertion.

Review complete for all numbered areas in the round 8 prompt. No application fixes were made.

## Status after fixes

| # | Finding | Status |
|---|---------|--------|
| 1 | Export Layers discards masks, effects, opacity | Fixed. Each layer is composited by itself through `Renderer::render_layer` (mask, style, opacity, clipped layers), and the trimmed file grows by the style's reach. Test: a hide-all mask exports clear, a drop shadow widens the file. |
| 2 | Transparent artboard rendered white | Fixed. A board with no background is left clear; the UI dialog and Compy's `transparent: true` pass None. Test. |
| 3 | Export Artboards crops the global composite | Fixed. `Renderer::render_artboard` draws one board's background and its own layers at the board's full size, past the canvas edge included. Test with two overlapping boards. |
| 4 | Rotate Canvas leaves artboard frames | Fixed. Each frame becomes the box around its turned corners. Test at a quarter turn. |
| 5 | Arbitrary angles clip the outside edge | Fixed. The new size rounds outward (with a hair of tolerance so an exact quarter turn stays exact). Test: 10 x 10 at 45 degrees is 15 x 15. |
| 6 | Reframe judges by transform bounds | Fixed. `Document::layer_coverage` renders the layer alone at a small scale and counts what shows through its mask; the visible box is what gets placed and scaled, with the transform following it. Test: a logo in a full-size clear layer moves into the margin. |
| 7 | Nested artboards on import | Fixed. Copied board records are cleared, so the source's boards come in as plain folders. Test. |
| 8 | Board drag joins an open edit | Fixed. `begin_board_drag` refuses while `busy_editing()`. |
| 9 | One undo step per artboard | Fixed. `add_as_artboards` wraps the batch in one edit (aborted when nothing was made). Test. |
| 10 | Sanitized names overwrite | Fixed. `unique_file` appends -2, -3 within a batch for Export Sizes and Export Artboards; Export Layers was already numbered. Test for both. |
| 11 | Pattern Overlay anchored to coverage | Fixed. `Effects::render_at` takes the layer's corner in the padded buffer and tiles from it. Test with a transparent margin. |
| 12 | EngineData scanners read inside strings | Fixed. `find_key` walks the data skipping parenthesized strings (escapes included) and matches whole tokens, so `/Font` no longer matches `/FontSize`; font names are read the same way. Unit test. |
| 13 | Type transform ignores rotation and shear | Fixed. Vertical scale is the length of the y basis vector, horizontal the x one; the paragraph width uses the horizontal one. Unit test at a quarter turn. |
| 14 | Swatches written to the working directory | Fixed. With neither XDG_CONFIG_HOME nor HOME set there is no path, and nothing is read or written. |
| 15 | AVIF staging PNG left behind | Fixed. A failed staging write removes the file before returning. |
| 16 | Swatch cap removes one item | Fixed. `with_swatch` drains everything past 64. Unit test from a 100-entry list. |
| 17 | 180 normalized to -180 | Fixed. Rotation stays in (-180, 180]. Test. |

Tests: 182 passed, 2 ignored.
