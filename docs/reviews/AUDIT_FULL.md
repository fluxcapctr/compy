# Whole-project audit, 2026-09-20

Seven Claude Opus 5 reviewers each took one subsystem of the whole project (about 28,000 lines of
Rust), looking for bugs in the interactions between features that the eight incremental Astra rounds
never covered. Their reports are in `docs/reviews/audit/` (document, formats, pixels, renderer, painting, ui,
assistant): 136 findings in all. Every P1 and nearly every P2 is fixed; the rest are marked below with
the reason. Tests: 188 passed, 2 ignored (network, model download).

## Document model and history (audit/document.md)

| # | Finding | Status |
|---|---------|--------|
| 1 | A refused canvas resize leaves the history open for good | Fixed. Nested edits keep a snapshot each (`History::pending` is a stack), `Document::edited` runs a closure as an edit and abandons it on failure, and Generative Expand, Crop, Layer Via, Canvas Size, Rotate Canvas, Flip Canvas and the stroke commit all use it or unwind. Test. |
| 2 | A layer sized to 300,000 px makes Fill allocate gigabytes | Fixed. Transform sides cap at 30,000, and every surface constructor refuses more than 400 megapixels before allocating. Test. |
| 3 | A pixel drag survives an undo that removes its layer | Fixed. `busy_editing` includes the drag; undo and redo refuse while any live preview holds the document; the UI's undo, redo and Delete wait for a drag to end; drag handlers check the layer still exists. Test. |
| 4 | Crop, Canvas Size, Image Size and Flip leave artboard frames | Fixed. `map_artboards` moves, scales or mirrors every frame, and guides go with them. Test. |
| 5 | A board made past the canvas edge lands away from its layers | Fixed. `make_room_for` returns the shift and the new frame takes it. Test. |
| 6 | A Layer Style preview leaks into other undo entries | Fixed. Menu commands wait while a stroke, drag or Layer Style preview holds the document (`preview_busy`). |
| 7 | A landing generation nests inside an open edit | Already deferred by `_poll` while the document is busy; the landing now also checks the canvas size it was made for. |
| 8 | Guides do not follow the canvas | Fixed for Canvas Size, Image Size, Flip and quarter turns; other angles clear them. Guides are now saved in the manifest too. |
| 9 | Matte cache keyed by a reusable pointer | Fixed. The cache holds the surface it was made from. |
| 10 | A clip stranded in another folder after a move | Fixed. A clipped layer whose base ends up elsewhere is released. |
| 11 | `move_layers` indexes unchecked ids | Fixed. Ids and the anchor are checked first. |
| 12 | Undo leaves the mask target stale | Fixed for the mask target. The multi-selection after an undo is left as it was. |
| 13 | Image Size drops type and shape editability | Fixed. The records come along, scaled. |
| 14 | Fill Path and Define Pattern ignore the selection | Left as designed: a path fill is its own area, and Define Pattern takes the selection's rectangle as documented. |
| 15 | A nested abort does nothing | Fixed with finding 1. Unit test. |

## File formats (audit/formats.md)

| # | Finding | Status |
|---|---------|--------|
| 1 | Nested descriptor lists overflow the stack | Fixed. Lists count toward the depth. Unit test. |
| 2 | A .comp save on a full disk reports success | Fixed. PNGs are encoded in memory, then written, flushed and synced; any error fails the save before the swap. |
| 3 | A malformed PSD length wraps in debug builds | Fixed. Resource, layer-mask and extra-data ends are clamped to the file. |
| 4 | PSD clipping bases keyed by depth | Fixed. One base slot per open folder. |
| 5 | Dropped or pasted images lose EXIF orientation | Fixed. |
| 6 | A file with new adjustment or Path shapes breaks on the Mac | Fixed by declaring version 8 for such files, so the Mac refuses them with its version message; plain files stay version 7. Test. |
| 7 | PSD export of artboards contradicts the merged image | A warning now names each board written as a plain folder. Photoshop's own artboard blocks are not written. |
| 8 | ZIP-compressed PSD channels refused | Fixed. ZIP and ZIP-with-prediction channels are read (8, 16 and 32 bit). |
| 9 | Imported images open at 72 ppi | Fixed. PNG pHYs and JFIF density are read. Test. |
| 10 | Pattern saves overwrite and are not atomic | Fixed. Staged and renamed; the CLI import keeps names apart. |
| 11 | Export names collide with earlier runs | Fixed. `unique_file` checks the disk too. |
| 12 | A missing image reports "exceeds the limits" | Fixed. Test updated. |
| 13 | GIMP tips keep 50 megapixels resident | Fixed. They shrink like Photoshop tips. |
| 14 | GIMP version 1 accepted then rejected | Fixed with a clear message. |
| 15 | PackBits tips allocated from a length table | Fixed. The room check includes what PackBits can compress to. |
| 16 | Guides never saved | Fixed. |
| 17 | PSD folder opacity dropped silently | Fixed. A warning. |
| 18 | A recovery from a newer version offered forever | Fixed. Files this build cannot open are not listed. |
| 19 | WebP over 16,383 px fails opaquely | Fixed. A plain message. |
| 20 | File names with no length cap | Fixed. 120 characters. |
| 21 | HEIC plane not rechecked | Fixed. |
| 22 | Shape, text and effects records unvalidated | Effects and text records are clamped on read now; path anchors are bounded by the manifest cap. |
| 23 | avifenc given a path that could read as an option | Fixed. Paths are made absolute. |

