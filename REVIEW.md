# Review prompt

Paste everything below the line into a code review assistant, pointing it at this folder:
`/home/estevens/code/compositor-linux`.

---

Review the Rust project at /home/estevens/code/compositor-linux for bugs.

Context: it is a Linux rebuild of a macOS image editor (Swift source in reference/, read-only, treated
as the spec). The original C pixel core is compiled unchanged from csrc/ (do not edit it). Everything
else is Rust on GTK4 and Cairo. CLAUDE.md and README.md explain the layout and conventions; read both
first.

Key facts you need:

- Cairo ARGB32 is premultiplied BGRA in memory. Look for places that treat it as RGBA or as straight
  alpha.
- Cairo ImageSurface::data() requires refcount 1; raw pointer reads via cairo_image_surface_get_data
  are used elsewhere. Check for aliasing or use-after-free around those.
- Undo is value snapshots sharing surfaces by refcount (src/history.rs, src/render/mod.rs State).
  Check that no edit mutates a surface still referenced by a snapshot.
- Layers and masks render from Lanczos halvings (src/raster.rs) keyed by (id, source, level); check
  the cache is invalidated on every mutation.
- The PSD reader (src/psd.rs) parses untrusted files: look for panics, integer overflow, unbounded
  allocation, out-of-bounds indexing on malformed input.
- The .comp loader (src/format/) enforces the validation rules in
  reference/Compositor/Persistence/ProjectStore.swift; check for rules that are missing or wrong.

What I want:

1. Concrete bugs with file:line, what input triggers them, and what goes wrong. Rank by severity.
2. Places where the Rust behavior differs from the Swift reference for the same feature (blend
   modes, mask coverage, clipping, transform math, brush accumulation, selection ops).
3. Panics reachable from user input (unwrap, expect, slicing, arithmetic) in src/.
4. Anything in tests/ that asserts the wrong value or is tautological.

Run cargo build and cargo test (needs gtk4 and cairo dev packages; 69 tests should pass) and report
anything that fails. If you can only read files and not run commands, skip this step. Do not refactor
or restyle; report only. No em dashes in your output.

---

# Round 2 prompt

Paste everything below the line into the review assistant. It assumes the round 1 findings in
`REVIEW_RESULTS.md` were fixed (commit 53e1fac).

---

Review the Rust project at /home/estevens/code/compositor-linux for bugs, second pass.

Context: a Linux rebuild of a macOS image editor. The Swift source in reference/ is the spec (read-only);
the C pixel core in csrc/ is compiled unchanged (do not edit it). Read CLAUDE.md and README.md first.
REVIEW_RESULTS.md holds the first review and a status table of what was fixed; do not re-report those
eleven findings unless a fix is wrong or incomplete.

Cover what the first pass did not:

1. Unsafe code and surface lifetimes. The first review said it did not check these. Audit every use of
   cairo_image_surface_get_data, with_bytes, with_bytes_mut and with_bytes_raw_mut in src/raster.rs and
   their callers. Look for reads while a Context or pattern still references the surface, writes without
   mark_dirty, surfaces shared between an undo snapshot and a live edit, and stride assumptions.
2. The round 1 fixes themselves: src/psd.rs validation (rect_dims, the channel length and budget checks,
   the merged image path), History::cancel and Document::abort_edit, Renderer::restore's placed-mask
   invalidation, the Clone Stamp source-over, the adjustment coverage clip, and
   Adjustment::record_is_valid against reference/Compositor/Document/LayerAdjustment.swift.
3. The GTK layer in src/ui/: RefCell borrow_mut while another borrow is live (a panic at runtime),
   callbacks that run after the tab was closed, dialogs that can be opened twice, actions that assume a
   current document, and the screenshot/script path.
4. The brush and warp engines (src/brush.rs, src/warp.rs) against reference/Compositor/Document/
   BrushStroke.swift and SmudgeLiquify.swift: dab spacing, opacity capping, selection respect, layer
   growth, the 64-pixel commit alignment, and heal/blur modes.
5. Opening odd inputs through every path (command line, drop, Open dialog): a .comp folder missing an
   image file, a manifest that is valid JSON but the wrong shape, a 0-byte file with a .psd or .png
   extension, a symlink, a path with no extension, a file that is not UTF-8 named, a 30,000 x 30,000
   image. Each should produce an error dialog or message, never a panic.
6. Anything in tests/ that asserts the wrong value or passes for the wrong reason.

What I want: concrete bugs with file:line, the input that triggers them, and what goes wrong, ranked by
severity. Run cargo build and cargo test (78 tests should pass) and report anything that fails. If you can
only read files and not run commands, skip that. Report only; do not refactor or restyle. Write the
report to /home/estevens/code/compositor-linux/REVIEW_RESULTS_2.md. No em dashes in your output.

---

# Round 3 prompt

Paste everything below the line into the review assistant. Rounds 1 and 2 are fixed (see REVIEW_RESULTS.md and
REVIEW_RESULTS_2.md); this round covers the code written since, at commit d0616dd and later.

---

Review the Rust project at /home/estevens/code/compositor-linux for bugs, third pass.

