# Pixel math and geometry audit (whole project, round 9)

Area: `src/effects.rs`, `src/filters.rs`, `src/blur.rs`, `src/matte.rs`, `src/gradient.rs`, `src/distort.rs`,
`src/warp.rs`, `src/path.rs`, `src/text.rs`, and the pixel callers in `src/document.rs` and `src/render/mod.rs`.
Nothing already listed in `REVIEW_RESULTS.md` through `REVIEW_RESULTS_8.md` is repeated. No file was edited,
no cargo command was run, the app was not started.

No P1 (crash from ordinary use) was found in this area. Finding 5 is the only reachable panic and it needs an
allocator address reuse, so it is filed as P2 plausible.

---

## 1. [P2] Every table adjustment unpremultiplies twice, so soft edges and semi-transparent pixels are barely adjusted

**Locations:** `src/filters.rs:953-959` (`apply_tables`), `src/filters.rs:617-620` (`run`, Levels branch),
against `csrc/LevelsPixels.c:3-16` (`levels_apply`). Users: `src/filters.rs:1200-1233` (Levels, Curves,
Exposure, Brightness/Contrast, Posterize as adjustment layers) and `src/render/mod.rs:792`, which feeds the
composite read back under an adjustment layer.

`levels_apply` already converts to straight color and back: it computes `x = p[c] * 255 / alpha`, looks the
table up at `x`, and writes `round(result * alpha)`. `apply_tables` wraps that call in
`unpremultiply_partial` / `premultiply_partial`, so the color is divided by alpha twice and multiplied by it
twice. The two errors cancel exactly for an identity table (which is why the round trip looks correct) and for
`alpha == 255` (which is what every test uses), and nowhere else. The net mapping is
`straight -> table(straight * 255 / alpha) * alpha / 255`, so the straight output can never exceed the pixel's
own alpha.

**Input that triggers it:** any Curves, Levels, Exposure, Brightness/Contrast or Posterize on a layer with
transparency, or any of those as an adjustment layer over a canvas region that is not fully opaque.

**Traced values** (arithmetic reproduced with the exact integer rounding of the Rust and the C):

| pixel | straight in | correct out | actual out |
| --- | --- | --- | --- |
| Exposure +2 stops, alpha 128 | 128 | 239 | 127.5 (unchanged) |
| Exposure +2 stops, alpha 64 | 64 | 125 | 63.8 |
| a flat white curve, alpha 128 | 128 | 255 | 127.5 |
| a flat white curve, alpha 32 | 32 | 255 | 31.9 |
| Posterize 2 levels, alpha 128 | 64 | 0 | 63.8 (not posterized) |

So an antialiased edge pixel at alpha 32 can never be lifted above straight 32 whatever the curve says: a
Curves or Levels adjustment layer leaves a dark fringe around every cutout, and Exposure on a 50 percent layer
does nothing at all.

**Cross-checks that make this a bug rather than a port of the spec:** `map_straight`
(`src/filters.rs:370-380`), used by Vibrance, Black and White, Photo Filter, Threshold, Selective Color and
Channel Mixer, does the conversion once and is correct, so two conventions coexist in one file. The Swift
reference calls `levels_apply` directly for Curves (`reference/Compositor/Document/Curves.swift:38`) and
Exposure (`reference/Compositor/Document/ImageAdjustments.swift:68`) with no extra pass; only
`LevelsFilter.run` (`reference/Compositor/Document/Levels.swift:76-86`) adds the redundant pass, which is the
reference's own defect. The Rust copied that defect and then spread it to the other four kinds.

**Confidence:** confirmed by tracing the Rust, the C, and the Swift, plus exact integer arithmetic.

## 2. [P2] High Pass subtracts premultiplied channels, so every cutout edge gets a bright halo

**Location:** `src/filters.rs:245-253`.

`high_pass` blurs all four premultiplied channels, then does `p[k] - b[k] + a / 2` on the premultiplied bytes.
Where alpha varies, `b[k]` carries the blurred alpha while `p[k]` carries the pixel's own, so an alpha edge is
read as a color edge. This is the same defect as round 7 finding 12, which was fixed in `sharpen`
(`src/filters.rs:394-396` now converts both sides to straight color) and left here.

