# Audit: document model and history

Scope: `src/document.rs`, `src/history.rs`, `src/selection.rs`, `src/transform.rs`, `src/autosave.rs`,
`src/format/mod.rs`, `src/format/validate.rs`, plus the call paths in `src/ui/mod.rs` (`edit`, `autosave_all`)
and `src/ui/agent.rs` (the tool wrapper and `agent_land_fill`). Supporting reads: `src/render/mod.rs`,
`src/render/live.rs`, `src/raster.rs`, `src/brush.rs` (`Stroke::new`), `src/ui/canvas.rs` (drag lifecycle),
`src/ui/dialogs.rs` (spin ranges).

Nothing already listed in `REVIEW_RESULTS.md` through `REVIEW_RESULTS_8.md` is repeated. Where a finding is
the untouched sibling of a fixed one, the relationship is called out so it can be judged quickly.

---

## 1. [P1] A refused nested `canvas_size` leaves the history open forever, and every later edit is silently swallowed

**Where:** `src/document.rs:649-650` (`expand_canvas_for_fill`), `src/document.rs:1150-1151` (`crop`),
`src/document.rs:705-706` (`layer_via`), `src/document.rs:1833-1847` (`finish_stroke_named`),
`src/document.rs:3184-3211` (`canvas_size` itself), `src/document.rs:1189-1205` (`rotate_canvas`),
`src/document.rs:1584-1595` (`flip_canvas`); mechanism in `src/history.rs:52-56, 68-97`.

**Trigger (confirmed end to end):** Image > Generative Expand. The dialog's spin buttons allow 1 to 30,000 on
each side (`src/ui/dialogs.rs:63-64`), so 30,000 x 30,000 is typeable. `expand_canvas_for_fill` validates only
`width < ow || height < oh` (`src/document.rs:647`), opens `begin_edit("Generative Expand")` at line 649, then
calls `self.canvas_size(...)?` at line 650. `canvas_size` rejects the request at `src/document.rs:3180`
(`width as i64 * height as i64 > 100_000_000`) and the `?` propagates out with the edit still open.

**What goes wrong:** `History::depth` is now 1 and nothing ever brings it back to 0.

- `can_undo`/`can_redo` require `depth == 0` (`src/history.rs:35-36`), so Undo and Redo are dead for the rest
  of the session on that document.
- Worse, every subsequent edit is *lost from history*: the next `begin_edit` takes `depth` 1 -> 2 without
  replacing `pending` (`src/history.rs:53`), and its `end_with` takes `depth` 2 -> 1 and returns without
  recording anything (`src/history.rs:71-72`). No further undo step is ever created, so all later work is
  unprotected.
- `busy_editing()` (`src/document.rs:1445`) is permanently true, so `autosave_all` skips this document for
  ever (`src/ui/mod.rs:1171`) and every assistant tool refuses with "The user is in the middle of an edit"
  (`src/ui/agent.rs:267`).
- `is_modified()` (`src/document.rs:2549`) is permanently true, so the close prompt never goes away even right
  after a save.

**Other instances of the same shape, same file:**
- `crop` (`:1147-1155`) checks 1..=30,000 per side but not the 100-megapixel product, so a large crop drag
  (the rect comes from an unclamped canvas drag, `src/ui/canvas.rs:1182, 1222`) hits the same `canvas_size`
  bail through a `?`.
- `canvas_size` itself opens its edit at `:3184` and then does fallible work: `new_argb(width, height)?`
  (`:3193`) and `Selection::from_shape(width, height, ...)?` (`:3208`), which for a near-100-megapixel canvas
  allocates 100 MB or more and can fail.
- `rotate_canvas` (`:1205`) rebuilds the selection with `?` *after* every layer has been turned and
  `set_size` has run, so a failure there leaves the document rotated, with no step and a stuck history.
- `flip_canvas` (`:1590-1595`) and `layer_via` (`:706`) and `finish_stroke_named` (`:1836-1841`) have the same
  begin-then-`?` shape.