Context: a Linux rebuild of a macOS image editor. The Swift source in reference/ is the spec (read-only);
the C pixel core in csrc/ is compiled unchanged (do not edit it). Read CLAUDE.md and README.md first.
REVIEW_RESULTS.md and REVIEW_RESULTS_2.md hold the earlier findings, all fixed; do not re-report them.

Cover only what is new since round 2. Rank by severity, give file:line, the input that triggers it, and
what goes wrong. Run cargo build and cargo test (115 tests should pass; one test is ignored because it
needs a downloaded model) and report anything that fails. If you can only read files, skip that.

1. Untrusted file parsers: src/abr.rs (Photoshop brush files, versions 1, 2 and 6, PackBits rows),
   src/heic.rs (libheif), and the changes to src/psd.rs. Panics, overflow, unbounded allocation,
   out-of-bounds indexing on malformed input. tests/fixtures/ has real samples.
2. src/brush_set.rs and the brush engine changes in src/brush.rs: shaped_tip, the angle jitter variants,
   the max accumulation for shaped tips, spacing. Check replay (Stroke::replay) gives the same result as
   the live stroke, and that a tip scaled past its source size or below 1 pixel behaves.
3. src/distort.rs (perspective warp), Document::commit_distort and preview_distort: numeric edge cases,
   masks placed apart from their layer, the corner drag in src/ui/canvas.rs.
4. Pixel moves (Document::begin_pixel_move, move_pixels, finish_pixel_move, cancel_pixel_move): layer
   growth, the mask carried onto the grown grid, the selection outline, undo state, rotated layers.
5. Multi-selection (Document::selected, select_layer_range, toggle_layer_selected, transform_members,
   group_box), merges of several layers, delete of several, and the layer drag-and-drop paths
   (move_layers, copy_layers, copy_layers_within, Place) including drops onto descendants and between
   documents. Check the panel code in src/ui/layers.rs for RefCell borrow_mut while borrowed.
6. Gradient, shape, crop and eyedropper (Document::gradient, add_shape_layer, redraw_shape, crop,
   sample_color) and their canvas drags; the crop snapping code in src/ui/canvas.rs.
7. Remove Background in src/matte.rs: the model download (partial files, bad network, wrong size), the
   session cache, resampling, the guided filter's numeric stability, refine's limit handling, and
   Document::remove_background with an existing mask.
8. The rulers, the brush picker and preset registry (src/ui/brushes.rs, thread-local state, remembered
   file list), the color wheel, the theme file watcher, and the Photoshop-style panel changes: borrow
   panics, widgets outliving their document, state that fails to sync.
9. Anything in tests/features_a.rs through features_e.rs, tests/abr_sample.rs and tests/brush.rs that
   asserts the wrong value or passes for the wrong reason.

Report only; do not refactor or restyle. Write the report to
/home/estevens/code/compositor-linux/REVIEW_RESULTS_3.md. No em dashes in your output.

# Round 4 prompt

Paste everything below the line into the review assistant. Rounds 1 to 3 are fixed (see REVIEW_RESULTS.md,
REVIEW_RESULTS_2.md and REVIEW_RESULTS_3.md); this round covers the code written since, commits 52f83ad
through 904e58f.

---

Review the Rust project at /home/estevens/code/compositor-linux for bugs, fourth pass.

Context: a Linux rebuild of a macOS image editor. The Swift source in reference/ is the spec (read-only);
the C pixel core in csrc/ is compiled unchanged (do not edit it). Read CLAUDE.md and README.md first.
REVIEW_RESULTS.md, REVIEW_RESULTS_2.md and REVIEW_RESULTS_3.md hold the earlier findings, all fixed; do
not re-report them.

Budget: if you are running low on tokens or time, stop, write what you have found so far, and end the
report with a line that says exactly where you stopped (which numbered area and which file) so the next
pass can pick up there. A partial report that says where it ended is worth more than an unfinished one.

Cover only what is new since round 3 (git log 52f83ad^..904e58f). Rank by severity, give file:line, the
input that triggers it, and what goes wrong. Run cargo build and cargo test (141 tests should pass; two are
ignored because they need the network or a downloaded model) and report anything that fails. If you can
only read files, skip that.

1. Type layers: src/text.rs (Pango rendering, caret and index_at on empty and multi-line text, the Google
   Fonts fetch: bad family names, network failures, the 64 MB limit, the fc-cache call), and
   Document::add_text_layer, set_text (the scale kept across edits, the mask placement), text_layer_at.
   The on-canvas editor in src/ui/canvas.rs (type_at, text_key, finish_text_edit, raster_point): byte
   indices on multi-byte characters, a layer deleted or undone while being edited, rotated or flipped
   layers, keys while an entry has focus, the empty layer dropped on finish.
2. Layer effects: src/effects.rs (blur, shift, the distance transform, the bevel lighting, the paint
   compositing, reach) on degenerate sizes (1 by 1, 0 opacity, size 0, 30,000 px wide layers, layers
   mostly off canvas), Renderer::styled and its cache key (surface pointer reuse after a free, previews
   during a stroke, masks), draw_own and paint_own after the refactor (the direct path, blend modes,
   folders, clips), and the Layer Style dialog in src/ui/effects.rs (state shared between pages, Cancel
   after the layer was deleted, the merged undo step).