## Pixel math (audit/pixels.md)

| # | Finding | Status |
|---|---------|--------|
| 1 | Levels, Curves, Exposure, Brightness/Contrast, Posterize unpremultiply twice | Fixed. The C routine does it once; the extra pass is gone. Test on a half-transparent pixel. |
| 2 | High Pass subtracts premultiplied channels | Fixed. Straight color both sides. Test. |
| 3 | Effect sizes never clamped | Fixed. `Effects::from_record` brings every number into range. Unit test. |
| 4 | Color filters on a mask keep only blue | Fixed. A mask comes back by luminance. |
| 5 | Matte cache pointer reuse | Fixed (document 9). |
| 6 | Radial Blur samples transparency past the layer | Fixed. The edge pixel repeats. |
| 7 | Motion Blur capped at 96 samples | Raised to 400. |
| 8 | Effect colors above 1 | Fixed with 3. |
| 9 | Liquify and Smudge bite off-canvas pixels | Left: the working copy is the canvas, as the reference does it. |
| 10 | Shadows/Highlights reads transparency as black | Fixed. The luma blur is weighted by coverage. |
| 11 | Blend If applies to the shadow and stroke | Left as the port's behavior; noted in the report. |
| 12 | Colorize with a negative hue | Fixed. |
| 13 | Matting guide on premultiplied luminance | Fixed. |
| 14 | Center stroke of width 1 sits outside | Fixed. Half outside, the rest inside. |
| 15 | Distort fades the outer half pixel | Fixed. Neighbours clamp. |
| 16 | Zero-length gradient, text bounds, LUT steps | Zero length gives the start color for every shape; text records are bounded on read. The 1024-step LUT stays. |

## Renderer (audit/renderer.md)

| # | Finding | Status |
|---|---------|--------|
| 1 | Clipping-stack children clipped in document units | Fixed. The clip is set in the group's own pixel space. Test at 2x with a pan. |
| 2 | Adjustment layers wrong on HiDPI | Fixed. The target's device scale and offset are honoured for the readback, the coverage and the write-back. Test. |
| 3 | Double unpremultiply | Fixed (pixels 1). |
| 4 | A stray path on the offscreen context | Fixed. |
| 5 | Mip level one step too deep on HiDPI; offscreen groups at half resolution | Fixed. `device_scale` includes the surface's own scale, and offscreen surfaces hold the target's pixels. |
| 6 | Overlapping artboard backgrounds in the wrong order | Fixed. |
| 7 | Styled effects keep a preview placement | Fixed. |
| 8 | A clipped layer paints over its base's drop shadow | Left; a judgement call noted in the report. |
| 9 | A separately placed mask drawn with no mip level | Left. |
| 10 | Adjustment clipped to a hidden base vanishes | Spec-faithful; left. |
| 11 | Validator caps sides but not the area | Fixed. |
| 12 | Coverage cache keyed by layer only | Left (unreachable through the UI). |
| 13 | Styled buffer peak memory | Left. |

## Painting (audit/painting.md)

| # | Finding | Status |
|---|---------|--------|
| 1 | Undo during a live stroke is reversed by the commit | Fixed. Undo, redo and menu commands wait for the stroke. |
| 2 | A huge coordinate hangs the app | Fixed. Points past ten million pixels are dropped. Test. |
| 3, 4 | A stretched layer paints ovals and leaves a doubled tail | Fixed. The layer is rasterized square before painting (`rasterize_for_painting`). Test. |
| 5 | Painting on a hidden layer | Fixed. Refused. Test. |
| 6 | Clone, Heal and Pattern paint pixels while the mask is targeted | Fixed. Refused with a message. |
| 7 | Paint on a type or shape layer vanishes at the next edit | Fixed. The layer is rasterized first, as its own step. |
| 8 | A failed replay leaves the stroke open | Fixed. Cancelled on error; the shift-click line path too. |
| 9 | Turned tip copies rebuilt every mouse-down | Fixed. The last set is cached by tip and settings. |
| 10 | The provisional tail recomposes whole tiles | Left (performance). |
| 11 | The picker re-decodes every tip per frame | Fixed. One small mask per thumbnail. |
| 12 | A round tip keeps the previous preset's spacing | Fixed. |
| 13 | A one-degree angle changes spacing | Fixed. Spacing follows hardness alone. |
| 14 | Dabs paint past the canvas | Fixed. Dabs clip to the canvas. |
| 15 | Spot Healing reach | Left (inherited from the reference). |
| 16 | A stroke can start during a warp | Fixed. |
| 17 | No tablet pressure | As the reference; left. |
| 18 | Eraser on a mask, agent opacity floor, duplicate brush list entries | Fixed. The cursor on a rotated layer and the first-dab dirty mark are left. |