**Input that triggers it:** High Pass at any radius on a layer with transparency, for example a uniform gray
128 opaque region beside transparent pixels.

**What goes wrong:** at an edge pixel with `a = 255` whose neighbourhood is half transparent, `b[k]` is about
64 while `p[k]` is 128, giving `128 - 64 + 127 = 191` instead of the neutral 127 the uniform color deserves.
The result is a bright rim along every cutout, which then propagates into any frequency-separation or
Overlay-sharpen workflow built on it.

**Confidence:** confirmed by tracing.

## 3. [P2] An effect size is never clamped, so one layer style can freeze the app (and overflow the blur's accumulator)

**Locations:** `src/effects.rs:330-334` (`blur`, sigma is `size / 2` with no bound), `src/blur.rs:33` and
`src/blur.rs:65` (the `(-r..=r)` priming sums, accumulated in a `u32`), entry points
`src/ui/agent.rs:406-411` (the `layer_style` tool checks only `is_number`) and `src/format/validate.rs`
(`layer.effects` is the one layer field with no validation at all; `Effects::from_record` at
`src/effects.rs:163` is plain serde).

**Input that triggers it:** `layer_style {"drop_shadow": {"size": 20000000}}` from the assistant tool, or the
same value hand written into a `.comp` manifest. The Layer Style dialog clamps size to 0 to 250
(`src/ui/effects.rs:178-197`), so only the tool and the file reach this.

**What goes wrong:** `box_radii` returns radii of about `size / 2`, and each row (and each column) primes its
running sum with `2r + 1` reads. At size 2e7 that is 1e7 iterations per row per pass, six passes, which is a
hang with no progress and no cancel; the 1x1 probe at `src/render/mod.rs:458` runs the same blur, so the
freeze happens before any buffer is even allocated, and reopening the saved file freezes again. Above
`r = 8.4e6` the `u32` sum also overflows: release builds wrap and produce garbage coverage, debug builds
(including `cargo test`) panic with "attempt to add with overflow". `reach()` is clamped to 4000
(`src/effects.rs:184`) so the buffer stays bounded, but the sigma it blurs with is not.

**Confidence:** confirmed by tracing; not executed (the app was not run).

## 4. [P2] Filters that produce color are allowed on a mask, and only the blue channel survives

**Locations:** `src/document.rs:326-341` (`filtered_mask` refuses only `needs_selection` kinds),
`src/document.rs:3601-3608` (`gray_from_a8`), `src/document.rs:3611-3618` (`a8_from_gray` keeps byte 0, the
blue channel).

**Input that triggers it:** click a mask thumbnail so the mask is the target, then run Image > Gradient Map,
Image > Photo Filter, Hue/Saturation with Colorize, or Levels/Curves on a single channel.

**What goes wrong:** the mask is expanded to opaque gray, the filter colors it, and only blue is read back.
Photo Filter's default warm color has blue 0, so `PhotoFilter::pixel` (`src/filters.rs:336-346`) scales blue by
`1 - d` and the whole mask darkens by about 25 percent for no visible reason, with the preserve-luminosity pass
making the amount depend on the gray. A red to white Gradient Map turns the dark half of the mask to 0 and the
light half to 255, which is a contrast crush rather than a gradient map. A red-channel-only Levels change does
nothing at all, while a blue-channel-only change rewrites the whole mask. Photoshop offers only the
channel-safe adjustments on a mask.

**Confidence:** confirmed by tracing.

## 5. [P2, plausible] The Remove Background cache is keyed by a raw surface pointer and can index a stale mask out of bounds

**Locations:** `src/document.rs:37` and `src/document.rs:507-512` (the key is `image.to_raw_none() as usize`,
and the cache is never cleared on edit, undo or close), used at `src/document.rs:522` and
`src/document.rs:525`.

**Input that triggers it:** run Remove Background on a layer, then change that layer so its surface is freed
and a new, larger surface is allocated (paint past the layer's edge so it grows, or Image Size, or undo and
redo), then run Remove Background again while the new surface happens to land on the freed address.