3. Guides, snapping and the grid: Document::snapped_guide, guide_snap_targets, grid_lines, snap_targets,
   snapped_move; the ruler press and guide drag in src/ui/canvas.rs; the grid drawing at extreme zoom.
4. New commands in src/document.rs: layer_via (copy and cut, with masks and rotated layers),
   merge_visible (hidden children of visible folders, adjustment layers, clipping masks), stamp_visible,
   move_layer_to_end, select_all_layers, reselect after a canvas resize, feather_selection, desaturate,
   begin_free_transform, commit_free_transform, cancel_free_transform (the open edit if the user saves,
   switches documents or closes while floating; undo during a float).
5. Shortcuts and menus in src/ui/mod.rs: the actions table, the parameterized select-layer-id and
   delete-guide actions (stale ids and indices), the window key controller (Tab and F while a dialog is
   open, keys during a canvas text edit), toggle_panels, the shortcuts window, the context menus in
   src/ui/canvas.rs and src/ui/layers.rs (RefCell borrow_mut while borrowed, popovers unparented twice,
   track_popover and close_popover).
6. The look in src/ui/theme.rs: system_font when omarchy is missing or slow, CSS that fails to parse on
   GTK 4.22 (warnings on stderr), providers installed twice, the screenshot harness change in
   snapshot_window.
7. Preview mode, the picture and overlay split, the Move tool letting go of the layer on empty canvas,
   the free transform floating layer interacting with multi-selection and folders.
8. Anything in tests/features_f.rs, tests/features_g.rs and the unit tests in src/text.rs and
   src/effects.rs that asserts the wrong value or passes for the wrong reason.

Report only; do not refactor or restyle. Write the report to
/home/estevens/code/compositor-linux/REVIEW_RESULTS_4.md. No em dashes in your output.

# Round 5 prompt

Paste everything below the line into the review assistant. Rounds 1 to 4 are fixed (see REVIEW_RESULTS.md
through REVIEW_RESULTS_4.md); this round covers the code written since, commits 272d01e through ac49b7f.

---

Review the Rust project at /home/estevens/code/compositor-linux for bugs, fifth pass.

Context: a Linux rebuild of a macOS image editor. The Swift source in reference/ is the spec (read-only);
the C pixel core in csrc/ is compiled unchanged (do not edit it). Read CLAUDE.md and README.md first.
REVIEW_RESULTS.md through REVIEW_RESULTS_4.md hold the earlier findings, all fixed; do not re-report
them, but do check that the round 4 fixes hold (they are the first area below).

Budget: if you are running low on tokens or time, stop, write what you have found so far, and end the
report with a line that says exactly where you stopped (which numbered area and which file) so the next
pass can pick up there. A partial report that says where it ended is worth more than an unfinished one.

Cover only what is new since round 4 (git log 272d01e^..ac49b7f). Rank by severity, give file:line, the
input that triggers it, and what goes wrong. Run cargo build and cargo test (147 tests should pass; two are
ignored because they need the network or a downloaded model) and report anything that fails. If you can
only read files, skip that.

1. The round 4 fixes, as fixes: Document::merge_floating (the grown grid, rotated and flipped sources,
   masks on the source, a float dragged entirely off the canvas, a source with effects or a clipping
   mask, the 100 megapixel fallback), commit_free_transform and cancel_free_transform, is_modified with
   a float or an open edit, save landing the float, Document::merge_visible reparenting, the effects
   compositing change in Renderer::draw_own (interior effects Atop inside a group, blend modes and
   opacity on styled layers, the direct path), Effects::render's three buffers, the Layer Style
   dialog's begin_layer_style and end_layer_style (the window closed while the layer is gone, a second
   dialog opened on the same layer, undo while it is open).
2. Curves: the editor in src/ui/filter_dialog.rs build_curves (point hit testing near the ends, a point
   dragged past its neighbours, 32 points, the readout, channel switching mid-drag, Remove on an end
   point, the histogram reused from Levels) and Kind::Curves in filters and Document::apply_filter;
   the Curves adjustment layer now opening this editor (current_adjustment, open_adjustment).
3. Color Balance: ColorBalance::shift and apply (values at 0 and 255, preserve luminosity pushing a
   channel out of range, fully transparent pixels, premultiplied round trips), normalized, is_identity.
4. Auto Tone, Auto Contrast and Auto Color (Levels::auto_tone, auto_color, auto_contrast, endpoints)
   on empty, single-value and clipped histograms; Document::auto_levels on a mask target.
5. Fade: Document::last_filter (set only when the grid is unchanged, invalidated when the layer changes,
   the pointer comparison against the current image, undo after a filter then Fade, Fade after a
   filter on a mask, memory held by the two surfaces), Kind::Fade in filtered and apply_filter, the
   dialog page.