## User interface (audit/ui.md)

| # | Finding | Status |
|---|---------|--------|
| 1 | Delete or undo mid-drag panics | Fixed. Delete, undo and redo wait; drag handlers end quietly when the layer is gone. |
| 2 | A tool switch mid mask stroke loses the stroke | Fixed. Whatever is in progress finishes first. |
| 3 | New Guide opens nothing | Fixed. |
| 4 | A gradient or shape drag commits after a tool switch | Fixed with 2. |
| 5 | A closed tab leaks its document and timer | Fixed. `Canvas::dispose` removes the timer, the draw function and the controllers when a page closes. |
| 6 | Space stuck after focus leaves | Fixed. |
| 7 | Options-bar fields do not repaint the canvas | Fixed. The bar carries a redraw hook. |
| 8 | The filter dialog follows the active layer | Fixed. The dialog is modal. |
| 9 | Opening a file twice shares one autosave | Fixed. The open tab comes to the front; a copy of an open package gets its own id. |
| 10 | Edit Shape Points leaves the old options bar | Fixed. |
| 11 | Shift+M and Shift+L do nothing | Fixed. |
| 12 | `render` writes before rejecting | Fixed. |
| 13 | A short subcommand launches the GUI | Fixed. Usage instead. |
| 14 | Imports exit 0 with nothing imported | Fixed. |
| 15 | `--filter` misses three kinds and swallows typos | Fixed. |
| 16 | Save As eats a dot in the name | Fixed. |
| 17 | A tiny layer cannot be moved | Fixed. A box under 24 points moves. |
| 18 | Tab hides the panels from any control | Left; Photoshop's behavior. |
| 19 | Escape closes few dialogs | Fixed. Every floating window. |
| 20 | Rebuilds scroll the layer list to the top | Fixed for the scroll position; an in-progress rename is still lost by an unrelated edit. |
| 21 | A cancelled layer drag pins the document | Fixed. |
| 22 | The header mark does not follow a theme change | Fixed. |
| 23 | A trailing comment in colors.toml disables the theme | Fixed; "mode" absent is judged from the background. |
| 24 | Text on the accent in a light theme | Fixed. |
| 25 | Dead condition in the History window | Fixed. |

## The assistant (audit/assistant.md)

| # | Finding | Status |
|---|---------|--------|
| 1 | File tools take any path | Fixed. `agent_path`: absolute (a leading ~ expanded), inside the home folder, outside hidden folders, no "..". Export and save refuse to replace a file unless asked. Unit test. |
| 2 | A finished result thrown away by the deadline | Fixed. A waiting result resets the client's clock; `_cancel` keeps a finished result for landing. |
| 3 | The socket in a shared /tmp | Fixed. Without a runtime directory the socket goes in a 0700 folder under the home. |
| 4 | Document text in the system prompt as instructions | Mitigated. The state is marked as data with an explicit rule, and the file tools are confined. |
| 5 | A big document makes the turn fail to start | Fixed. The prompt's state is capped (150 layers, no tip or pattern lists, 96 KB). |
| 6 | Generative Expand does not repaint; grows for an estimate | Fixed. The estimate leaves the canvas alone; the growth shows at once. |
| 7 | Reads that mutate skip the guards | Fixed. Selecting, saving and stepping the history wait like other changes; undo reports whether anything happened. |
| 8 | A batch of variations outlives the deadline | Fixed. The job announces a budget per variation. |
| 9 | Stopping a turn or quitting leaves the job running | Fixed. Jobs are cancelled; the socket file is removed at exit. |
| 10 | Layers uploaded at full resolution | Fixed. Scaled to the models' side limit. |
| 11 | The catalog asks for an impossible selection | Fixed. `select_layers` selects several. |
| 12 | Numbers as strings become defaults | Fixed. Numbers and flags given as text are read. Unit test. |
| 13 | Dictation stays armed after silence | Fixed. |
| 14 | Two instances racing on the socket | The race remains; the stale file is removed at exit. |
| 15 | A landing after the canvas changed size | Fixed. Refused with a message. |
| 16 | The layer branch lands outside an edit | Fixed. One named step. |
| 17 to 27 | Job ids, status scope, hardcoded costs, raw model ids, dead code, 429 retries, per-connection cap, write timeout, half-close, session id timing, small items | Fixed: raw model ids, dead code, per-frame cap, write timeout, session id after spawn, delete and duplicate on no layer, the accept loop. Left: guessable job ids, status scope, hardcoded costs, 429 retries, half-close, the skill file rewrite, the alpha sent to layer models. |
