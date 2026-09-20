# Painting audit: brush engine, stroke lifecycle, presets, pointer events

Scope: `src/brush.rs`, the stroke lifecycle in `src/document.rs`, `src/ui/brushes.rs`, the brush parts of
`src/ui/tools.rs` and `src/ui/canvas.rs`, `brush_stroke` in `src/ui/agent.rs`, and the C routines in `csrc/`
that dab or blend. Spec comparisons are against `reference/Compositor/Document/BrushStroke.swift`,
`reference/Compositor/Document/EditorSession+Brush.swift` and `reference/Compositor/Document/EditorSession.swift`.
Findings already recorded in `REVIEW_RESULTS.md` through `REVIEW_RESULTS_8.md` are excluded. Report only; no
project file was changed, no cargo command was run, the app was not started.

---

## 1. [P1] Undo during a live stroke is silently reversed when the stroke commits

**Locations:** `src/document.rs:214` (`undo`), `src/document.rs:124-130` (`apply`), `src/render/mod.rs:517-536`
(`restore`), `src/document.rs:1786-1852` (`finish_stroke_named`), `src/ui/mod.rs:473` (the Ctrl+Z action, always
enabled), `src/ui/canvas.rs:572` (drag end). Spec: `reference/.../EditorSession.swift:444-449`.

**Sequence:** press the mouse with the Brush and keep it down, press Ctrl+Z (or Ctrl+Shift+Z), release.

`History::can_undo` only requires `depth == 0` (`src/history.rs:35`), and no edit is open during a stroke, so
Ctrl+Z runs. `Document::apply` calls `Renderer::restore`, which replaces `layers`, `images` and `masks` but never
touches `self.previews` or `preview_transforms`. The live stroke keeps its own preview grid, whose `base` tiles
were captured from the pre-undo pixels at `Stroke::allocate` (`src/brush.rs:459-469`). On release,
`finish_stroke_named` adopts that grid as the layer's pixels (`src/document.rs:1834-1844`). The undone edit is
therefore reinstated together with the paint, and the History panel still shows the step as undone.

The reference forbids exactly this: `canUseHistory` is `!isProjectBusy && ... && brushStroke == nil && warpStroke == nil
&& ... && transformEdit == nil`, and `canUndo`/`canRedo` are gated on it. The port has no equivalent guard and no
action-enable logic at all (`grep set_enabled src/ui/mod.rs` is empty).

The same hole is open for every other keyboard-reachable edit, because `App::edit` and `App::with_doc`
(`src/ui/mod.rs:1139-1159`) never consult `busy_editing()`: Alt+Backspace (Fill), Ctrl+E (Merge), Delete,
Ctrl+Left/Right nudge and so on all run mid-drag and are then overwritten by the stroke's commit. Undo is the
worst case because the user believes work was thrown away and it comes back.

**Confidence:** confirmed by tracing.

---

## 2. [P1] A large coordinate from `brush_stroke` hangs the app with no way out