6. On-canvas typing after the round 4 changes: raster_point through document_to_layer, the caret and
   frame through pixel_to_document, sync_inspector clearing a stale editor, Return with the Move tool
   now parking the handles (does it steal Return from anything else), handles_parked reset paths.
7. The start page in src/ui/mod.rs (start_page, parse_preset, the new-preset action with a malformed
   string, presets at 300 ppi creating large canvases, the custom fields, focus and keyboard use with
   no document open, the window key controller when no page exists).
8. The toggle-handles action and Doc::show_handles, F1, the layer row CSS, and anything in
   tests/features_g.rs (the new tests) or the unit tests in src/effects.rs that asserts the wrong value
   or passes for the wrong reason.

Report only; do not refactor or restyle. Write the report to
/home/estevens/code/compositor-linux/REVIEW_RESULTS_5.md. No em dashes in your output.

# Round 6 prompt

Paste everything below the line into the review assistant. Rounds 1 to 5 are fixed (see REVIEW_RESULTS.md
through REVIEW_RESULTS_5.md); this round covers the code written since, commits c5d5b17 through 730037a.

---

Review the Rust project at /home/estevens/code/compositor-linux for bugs, sixth pass.

Context: a Linux rebuild of a macOS image editor, now named Compy, with an in-app assistant (also called
Compy) that drives the document through Claude Code. The Swift source in reference/ is the spec
(read-only); the C pixel core in csrc/ is compiled unchanged (do not edit it). Read CLAUDE.md and
README.md first. REVIEW_RESULTS.md through REVIEW_RESULTS_5.md hold the earlier findings, all fixed; do
not re-report them, but do check that the round 5 fixes hold (they are the first area below).

Budget: if you are running low on tokens or time, stop, write what you have found so far, and end the
report with a line that says exactly where you stopped (which numbered area and which file) so the next
pass can pick up there. A partial report that says where it ended is worth more than an unfinished one.

Safety: do not run the app with COMPOSITOR_GPU set to anything. The GPU presentation path has hung the
graphics card on this machine. Review src/gpu and src/render/gpu_plan.rs by reading only. Do not start
the app with --assistant while a real one is open (it would take over the agent socket). Do not send
anything to fal.ai; tests that need the network are ignored.

Cover only what is new since round 5 (git log c5d5b17^..730037a). Rank by severity, give file:line, the
input that triggers it, and what goes wrong. Run cargo build and cargo test (157 tests should pass;
the ignored ones need the network, a downloaded model or a GPU) and report anything that fails. If you
can only read files, skip that.

1. The round 5 fixes, as fixes: check each item in REVIEW_RESULTS_5.md against the current code.
2. The agent protocol in src/agent.rs: the Unix socket line protocol (a request with no newline, a
   response for a different id, a second client while one waits, the socket file left behind by a
   crashed app and a new app starting), call() timeouts, run_mcp() (malformed JSON-RPC, notifications
   without an id, tools/call with missing arguments, image results, a tool that returns a job id and the
   _poll loop never finishing), parse_color, and the tool catalog schemas against what
   App::agent_tool actually reads from args.
3. Tool implementations in src/ui/agent.rs App::agent_tool: every tool with no document open, with a
   deleted or hidden active layer, with a mask target, with an open edit (a free transform or a type
   edit in progress), with args of the wrong type; undoability (one history step per call, nested
   edits closed on error, abort_edit on bail), and refresh after each. select_layer_pixels,
   feather_selection, place_layer and reorder_layer with out-of-range values, canvas_size and image_size
   with huge values, text_layer and set_text with an empty string, layer_style with unknown keys,
   export and save to unwritable paths, open with a missing file, zoom with nonsense.
4. Long jobs: the JOBS and NEXT_JOB thread locals, a job polled after the app dropped it, results
   landing on a different document than the one the job started on (the user switched tabs), a job
   finishing after its document closed, agent_land_fill (the fit-inside math for a picture with an
   odd aspect ratio, a place of zero size, images that are not PNG, mode layer with no images),
   genfill_inputs used for the agent, copy_layer_pixels on an empty layer, cost estimates.
5. Fal model families in src/genfill.rs: resolve_model, nearest_aspect, agent_body (transparent with a
   non-GPT model, an edit with count above 4, a resolution tier for a 64 px request), agent_models()
   with a malformed config file, png_size on a truncated file, Fal::run parsing a response with images
   as data URIs or with an error object, the queue poll loop on a job that fails, cancellation.
6. The Assistant panel: new, dock, undock, redock, reveal, toggle (the popout closed by the window
   manager, redock while popped out, the notebook page switch with no document, a second Ctrl+K while
   popped out); send_now (the claude child process left running when the app quits, the watchdog
   firing after a reply arrived, stderr filling the pipe and blocking, the session id reset on a
   resume failure, the queue drained out of order, busy never cleared on a spawn error); handle_line
   with partial lines, non-JSON lines and result lines without text; context() with a huge document
   state; the transcript growing without bound; the mcp.json and skill files written at startup
   (races between two apps, a read-only config dir).