**Note on prior work:** `REVIEW_RESULTS.md` #8 fixed exactly this class for `image_size` (which now uses a
`match` with `abort_edit`, `:3231-3235`), and `REVIEW_RESULTS_6.md` #6 added the 100-megapixel guard to
`canvas_size` that the callers above now trip over. The guard landed without the callers being converted to
the `match`/`abort_edit` shape.

**Confidence:** confirmed by reading the full path including the dialog's numeric range and `History`'s depth
arithmetic.

---

## 2. [P1] A layer sized past the canvas limits makes Fill allocate a multi-gigabyte mask and abort the process

**Where:** `src/document.rs:1346-1352` (`fill_with`, mask branch), and the same grid derivation at
`:937` (`gradient_fill`), `:1368` (`fill_with`, pixel branch), `:2445` (`begin_mask_stroke_kind`),
`:3312` (`stroke_selection`), `:3465-3471` (`fill_pattern`), `:1746` (`begin_stroke`).
Allocation site: `src/raster.rs:44-48` (`a8_filled` builds `vec![value; stride * height]` before Cairo sees
the size).

**Trigger (confirmed by reading; the abort is the allocator's behaviour):**
1. New Layer (blank, no image, so `renderer.image_size(id)` is `None`).
2. Layer > Add Reveal-All Mask, which stores a 1 x 1 mask (`src/document.rs:2338`) and sets `mask_target`.
3. Resize the layer past the canvas limits. `Transform::is_valid` accepts sides up to **300,000**
   (`src/format/mod.rs:108`), while every canvas and image path caps at 30,000 / 100 megapixels. The
   assistant's `place_layer` tool sets `t.size.0 = w.max(1.0)` with no upper bound
   (`src/ui/agent.rs:303-306`) and `set_transform` accepts it (`src/document.rs:1871`). A Move-tool resize
   drag reaches the same ceiling (`src/transform.rs:67`).
4. Edit > Fill.

`fill_with` takes the mask branch, finds no image, and falls back to
`(transform.size.0.round().max(1.0) as i32, ...)` = (300000, 300000) at `:1346`. Because the mask is 1 x 1 it
calls `a8_filled(300000, 300000, v)` at `:1347`, which asks for a 9e10-byte `Vec` before Cairo's
`create_for_data` can reject the size. Rust's allocator failure path aborts the process; with overcommit it is
an OOM kill instead. Either way the document and any unsaved work are gone. At a merely large 30,000 x 30,000
the same line asks for 900 MB and `coverage_on_layer` (`src/selection.rs:118-133`) asks for another.

`gradient_fill`, `fill_pattern`, `stroke_selection` and `begin_stroke` derive their grid the same way and then
call `new_argb`; Cairo rejects anything over 32,767 per side cleanly, so those degrade to an error rather than
an abort, but at 30,000 they still attempt a 3.6 GB surface on the GTK thread. `Stroke::new` has a
100-megapixel guard, but only on the *grown* grid; its fallback is the layer's own unchecked size
(`src/brush.rs:231-235`).

**Fix direction:** either bound `Transform::is_valid` to the same 30,000 the rest of the app uses, or clamp
every `image_size(id).unwrap_or(transform.size ...)` fallback and give `a8_filled` a size guard before the
`vec!`.

**Note on prior work:** `REVIEW_RESULTS_6.md` #6 added the 100-megapixel bound to `image_size` and
`canvas_size`. The layer-transform-derived grids were not covered.

**Confidence:** the reachable path is confirmed by reading; the exact failure mode (abort vs OOM kill) depends
on the allocator and overcommit setting.

---

## 3. [P1] A pixel move keeps a layer id that undo or a delete can invalidate, then panics on `self.index[&id]`

**Where:** `src/document.rs:1936` (`pixel_move` stores `id`), then `:1947` (`move_pixels`), `:1967`
(`compose_pixel_move`), `:2002, 2005` (`finish_pixel_move`). Panic site: `src/render/mod.rs:194`
(`pub fn layer(&self, id: Uuid) -> &Layer { &self.layers[self.index[&id]] }`).

**Trigger (confirmed):** `begin_pixel_move` opens no history edit and sets no flag that anything checks.
`busy_editing()` (`src/document.rs:1445`) lists `stroke`, `warp`, `effects_preview` and `history.is_editing()`
but **not** `pixel_move`. The drag spans GTK events (press at `src/ui/canvas.rs:885`, release at
`src/ui/canvas.rs:988`), so the main loop runs in between.

- The assistant's `undo` tool is classified as a *read* (`src/ui/agent.rs:255`), so it skips both the
  `floating` commit and the `busy_editing` refusal at `:266-267` and calls `dd.undo()` directly
  (`src/ui/agent.rs:577`). With `depth == 0` that undo succeeds.
- If the undone step created or deleted the layer being dragged, `Renderer::restore` reindexes without it
  (`src/render/mod.rs:529-535`), and the next pointer-motion event calls `move_pixels` ->
  `self.renderer.layer(id).transform` -> `self.index[&id]` -> panic.
- The plain UI Undo action is also unguarded (`src/ui/mod.rs:473`), so a Ctrl+Z that reaches the window during
  the drag does the same.
- `delete_layer` and `merge_layers` from the assistant reach the layer the same way (they pass the
  `busy_editing` check because it ignores `pixel_move`).

**Silent-corruption variant of the same hole:** if the undo merely *replaces* the layer's pixels rather than
removing the layer, `finish_pixel_move` composes `pm.base` and `pm.lifted`, which were lifted from the
pre-undo image, and writes them back with `set_image` (`:2007`). The undo is silently reversed and the
intervening pixels are lost.

**Related:** `Selection::combined` (`src/selection.rs:80-95`) indexes `other` with `self`'s width and height.
If a canvas resize lands between `begin_pixel_move` and `finish_pixel_move`, `finish_pixel_move` restores a
stale-size selection at `:2010` and the next `apply_selection` can index out of bounds. Same root cause.

**Confidence:** confirmed by reading `busy_editing`, the agent's `reads` list, the UI undo action, the drag
lifecycle, and `Renderer::layer`.

---

## 4. [P2] Crop, Canvas Size, Image Size and Flip Canvas move every layer but leave artboard frames behind

**Where:** `src/document.rs:3185-3190` (`canvas_size` shifts `transform` and `mask_placement` only),
`src/document.rs:3242-3275` (`resample`, same), `src/document.rs:1585-1588` (`flip_canvas`, same).
Compare `rotate_canvas` at `:1195-1200`, which does turn `layer.artboard`, and `make_room_for` at
`:2626`, which shifts frames after the canvas grows.

**Trigger:** a document with an artboard. Image > Canvas Size with any anchor other than the top left (so
`dx`/`dy` are non-zero), or the Crop tool, or Crop to Selection, or Image > Image Size, or Image > Flip Canvas
Horizontal.

**What goes wrong:** the artboard's children move with the canvas while `Layer.artboard`'s `x`, `y`, `width`,
`height` stay at the old numbers. The board's background fill and its clip rectangle end up somewhere else on
the canvas, and `artboard_of` / `artboard_at` / `export_artboards` all work from the stale frame. Image Size
additionally leaves the frame at the *old scale*, so a 2x resample halves the board relative to its contents.

**Note on prior work:** `REVIEW_RESULTS_8.md` #4 fixed exactly this for Rotate Canvas. The four sibling
geometry operations were not touched.

**Confidence:** confirmed by reading; all three functions are short and contain no `artboard` reference.

---

## 5. [P2] `add_artboard` places the new board in pre-shift coordinates after `make_room_for` moved everything

**Where:** `src/document.rs:2641-2657`. Compare `set_artboard_frame` at `:2666-2670`, which explicitly handles
this ("make_room_for may have shifted everything; the frame is placed after it, from the shifted old one").

**Trigger:** Layer > Artboard from Layers (menu, `src/ui/mod.rs:499`, and the assistant tool
`artboard_from_layers`, `src/ui/agent.rs:516`) with a selected layer that extends past the left or top canvas
edge, so the computed frame has a negative origin (`src/document.rs:2753-2756`).

**What goes wrong:** `add_artboard` calls `make_room_for(board.rect())` at `:2645`, which grows the canvas and
shifts every layer and every *existing* artboard by `(-x0, -y0)` (`:2624-2626`). It then inserts the record
with `record.artboard = Some(board)` at `:2651`, still carrying the un-shifted `frame.0`/`frame.1`. The new
board is displaced from its own contents by exactly the shift. `artboard_from_layers` then moves the layers
into it (`:2760`), and since it computed `frame` from the pre-shift bounds the error compounds.
`Artboard::is_valid` (`src/format/mod.rs:270`) allows negative `x`/`y`, so the bad state saves and reloads.

`import_as_artboard` (`:2715-2735`) re-reads the frame back from the record, so its board and its own children
stay mutually consistent, but both are displaced relative to the rest of the document.

**Confidence:** confirmed by reading both functions side by side.

---

## 6. [P2] A Layer Style preview leaks into unrelated undo entries, so Cancel does not really cancel

**Where:** `src/document.rs:1103-1130` (`begin_layer_style` / `preview_effects` / `end_layer_style`),
`src/document.rs:123` (`state()` snapshots `renderer.snapshot()`), `src/ui/mod.rs:1150-1159` (`edit` has no
`busy_editing` guard).

**Trigger (confirmed by reading):**
1. Open Layer Style on a layer and drag a slider. `preview_effects` writes straight into the renderer
   (`:1116`) with no `begin_edit`, by design after the `REVIEW_RESULTS_5.md` #2 fix.
2. The window is non-modal, and the UI's `edit()` wrapper does not consult `busy_editing()`, so any menu
   action still runs. Do a Fill. Its `begin_edit` snapshots the renderer *including the previewed effects*, so
   the recorded entry has `before` = with-preview and `after` = with-preview-plus-fill.
3. Press Cancel. `end_layer_style(false)` puts the original effects back with `set_effects` and records no
   step (`:1124`).
4. Undo the Fill. The restored `before` state carries the previewed effects, so the cancelled layer style
   comes back.

**What goes wrong:** the fix that took the preview out of history left the preview visible to every snapshot
taken while the window is open. The assistant is protected (`busy_editing()` includes `effects_preview`,
`src/ui/agent.rs:267`); the UI is not.

**Confidence:** confirmed by reading; not executed.

---

## 7. [P2] A background generative fill lands inside whatever edit the user has open

**Where:** `src/ui/agent.rs:721-733` (`agent_land_fill`, the `window` branch) and `:710-716` (the `layer`
branch).

**Trigger:** start a generation, then begin a Free Transform (Ctrl+T) while it runs. The result arrives on the
main loop from the job poll. Unlike the tool wrapper at `src/ui/agent.rs:264-268`, this path checks neither
`floating` nor `busy_editing()`; it calls `dd.begin_edit("Generative Fill")` at `:725` straight into the open
Free Transform edit (depth 1 -> 2).

**What goes wrong:**
- `end_edit` at `:729` takes depth back to 1 and records nothing, so the generated layer becomes part of the
  user's Free Transform step. Pressing Escape (`cancel_free_transform` -> `abort_edit`, `src/document.rs:790-793`)
  throws the generated layer away with no warning.
- On the error branch, `abort_edit` at depth 2 only decrements the depth and restores nothing
  (`src/history.rs:58-64`), so a half-applied fill stays.
- The `layer` branch at `:712` calls `select_layer` mid-stroke, changing the active layer under a live brush
  stroke (the stroke itself is safe, it uses `stroke_layer`).

**Confidence:** confirmed by reading both paths and `History::cancel`.

---

## 8. [P3] Guides do not follow the canvas, and Rotate Canvas discards them with no way back

**Where:** guides live outside the history snapshot (`src/document.rs:39-41`; `State` is only
`render`/`selection`/`active`, `:76-80`). `canvas_size` (`:3178-3213`), `resample` (`:3238-3283`) and
`flip_canvas` (`:1581-1599`) never touch `guides_v`/`guides_h`; `rotate_canvas` clears them outright
(`:1207-1208`).

**What goes wrong:** after a Crop or a Canvas Size with a non-zero offset the guides sit at the old document
coordinates while the picture has moved, and because `snap_targets` feeds them to `snapped_move`
(`:2168-2172, 2227-2234`) a snapped drag settles a layer on the wrong line. After Rotate Canvas the guides are
gone and Undo does not bring them back, since they are not part of `State`.

Guides are this port's own extension (the reference has no persistent guides, only transient snap guides in
`reference/Compositor/Document/EditorSession.swift:165`), so there is no spec to port; the inconsistency is
internal.

