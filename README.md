# Compositor on Linux

A Linux rebuild of [Compositor](https://github.com/robbietilton/Compositor), a layer-based image editor for
macOS. The original app's C pixel core (`csrc/`) is compiled unchanged; the rest is rebuilt in Rust on Cairo,
with the Swift app checked out as a read-only submodule in `reference/` and used as the specification.

Progress follows the phases in the build plan:

| Phase | State |
| --- | --- |
| 1. Read a `.comp` file and render it flat | Done. `compositor render` and `compositor info`. |
| 2. The viewer (GTK4 window, pan, zoom, layer panel) | Done. `compositor project.comp` opens the app. |
| 3. Wire in the C core tool by tool | Done. Wand, Levels, Add Noise, Grain, Lens Correction, Gradient Map, Content-Aware Fill, Heal Selection, undo. |
| 4. Brush and tiled rendering | Done. Brush, Eraser, Spot Healing Brush, Clone Stamp, Blur; live preview at any zoom. |
| 5. Transforms, selections and masks | Done. Move/scale/rotate with snapping, marquee and lasso, masks, clipping masks, mask painting. |
| 6. Files, sizes, adjustment layers, extras | Done, except the GPU brush and ONNX background removal (see below). |

## Build

Needs Rust (stable, edition 2024), a C compiler, and the GTK4 and Cairo development packages (`gtk4` and
`cairo` on Arch; `libgtk-4-dev` on Debian, which pulls in Cairo).

```
git clone --recurse-submodules <this repo>
cargo build --release
cargo test
./install.sh
```

`install.sh` builds the release binary and installs it for the current user only: `~/.local/bin/compositor`,
the app icon under `~/.local/share/icons/hicolor/`, and `~/.local/share/applications/compositor.desktop`,
which is what makes the app show up in launchers (Omarchy's included) and lets `.psd` files open with it
from a file manager. Nothing is written outside your home directory; run it again after pulling changes.

## Use

```
compositor project.comp [more.comp ...]  # open the viewer, one tab per project
compositor info  project.comp            # the layer tree, with what each layer carries
compositor render project.comp out.png   # flatten to a straight-alpha PNG at the document's resolution
compositor psd    project.comp out.psd   # write a layered Photoshop file (or in.psd out.comp to convert back)
compositor file.psd                      # open a Photoshop file in the viewer
```

In the viewer: scroll to pan, Ctrl+scroll or pinch to zoom around the pointer, middle-drag or Space+drag
to pan, Ctrl+0 fits, Ctrl+1 is 100%, Ctrl+plus and Ctrl+minus step the zoom, Ctrl+O opens, Ctrl+W closes
the tab. The layers panel toggles visibility, collapses folders, and sets the selected layer's blend mode
and opacity, redrawing as you go. From 200% the canvas shows hard-edged document pixels; from 800% a
pixel grid. `--screenshot out.png [--zoom 4]` renders the window to a file and quits, for checking the UI
from a script; `COMPOSITOR_TRACE=1` prints each frame's draw time.

**Editing (phase 3).** The rail on the left holds the Magic Wand (W), Hand (H) and Zoom (Z). The wand's
options bar sets New / Add / Subtract (Shift adds, Alt subtracts while clicking), tolerance, point or
averaged sampling, this layer or all layers, and contiguous. The menu (top right) has Edit (Undo Ctrl+Z,
Redo Ctrl+Shift+Z), Select (All Ctrl+A, Deselect Ctrl+D, Inverse Ctrl+Shift+I), Image (Levels Ctrl+L,
Gradient Map, Grain) and Filter (Add Noise, Lens Correction, Content-Aware Fill Shift+F5, Heal Selection).
Filters run on the active image layer inside the selection, preview live on the canvas, and commit as one
undo step. Every pixel operation is the original C code; only byte order and premultiplication are handled
here. `--wand x,y` and `--filter levels` script those for screenshots.

**Painting (phase 4).** Brush (B), Eraser (E), Spot Healing Brush (J), Clone Stamp (S, Alt-click sets the
source) and Smear (R, with Liquify, Blur and Smudge modes: Liquify pushes pixels along the drag, Smudge
drags color, Blur softens) share size, hardness and opacity in the options bar; `[` and `]` step the size, `{`
and `}` the hardness, and the number keys set opacity. Shift-click paints a straight line from where the
last stroke ended. Strokes follow a smoothed curve through the pointer samples, accumulate coverage in
256-pixel tiles with the opacity as a cap on the whole stroke, respect the selection, and commit as one undo
step. Painting past a layer's edge grows the layer. While a stroke is in progress the canvas draws a live
preview grid whose reduced copies are updated only where the stroke touched, so a 300-pixel brush on a
50-megapixel layer costs 10 to 20 ms per pointer step at any zoom and about 12 ms on mouse-up, because the
preview and its reduced copies become the layer's own pixels rather than being copied.

**Transforms, selections and masks (phase 5).** The Move tool (V) drags the active layer, or a whole folder,
with eight scale handles and a rotation handle above the top edge: Shift constrains, Alt scales from the
center, the corner handles keep proportions unless you hold Shift (or turn off Lock ratio), moves snap to
the canvas and to the other layers' edges and centers with a guide drawn along the match (Ctrl drags freely),
Ctrl-click or Auto-select picks the layer under the pointer, arrow keys nudge by 1 or 10 pixels, and the
options bar shows X, Y, W, H and angle you can type into, plus Flip H and Flip V. The Marquee (M) drags a
rectangle or ellipse (Shift squares it), the Lasso (L) draws freehand or, in Polygonal mode, click by click
(click the first corner, or press Return, to close; Backspace removes a corner; Escape cancels); Shift adds
and Alt subtracts with all three selection tools, and dragging inside a selection in New mode moves its
outline. The layers panel shows a mask thumbnail beside a masked layer: click it to paint the mask (the
options bar then offers Black · Hide or White · Reveal), click the layer thumbnail to go back to its pixels,
Ctrl-click a layer thumbnail to load its pixels as a selection, and Alt-click a row to clip it to the layer
below (Alt+G does the same). The Layer menu adds masks (Reveal All, Hide All, or from the selection),
enables, inverts and deletes them, toggles clipping, and flips. Masks follow their layer when linked and stay
put when unlinked, as in the reference.

**Files, sizes and adjustment layers (phase 6).** The File menu has New Canvas (Ctrl+N), Save (Ctrl+S) and
Save As, which write a `.comp` package staged beside the destination and swapped in whole, Import Image
(PNG, JPEG, TIFF, with EXIF orientation applied) as a new layer, Export PNG, and Export JPEG with a quality
slider, a background color, the encoded size and a preview. Dropping a `.comp` on the window opens it and
dropping an image imports it; closing a tab with unsaved changes asks first, and a dot marks unsaved tabs.
Edit has Copy Merged (Ctrl+Shift+C) to the system clipboard. Layer has New Layer, New Folder, Duplicate,
Delete, Rename, Move Up and Move Down, the same buttons in the panel's footer, and New Adjustment Layer
(Hue/Saturation, Levels, Curves, Exposure, Gradient Map, Grain); adjustment layers render exactly as the
reference does, adjusting everything beneath them, clipped by their own mask and the folders around them,
at their opacity and blend mode, and double-clicking one opens its settings with a live preview. Image has
Canvas Size with an anchor and optional fill (which becomes a "Canvas Extension" layer under everything),
Image Size (layers are resampled in place at the new size, rotation baked, masks with them), Crop to
Selection, and Hue/Saturation and Exposure as filters. Select has Expand and Contract, computed with a
Euclidean distance transform so corners round as Photoshop's do.

**Photoshop files.** File has Open PSD (Ctrl+Alt+O) and Export PSD, and a `.psd` dropped on the window or
passed on the command line opens too. The reader and writer are in `src/psd.rs`, with no outside library:
8-bit RGB with layers, folders (nested), layer and folder masks, opacity, the thirteen blend modes, hidden
layers, clipping and the document resolution all carry across. Export bakes each layer's position, scale,
rotation and flips into pixels at its document placement (PSD has no live transforms), so a round trip
keeps the picture but not the ability to un-rotate. Adjustment layers are left out of an export (the
flattened preview still shows their effect) and come in from a PSD as the same kind with default settings,
since Photoshop's settings blocks are not read; either case is reported when it happens. 16-bit and 32-bit
files open at 8 bits; CMYK, Lab, indexed and PSB files are refused with a message. A PSD opens as an
unsaved document that saves as `.comp`.

The Filter menu also has Gaussian Blur and Motion Blur, which give the layer a transparent margin to spread
into and trim the rim they did not reach, so a blurred layer grows a little, as in the reference.

Not built: the GPU brush (the software path meets the phase 4 budget), Remove Background (needs an ONNX
runtime and a bundled model; the mask plumbing it would feed is in place), free distort, multi-layer
selection, a Curves editor (curve points load, save and render, but there is no widget to edit them), the
Gradient, Shape and Eyedropper tools, a Crop tool with handles (Crop to Selection exists), moving pixels
inside a selection, Merge Down, drag-to-reorder in the layer panel, Flip Canvas, and HEIC import.

The tool icons are drawn as line glyphs in the style of the Mac app's SF Symbols, in the theme's text color.

One deliberate deviation: a committed stroke's pixels are cropped to bounds snapped outward to a 64-pixel
alignment, so a layer can carry up to 64 transparent pixels of margin the Mac app would trim. That alignment
is what lets the reduced copies be cropped instead of rebuilt.

One honest gap against the Mac app: Content-Aware Fill works within the layer's own pixel grid and does not
yet extend the layer to cover a selection past its edge.

The canvas keeps its last composited frame and draws only the overlays (cursor, marching ants, transform
box) on top of it, so moving the mouse costs nothing; the frame is rebuilt when the view changes (about
35 ms for the 50-megapixel test document at fit, 85 ms at 100%) and only in the touched region while a
stroke runs (1 to 5 ms per pointer step). Every offscreen buffer is sized to the window, not the document,
and masks draw from the same sharp halvings as pixels. `COMPOSITOR_TRACE=1` prints what each frame did
and how long the rendering took; note that GTK hands the widget a recording context, so only the cached
frame's own timing reflects real rasterizing cost.

## What phase 1 renders

Everything the file format specifies through version 6, plus the version 7 fields the current app writes:

- layers and folders, drawing order, inherited visibility
- transforms: position, size, clockwise rotation, flips, sampling mode
- opacity and all thirteen blend modes, via Cairo's compositing operators
- layer masks, folder masks (pass-through, multiplied into every descendant), disabled masks
- masks moved apart from their layer (`maskPlacement`)
- clipping masks (`maskSourceID`), both as contiguous stacks sharing the base's alpha and as links to a
  non-adjacent source
- sharp downsampling through Lanczos halvings for large reductions
- every validation rule in `ProjectStore.swift`: format, versions, limits, filenames, tree shape, cycle checks


## Layout

```
csrc/        the C pixel core, byte for byte from reference/Compositor/Rendering
src/ffi.rs   bindings and safe wrappers over csrc
src/format/  manifest parsing, validation, package loading
src/png_io   PNG decode to premultiplied ARGB32 and A8; encode with resolution
src/raster   surface helpers and the Lanczos halving
src/render/  the Cairo compositor and clipping-mask logic; draws into any context, export or canvas
src/psd      Photoshop read and write: layer records, PackBits channels, folders, masks, clipping
src/document the editable document: undo history, selection, wand, filters on the active layer
src/selection mask-based selections with traced outlines, boolean combination, coverage on a layer grid
src/filters  the filters and adjustments over the C core, with byte order and selection blending
src/brush    the stroke engine: tips, dab spacing, curve smoothing, tiles, compose, heal, commit bounds
src/transform drag modes (move, eight handles, rotate) with the modifier rules, snapping, hit testing
src/ui/dialogs New Canvas, Canvas Size, Image Size, Expand/Contract, Rename, JPEG export, unsaved prompt
src/blur     Gaussian blur as threaded box passes, and the Blur tool's lazily blurred tiles
src/warp     Smudge and Liquify: the working copy at document size, dabbed and painted back along the stroke
src/history  value-snapshot undo with entry and byte limits
src/viewport the canvas view math (fit, zoom around a point, pan), a port of CanvasViewport
src/ui/      the GTK4 app: window and tabs, canvas widget, layers panel
tests/       fixture-built .comp packages with pixel-exact expectations
reference/   the macOS app, as a submodule, read-only
```

## Verifying against the original

The macOS app needs macOS 26 to build, so there is no machine here that can produce reference renders. The
tests instead construct packages whose flattened result is known analytically (blend math, placements,
rotations, mask coverage). When a Mac with the app is available, save a set of `.comp` files covering every
format feature, export each with Copy Merged, and add them under `tests/fixtures/` as the regression suite.