7. Voice: watch_voice polling the voxtype state file (the file missing, the daemon not installed,
   recording that never ends, the 900 ms auto-send racing a manual send, dictation while the entry
   has focus in the popout, the mic button when voxtype is absent), and Page Down handling.
8. Autosave (src/autosave.rs and the timer in src/ui/mod.rs): the 120 s timer while a job is running,
   a write racing a save to the same path, recovery entries for a document that was saved and closed
   cleanly, discard-recovered on a missing file, the .title file out of step with the .comp, the
   packed bytes captured while an edit is open, disk full.
9. The Pen tool (src/path.rs and pen_* in src/ui/canvas.rs): a path of one point stroked, filled or
   selected; closing with fewer than three points; handles dragged onto their anchor; Delete on an
   empty path; fill_path and stroke_path on a mask target; select_path with feather; the overlay after
   undo; the --path script flag parser with malformed input.
10. The menu bar, the layers background menu (parented to the panel: does it survive a layout change,
    what happens with a selection right-click on the same spot), Ctrl+S feedback, Return in fields
    parking handles, the brush popover tracking (track_popover and close_popover with a popover that
    was already destroyed), the dialog fixes from 5d95212 (Curves drag claims, Color Balance borrow,
    Levels channel dropdown), and the start page recovery and recent-files sections (a recent file
    that no longer exists, a title with markup characters, thumbnails for huge files).
11. The GPU code by reading only: src/gpu/mod.rs, shader.wgsl, present.rs, src/render/gpu_plan.rs,
    and the gate in src/ui/canvas.rs that keeps it off unless COMPOSITOR_GPU is set. Confirm nothing
    runs on the GPU by default, and note anything in the parked code that would be wrong if it were
    turned on (buffer lifetime, drop order, the LINEAR export image).
12. Anything in tests/features_g.rs, tests/autosave.rs, tests/gpu.rs or the unit tests in
    src/agent.rs and src/genfill.rs that asserts the wrong value or passes for the wrong reason.

Report only; do not refactor or restyle. Write the report to
/home/estevens/code/compositor-linux/REVIEW_RESULTS_6.md. No em dashes in your output.

# Round 7 prompt

Paste everything below the line into the review assistant. Rounds 1 to 6 are fixed (see REVIEW_RESULTS.md
through REVIEW_RESULTS_6.md); this round covers the code written since, commits d304f62 through 894a95c.

---

Review the Rust project at /home/estevens/code/compositor-linux for bugs, seventh pass.

Context: a Linux rebuild of a macOS image editor named Compy, with an in-app assistant (also Compy) that
drives the document through Claude Code over a Unix socket and an MCP server. The Swift source in
reference/ is the spec (read-only); the C pixel core in csrc/ is compiled unchanged (do not edit it).
Read CLAUDE.md and README.md first. REVIEW_RESULTS.md through REVIEW_RESULTS_6.md hold the earlier
findings, all fixed; do not re-report them, but do check that the round 6 fixes hold (area 1).

Budget: if you are running low on tokens or time, stop, write what you have found so far, and end the
report with a line that says exactly where you stopped (which numbered area and which file) so the next
pass can pick up there. A partial report that says where it ended is worth more than an unfinished one.

Safety: do not run the app with COMPOSITOR_GPU set to anything (the GPU path is parked; see
GPU_HANDOFF.md, and do not re-review it). Do not start the app with --assistant while a real one is open.
Do not send anything to fal.ai; tests that need the network are ignored. Do not write into
~/.local/share/compositor/brushes or patterns; the user's brush sets and 114 patterns live there.

Cover only what is new since round 6 (git log d304f62^..894a95c). Rank by severity, give file:line, the
input that triggers it, and what goes wrong. Run cargo build and cargo test (169 tests should pass; the
ignored ones need the network, a downloaded model or a GPU) and report anything that fails. If you can
only read files, skip that.

1. The round 6 fixes, as fixes: check each item in the status table of REVIEW_RESULTS_6.md against the
   current code.