**Confidence:** confirmed by reading.

---

## 9. [P3] `matte_cache` is keyed by a raw surface pointer that a freed surface can hand back

**Where:** `src/document.rs:37` (the field is `Option<(usize, Vec<f32>)>`, the pointer only, no surface kept)
and `:506-512`.

**Trigger:** run Remove Background on layer A, then do anything that drops A's old surface (a filter, a
stroke, an undo that releases the last reference), then run Remove Background on a layer whose freshly
allocated surface happens to reuse the same heap address.

**What goes wrong:** `key = image.to_raw_none() as usize` matches the cached entry and the wrong subject mask
is refined and committed. Note the contrast with `last_filter` (`:50`), which keeps strong references to the
surfaces it compares by pointer (`:348`) and is therefore safe.

**Confidence:** plausible. The code path is confirmed; whether the allocator reuses the address is not
something I can verify statically. The fix is cheap: hold the `ImageSurface` in the cache tuple.

---

## 10. [P3] `move_layers` can strand a clipping mask's base in another folder

**Where:** `src/document.rs:2887-2919`. `validate::hierarchy` (`src/format/validate.rs:67-86`) and
`live_mask_graph` (`:90-108`) check for cycles, non-group parents and group/adjustment bases, but neither
requires a clipped layer and its `mask_source_id` to share a parent.