**What goes wrong:** the cached `Vec<f32>` still has the old `w * h` length, while `levels[y * w + x]` and
`a8_from_levels(&levels, w, h)` index with the new size. A larger new image panics with an index out of
bounds; a smaller one silently applies a mask sampled on the wrong grid. `src/render/mod.rs:462` uses the same
pointer-as-key trick for the styled cache, where the consequence is only a stale style.

**Confidence:** plausible. The out-of-bounds arithmetic is certain; the address reuse was not forced.

## 6. [P3] Radial Blur reads transparency past the layer and its margin ignores the amount

**Locations:** `src/filters.rs:213-218` (`fetch` returns `[0; 4]` outside), `src/filters.rs:32`
(`Kind::RadialBlur => 8`, a constant margin).

**Input that triggers it:** Radial Blur, spin or zoom, at any amount on a layer that fills its own bounds (an
imported photo).

**What goes wrong:** every sample that rotates or zooms off the layer contributes transparent black, so the
average is pulled toward transparent. The corners and edges of the image fade and darken in a swirl, worst at
the corners where the rotation arc leaves the rectangle soonest. The 8-pixel margin the filter asks for is
unrelated to the reach of a spin of `amount` degrees at radius `r` (about `r * amount * pi / 180` pixels) and
only adds more transparency to sample. Photoshop clamps to the edge pixel here. The sampling is also nearest
neighbour (`x.round()`), so the streaks are stepped at large radii.

**Confidence:** confirmed by tracing.

## 7. [P3] Motion Blur is capped at 96 samples, so a long streak becomes a row of ghosts

**Location:** `src/filters.rs:683`, `samples = (distance.ceil() as usize).clamp(2, 96)`.

**Input that triggers it:** Motion Blur with distance above about 100 pixels (the setting allows 2000).

**What goes wrong:** the samples are spread evenly over the whole streak, so at distance 2000 they sit 20.8
pixels apart. A point of light becomes 96 separate copies rather than a continuous smear, and hard edges show
as repeated ghost edges. The direction itself is fine: the streak is symmetric about the pixel (`t` runs from
-0.5 to 0.5), so the sign convention of `(cos, -sin)` cannot be observed at all.

**Confidence:** confirmed by tracing.

## 8. [P3] An effect color above 1 writes a premultiplied pixel whose channel exceeds its alpha

**Locations:** `src/effects.rs:364-372` (`paint_one`) and `src/effects.rs:374-386` (`paint`); no clamp on
`Shadow.color`, `Glow.color`, `Stroke.color`, `Overlay.color` anywhere in `from_record`
(`src/effects.rs:163`) or `src/format/validate.rs`.

**Input that triggers it:** a `.comp` manifest with `"colorOverlay": {"color": [4.0, 0, 0], "opacity": 0.5}`.
The agent tool is safe here because it parses `#rrggbb`, and the dialog uses a color picker.

**What goes wrong:** `paint` computes `p[2] = min(255, r * c * 255)` but `p[3] = c * 255`, so the channel is
clamped to 255 while the alpha stays at 128. Cairo then composites an invalid premultiplied pixel, which shows
as an overbright, hue-shifted block rather than a red overlay. The gradient overlay is not affected because it
goes through `Gradient::normalized`, which clamps colors (`src/gradient.rs:42`).

**Confidence:** confirmed by tracing.

## 9. [P3] Liquify and Smudge at the canvas edge erase the layer's off-canvas pixels

**Locations:** `src/document.rs:1644` (the working copy is `sample(false)`, clipped to the canvas),
`src/document.rs:1665-1686` (`finish_warp` stamps it back with `Kind::Clone { replaces: true }`),
`src/brush.rs:68` (`Sample::pixel` returns `[0; 4]` outside the document) and `src/brush.rs:506` (`replaces`
copies that alpha in).

**Input that triggers it:** a layer larger than the canvas (or moved so part hangs off), then a Liquify or
Smudge stroke along that edge.