2. Filters and adjustments in src/filters.rs: Sharpen (unsharp threshold on premultiplied channels,
   the luminosity branch's noise gate, a fully transparent layer, alpha edges), BrightnessContrast::table
   (monotonic, endpoints), Vibrance::pixel (skin term, saturation below -100 clamped), BlackWhite::gray
   (ties between channels, negative weights), PhotoFilter::pixel (preserve luminosity with a zero
   luminance), Threshold, Posterize::table (levels 2 and 255), ShadowsHighlights::apply (the blurred
   luma on transparent pixels, radius 500 on a 1 pixel layer), SelectiveColor::membership and pixel
   (whites, neutrals and blacks summing, relative vs absolute, black slider sign), ChannelMixer,
   RadialBlur::apply (center outside the layer, amount 100 zoom sampling past the edges, cost at 100
   megapixels with 24 samples per pixel), high_pass. map_straight rounding. Every new
   Adjustment variant: from_record with missing or wrong fields, record_is_valid bounds against what
   normalized() accepts, to_record round trips, is_identity short circuits, apply on alpha 0 pixels.
   ADJUSTMENT_KINDS in src/format and the validator. The GPU plan's `_ => return None` for new kinds.
3. The dialog pages in src/ui/filter_dialog.rs for every new Kind (open_adjustment mapping,
   current_adjustment, the Selective Color range dropdown and its slider sync, the Channel Mixer's
   twelve sliders, the Gradient Map bar and editor hook), and the name tables in src/ui/mod.rs,
   src/main.rs and src/ui/agent.rs (a kind present in one and missing in another).
4. Gradients: src/gradient.rs (normalized with NaN, duplicate positions, knots, at() at the ends,
   reversed twice, table, from_json with junk, presets), shape_t for each shape at the start point and
   with start == end, raster at document size (cost, the 1024-step lut), Document::gradient_fill (the
   Reflected pattern's stop order, angle and diamond on a rotated or flipped layer grid, mask target,
   preview then commit, opacity), the old gradient() wrapper, Doc's gradient_preset index against
   gradient_preset_names, the editor in src/ui/gradient_editor.rs (add on a stop's edge, drag past the
   ends, delete down to two, the picked index after normalization, the color button's callback while
   syncing, a preset chosen while dragging), the options bar preview redraw on palette change,
   GradientMap::stops in records (validation, reversed with stops, the GPU LUT).
5. Blend If: src/effects.rs BlendIf (weight at feather 0 and 127, black above white, serde defaults),
   Effects::blend_if and is_active, Renderer::styled's early return (the render() call on a 1x1 alpha
   is made three times), draw_own's `direct` and apply_blend_if (the source surface's device offset,
   HiDPI device scale, the target readable or not, the offscreen routing in draw(), a layer partly off
   canvas, a clipped layer, a layer inside a folder, blend modes and opacity after masking, the mask's
   A8 stride), and the Layer Style page (enabled/set_enabled/original_page).
6. Patterns: src/patterns.rs (clean() and names that collide, list() ordering, load on a non-PNG),
   Document::define_pattern (selection bounds vs composite, 4096 limit, transparent areas),
   fill_pattern (scale matrix direction, mask target gray conversion, selection coverage, opacity),
   StrokeKind::Pattern (tiled Sample::pixel with negative coordinates, a 1x1 pattern), the Clone
   tool's Pattern dropdown (index vs names after a new pattern is defined while the app runs), the Fill
   with Pattern dialog, the Compy tools define_pattern and fill_pattern, and `compositor patterns
   import` (src/main.rs) with abr::patterns and read_pattern in src/abr.rs (channel count 24 with
   unwritten channels, a channel rect smaller than the pattern, 16-bit depth, PackBits row tables,
   a name of 4096 chars, the budget, gray vs RGB, versions 6/7/10 and a .pat file).
7. Brush files: src/abr.rs versions 7 and 10 accepted with the version 6 record layout (is the
   subversion 2 skip of 264 bytes right for version 10; check against the RSCO files in
   ~/.local/share/compositor/brushes read-only), the 400 megapixel budget against memory,
   shrink_tip (odd sizes, factor rounding, a 1 pixel wide tip), and the picker's load path.
8. Paragraph text: TextStyle.width (serde default, from_record of an old file), layout_for with
   width and wrap, render()'s extents with a width narrower than one word, caret and index_at with
   wrapping, the Type page Width field and show(), the TextBox drag in src/ui/canvas.rs (set_text
   merging history entries per motion event, a drag that starts on an existing type layer, width below
   24, the drag when text_edit is None), Compy's width argument.
9. Path shapes: Document::add_path_shape_layer, shape_path, set_shape_path, redraw_shape's Path
   branch (baseWidth 0, a shape scaled to 1 pixel, rotation ignored: is that stated), anchors_json and
   anchors_from_json (partial handles), path_shape_image (an open path filled closed), the canvas's
   path_shape/shape_edit/shape_apply (tool switch while a stroke is in progress, applying to a
   rectangle shape), the PSD exporter with a Path shape record.