**Trigger:** drag a clipped layer into a folder while its base stays outside. `move_layers` rewrites
`parent_id` for the dragged roots (`:2908`) and leaves `mask_source_id` alone; `validate::hierarchy(&next)` at
`:2914` accepts it, and it saves and reloads.

**What goes wrong:** `prepare_stacks` requires `self.parent(child) == self.parent(base)`
(`src/render/live.rs:36`), so the layer stops being part of the clipping stack and falls back to
`draw_clipped`. It is still clipped, but now also carries the new folder's mask and opacity, and the soft-edge
handling the stack provides is lost. The layers panel shows a clip arrow pointing at a layer that is not
beneath it.

`copy_layers` is careful here (`:2945` drops a clip whose base was not copied), as is `merge_layers`
(`:1563, 1568-1571`). Only `move_layers` leaves the link dangling across folders.

**Confidence:** confirmed by reading; whether the reference forbids the arrangement outright I did not verify.

---

## 11. [P3] `move_layers` and `copy_layers` index unchecked layer ids

**Where:** `src/document.rs:2895-2896` and `:2930-2931` (`self.renderer.layer(a)` for the `Place::Above` /
`Place::Below` anchor), and `:2907` (`self.renderer.layer(*id)` over the raw `ids` slice).

`move_layers` filters `ids` through `has_layer` when building `moving` at `:2891`, then at `:2907` iterates
the *unfiltered* `ids` and calls `self.renderer.layer(*id)`, which panics via `self.index[&id]`
(`src/render/mod.rs:194`) on a stale id. The `Place` anchor is never checked at all in either function
(contrast the parent check two lines later, `:2900-2903`, which does use `has_layer`).

