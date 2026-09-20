# Review results, round 7

Reviewed on 2026-09-19 at HEAD `e973d06ed9585ab640efee2371c32d43271cd58d`. Implementation scope: `d304f62^..894a95c`, plus verification of the round 6 fixes. Application source was not changed.

Found **17 issues: 2 P1, 14 P2, 1 P3**. Findings distinguish executed reproductions from static analysis. No live fal requests, app startup, agent socket calls, GPU execution, or writes to the user's brushes/patterns directories were performed.

## Validation

- `env -u COMPOSITOR_GPU cargo build`: passed. Four warnings remain: an unused doc comment, deprecated GTK coordinate translation, unused `OP_COPY`, and unused `Assistant::present`.
- `env -u COMPOSITOR_GPU cargo test`: **169 passed, 0 failed, 2 ignored**. The ignored tests require Google Fonts/network access and a downloaded matting model. The GPU test returns early with GPU mode unset; its passing result does not validate GPU behavior.
- Compiled and executed a separate Rust probe against the built library, outside the repository. It reproduced the stroke/result race, HiDPI Blend If failure, ignored mask stroke opacity, duplicate-stop conversion, sharpening at alpha edges, and three pattern-decoding failures. Test log: `/tmp/compositor-round7-tests.log`; temporary probe and synthetic inputs: `/tmp/compositor-round7/`. These are temporary audit artifacts, not regression tests added to the project.
- Read-only decoding of all nine `RSCO*.abr` files succeeded: Blacksmith 5 tips, Champs Super Soft 3, DupliTone 3, Edge and Fold Vol 1 25, Vol 2 25, SparkPrint 12, Subtle Retro Vol 1 3, Vol 2 2, Woodland Wonderland 23. The real version 10/subversion 2 files decode with the current header skip. No real version 7 sample was available in that set.

## Findings

### 1. [P1] A completed assistant job can overwrite its generated image with an in-progress brush stroke

**Locations:** `src/ui/agent.rs:215`, `src/ui/agent.rs:624`, `src/document.rs:1608`, `src/document.rs:1653`.

Start an assistant generation, then begin painting while it runs. If `_poll` lands the result before mouse release, `agent_land_fill` selects the source and adds a new active image layer without checking `busy_editing`. `_poll` returns before reaching the normal tool edit guard at `src/ui/agent.rs:258`. `finish_stroke_named` takes its destination from the current active layer, not the layer that owned the stroke preview. It then copies the old stroke preview into the generated layer.

**Executed reproduction:** begin a red brush stroke on a white 40x40 layer, insert a solid blue image through `add_image_surface`, then finish the stroke. The generated layer's center changes from BGRA `[255,0,0,255]` to `[0,0,255,255]`; the original layer remains white. This duplicates the document calls made by the landing path without making a network request.

This is an incomplete round 6 finding 2 fix. Defer landing while the originating document has a transient edit, retain the result until it can land, and bind stroke completion to its original layer. The same bypass also permits results to enter an open Free Transform or Layer Style transaction.

### 2. [P1] Pattern import has no aggregate decoded-memory bound

**Locations:** `src/abr.rs:218`, `src/abr.rs:230`, `src/abr.rs:265`, `src/abr.rs:283`, `src/abr.rs:290`; caller `src/main.rs:38`.

`patterns()` retains every decoded RGBA pattern in a vector. The 200-million-sample budget resets for each record and charges channel decoding, not the full-size channel padding or RGBA output. An accepted 8192x8192 pattern with a 1x1 channel expands a tiny record into a 64 MiB padded plane and a 256 MiB RGBA result. Sixteen such records retain 4 GiB of RGBA alone. The CLI decodes the entire list before saving anything.

**Static allocation analysis; an OOM payload was deliberately not executed.** The small-channel case is accepted by the same branch exercised in finding 6. Enforce a shared output-byte budget before allocating padded planes and RGBA, and bound retained results or stream them to the caller. The existing brush-tip budget does not protect this new importer.

### 3. [P2] Blend If samples the wrong pixels on a HiDPI Cairo target

**Locations:** `src/render/mod.rs:819`, `src/render/mod.rs:839`, `src/render/mod.rs:845`; region construction at `src/render/mod.rs:140`.

`apply_blend_if` treats the region coordinates as physical pixel indices, incorporates only the source's device offset, and ignores surface device scale when sampling both source and underlying pixels. A Cairo surface with `set_device_scale(2,2)` therefore samples a different logical location from the one being painted.