10. Compy the assistant: brush_stroke in src/ui/agent.rs (5000 points, tip lookup by substring, a
    preset's spacing and jitter, heal on a mask, pattern kind without a name, the edit wrapper around
    replay_stroke), the state's brush_tips and patterns lists (cost per call), running_job_status and
    the turn clock in send_now (a job left in JOBS after a failure, the label for generative_fill,
    status overwritten after Done), model_verb, the agent tool schemas against agent_tool's args for
    every new tool (gradient_fill, stroke_selection, select_color_range, align_layers,
    distribute_layers, define_pattern, fill_pattern, brush_stroke, layer_style's blend_if key).
11. Earlier in this range: Dodge/Burn/Sponge in src/brush.rs (alpha 0 pixels, the 0.6 factor, range
    weights at luma 0 and 1, base alpha kept), the Dodge tool's mask refusal, stroke_selection (band
    for width 1, position center with width 1, a selection touching the canvas edge, mask target),
    select_color_range (fuzziness 0, transparent pixels, all_layers false with no active layer),
    align_layers and distribute_layers (rotated layers' bounds, groups, one layer selected, the
    Distribute rounding), the header bar mark (icons::compy_mark's thread local decode), the canvas
    redraw idle after the first frame (any chance of a redraw loop), the assistant status timer.
12. Anything in tests/features_g.rs (the new tests) or the unit tests in src/gradient.rs, src/abr.rs
    and src/filters.rs that asserts the wrong value or passes for the wrong reason.

Report only; do not refactor or restyle. Write the report to
/home/estevens/code/compositor-linux/REVIEW_RESULTS_7.md. No em dashes in your output.

# Round 8 prompt

Paste everything below the line into the review assistant. Rounds 1 to 7 are fixed (see REVIEW_RESULTS.md
through REVIEW_RESULTS_7.md); this round covers the code written since, commits 663891e through 2f2e459.

---

Review the Rust project at /home/estevens/code/compositor-linux for bugs, eighth pass.

Context: a Linux rebuild of a macOS image editor named Compy, with an in-app assistant (also Compy) that
drives the document through Claude Code over a Unix socket and an MCP server. The Swift source in
reference/ is the spec (read-only); the C pixel core in csrc/ is compiled unchanged (do not edit it).
Read CLAUDE.md and README.md first. REVIEW_RESULTS.md through REVIEW_RESULTS_7.md hold the earlier
findings, all fixed; do not re-report them, but do check that the round 7 fixes hold (area 1).

Budget: if you are running low on tokens or time, stop, write what you have found so far, and end the
report with a line that says exactly where you stopped (which numbered area and which file) so the next
pass can pick up there. A partial report that says where it ended is worth more than an unfinished one.

Safety: do not run the app with COMPOSITOR_GPU set to anything (the GPU path is parked; see
GPU_HANDOFF.md, and do not re-review it). Do not start the app with --assistant while a real one is open.
Do not send anything to fal.ai; tests that need the network are ignored. Do not write into
~/.local/share/compositor/brushes or patterns, or ~/.config/compositor (swatches.json and
agent-models.json are the user's); tests that touch them set XDG_CONFIG_HOME or XDG_DATA_HOME to a temp
dir under an ENV_LOCK mutex. Do not run avifenc on anything outside a temp dir.

Cover only what is new since round 7 (git log 72e0b23..2f2e459). Rank by severity, give file:line, the
input that triggers it, and what goes wrong. Run cargo build and cargo test (178 tests should pass; the
2 ignored need the network or a downloaded model) and report anything that fails. If you can only read
files, skip that.

1. The round 7 fixes, as fixes: check each item in the status table of REVIEW_RESULTS_7.md against the
   current code.
2. Export Sizes, src/export_sizes.rs: presets() and preset_named (case, spaces, the "story or reel"
   alias), Fit::from_name, file_name (a title with a slash or a dot, two presets that collapse to the
   same name), remake (image_size's uniform scale on a document with a non-uniform target, canvas_size
   pad and crop anchors, Reframe's element rule: type layers and layers whose coverage is under 0.5
   move, does it read coverage on a rotated or masked layer, an element larger than the target,
   background with Fit::Pad on a transparent document, a 1x1 preset, a preset larger than the size
   bound), export_all (a folder that does not exist, one size failing while others succeed, the
   returned warnings), add_as_artboards (placement to the right of the last board, the canvas growing,
   names on a clash, the edit wrapper so one undo removes all boards), Document::duplicate (shared
   caches, ids kept or remade, history empty). The dialog in src/ui/dialogs.rs::export_sizes (tick
   state, custom size validation, the folder chooser's None meaning artboards) and the Compy tool
   export_sizes (sizes as objects with missing width, as_artboards ignoring folder).
3. PSD, src/psd.rs and src/psd_desc.rs: Descriptor parse (nested lists, an unknown item type, a
   length that runs past the data, a class name of length 0 meaning a 4 char key, unicode strings with
   odd lengths, Unit and Enum keys, recursion depth on a hostile file), write then parse round trip for
   every Item variant, read_tysh (the transform matrix applied to position and size, resolution, a
   font size in points vs pixels, engine_number/engine_values/engine_font_names/engine_string scanning
   the EngineData for a key that appears twice or inside a string, a type layer with a width or
   paragraph settings, missing engine data falls back to pixels with a warning), read_lfx2 and
   effects_from_descriptor (each effect's enabled flag, opacity scale, angle convention, blur vs size,
   a GrFl gradient with more than two stops or with opacity stops, patterns not exported, Blend If
   round trip or stated as not carried), effects_descriptor (keys and units Photoshop expects, the
   descriptor version, the block length padding to 4), the layer record's extra data length with both
   TySh and lfx2 present, and export with a Path shape layer. Check the round trip in tests/psd.rs
   asserts what Photoshop would read, not only what this code writes.
4. Gradient and Pattern Overlay, src/effects.rs: GradientOverlay (angle direction against
   Photoshop's, radial center and radius, reverse, opacity with the layer's own alpha, a layer partly
   off canvas, the gradient clipped to the layer's coverage via coverage_box), PatternOverlay (scale 0,
   a missing pattern name at draw time, the tile origin against the layer's position after a move,
   HiDPI), paint_one's order of the overlays against Photoshop's stacking (Color, Gradient, Pattern
   overlays under Inner Shadow and Glow), the Layer Style pages for both, serde defaults for old files,
   and the effects on a group layer.
5. History, src/document.rs history_names and step_history (negative and positive steps past the
   ends, an open edit while stepping, the names of nested edits, the redo list after a new edit),
   the History dialog (refresh after a step, selecting the current row, a document closed while the
   window is open, the Alt+H binding). Swatches in src/ui/color_wheel.rs (swatches_path with
   XDG_CONFIG_HOME unset and HOME unset, a corrupt swatches.json, the 64 cap dropping the oldest,
   remove of a color that is not there, fill_swatches on every open).
6. Rotate Canvas and Straighten: rotate_canvas (degrees normalized to (-180, 180], 90 exactly vs
   89.9999, the canvas size after an arbitrary angle, layer transforms composed with the turn, masks
   and vector shapes and type layers turned, guides and artboards after the turn, the selection
   dropped or turned, one undo), straighten (a == b, a vertical line chooses vertical, the crop after
   the turn, points outside the canvas), the angle dialog, the Rotate Canvas submenu actions.
7. WebP, GIF, AVIF, Export Layers: export_webp (lossless flag, alpha, a document wider than 16383,
   the WebP limit), export_gif (the 256 color quantization on a photo, transparency index, alpha
   threshold, a fully transparent document), export_avif (avifenc missing or failing, quality 0 and
   100 mapping, the temp PNG cleaned up, a path with spaces, stderr surfaced), export_layers (trim on
   an empty layer, numbering by written count, hidden layers and groups skipped or flattened, a name
   with a slash, masks applied or not, effects included or not), the Compy tools export_layers and
   the quality dialog.
8. Artboards, src/format (Artboard struct, Layer.artboard on a non-group rejected by validate.rs, old
   files without the field, negative or zero size), Document::add_artboard (name kept unless it
   clashes, background None vs transparent), set_artboard_frame (shrinking below the layers, moving
   layers with the frame or not, undo), artboard_at (the 18 px name strip in document units vs zoom),
   nudge_artboard, fit_canvas_to_artboards (a board at negative coordinates, the canvas shrinking and
   layers outside any board), import_as_artboard (a source with its own artboards, ids remade),
   artboard_from_layers (rotated layers' bounds, one layer, layers from different groups, a layer that
   is already inside a board), export_artboards (a board partly off canvas, the name as a file name,
   jpeg on a transparent board, overlapping boards). Renderer: draw_artboard_backgrounds order,
   artboard_frame, draw_within_artboard's clip on HiDPI and with the frame cache, set_artboard clearing
   placed, has_artboards turning the checkerboard off, effects of a layer inside a board spilling past
   the frame, a mask or clipping mask inside a board, blend modes against the board background. Canvas
   board_drag (begin_board_drag with the Move tool only, drag past the canvas edge, the label overlay
   at every zoom, a board drag while a transform is in progress). Layers panel kind "artboard" and its
   glyph. The Compy tools new_artboard (preset lookup, x and y given, transparent), move_artboard
   (by name vs id, resize with negative width), the PSD exporter with artboards (flattened or
   stated).
9. Today's UI changes (commit 2f2e459): the menu in src/ui/mod.rs::menu() and sections(): every
   action string it references exists in the actions table (including the parameterized
   filter::, new-adjustment::, align:: and distribute:: ones), nothing that was in the round 7 menu is
   missing now (diff the two), duplicate entries (Free Transform is in Edit and Layer > Transform,
   Duplicate Layer and Layer via Copy are the same action: fine if intended, report if any label lies
   about what its action does), the SHORTCUTS table and README against the new places. center_spins
   (recursion cost on a wide widget tree, SpinButton inside a Popover child). The color picker in
   src/ui/color_wheel.rs: region() at the gap and past the strip, pick() with hue 360 wrapping to
   359.999 (marker at the bottom row), the square image regenerated on every hue drag step (cost at
   256x256), the RGB entries' connect_changed firing three times during sync_channels (syncing flag),
   a typed value above 255 or empty, hex typed with 3 digits, set_color keeping the hue on gray,
   the strip marker position for hue near 360, the marker contrast rule. The layer row cursor
   (set_cursor_from_name("pointer") on a ListBoxRow: does it override the eye button's and drag
   handle's cursors), the assistant header mark (compy_mark(16) repainting on theme change), the
   start page with the logo removed (spacing left behind), install.sh still finding the logo for the
   icon.
10. Tests: anything in tests/features_g.rs (export sizes, rotate/straighten/history/webp/layers/
    overlays, gif/avif, artboards), tests/psd.rs (layer styles round trip) or the unit tests in
    src/export_sizes.rs, src/psd_desc.rs and src/ui/color_wheel.rs that asserts the wrong value or
    passes for the wrong reason (a tolerance wide enough to hide a bug, a test that only checks the
    file exists).

Report only; do not refactor or restyle. Write the report to
/home/estevens/code/compositor-linux/REVIEW_RESULTS_8.md. No em dashes in your output.