**Trigger:** a layers-panel drag whose source rows or drop target were removed between press and drop, for
example by a generative fill landing (`src/ui/agent.rs:721`) or by an assistant `delete_layer`. The drag state
is held across main-loop iterations in `src/ui/layers.rs:475-479`.

**Confidence:** the missing guard is confirmed; the interleaving that produces a stale id is plausible rather
than demonstrated.

---

## 12. [P3] Undo leaves `selected` and `mask_target` stale, and the brush silently changes target

**Where:** `src/document.rs:124-130` (`apply`).

`apply` restores `render`, `selection` and `active`, then rebuilds `selected` as "whatever still exists, plus
the active layer". It never touches `mask_target`, `last_selection`, `floating`, `pixel_move`, `matte_cache`
or `effects_preview`.

Concrete consequences:
- Undoing a Delete Layer restores the layers but collapses a multi-layer selection down to the active layer,
  so the next Align, Distribute, Merge Layers or group transform acts on a different set than before.
- Undoing Delete Layer Mask (`:2348-2355`, which sets `mask_target = false` inside the edit) restores the mask
  but leaves `mask_target` false, so the Brush silently paints pixels instead of the mask with no visible
  change in the panel state.
- Redoing Add Mask leaves `mask_target` true with no mask, which is harmless only because `active_mask()`
  re-checks `renderer.mask(id)` (`:317-324`).