**What goes wrong:** the committing stamp covers a disc of `diameter + 4` around every stroke point, so it
reaches past the canvas boundary. There the sample is transparent and `replaces` blends toward it, cutting a
soft bite out of the layer's hidden pixels. The pixels are invisible at the time, so the damage is only noticed
after a canvas resize or a move; it is one undo step, so it is recoverable if caught.

**Confidence:** confirmed by tracing.

## 10. [P3] Shadows/Highlights treats transparent pixels as black when it blurs the luma

**Location:** `src/filters.rs:126-127`.

`luma[i]` is set to 0 wherever alpha is 0, and that map is then blurred by `radius / 2`. Inside a cutout edge
the neighbourhood therefore reads darker than it is, `lift` (line 133) grows, and a bright halo runs along the
inside of every transparent boundary. The fix shape is the same as a premultiplied-aware blur: weight the luma
by coverage and divide by the blurred coverage.

**Input that triggers it:** Shadows/Highlights with a large radius on a cut-out subject.

**Confidence:** confirmed by tracing.

## 11. [P3] Blend If is applied to the drop shadow and the stroke, judged by their own tones

**Locations:** `src/render/mod.rs:883-903` (the group holds `below`, the layer and `above` before
`apply_blend_if` runs) and `src/render/mod.rs:913-960` (the luma is read from that group).

**Input that triggers it:** a layer with a black drop shadow and This Layer black raised to 60.

**What goes wrong:** the blend-if mask is computed from the composed group, so the shadow's own dark luma
fails the This Layer test and the shadow disappears along with the dark pixels it was cast from; a white
stroke disappears when This Layer white is lowered. In Photoshop the ranges gate the layer's pixels while its
effects keep being drawn from the layer's alpha. There is no Swift spec to check against: layer effects and
Blend If are Linux-side extensions (the reference has no such file, and `src/psd.rs:499` says as much).

**Confidence:** confirmed by tracing; the intended behavior is a judgement call, so this may be a deliberate
choice worth documenting rather than a defect.

## 12. [P3] Colorize with a negative hue produces the wrong color

**Locations:** `src/filters.rs:874` (`hue = a[0] % 360.0`, no wrap into 0 to 360, unlike the non-colorize
branch at `src/filters.rs:880-881`), consumed by `src/filters.rs:916-923` (`to_rgb`), permitted by
`src/filters.rs:1119` (`record_is_valid` allows hue from -360 to 360).