**Executed reproduction:** a 40x40 document has a dark left half, light right half, and a red top layer with `under_black=128`, `feather=0`. On a 40x40 scale-1 target the right side is red, BGRA `[0,0,255,255]`. On an 80x80 target with scale 2, the same logical point is light gray `[230,230,230,255]`: the red layer was incorrectly hidden. Account for source/target scale and offset consistently when building and applying the mask.

### 4. [P2] Unwritten pattern channels desynchronize the parser

**Location:** `src/abr.rs:268`.

The parser reads `written` and then unconditionally reads a length before testing `written == 0`. An unwritten virtual-memory channel consists only of its four-byte flag. The extra read consumes the next channel's flag as a length, so subsequent channels are parsed at the wrong offset. This breaks libraries with unused channels before their usable color channels.

**Executed reproduction:** a gray pattern declaring 24 channels, with an unwritten channel followed by one valid raw channel, returns zero patterns and logs `a pattern channel could not be read`. The early return before reading a length is also explicit in the independent [psd-tools VirtualMemoryArray reader and writer](https://github.com/psd-tools/psd-tools/blob/main/src/psd_tools/psd/patterns.py). Check the written flag before reading any other channel fields; separately handle written channels with zero length.

### 5. [P2] Brush-tip shrinking corrupts large pattern channels

**Locations:** `src/abr.rs:187`, `src/abr.rs:278`, `src/abr.rs:280`.

Pattern decoding reuses `read_tip`, which now always calls `shrink_tip`. A pattern channel wider or taller than 2048 is downsampled as though it were a brush. `read_pattern` retains the original pattern dimensions and copies that smaller image into the upper-left corner, padding the rest with zero rather than rescaling or preserving the data.

**Executed reproduction:** a 4096x1 all-white gray pattern imports as 4096x1, but pixels from x=2048 onward are opaque black. Brush limiting must be separate from the full-resolution pattern-channel decoder.

### 6. [P2] Pattern channel rectangle origins are ignored

**Location:** `src/abr.rs:281`.

For a channel smaller than its pattern, the copy starts at destination `(0,0)` regardless of the channel's `ct/cl` and the enclosing pattern's `top/left`. The code discards the latter and uses only the channel width and height. Offset channel content is moved to the corner, and differently bounded color planes can be misregistered.

**Executed reproduction:** a 4x1 pattern whose single white channel pixel occupies `[left=2,right=3)` imports with white at x=0 and black at x=2. Place each plane at its rectangle's offset relative to the pattern rectangle, clipping the intersection.

### 7. [P2] The advertised Photoshop .pat import path always returns no patterns

**Locations:** `src/abr.rs:213`, `src/main.rs:33`.

The CLI advertises `.abr|.pat`, but `patterns()` first requires a big-endian u16 equal to 6, 7, or 10 and then expects ABR `8BIM` sections. A Photoshop PAT begins with `8BPT`, so its first u16 is `0x3842` and this function immediately returns an empty successful result. The importer prints `imported 0 patterns` for a valid library.

**Static verification:** the incompatible header is sufficient to establish the rejection. The separate PAT header and version/count parsing are visible in the [Photoshop PAT loader implementation](https://github.com/maz-1/archlinux_packages/blob/master/gimp-photoshop-pattern/ps-pat-load_1.c). Add a real PAT container branch rather than treating the format as an ABR section. A real PAT library was not imported during this audit.

### 8. [P2] Paragraph-text drag creation is unreachable

**Locations:** `src/ui/canvas.rs:507`, `src/ui/canvas.rs:1029`, `src/ui/canvas.rs:1088`.

`begin_tool_drag` contains a new `Tool::Type` arm producing `ToolDrag::TextBox`, but its only caller dispatches only `Gradient | Shape | Crop`. With Type selected, dragging never reaches the text-box width update. A click may create or pick a text layer, but dragging does not set paragraph width as the options tooltip promises.

**Static control-flow verification.** Include Type in the drag dispatch and test the gesture itself. The existing paragraph test sets `TextStyle.width` directly, so it cannot detect this disconnected UI path.

### 9. [P2] Coincident gradient stops become a ramp in Cairo gradients

**Locations:** `src/gradient.rs:73`, `src/gradient.rs:85`.

`knots()` deduplicates stop positions, and `fill_pattern()` emits only `at(k)` at each remaining position. Two different colors at the same position intentionally form a hard transition, but one side of that transition is lost. This makes the Cairo-backed gradient fills disagree with `Gradient::at`, the LUT, and rasterized shapes.

**Executed reproduction:** stops `(0,black), (0.5,black), (0.5,white), (1,white)` give white from `at(0.75)`. A 100-pixel Cairo linear gradient created with `fill_pattern` gives gray `[130,130,130,255]` at x=75. Preserve both sides of coincident color/opacity stops when constructing a Cairo pattern. The editor permits coincident positions, and the agent can supply them directly.

### 10. [P2] Reverse is discarded for the two palette gradients on masks

**Location:** `src/ui/canvas.rs:1004`.

`current_gradient` first reverses `g`, but when a mask is targeted and preset 0 or 1 is selected it returns a newly constructed white/black or foreground-to-transparent gradient instead. That replacement never receives `gradient_reversed`. Toggling Reverse therefore has no effect for these mask gradients even though it works for custom and built-in color gradients.

**Static verification.** Build the mask-specific gradient first, then apply reversal to the final gradient.

### 11. [P2] Stroke Selection ignores opacity on a mask

**Location:** `src/document.rs:2924`.

The pixel branch uses the clamped opacity in `set_source_rgba`, but the mask branch replaces the selection with the stroke band and calls `fill(color)`. That call has no opacity parameter, so mask strokes always paint at full strength.

**Executed reproduction:** create a hide-all mask, select a rectangle, then call `stroke_selection(2, white, inside, 0.0)`. The mask's maximum changes from 0 to 255 despite zero requested opacity. Apply opacity to the mask coverage or composite the painted mask at the requested strength.

### 12. [P2] Sharpen treats alpha boundaries as color contrast

**Location:** `src/filters.rs:385`.

Both sharpening branches blur premultiplied color and alpha, subtract the blurred color bytes from the original color bytes, and keep the original alpha without using the blurred alpha. A boundary between constant straight color and transparency is thus interpreted as a dark color boundary. Cutout edges become brighter, and threshold/noise behavior changes with opacity.

**Executed reproduction:** input BGRA pixels `[128,128,128,255], [128,128,128,255], [0,0,0,0]`, Unsharp Mask amount 100, radius 2, threshold 0, produces gray values 159 and 171 in the two visible pixels. Their straight color was uniformly 128. Compute the color-detail signal with alpha-aware normalization and keep alpha handling explicit. The existing flat-color test uses opaque pixels throughout and misses this case.

### 13. [P2] The filter schema cannot express Shadows/Highlights numeric controls

**Locations:** `src/agent.rs:56`, `src/ui/agent.rs:323`.

The `filter` tool schema still declares `shadows` and `highlights` as arrays for Color Balance. Its new Shadows/Highlights handler reads those same fields with `as_f64`. The documented numeric call violates the schema, while schema-conforming arrays are ignored and leave default filter strengths in place. For example, requesting a zero-strength Shadows/Highlights filter cannot be expressed correctly through a schema-enforcing client.

**Static schema/handler comparison.** Give the two filter kinds compatible conditional schemas or distinct fields. The adjustment-layer schema already uses numbers for this kind. Also remove or implement the `black_ink` field in the filter schema: the Selective Color handler only reads `black`.

### 14. [P2] The five-minute watchdog still kills a progressing generation

**Locations:** `src/ui/agent.rs:943`, `src/ui/agent.rs:953`, `src/ui/agent.rs:955`; job timeout `src/agent.rs:92`.

While Claude waits for a synchronous MCP generation call, it can produce no stdout/stderr until that call returns. The timer displays changing fal progress through `running_job_status`, but only received child output updates `quiet_since`. A healthy job taking more than five minutes therefore has its Claude process killed even though its progress is visible and the server allows the job ten minutes.

**Static lifecycle verification; no paid job was run.** This is the remaining long-job case of round 6 finding 14. Account for an owned active job, bounded by the job deadline, when deciding whether a turn is stalled. Updating only the displayed status does not keep the turn alive.

### 15. [P2] The socket's initial request read remains unbounded

**Location:** `src/ui/agent.rs:69`.

The round 6 timeout fix bounds the supplied client's response wait and the later generation poll loop. An accepted server connection still calls `BufReader::read_line` with no server read timeout or line-size limit. A client that connects without sending a newline retains its handler thread indefinitely; a client streaming a line without a newline can grow the string indefinitely. The job timer has not started at this point.

**Static verification; no live socket was contacted.** This is an incomplete round 6 finding 12 fix. Bound the accepted connection's request read and its maximum frame size before JSON parsing. A timeout configured by Compy's own client does not protect the server from other or malfunctioning clients.

### 16. [P2] Define Pattern samples the composite even when it promises the active layer

**Locations:** `src/document.rs:3052`, `src/document.rs:3070`.

With no selection, `define_pattern` uses the active layer's bounds and says it will save the whole active layer. It nevertheless always calls `renderer.draw`, which samples every visible layer. Defining a pattern from a partly transparent layer above an opaque background bakes the background into the pattern; overlapping layers above it also become part of the tile.

**Static rendering-path verification.** Keep composite sampling for the selection case, but render the active layer for the no-selection case. The current pattern test always supplies a selection and therefore cannot distinguish the two behaviors.

### 17. [P3] A newly defined pattern never appears in the existing Pattern Stamp dropdown

**Location:** `src/ui/tools.rs:455`.

The options bar obtains `patterns::list()` only during construction and captures that vector in the dropdown callback. There is no retained dropdown field or refresh path for it. Define a pattern while a document is open, then select Clone/Pattern Stamp: that document's dropdown still lists only patterns that existed when its options bar was created. A newly constructed document/options bar can see the new pattern, but switching tools does not refresh the existing one.

**Static widget-lifetime verification.** Refresh the dropdown when patterns change or when it is opened, preserving the selected pattern by name.

## Round 6 fix verification

These are source-verification results unless a test or probe is explicitly identified above. They do not claim live process, voice, network, or disk-failure testing.

| Round 6 item | Result of this pass |
|---|---|
| 1. Result destination and selection | Originating document id, source id, and selection are retained. Closed-document handling is explicit. Landing still needs the transient-edit protection in finding 1. |
| 2. Open edits | Ordinary mutating calls have the guard, but completed jobs bypass it. Finding 1 reproduces the consequence. |
| 3. Socket ownership | A connect probe precedes unlink/bind. The reported ordinary second-instance takeover is addressed. |
| 4. Old turn controls a new child | Turn ids guard timers and shutdown/reset advances ownership. No new defect established in that fix. |
| 5. Model config recursion and overwriting | Nonrecursive fallback and preservation of malformed config are present; relevant unit tests pass. |
| 6. Resize allocation | Total-pixel checks are present; the resize-limit test passes. |
| 7. Autosave races | Page removal prunes pages; a single worker and discard-generation checks address the recorded races. The earlier page-retention allegation was already corrected in the status table. |
| 8. Failed autosave and transient snapshots | Completion is acknowledged after success, and busy documents are skipped. No new defect established. |
| 9. Path operations on masks | Mask-specific branches are present; the mask path regression test passes. |
| 10. Layer Style validation | Keys, object fields, colors and numeric inputs are checked before mutation. |
| 11. Atomic agent edits | The ordinary mutation wrapper ends or aborts its outer edit. Async landing is a separate path, covered by finding 1. |
| 12. Protocol waits and malformed calls | Client response and job waits are bounded, response ids checked, malformed MCP calls answered. Initial server framing remains unbounded, finding 15. |
| 13. Cancellation | Disconnect detection reaches `_cancel` and the worker cancellation flag. Not exercised against fal. |
| 14. Lifecycle/watchdog/voice | Turn shutdown, popout focus, manual-send dictation termination and config errors are handled. Progressing jobs can still hit the watchdog, finding 14. |
| 15. Duplicate queued messages | `submit` displays messages; `send_now` no longer appends them again. |
| 16. Result aspect ratio | The general image decoder supplies dimensions before fitting. |
| 17. GPU | Deliberately not re-reviewed or executed, as directed by the round 7 prompt. |
| 18. Open failure and empty text | Open returns success/failure; agent text creation/replacement rejects empty text. |
| 19. Degenerate Pen handles | Near-anchor handles are removed. |
| Tests | The Pen assertion now compares different flattening steps. GPU success still means skipped execution when GPU mode is unset. |

## Coverage and test gaps

All twelve numbered round 7 areas received source review. Focused executed probes cover the failures listed above; the remaining UI findings are control-flow analysis, not interactive GTK reproductions.

| Area | Coverage and limits |
|---|---|
| 1 | Round 6 status items checked as listed above, excluding the explicitly parked GPU item. |
| 2 | New filter math, alpha handling, adjustment records/validation and identity paths reviewed. Finding 12 is reproduced. Maximum-size filter workloads were not stress-tested. |
| 3 | Dialog kind/adjustment mappings, Selective Color synchronization, mixer controls and agent naming/schema paths reviewed. No GUI dialogs were opened. |
| 4 | Gradient evaluation, Cairo conversion, shapes, editor, palette/mask construction and records reviewed. Findings 9 and 10. |
| 5 | Blend If weights, rendering integration and Layer Style controls reviewed. Scale-1/scale-2 comparison reproduced finding 3. This is CPU Cairo review only. |
| 6 | Pattern persistence, define/fill/stamp, picker, CLI and parser reviewed. Findings 2, 4, 5, 6, 7, 16, 17. Synthetic parser inputs were kept outside the user's library. |
| 7 | ABR bounds, shrinking and picker load path reviewed; all nine real RSCO files decoded read-only. This verifies those files, not every possible version 7/10 record. |
| 8 | Width serialization, Pango layout, caret paths, options and canvas drag dispatch reviewed. Finding 8. Direct paragraph-layout tests pass. |
| 9 | Path shape records, bounds, reconstruction, masks and canvas entry points reviewed. Rotation omission is explicitly stated in `shape_path`; PSD export takes the rasterized pixel-layer route rather than preserving editable path metadata. No independent vector-editing round-trip guarantee was established. |
| 10 | New tools, brush settings, schema/argument compatibility, job status and turn timer reviewed. Findings 1, 13, 14. No assistant process or paid operation was launched. |
| 11 | Dodge/Burn/Sponge alpha handling, selection stroke, color range, alignment/distribution, icon and redraw changes reviewed. Finding 11. |
| 12 | New `features_g` and gradient/ABR tests reviewed. Existing assertions do not establish the missing cases below. |

The tests pass but omit important boundaries: coincident gradient stops and Cairo/LUT agreement; Blend If with device scale; zero-opacity mask strokes; sharpening at an alpha boundary; Type gesture dispatch; pattern import with unwritten, offset, or oversized channels; aggregate pattern output budgets; and async result landing during a stroke. The ABR unit test contains an empty `patt` section, so its success does not test the pattern decoder. The pattern integration test checks define/fill/stamp with a selection, not importing Photoshop patterns or refreshing the dropdown.

The pattern integration test also changes process-wide `XDG_DATA_HOME` without restoring it. It successfully isolates its writes from the user's pattern library, but parallel tests in that binary should not rely on an unchanged data-home environment. I did not establish a currently failing parallel consumer, so this is a test-isolation note rather than another ranked bug.

Review complete for this pass. No application fixes were made. Interactive GTK gesture testing, live assistant/network cancellation, full-size memory/performance stress tests, and GPU behavior remain unverified as described above.

## Status after fixes

| # | Finding | Status |
|---|---------|--------|
| 1 | A landing result meets an in-progress stroke | Fixed. `_poll` keeps a finished job waiting while its document is mid-edit (a stroke, a transform, a dialog, on-canvas typing), and a stroke now remembers the layer it began on (`stroke_layer`) so it lands there whatever became active. Test. |
| 2 | Pattern import unbounded | Fixed. A 768 MB output budget across the file, charged per pattern for its planes and RGBA before anything is allocated; the rest are skipped with a message. |
| 3 | Blend If on a HiDPI target | Fixed. Device coordinates go through each surface's device scale and offset; the mask carries the group's scale. Test at 2x. |
| 4 | Unwritten pattern channels | Fixed. The flag is read alone; only a written channel has a length. Test. |
| 5 | Pattern channels shrunk like tips | Fixed. `read_plane` shrinks only for brush tips. Test with a 4096-wide pattern. |
| 6 | Channel rectangle origins | Fixed. Planes land at their offset from the pattern's corner. Test. |
| 7 | .pat files returned nothing | Fixed. The 8BPT container is read: version, count, then the same records. Test. |
| 8 | Type drag unreachable | Fixed. The Type tool's press also starts the text box drag. |
| 9 | Coincident gradient stops | Fixed. Both sides of a hard edge go into the Cairo pattern. Test against a painted gradient. |
| 10 | Reverse lost on mask palette gradients | Fixed. Reverse is applied after the mask substitution. |
| 11 | Stroke Selection ignores opacity on a mask | Fixed. `fill_with(color, alpha)` scales the mask coverage. Test at 0 and 0.5. |
| 12 | Sharpen at alpha edges | Fixed. Both sharpen branches compare straight colors and re-premultiply. Test. |
| 13 | Filter schema for Shadows/Highlights | Fixed. `shadow_amount` and `highlight_amount` (numbers) with the old names still read; `black_ink` replaced by `black_amount`, which Selective Color reads. |
| 14 | Watchdog kills a progressing job | Fixed. A running job resets the quiet clock. |
| 15 | Unbounded request read | Fixed. Sixty-second read timeout and a 16 MB frame limit on the request line. |
| 16 | Define Pattern samples the composite for a whole layer | Fixed. Without a selection the active layer is drawn alone. Test. |
| 17 | Pattern dropdown never refreshes | Fixed. The list is rebuilt from the folder each time the Clone tool's options show, keeping the choice by name. |
| Tests | XDG_DATA_HOME left changed | Fixed. Both pattern tests restore it. |