**Confidence:** confirmed by reading.

---

## 13. [P3] Image Size destroys type and shape layers' editability

**Where:** `src/document.rs:3256` (`resample` calls `self.renderer.set_image`), and
`src/render/mod.rs:209-217`, where `set_image` clears both `shape` and `text` on the record.

After Image > Image Size every type layer stops returning a style from `text_style` (`:1063`) and every shape
layer stops redrawing through `redraw_shape` (`:1023-1038`), so a later resize stretches the raster instead of
re-rendering the shape at its correct corner radius. `resample` should use `set_text_image` /
`set_shape_image` (which keep the record) for those layers, with the style's `baseWidth`/`baseHeight` scaled.

**Confidence:** confirmed by reading. Whether this matches the reference's `ImageResizer` I did not check line
by line, so it may be deliberate.

---

## 14. [P3] `fill_path` and `define_pattern` ignore the active selection

**Where:** `src/document.rs:1397-1414` (`fill_path`, the pixel branch) and `:3429-3453` (`define_pattern`).

`fill_path` traces and fills the closed path with no reference to `self.selection`, so a fill runs outside an
active selection. The mask branch above it (`:1388-1396`) deliberately *replaces* the selection with the path
area, which is consistent with itself but also ignores the standing selection.

`define_pattern` uses only `selection.bounds` (`:3431-3438`), so a lasso or wand selection yields a
rectangular tile that includes unselected pixels. This matches the doc comment, so it may be intended; worth a
one-line note in the comment either way.

Everything else in this area does honour the selection correctly, including under rotation and scaling: see
the "checked and sound" list below.

**Confidence:** confirmed by reading.

---

## 15. [P3] A nested `abort_edit` silently does nothing, so partial work survives

**Where:** `src/history.rs:58-64` (`cancel` decrements the depth and returns `None` whenever `depth > 0` after
the decrement), reached from `src/document.rs:175-177`.

Callers that assume `abort_edit` rolls back: `align_layers` (`:3395`), `commit_distort` (`:2126`),
`straighten` (`:1224, 1232`), `import_as_artboard` (`:2718, 2722`), `artboard_from_layers` (`:2758, 2760`),
`begin_free_transform` (`:722`), plus the assistant wrapper (`src/ui/agent.rs:672`). Each is correct when it
is the outermost edit. When one runs inside another edit (the assistant wrapper always nests, and
`straighten` nests `rotate_canvas` and `crop`), the abort only decrements the depth and the partial mutation
stays committed into the outer step.