**Input that triggers it:** a saved Hue/Saturation adjustment whose colorize hue is -30 (accepted by the
validator, written by a Swift-side file or the agent's `hue` argument).

**What goes wrong:** `sector` goes negative, `second` becomes negative, and `sector as i32` truncates toward
zero into the wrong match arm, so -30 does not give the color 330 gives. Everything is clamped afterwards so
there is no invalid pixel, just the wrong hue.

**Confidence:** confirmed by tracing.

## 13. [P3] The matting guide is a premultiplied luminance, so transparent areas read as dark edges

**Location:** `src/matte.rs:180-183` (`gray_levels` divides by 255 but never by alpha), used as the guide in
`src/matte.rs:197` and `src/matte.rs:199`.

On a layer that already has transparency (a previous cut-out, a soft brushed edge) the guide shows an edge
wherever alpha changes even when the color does not, so the guided filter snaps the matte onto that false edge.
Everywhere else in the codebase this conversion divides by alpha first (see `select_color_range` at
`src/document.rs:3350`).

**Confidence:** confirmed by tracing.

## 14. [P3] Edit > Stroke with Center and width 1 paints entirely outside the selection

**Location:** `src/document.rs:3294-3298`.

`half = (w / 2).max(1)` and `inner = current.resized(-(w - half))`. At `w = 1` that is `half = 1` and
`resized(0)`, so the band is one pixel outside the selection and nothing inside it, which is what Outside
draws. Widths 2 and 3 are also asymmetric (more inside than outside). Photoshop straddles the edge.

**Confidence:** confirmed by tracing.

## 15. [P3] Free distort fades the outermost half pixel of every warped layer

**Location:** `src/distort.rs:103-119`; the bilinear `sample` skips out-of-range neighbours instead of
clamping, and the accumulator is not renormalized by the weight that was dropped.

At `u = 0` the source x is -0.5, so half the weight comes from a pixel that does not exist and the result is
half strength. Every distort therefore shaves a soft, half-transparent line off all four edges, and repeated
distorts compound it. The same code path is used for masks (`is_mask`), where the fade is toward black.

**Confidence:** confirmed by tracing.

## 16. [P3] Minor geometry and quantization notes

- `gradient::shape_t` with `start == end` (`src/gradient.rs:138-157`) is inconsistent between shapes:
  `len` becomes 1e-6, so Radial returns 1 everywhere (the end color fills the canvas) while Diamond, Linear and
  Reflected return 0 (the start color fills it) and Angle still sweeps. Reachable from the agent's
  `gradient_fill`, which takes explicit endpoints; the canvas drag cannot produce it.
- The `raster` lookup table has 1024 steps (`src/gradient.rs:165,170`), finer than 8-bit output for a smooth
  ramp but enough to soften a deliberately hard edge (two stops 0.0005 apart) into a ramp a few pixels wide on
  a large Angle or Diamond gradient.
- `Effects::from_record` (`src/effects.rs:163`) is all-or-nothing serde: one mistyped field ("size": "10")
  makes the whole layer style silently vanish from the render while remaining in the file and being written
  back on save.
- `TextStyle::from_record` (`src/text.rs:37`) has no bounds either, so a file carrying `"size": 1e9` reaches
  `set_absolute_size(1e9 * 1024)` and Pango's `int` size field. `render` still refuses the resulting surface at
  `src/text.rs:97`, so the visible effect is an error rather than a bad allocation.
- A layer style on a very large layer allocates up to three `w * h * 4` buffers plus their Cairo copies under
  the 120-megapixel cap at `src/render/mod.rs:471`, which is about 1.4 GB of transient allocation for a
  10000 by 10000 layer.

---

## Checked and found sound

- **Premultiplication in the C core.** `adjust_gradient_map` (`csrc/AdjustPixels.c:4-26`), `adjust_grain`
  (`:47-87`) and `noise_add` (`csrc/NoisePixels.c:17-42`) all unpremultiply, work on straight color, and
  re-premultiply by the pixel's own alpha, so none of them can emit a channel above its alpha. The red/blue
  swaps around them in `filters::run` and `Adjustment::apply` pair up correctly.
- **`map_straight` (`src/filters.rs:370-380`) and the Hue/Saturation cube path (`:1242-1250`)** round-trip
  premultiplication exactly and cannot produce a channel above alpha (`(255 * a + 127) / 255 == a`).
- **`Kind::Invert`** (`src/filters.rs:636`) is the correct premultiplied inversion, and it is also correct on
  the opaque gray a mask is expanded into.
- **The selection blend at the end of `run`** (`src/filters.rs:659-673`) mixes two premultiplied pixels
  linearly, which preserves the premultiplied invariant.
- **`effects::paint` and `paint_one`** write valid premultiplied ARGB32 for any color inside 0 to 1, and the
  gradient and pattern overlays hand them straight color plus a coverage, which is the right contract.
- **`Effects::reach`** covers everything that actually paints: the drop shadow needs `distance + 3 * size` and
  the blur reaches `1.5 * size`; the outside and center strokes need `size + 1` and get `size + 2`; outer bevel
  and emboss need about `1.5 * size` and get `2 * size`; the interior effects need nothing. `coverage_box` on an
  all-zero alpha returns None and the fallback box is never used, because every overlay skips pixels with no
  coverage.
- **The `Atop` confinement of the interior effects** (`src/render/mod.rs:884-898`) is correct at antialiased
  edges: the overlay carries full coverage and the layer's own alpha limits it, so edges recolor without
  thickening. The push/pop group pairs in `gradient_fill` and `fill_pattern` are also correct, because
  `push_group` saves the graphics state and `pop_group` restores it, so the pattern matrix matches the CTM.
- **Blur edges** clamp to the edge pixel in both passes (`src/blur.rs:32`, `src/blur.rs:64`), so there is no
  edge darkening, and `Kind::margin` gives Gaussian Blur, Unsharp Mask, Smart Sharpen and High Pass a
  transparent margin at least as wide as `blur::reach`. `LazyBlur::blurred` uses that same reach as its tile
  margin, so tile seams match a blur of the whole.
- **Motion Blur direction** matches the dialog: the streak is symmetric, so only the axis is observable, and
  `(cos, -sin)` is the right axis for degrees counterclockwise from horizontal on a y-down grid.
- **`Range::normalized`** cannot panic in `clamp`: black is clamped to at most 254 before it is used as the
  lower bound for white. `Posterize::table` cannot divide by zero (levels are clamped to 2 or more), and
  `to_hsl` cannot divide by zero (the zero-denominator cases all have delta 0 and return early).
- **`record_is_valid` versus `apply`** for Curves: the validator demands four channels, 2 to 32 points, x
  strictly increasing from 0 to 255, which is exactly what `Curves::value` needs to avoid a zero-width span;
  `from_record` is looser but every file path goes through `src/format/validate.rs:22`, the PSD importer only
  uses `from_kind` defaults (`src/psd.rs:365`), and the curve editor enforces a one-unit gap when adding and
  when dragging (`src/ui/filter_dialog.rs:594,622`), so the NaN path is not reachable. The neighbour clamp
  cannot invert either, for the same reason.
- **NaN and negative-float casts.** Rust's saturating float-to-int casts make `(t * 255.0).round() as usize`,
  `(w * SCALE) as i32` and the `as u32` style and position fields safe; the gradient LUT index is clamped, the
  trilinear cube indices are clamped, and `response[hue.round() as usize]` is bounded by `min(360)`. A NaN
  coverage reaching `paint_one` takes the `min(1.0)` branch and paints at full strength rather than panicking.
- **Allocation limits.** Every `vec![0; w * h]` in this area is behind a cap: 120 megapixels for the styled
  buffer, 100 megapixels and 30000 per side for filtered layers, distorted shapes and rendered text, 4096 per
  side for a defined pattern, 100 megapixels for the Blend If mask.
- **Gradients.** `normalized` guarantees at least two sorted, clamped stops of each kind, so `lerp_stops`
  cannot index an empty slice; `at` interpolates color and opacity separately in straight space, which is the
  Photoshop convention; `raster`'s lookup premultiplies with `round(c * a * 255)` against `round(a * 255)`,
  which cannot exceed the alpha; `reversed` mirrors alpha stops with the color stops; the gradient overlay's
  angle convention (0 left to right, 90 bottom to top) matches its own documentation, and its projection is
  normalized by `|cos| + |sin|` so the ramp reaches both ends of the box at any angle.
- **Paths and text.** `flatten` bounds its step count to 20000 and handles a one-anchor path; coincident
  anchors degenerate to a `line_to` rather than a bad curve; `selection` always closes the path; `caret` and
  `index_at` clamp their byte indices and cannot slice off a UTF-8 boundary (`xy_to_index` returns boundaries,
  and `text[len..]` is empty); empty text and newline-only text still get a line's height; a missing font falls
  back through Pango's font map rather than failing.
- **Selection-based operations.** `fill_with`, `stroke_selection`, `gradient_fill` and `fill_pattern` all map
  the document-space selection onto the layer's pixel grid with `coverage_on_layer`, whose pattern matrix is
  `pixel_to_document`, which is the correct direction for a rotated or flipped layer. `fill_pattern`'s scale
  matrix is `1 / scale` (the correct direction for a pattern matrix) and its tile origin is the document
  origin, as documented. `define_pattern` takes the axis-aligned document bounds of a rotated layer, which is
  the right rectangle, and refuses an empty or oversized one.
- **Liquify and Smudge sampling.** `push` interpolates premultiplied channels bilinearly inside a
  pre-dab copy, which keeps the premultiplied invariant; `smudge` calls `clamp_premultiplied` afterwards; the
  index clamps (`min(cw - 2)`, the `cw > 1` and `ch > 1` guards) hold for a one-pixel band. `distort::warp`
  refuses bow-tie and collapsed corner sets before inverting the homography, and the homography round trips its
  own corners.