**Locations:** `src/brush.rs:286-287` (`append`'s only guard), `src/brush.rs:385-392` (`curve`),
`src/brush.rs:396-408` (`walk`), `src/ui/agent.rs:447-449` (points parsed with no magnitude check).
Spec: `reference/.../BrushStroke.swift:219`.

**Input:** `brush_stroke` with `points: [[0,0],[1e300,0],[10,10]]` (any JSON number is accepted; only `NaN` and
`Infinity` are impossible in JSON, and those are the only things `append` rejects).

`Stroke::append` checks `is_finite` and nothing else. With three samples, `curve` computes
`pieces = (hypot(dx,dy) / 2).ceil() as usize`; `1e300 as usize` saturates to `usize::MAX`, and `for index in
1..=pieces` never ends. `walk`'s `while distance <= length` with `spacing >= 0.25` has the same shape. The GTK
main loop is blocked, the agent socket stops answering, and the only recovery is killing the process, which loses
everything since the last autosave (and autosave itself is blocked, see finding 9).

The reference guards the entry point precisely for this: `guard point.x.isFinite, point.y.isFinite,
abs(point.x) <= 10_000_000, abs(point.y) <= 10_000_000 else { return }`. That bound is missing in the port.
Values well short of the hang (1e8 or so) still cost minutes of unresponsive UI.

The pointer paths are safe because view coordinates are bounded by the widget and the zoom range, so this is
reachable only through the agent tool and `Document::replay_stroke`/`stroke_path` callers.

**Confidence:** confirmed by tracing.

---

## 3. [P2] The tip is a circle in layer pixels whatever the layer's transform, so non-uniform scale, shear or heavy upscale paints the wrong shape and size

**Locations:** `src/brush.rs:245-251` (`scale` is only the x column of `to_document`; `grid_diameter` is
clamped with `.max(1.0)`), `src/brush.rs:141-161` (`tip` is radially symmetric in grid pixels),
`src/brush.rs:419-457` (the dab is stamped axis-aligned in the grid).
Spec: `reference/.../BrushStroke.swift:183-191`.

```rust
let scale = (to_document.xx() * to_document.xx() + to_document.yx() * to_document.yx()).sqrt();
let grid_diameter = (settings.diameter / scale).max(1.0);
```

**Sequence:** Free Transform a layer to 400 percent width and 100 percent height, then paint with a 40 px brush.

`scale` is 4, `grid_diameter` is 10, and a 10 px circle in layer pixels maps back to a 40 x 10 document ellipse.
A 40 px round brush paints a flat ellipse. Shear behaves the same way; a layer scaled *up* hits the other end,
where `grid_diameter` would fall below 1 and `.max(1.0)` silently makes the mark one layer pixel wide, which at
1000 percent is 10 document pixels instead of the requested diameter.

The reference computes both axis scales and only uses the fast grid tip when the mapping is square:

```swift
let square = abs(pixelToDocument.b) < 1e-9 && abs(pixelToDocument.c) < 1e-9
    && scaleX > 1e-9 && abs(scaleX - scaleY) < 1e-9
gridTip = gpu == nil && square && gridDiameter >= 1 && gridDiameter <= Self.gridTipLimit ? ... : nil
stamp = gpu == nil && gridTip == nil && settings.diameter <= Self.stampLimit ? ... : nil
```

The `stamp` fallback draws the tip through `pixelToDocument.inverted()`, so it stays a circle of `diameter`
document pixels on any transform. The port has no `square` test and no stamp path.

**Confidence:** confirmed by tracing.

---

## 4. [P2] The provisional tail is not fully undone on a non-uniformly scaled layer, leaving doubled coverage

**Locations:** `src/brush.rs:358-371` (`keys_reached`), `src/brush.rs:333-341` (`draw_tail`),
`src/brush.rs:343-355` (`remove_tail`).

`keys_reached` inflates the segment by `diameter / 2 + 2` **document** pixels and maps that box into the grid,
but the dab itself is a square of `grid_diameter = diameter / scale_x` grid pixels in *both* axes (finding 3).
When `scale_y > scale_x` (a layer squashed horizontally, for example 50 percent width and 100 percent height),
the tip reaches `diameter / scale_x / 2` grid pixels vertically while `keys_reached` only inflates by
`diameter / 2 * (1 / scale_y)`. Tiles the tail touched are then missing from `tail_backup`, `remove_tail` never
restores them, and the provisional straight tail stays in the coverage buffer permanently. The final curve then
accumulates on top of it, so a band along the stroke reads at roughly double coverage on a soft tip and shows the
straight chord's silhouette next to the curve.

The reference does not have this because a non-square mapping never uses the grid tip at all; its `affected`
rectangle is the dab circle mapped through the same inverse, so the reach and the mark always agree.

**Confidence:** confirmed by tracing.

---

## 5. [P2] Painting is allowed on a hidden layer (or one inside a hidden group)

**Location:** `src/document.rs:1721-1726` (`begin_stroke`'s only refusals) and
`src/document.rs:2436-2441` (`begin_mask_stroke_kind`). Spec: `reference/.../EditorSession+Brush.swift:4-10`.

```rust
if layer.is_group() || layer.adjustment.is_some() { bail!("Select an image layer first."); }
if self.selection.as_ref().is_some_and(|s| s.is_empty()) { bail!("Nothing is selected."); }
```

`canPaint` in the reference additionally requires
`activeLayerID.map { document?.effectiveVisibleIDs.contains($0) == true } == true` and
`selectedLayerIDs.count == 1`. In the port, selecting a hidden layer and dragging starts a real stroke: nothing
appears on screen (the preview replaces a layer that is not drawn), so the user assumes the tool is not working,
keeps stroking, and every drag commits a paint step into the invisible layer. Undoing the visible-looking
"nothing happened" then walks back through a pile of Brush Stroke steps.

**Confidence:** confirmed by tracing.

---

## 6. [P2] Clone Stamp, Spot Healing and Pattern Stamp paint the layer's pixels while its mask is the target

**Location:** `src/ui/canvas.rs:1524-1531`.

```rust
let mask = d.document.mask_target() && matches!(d.tool, Tool::Brush | Tool::Eraser | Tool::Blur);
```

Dodge/Burn/Sponge are refused with a message a few lines above (`src/ui/canvas.rs:1512`), but Heal, Clone and the
Pattern Stamp fall through to `d.document.begin_stroke(...)`, which paints the image. The user has the mask
thumbnail selected in the Layers panel and the brush quietly edits the picture behind it. The reference gates all
of these at once: `guard tool == .brush || tool == .blur || (tool.isBrushTool && !isMaskSelected), canPaint, ...`.

This is the same defect class as the fixed `REVIEW_RESULTS_6.md` finding 9 (Fill Path and Stroke Path on a mask),
left open for the brush tools themselves. The agent path is correct here: `src/ui/agent.rs:481-485` refuses
anything but paint and erase on a mask.

**Confidence:** confirmed by tracing.

---

## 7. [P2] Painting a type or shape layer is thrown away by the next resize or text edit

**Locations:** `src/document.rs:1725` (no refusal for `layer.text` / `layer.shape`),
`src/document.rs:1023-1038` (`redraw_shape`), `src/document.rs:1069` (`set_text`),
`src/document.rs:1878` (`set_transform` calls `redraw_shape` on any size change),
`src/document.rs:1833-1847` (the commit replaces the pixels and leaves `shape`/`text` in place).

`begin_stroke` refuses only folders and adjustment layers. A type or vector layer keeps its `text` / `shape`
JSON while its raster is replaced by the stroke's grid. Any later size change re-runs `redraw_shape`, which
rebuilds the flat shape from the JSON and discards everything painted; editing the text does the same through
`set_text`. Photoshop either rasterizes first (dropping the vector data) or refuses. The port does neither, so
the paint looks committed, survives save and reload, and then vanishes the first time the layer is resized.

**Confidence:** confirmed by tracing (the deletion path is `set_transform` -> `redraw_shape` ->
`set_shape_image`; nothing preserves the painted pixels).

---

## 8. [P2] A failed `replay_stroke` leaves `stroke` Some with no edit and no way to close it

**Locations:** `src/document.rs:1711-1717`; compare the mask twin at `src/document.rs:1430-1441`, which does it
correctly; `src/document.rs:1757-1770` (`continue_stroke` restores rather than cancels);
`src/ui/canvas.rs:1531-1538`. Spec: `reference/.../EditorSession+Brush.swift:64-68`.

```rust
self.begin_stroke(*first, settings, kind)?;
let id = self.stroke_layer.or(self.active).ok_or_else(...)?;
if let Some(stroke) = self.stroke.as_mut() { stroke.replay(rest)?; if let Some(rect) = stroke.changed { self.renderer.preview_changed(id, rect)?; } }
self.finish_stroke()
```

Both `?` leave `self.stroke` and `self.stroke_layer` set and skip `finish_stroke`. `replay_mask_stroke` on line
1438 handles the identical situation with `if let Err(e) = result { self.cancel_stroke(); return Err(e); }`, and
the reference's `continueBrush` does `catch { cancelBrush(); brushError = ... }`. The port's `continue_stroke`
instead puts the stroke back (line 1768) after an error.

The consequences of a stroke stuck open are wide, because `busy_editing()` (`src/document.rs:1445`) is
`stroke.is_some() || ...`:

- every later `begin_stroke` bails with "a stroke is already in progress";
- every mutating agent tool bails with "The user is in the middle of an edit" (`src/ui/agent.rs:267`);
- **autosave stops for that document permanently** (`src/ui/mod.rs:1171` skips a busy document).

Nothing in the UI ever calls `cancel_stroke` (`grep cancel_stroke src/ui` finds no hit), so there is no Escape,
no tool switch and no recovery short of closing the tab and losing the work.

The same shape exists on the pointer path: `Canvas::begin_stroke` sets `painting` only when the whole result is
`Ok`, but a shift-click straight line runs `document.begin_stroke` and then `document.continue_stroke`
(`src/ui/canvas.rs:1532`). If the second call fails, the document holds an open stroke while `painting` stays
false, so the drag-end handler at line 572 never calls `finish_stroke`.

**Confidence:** plausible. The path is exact; what I could not establish is a readily reachable error from
`Stroke::append`/`preview_changed` (the raw-pointer borrow in `src/raster.rs:196-207` and the bail-not-panic
clamping in `halve_into` make the failure rare). The fix is the same either way, and the mask twin already has it.

---

## 9. [P2] Every mouse-down with a sampled tip rebuilds a dozen full-size rotated copies of it

**Locations:** `src/brush.rs:253-272` (`variants`), `src/brush.rs:113-138` (`shaped_tip`),
`src/abr.rs:47-50` (`default_jitter` returns 1.0 for any tip within a 0.7 to 1.43 aspect ratio),
`src/ui/brushes.rs:195` (picking a preset copies its jitter into the brush).

```rust
let turns = if settings.angle_jitter > 0.0 { 12usize.div_ceil(frames + 1).max(1) } else { 1 };
for frame in 0..=frames { for i in 0..turns { ... let (s, px) = shaped_tip(grid_diameter, &turned); out.push(...) } }
```

Most sampled tips are roughly square, so `default_jitter` gives them 1.0 and the variant table is always built.
Each `shaped_tip` call clones the preset's pixels, creates an A8 source surface and an A8 output surface of
`ceil(hypot(sw, sh))` on a side, and paints a rotated, filtered scale.

Worked case: a 2048 x 2048 tip at diameter 2000 on a 1:1 layer. `size` is 2829, so one variant retains 8.0 MB and
transiently allocates about 16 MB more; twelve variants retain roughly 96 MB and touch 200 MB, with twelve
full 8-megapixel filtered rotations. That runs synchronously inside `Stroke::new`, that is, between button-down
and the first dab, on **every** stroke. Even a 512 px tip at diameter 500 costs twelve 500 KB rotations per
mouse-down. Nothing caches variants across strokes, and they are rebuilt identically when only the position
changed.

The reference has no equivalent (its tips are procedural), so there is no spec cover for the cost.

**Confidence:** confirmed by tracing; sizes computed from the code, not measured at runtime.

---

## 10. [P2] The provisional tail copies and recomposes whole tiles on every motion event

**Locations:** `src/brush.rs:333-341` (`draw_tail` clones `tile.coverage` for every reached tile),
`src/brush.rs:343-355` (`remove_tail` sets `tile.dirty = Some((0, 0, tile.w, tile.h))`),
`src/brush.rs:472-589` (`publish` recomposes the whole dirty rectangle),
`src/render/mod.rs:302-315` (`preview_changed` then re-halves that union at six levels).

`keys_reached` covers the segment inflated by `diameter / 2 + 2`. For a 2000 px brush at 100 percent zoom that is
a roughly 2100 x 2100 grid region, about 81 tiles of 256 x 256. Per motion event that is 81 x 65536 = 5.3 MB of
coverage cloned for the backup, then 81 whole tiles marked dirty and recomposed, which is 81 x 65536 x 4 = 21 MB
of per-pixel compose work, plus six levels of halving over the union, for a segment that may have moved three
pixels. At 60 motion events a second that is well over a gigabyte a second of pointless work.

The same shape is in the reference (`tailBackup[key] = coverage[key]?.makeImage()`, `dirtyTiles[key] = local`),
but the reference reaches it only when the Metal path is unavailable and documents the trade-off in
`reference/docs/brush-performance.md`; the port has no GPU coverage path, so this is always the live path.
The cheap correction is to restrict the restored dirty rectangle to the tail's own bounds rather than the whole
tile.

**Confidence:** confirmed by tracing.

---

## 11. [P2] The preset picker re-decodes every visible tip at full size on every draw

**Location:** `src/ui/brushes.rs:77-115` (`thumbnail`'s `set_draw_func` closure),
called from `src/ui/brushes.rs:190-192` for every entry in the grid.

```rust
let stride = cairo::Format::A8.stride_for_width(p.width as u32).unwrap_or(p.width as i32) as usize;
let mut data = vec![0u8; stride * p.height];
for y in 0..p.height { data[...].copy_from_slice(&p.pixels[...]); }
if let Ok(mask) = crate::raster::a8_from_data(p.width as i32, p.height as i32, data, stride as i32) { ... }
```

This runs inside the draw callback, so the full-size A8 buffer is reallocated, copied and wrapped in a new Cairo
surface on every frame, for every thumbnail on screen, and is then scaled down to 44 pixels with the default
filter. A set of 2048 x 2048 tips costs 4.2 MB of allocation and copying per thumbnail per frame; with two dozen
visible that is roughly 100 MB of churn per redraw and a several-megapixel downscale each. Scrolling the picker
after loading a large `.abr` set is the worst case. A once-per-preset 44 px cached surface removes it entirely.

Related, at the same place: `presets()` retains every decoded tip for the session at up to 2048 x 2048 (4 MB
each). `src/abr.rs:63` charges a 400-megapixel budget **per file**, so loading several large sets has no
aggregate ceiling, unlike the pattern path which was given one in `REVIEW_RESULTS_7.md` finding 2.

**Confidence:** confirmed by tracing.

---

## 12. [P2] Choosing a round tip keeps the previous preset's spacing, and the options bar never shows it

**Locations:** `src/ui/brushes.rs:194-199` (the picker's click handler),
`src/ui/tools.rs:371` (the picker's `changed` callback only re-syncs hardness),
`src/ui/tools.rs:386-390` (the Spacing field), `src/ui/tools.rs:657-667` (`sync_brush`, which does handle it).

```rust
match &preset {
    None => { d.brush.hardness = hardness; d.brush.angle_jitter = 0.0; }
    Some(p) => { d.brush.spacing = Some(p.spacing / 100.0); d.brush.angle_jitter = p.jitter; }
}
```

**Sequence:** pick a textured preset whose saved spacing is 100 percent, then pick Hard Round.

The `None` arm resets hardness and jitter but not `spacing`, so the round tip inherits `Some(1.0)` and paints a
string of separated dots instead of a solid mark. The Spacing field still reads 0 ("automatic"), because the
picker's `changed` callback only pushes `hardness` back into the UI; `sync_brush`, which would fix all of the
fields, is called only from `sync_brush_options` and the number keys. The user sees "spacing 0" and a dotted
stroke with no way to connect the two.

**Confidence:** confirmed by tracing.

---

## 13. [P3] Setting Angle to anything nonzero silently changes accumulation and default spacing

**Locations:** `src/brush.rs:51` (`shaped()`), `src/brush.rs:273-276` (default spacing),
`src/brush.rs:433` (`hard`). Spec: `reference/.../BrushStroke.swift:406`.

`shaped()` is true as soon as `angle % 360.0 != 0.0`, and it is used for two unrelated decisions: the default
spacing drops from 1.5 percent to 2.5 percent, and coverage accumulation switches from screen to max. So nudging
the Angle spin button by one degree on a plain hard round tip, which changes the mark by nothing visible, also
changes how overlapping dabs combine and how far apart they are laid. The reference's `spacingFraction` looks only
at hardness. The max-versus-screen choice for shaped tips is documented and deliberate at `src/brush.rs:431-432`;
tying it to a 1-degree angle is the part that surprises.

**Confidence:** confirmed by tracing.

---

## 14. [P3] Dabs are clipped to the canvas plus a full diameter, so strokes at the edge grow the layer past the canvas

**Location:** `src/brush.rs:420`. Spec: `reference/.../BrushStroke.swift:683-694`.

```rust
if point.0 < -self.settings.diameter || ... || point.1 > self.canvas.1 + self.settings.diameter { return; }
```

The reference clips each dab to the canvas twice, first as `circle.intersection(canvas)` and then as
`(blit ?? clipped.applying(inverse)).intersection(pixelCanvas)`, so no paint ever lands outside the document. The
port only rejects dab *centres* more than a diameter outside, and never clips the stamped box. Painting along the
edge therefore writes up to a radius of paint outside the canvas, `committed_bounds` keeps it (alpha is nonzero
there), and the layer grows by up to `GRID_ALIGN` beyond the document for pixels nobody can see, in every
saved file.

**Confidence:** confirmed by tracing.

---

## 15. [P3] Spot Healing can allocate the whole grid for a long stroke

**Location:** `src/brush.rs:602-611`.

```rust
let reach = (((px1 - px0).max(py1 - py0) + 32) as f64 * 3.2).ceil() as usize;
```

The reach is derived from the painted box's longest side, so a 1000 px heal stroke expands the worked region by
3300 pixels on each side, clamped only by the grid. On a 6000 x 4000 layer that is the whole layer: 96 MB for
`pixels`, 24 MB for `coverage`, plus whatever `spot_heal` allocates internally, and a patch search over 24
megapixels. The formula matches the reference exactly (`BrushStroke.swift:774`), so this is inherited rather than
introduced, but the reference caps the stroke earlier with `stroke.pixelLimit = 100_000_000 - used`
(`EditorSession+Brush.swift:16-25`), which the port does not port: `Stroke::allocate` (`src/brush.rs:459-469`)
has no per-document pixel budget at all, only the one-time grid bound at `src/brush.rs:231`.

**Confidence:** confirmed by tracing.

---

## 16. [P3] `begin_stroke` does not refuse while a warp is running

**Locations:** `src/document.rs:1722` and `src/document.rs:2436` check only `self.stroke.is_some()`, while
`begin_warp` at `src/document.rs:1641` correctly checks `stroke_active()`.

If a stroke is started while `self.warp` is Some, `continue_stroke` routes to `continue_warp`
(`src/document.rs:1758`) and `finish_stroke` routes to `finish_warp` (`src/document.rs:1782`), which overwrites
`self.stroke` at line 1684 with its own clone stroke. The started stroke's paint is dropped and the warp is
committed early. This is currently hard to reach, because the pointer grab keeps a second drag from starting and
the agent is blocked by `busy_editing()`, but `stroke_path` (`src/document.rs:1418`, the "Stroke with Brush"
context action) reaches `begin_stroke` from a menu. Mirroring the `stroke_active()` check into both begin
functions closes it.

**Confidence:** plausible (the mishandling is confirmed; the trigger requires a menu action to fire during a warp
drag).

---

## 17. [P3] Tablet pressure is not read at all

**Locations:** no `pressure`, `AxisUse` or device-tool reference anywhere under `src/`;
`src/ui/canvas.rs:487-583` uses `GestureDrag` offsets only.

There is no pressure support, so the "pressure 0 at the first event" case cannot occur. The reference has none
either (`grep -r pressure reference/Compositor` is empty), so this matches the spec; noted because it was asked
about and because the stroke path also does not use `gdk` motion-event history, so a fast flick on a 240 Hz
tablet is sampled only as often as GTK delivers frames and the Catmull-Rom smoothing has to carry it.

**Confidence:** confirmed by search.

---

## 18. [P3] Smaller divergences and rough edges

- **Eraser on a mask always paints black.** `src/ui/canvas.rs:1528` computes
  `white = d.mask_paint_white && d.tool == Tool::Brush`, so the Eraser on a mask ignores the Black/White choice.
  The reference sets the mask tone from `maskPaintWhite` for every tool and clears `erasing` on a mask
  (`EditorSession+Brush.swift:50-53`).
- **Agent opacity floor.** `src/ui/agent.rs:456` clamps opacity to `0.0..1.0` but `src/brush.rs:220` requires
  `0.01..=1.0`, so `opacity: 0` answers with the internal "brush settings out of range" rather than a clear
  message.
- **Tip lookup by substring.** `src/ui/agent.rs:460` tries an exact case-insensitive name and then the first
  substring match in list order. Two sets each containing a "Round Point" give the earlier-loaded one with no
  ambiguity reported. The exact-first ordering is right; a note in the error when several match would help.
- **Duplicate entries in `brushes.list`.** `src/ui/brushes.rs:65-72`: the `remember` branch de-duplicates
  `FILES`, the `else` branch pushes unconditionally, so auto-discovered files accumulate and are then written
  out wholesale the next time the user loads a file by hand.
- **The brush cursor ignores the layer's rotation.** `src/ui/canvas.rs:2212-2227` draws the tip's ellipse at the
  brush angle in view space; on a rotated layer the painted mark is turned with the layer and the cursor is not.
- **`self.dirty` is not marked for the first dab or the final flush.** `mark_dirty_grid`
  (`src/document.rs:1633-1637`) is called only from `continue_stroke`. `begin_stroke` and `finish_stroke_named`
  change the preview without marking, so a partial redraw can be clipped to a stale rectangle
  (`src/ui/canvas.rs:186-192`). Both cases are visually near-identical to what is already on screen, so the
  effect is a missing first dab until the next motion event.
- **Extreme aspect-ratio tips paint almost nothing.** A 2048 x 1 tip at diameter 40 gives
  `sh = 40 / 2048 = 0.02`, so `shaped_tip` (`src/brush.rs:120-136`) resolves to about 2 percent coverage with no
  message. `shrink_tip` (`src/abr.rs:197-213`) cannot produce a zero dimension (`div_ceil` keeps at least 1 and
  `read_plane` rejects non-positive rectangles at `src/abr.rs:146`), so that case is safe.

---

## Checked and found sound

- **Dab placement.** `tip` centres the disc at `size / 2` and `dab` places the box at
  `round(cx - size / 2)`, so the disc centre lands within half a pixel of the sample with no systematic bias.
  Even diameters land on a pixel corner and odd ones on a pixel centre, which is correct for both.
- **Tip indexing.** `gx0/gy0/gx1/gy1` in `src/brush.rs:428-429` are clamped so `ty_` and `tx_` are always inside
  `0..tip_size`; no path indexes past the tip or the tile buffers.
- **Premultiplied compositing.** `Kind::Paint` writes an opaque BGRA source through `src * a + base * (1 - a)`,
  which is correct premultiplied source-over for Cairo ARGB32; the channel order (`color[2]` into `o[0]`) is
  right. `Kind::Erase` scales all four channels, which is correct destination-out. Dodge/Burn/Sponge
  un-premultiply by `base[3]`, work on straight colour, and re-premultiply, guarding `alpha <= 0`.
- **Opacity model.** Coverage accumulates per stroke in the tile buffers and is composed once against the
  pre-stroke `base` with `a = cov * opacity`, so overlapping dabs cap at the stroke opacity as in Photoshop.
  This matches the reference exactly (`BrushStroke.swift:136-138, 460, 483`). Neither app has a separate flow
  control, so there is nothing to diverge on.
- **Accumulation mode.** Screen for soft round tips, max for hard ones, matching the reference's `.screen` and
  `.lighten` blend modes; the screen arithmetic `current + t - (current * t + 127) / 255` is correct with
  rounding.
- **`Sample::pixel`.** The non-tiled branch rejects negatives and out-of-range before indexing; the tiled branch
  uses `rem_euclid`, so negative document coordinates (the grid left of the canvas) wrap correctly.
- **Pattern origin.** The pattern is sampled at the grid pixel's own document position with a zero offset
  (`src/brush.rs:500-511`, `src/document.rs:1739-1744`), so tiles are anchored to the document and do not slide
  with the stroke.
- **Clone running off the source.** An out-of-range sample returns `[0; 4]`; with `replaces: false` the
  premultiplied source-over leaves `base` untouched, so nothing is erased (the `REVIEW_RESULTS.md` finding 6 fix
  is intact).
- **Sponge on grey.** `l == v` makes `out = l` for both directions, so grey is unchanged, which is correct.
  Dodge and Burn at luma 0 and 1 stay in range through the `clamp` on `k` and the output clamps.
- **Selections.** `Selection::coverage_on_grid` (`src/selection.rs:137-154`) builds the pattern matrix as
  `translate(tile) * to_document`, which maps a rotated or scaled layer correctly, and the coverage is multiplied
  per pixel in `publish`, so a feathered edge is honoured per dab rather than clipped at the end.
- **Growing past the layer's edge.** The grid is the layer's pixels unioned with the canvas, aligned to
  `GRID_ALIGN`, with a hard refusal past 30,000 on a side or 100 megapixels (`src/brush.rs:223-235`), and
  `committed_bounds` snaps outward to the same alignment so the halvings can be cropped. The layer grows, and
  `finish_stroke_named` carries the mask onto the new grid (white where it grew).
- **Mask painting.** `begin_mask_stroke_kind` refuses when there is no mask and when the mask is disabled
  (`src/document.rs:2439-2440`), paints into an ARGB grid whose grey is extracted from the B channel
  (`sync_mask_preview`), and uses the mask's own placement as the grid when it has one, matching the reference's
  `placedMask` handling. `grow` is false for masks, as in the reference.
- **Jitter determinism.** `draw_tail` saves and restores `self.dabs` alongside the path state
  (`src/brush.rs:338-340`), so live painting and replay pick the same variants; the `REVIEW_RESULTS_3.md`
  finding 7 fix is intact.
- **Keyboard focus.** `src/ui/mod.rs:392-398` returns `Proceed` whenever the focused widget is a `gtk::Editable`
  or `gtk::Text`, which covers a `SpinButton`'s inner text, so typing "2000" into Size and pressing B does not
  switch tools. `brush_key` also refuses while a stroke is live (`src/ui/canvas.rs:1658`), so `[` and `]` cannot
  change a diameter that is already baked into the grid tip.
- **Buttons and Space.** `drag_begin` (`src/ui/canvas.rs:493-522`) starts a stroke only for button 1; button 2
  and Space-plus-button-1 pan, and button 3 falls through to `EventSequenceState::Denied`, so the right-button
  brush popover does not also paint.
- **Tabs.** Each page owns its own `Canvas` and `Doc` (`src/ui/mod.rs:1054`), so a tab switch or close cannot
  leave one canvas driving another document's stroke.
- **The C core.** `csrc/BrushPixels.c` touches only index 3 for alpha and is channel-order agnostic;
  `csrc/HealPixels.c` does no luminance weighting (its patch cost is symmetric per channel), so passing Cairo's
  BGRA to `spot_heal` without the boundary swap described in `CLAUDE.md` is correct here.
- **Agent stroke and history.** `src/ui/agent.rs:267` refuses every mutating tool while `busy_editing()`, so the
  `REVIEW_RESULTS_7.md` finding 1 fix (a landing result meeting a live stroke) is intact; nested `begin_edit`
  from `finish_stroke_named` folds into the agent's outer edit (`src/history.rs:52-55`), giving one undo step.