The dangerous composition is `begin_free_transform` (`src/document.rs:717-727`): it opens an edit at `:721`,
then calls `layer_via`, which opens a second one at `:705`. If `layer_via` fails after its `begin_edit` (the
`?` at `:706`), the single `abort_edit` at `:722` cannot unwind both levels and the document is left stuck in
the state described in finding 1.

**Confidence:** confirmed by reading `History::cancel`.

---

## Answers to the specific questions

**1. Nested edits.** Every public mutating method either opens its own edit or is documented as needing the
caller's. The documented caller-driven ones are `nudge_artboard` (`:2689-2691`), `preview_effects`,
`preview_filter`, `preview_distort`, `set_adjustment(commit=false)`, `gradient_fill(commit=false)`,
`remove_background(commit=false)`, `begin_stroke`/`continue_stroke`, `begin_pixel_move`/`move_pixels`, and
`begin_free_transform` (which deliberately leaves its edit open until commit or cancel). The layer-selection
methods (`select_layer`, `toggle_layer_selected`, `select_layer_range`, `select_all_layers`,
`set_mask_target`) mutate outside history on purpose; the field comment at `:31-32` says so. The
begin-then-`?` leaks are finding 1; the nested-abort problem is finding 15.

**2. Undo correctness.** Stale after undo: `selected`, `mask_target`, `last_selection`, guides, `matte_cache`,
`pixel_move`, and the renderer's preview surfaces for layers that no longer exist (`Renderer::restore`,
`src/render/mod.rs:517-536`, never clears `previews`/`mask_previews` for removed ids, though nothing then
draws them). Artboard frames *are* part of the snapshot (they live on `Layer`), so undo restores them
correctly; the problem in finding 4 is that the forward operations never move them. The only stale id that
reaches `self.index[&id]` and panics is `pixel_move.id` (finding 3). `floating`, `stroke_layer`,
`effects_preview` and `last_filter` all re-check with `has_layer` or by id comparison before indexing
(`:732, 1789, 1114, 1122, 348`).

**3. Hierarchy invariants.** `descendants()` (`:1512-1519`) is iterative and bounded by its visited set, so a
parent cycle cannot hang it. Parent cycles, non-group parents and over-64 nesting are all caught by
`validate::hierarchy`, which `move_layers` (`:2914`), `copy_layers` (`:2958`) and `merge_layers` (`:1565`) all
run before mutating. `Place::Into` of a layer into itself or a descendant is refused at `:2899-2903`. The one
invariant nothing enforces is a clipped layer's base being a sibling (finding 10). `block_end` (`:2984-2989`)
and `entries_ordered` (`src/format/mod.rs:424-440`) order siblings by array position only, so `move_layer` and
`move_layer_to_end` swapping non-adjacent array slots is safe.

**4. Selection and masks.** See finding 14 for the two that forget it. Everything else applies the selection
in the correct space: `coverage_on_layer` maps the document-space mask through `pixel_to_document`
(`src/selection.rs:118-133`), and `gradient_fill` (`:956-967`) and `fill_pattern` (`:3482-3499`) define their
source in document space and then transform the context, so a rotated or scaled layer is handled correctly.

**5. Canvas geometry.** See findings 4, 5 and 8 for artboards and guides. Selection, mask placements, text and
shape records all move consistently. Size checks are present in `filtered` (`:383`), `merge_floating`
(`:756`), `compose_pixel_move` (`:1974`), `add_shape_layer` (`:1003`), `add_path_shape_layer` (`:3523`),
`decode_image` (`:3082`), `canvas_size` (`:3179-3180`) and `image_size` (`:3229-3230`). Missing: `crop` checks
sides but not the product (finding 1), `expand_canvas_for_fill` checks neither (finding 1), and every
`transform.size`-derived grid checks nothing (finding 2).

**6. Autosave.** `autosave_snapshot` (`:2515-2524`) runs on the main thread and packs plain bytes, so the
worker thread cannot race the document. `autosave_all` correctly refuses while `busy_editing()`
(`src/ui/mod.rs:1171`) and serialises workers with the `writing` flag. The discard-during-write race is
handled by the `DISCARDS` counter (`src/autosave.rs:38-45, 60-64`). Two gaps: a pixel move in progress is not
`busy_editing`, so an autosave taken mid-drag records the pre-move state (harmless, the move is only a
preview); and after a leaked edit `busy_editing()` is permanently true, so autosave stops for good (finding
1). An assistant job writing into the document does so on the main loop, so there is no data race, only the
transaction problem in finding 7.

**7. Copy-on-write sharing.** Sound. I traced every `with_bytes_mut` / `with_bytes_raw_mut` call site
(`src/raster.rs`, `src/gradient.rs:166`, `src/distort.rs:142`, `src/render/live.rs:61,74`, `src/matte.rs:124,
221, 236`, `src/warp.rs:73`, `src/brush.rs:580, 630`, `src/document.rs:353, 473, 982, 2058, 2471, 2491, 3606`)
and every one of them writes into a surface it just created, or into a preview surface built fresh by
`begin_preview` (`src/render/mod.rs:271`) or `begin_mask_preview` (`src/render/mod.rs:371`), which copies the
mask's pixels rather than cloning the handle. `set_image`/`set_mask`/`adopt_preview`/`adopt_mask_preview` all
replace by insertion, never in place. Surfaces shared by `duplicate()`, `duplicate_layer()` and the history
are therefore never written through.

---

## Checked and found sound

- `History` depth, merge window, revision bookkeeping and `trim` byte accounting (`src/history.rs`), and the
  byte estimator in `finish_edit` (`src/document.rs:185-201`), which correctly counts selection masks that the
  current state no longer holds.
- `descendants`, `transform_members` (64-step cap), `insertion`'s ancestor walk (64), `artboard_of` (64), and
  `entries_ordered`'s depth cap: no unbounded recursion anywhere in the hierarchy code.
- Cycle, non-group-parent, duplicate-id, missing-`activeLayerID` and asset-name rules in
  `src/format/validate.rs`, and the fact that `Document::new` is only fed manifests that went through
  `Manifest::parse` (including the PSD path, `src/psd.rs:408`).
- The package write in `src/format/save` (staging directory, backup, rollback on rename failure) and the
  symlink and size checks in `check_file` / `check_size`.
- `merge_plan` / `merge_layers`: the insertion index arithmetic at `:1559-1560`, the reclip remapping at
  `:1563, 1568-1571`, and the dry-run `validate::hierarchy` on the proposed arrangement before anything
  changes.
- `merge_visible` reparenting of hidden layers out of merged folders (`:823-829`) and its insertion index.
- `move_layer` / `move_layer_to_end` swapping array slots: correct, because sibling order depends only on
  relative array position among layers sharing a parent.
- `carried_mask_placement`, `grown_mask`, `distorted_mask` and the mask-carrying branches of `apply_filter`,
  `finish_stroke_named`, `finish_pixel_move` and `commit_distort`: linked and unlinked masks are handled
  consistently and the growth cases are all size-checked.
- `Selection` combine, invert, translate, feather, `resized` (the distance transform and the canvas-edge pull
  in Contract), and `coverage_on_layer` / `coverage_on_grid` under rotation and flips.
- `Transform::following`, `placing`, `mirrored`, `bounds`, `contains` and `is_valid`'s NaN rejection; NaN from
  user input is filtered at `set_transform` (`:1871`), `rotate_canvas` (`:1160`), `fill_pattern` (`:3460`),
  `wand` (`:277`) and `Drag::updated` (`src/transform.rs:67`).
- `autosave` packing, unpacking, staging, discard counting and `recoverable()` parsing.
- The assistant tool wrapper's all-or-nothing edit (`src/ui/agent.rs:264-272, 672`) for the normal case where
  the inner document method balances its own edits.
- Copy-on-write surface sharing (question 7 above): no in-place mutation of a shared surface exists.
