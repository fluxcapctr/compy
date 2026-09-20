# Review results, round 6

Reviewed on 2026-09-19 at HEAD `fe931c5`. The requested implementation scope was commits `c5d5b17^..730037a`. Earlier findings are not repeated. No application source files were changed.

`cargo build` succeeded. `cargo test` succeeded: **158 passed, 0 failed, 2 ignored**. The ignored tests are the Google Fonts network test and the Remove Background model test. The GPU integration test reported success without creating a GPU because GPU use was not enabled. The prompt expected 157 passing tests.

## Findings

### 1. [P1] A generated result lands on whichever document and selection are current when polling finishes

**Locations:** `src/ui/agent.rs:154-165`, `src/ui/agent.rs:320-333`, `src/ui/agent.rs:371-385`, `src/ui/agent.rs:401-435`.

Start Generative Fill in document A, then switch to document B before fal returns. `_poll` stores no document id, page reference, selection, or source-layer id. When the job completes, `agent_land_fill` calls `with_current` and inserts the result in document B. If all documents were closed, the closure never runs and the tool returns a successful null result, losing the paid result.

There is a second variant without changing tabs. `genfill_inputs` sends the selection that existed when the job started, but `apply_genfill` builds the returned layer mask from the selection that exists when the job finishes. Changing or clearing the selection while fal is working makes the result cover a different area from the one sent to the model. Layer-mode jobs are likewise inserted relative to the active layer at completion rather than their source layer. Each job needs to retain and validate its originating document, source layer, placement, and selection snapshot before landing.

### 2. [P1] Agent tools can consume or discard an open Free Transform

**Locations:** `src/ui/agent.rs:173-220`; `src/document.rs:707-726`; nested history behavior at `src/history.rs:46-70`.

Start Free Transform on selected pixels, then let the agent call `delete_layer`. The floating layer is active, so the tool deletes it while `Document::floating` still points to it. Pressing Return, saving, or any other path that calls `commit_free_transform` reaches the missing-layer branch at `src/document.rs:722`, closes the outer edit, and commits the source layer with the selected pixels cut out and the floating copy gone. Undo may recover the state, but the current document has lost those pixels.

Other agent edits made while Free Transform is open nest into its history transaction. Escape then rolls them back even though their tool calls reported success, while Return records them under the single `Free Transform` step. Agent entry should either resolve the open UI edit first or reject tools that cannot safely run during it.

### 3. [P1] Starting a second app instance steals the first instance's agent socket

**Location:** `src/ui/agent.rs:30-33`.

`serve` unconditionally unlinks the socket path before binding. Unix permits unlinking a listening socket, so launching a second Compy makes the first listener unreachable and binds the shared path to the second app. The first window's assistant and any existing MCP clients then address the wrong application or fail. Stale-socket recovery should first probe ownership and only unlink after confirming that no live listener owns the path.

### 4. [P1] Resetting a conversation lets the old turn control the new child process

**Locations:** `src/ui/agent.rs:625-633`, `src/ui/agent.rs:678-724`.

Press New Conversation during a running turn and immediately send another message. `reset` kills the old PID and clears `busy`, allowing the new turn to replace the shared `child_pid`. When the old turn's reader later receives its `done` event, its timer clears the new PID and the new turn's busy state at lines 703-706. The UI now permits a third concurrent turn, and the new child can no longer be stopped through the stored PID.

The timers have no per-turn identity. An old watchdog can also take and kill the new turn's PID once its own five-minute deadline arrives. Process state, queue draining, and watchdog actions must be tied to a unique turn or child rather than shared cells.

### 5. [P1] A valid model config with an empty field causes unbounded recursion and aborts the app

**Location:** `src/genfill.rs:90-107`.

Put `{"generate":"","edit":""}` in `agent-models.json`, then ask Compy to generate or edit an image without an explicit model. `resolve_model("")` reloads the same valid empty field and recursively calls itself forever. Rust eventually aborts on stack overflow. Validate both configured strings and fall back directly to a nonempty default without recursive config lookup.

Malformed JSON takes a separate destructive path: `agent_models` silently overwrites the user's file with defaults. A temporary parse failure or partially written file therefore destroys the configured model choices instead of reporting and preserving it.

### 6. [P1] Agent image resizing can request multi-gigabyte surfaces on the GTK thread

**Locations:** `src/ui/agent.rs:283-284`; `src/document.rs:2786-2810`.

Call `image_size` with 30,000 by 30,000 on a document containing a full-canvas pixel layer. The values pass the per-side validation, and resampling attempts to allocate a roughly 3.6 GB ARGB surface on the GTK thread. This can freeze Compy or make the operating system kill it. `canvas_size` has the same 900-megapixel acceptance and can allocate a huge selection or extension layer. The existing transform and filter paths use a 100-megapixel aggregate bound; these agent-exposed resize paths need an equivalent total-pixel and allocation check before mutation.

### 7. [P2] Autosave can resurrect a document that the user closed and chose not to save

**Locations:** `src/ui/mod.rs:963-983`, `src/ui/mod.rs:1037-1046`.

Close a modified tab before its first autosave and choose to discard the changes. `close_page` removes the notebook page but never removes its `Page` from `App::pages`. At the next two-minute tick, `autosave_all` still iterates that closed, modified document and writes a recovery package for work the user explicitly discarded.

An already running worker creates a related race. Saving or closing calls `autosave::discard`, but an earlier detached worker can finish afterward and recreate the package. Overlapping workers can also finish out of order and replace a newer autosave with an older snapshot. Closed pages must leave `App::pages`, and autosave writes need per-document generations or cancellation plus an atomic replace.

### 8. [P2] A failed autosave is treated as captured and open edits are serialized as committed state

**Locations:** `src/ui/mod.rs:1037-1045`, `src/document.rs:2277-2285`, `src/autosave.rs:39-49`.

`autosave_serial` is advanced before the worker writes. If the disk is full, the directory is read-only, or package creation fails, later ticks skip the document until another edit changes its serial. A document left untouched after that failure has no current recovery copy despite repeated timer ticks.

The snapshot also serializes the live renderer without resolving transient edits. During Free Transform it writes the cut source plus `Floating Selection` as ordinary layers, with no linkage needed to commit or cancel the operation after recovery. During Layer Style it writes the previewed effect even if the user subsequently presses Cancel. Previewing and cancelling does not change `edit_serial`, so that incorrect snapshot is not replaced. Mark a serial complete only after a successful worker result, and snapshot a defined committed or safely recoverable edit state.

### 9. [P2] Fill Path and Stroke Path paint the image while a layer mask is targeted

**Locations:** `src/document.rs:1192-1220`, `src/document.rs:1483-1517`.

Target a layer mask, draw a Pen path, and choose Fill Path or Stroke with Brush. `fill_path` always obtains `active_image` and replaces the image surface. `stroke_path` calls `replay_stroke`, whose `begin_stroke` never selects the mask preview path or sets `stroke_mask`. Both commands therefore change image pixels behind the mask while the UI says the mask is the active target. The ordinary Fill and live Brush paths correctly branch to the active mask.

### 10. [P2] A misspelled Layer Style key clears every existing effect

**Locations:** `src/agent.rs:61`, `src/ui/agent.rs:269-281`.

Give a styled layer a call such as `{"drop_shdow":{"size":10}}`. The schema allows additional properties, and `agent_tool` ignores the unknown key, starts from `Effects::default`, and passes that empty value to `set_effects`. The document clears all effects and returns `"styled"`. Wrong value types also enable an effect with defaults because the code only tests whether the key exists. Reject unknown keys and invalid object fields before changing the layer.

### 11. [P2] Several successful agent calls create multiple undo steps, and an error can leave a partial edit

**Locations:** `src/ui/agent.rs:195-204`, `src/ui/agent.rs:240-254`.

`new_layer` with a name records `New Layer` and `Rename Layer` separately. `set_layer` can record visibility, opacity, and blend as three steps. `adjustment_layer` first creates the layer, then records its settings, then optionally records clipping. This violates the tool contract that one call is one undoable step.

The same structure makes failures non-atomic. For example, `set_layer` can apply visibility and opacity before discovering an unknown blend string and returning an error. The model is told the call failed even though two changes remain in the document. Validate all arguments first, then wrap each mutating tool in one outer edit and abort that edit on every error.

### 12. [P2] The socket and MCP paths can wait forever or silently drop requests

**Locations:** `src/agent.rs:82-95`, `src/agent.rs:98-140`, `src/ui/agent.rs:39-56`.

`call` sets no connect, read, or write deadline and does not verify that the returned response id matches its request. A UI thread that is blocked, a request without its terminating newline, or a job whose poll never completes leaves the MCP server and Claude turn waiting indefinitely. The server-side job loop also has no deadline of its own.

`run_mcp` silently continues on malformed JSON at line 104. A JSON-RPC peer waiting for the required parse-error response hangs instead. Missing or wrongly typed `tools/call` fields are converted to empty names or empty arguments rather than an invalid-params response. Add bounded waits, id validation, and protocol error responses.

### 13. [P2] Cancelling the assistant does not cancel a paid fal job

**Locations:** `src/ui/agent.rs:327-332`, `src/ui/agent.rs:375-381`, `src/ui/agent.rs:625-627`; cancellation support at `src/genfill.rs:242-266`.

Every agent generation passes `&|| false` to the fal backend. Killing or resetting Claude only closes the MCP client process; the detached fal thread keeps running and the socket handler keeps polling even after its eventual write can no longer reach that client. The request can still incur a charge and, while the app remains open, can later land an image the user believed was cancelled. Connect client/process cancellation to the job's cancellation closure and remove abandoned jobs.

### 14. [P2] Assistant process and voice lifecycle state survives beyond the event that owns it

**Locations:** `src/ui/agent.rs:574-597`, `src/ui/agent.rs:650-719`; ownership fields at `src/ui/agent.rs:441-464`.

The assistant has no shutdown path that kills its child when the app exits. `App` owns `Rc<Assistant>` and `Assistant` owns `Rc<App>`, forming a cycle, so dropping the window does not run a useful cleanup even before process exit. A Claude child can remain running after Compy closes.

The watchdog is based on total elapsed time, despite its comment saying the turn went quiet. It kills an active turn at five minutes even if replies and tool events continue. Config and skill directory creation errors are ignored before spawning, so a read-only config directory leaves a stale session id and produces opaque child failures.

Voice has a separate focus bug. `watch_voice` only starts dictation when the main application window is active. When the Compy popout has focus, the main window is inactive, so Page Down can type words into the focused popout entry but `dictating` is never set and the 900 ms auto-send never runs. Manual Send also does not clear `dictating`, allowing a later entry change to be auto-sent by the old recording state.

### 15. [P2] Queued chat messages are shown twice

**Locations:** `src/ui/agent.rs:636-652`, `src/ui/agent.rs:715-719`.

Type a second message while Compy is busy. `submit` appends it to the transcript and pushes it to the queue. Completion removes it from the queue before scheduling `send_now`; `send_now` therefore no longer finds it in the queue and appends it again. Identical messages elsewhere in the queue can make the behavior inconsistent. Store whether a message was already displayed, or leave it queued until after `send_now` consumes that state.

### 16. [P2] Non-PNG generation results are stretched instead of fitted to their actual aspect ratio

**Locations:** `src/ui/agent.rs:401-423`, `src/genfill.rs:155-161`; general decoding at `src/document.rs:2690-2701`.

Upscale, relight, or a custom fal model can return JPEG or WebP bytes. `add_image_layer` can decode those formats, but the fit calculation first calls `png_size`. For every non-PNG it leaves the requested `place` dimensions unchanged, so a result whose model-selected aspect ratio differs is stretched. Determine dimensions with the same general decoder used to create the layer, and reject zero or invalid placement sizes before calculating the scale.

### 17. [P2, opt-in] GPU texture identity can reuse stale pixels, and presentation buffers have no consumer release tracking

**Locations:** `src/gpu/mod.rs:14-17`, `src/gpu/mod.rs:240-270`, `src/gpu/present.rs:113-127`, `src/ui/canvas.rs:1696-1713`.

The cache key identifies a Cairo surface only by layer id, slot, and raw pointer. Replace a layer surface after its prior surface is freed, and the allocator can reuse the same pointer. The new ordinary image has no dirty rectangle, so `upload` treats the old GPU texture as current and never uploads the replacement. Because each use refreshes the cache frame, the stale texture can remain indefinitely.

The presentation path rotates three dma-bufs on the assumption that GTK has finished with a buffer before its fourth use. It gets no release fence or callback from the GDK texture and can also clear the buffer set on resize while textures backed by those buffers are still queued by the compositor. If this path is enabled, a slow compositor can read memory while Vulkan overwrites or destroys it. The code correctly gates both GPU modes behind exact `COMPOSITOR_GPU=present` or `COMPOSITOR_GPU=readback` values, so no GPU setup runs by default.

### 18. [P3] Some tool failures are reported as success and empty type layers remain behind

**Locations:** `src/ui/agent.rs:168-170`, `src/ui/agent.rs:256-268`; error handling in `src/ui/mod.rs:911-925`.

`open` calls the UI helper that displays its own error and returns no status, then always returns `"opened"`. Opening a missing or unreadable path therefore tells Claude it succeeded even though no tab was added. `text_layer` accepts an empty string and leaves an empty type layer, while the interactive Type tool deletes an empty layer when editing ends. `set_text` can likewise turn an existing type layer into an empty persistent layer. Agent operations need result-bearing open handling and the same empty-text lifecycle as the UI.

### 19. [P3] A Pen handle parked on its anchor makes the anchor unreachable

**Locations:** `src/path.rs:79-87`, `src/ui/canvas.rs:1184-1194`.

Drag an existing incoming or outgoing handle exactly onto its anchor, then try to drag the anchor. The handle remains `Some(anchor)`, and hit testing checks handles before anchors, so every click at that point selects the zero-length handle. The anchor cannot be picked up again. A newly placed handle also retains its last nontrivial position if the pointer is dragged out and then returned within one pixel of the anchor because the `Place` branch never clears earlier handles. Normalize a handle at its anchor to `None`, or prefer the anchor when the handle has zero length.

## Test coverage findings

The GPU test at `tests/gpu.rs:35-38` is counted as passed when `Gpu::new()` returns `None`. Since `Gpu::new` itself requires the opt-in environment variable, the normal suite exercises none of the GPU plan, shader, upload, readback, or presentation code and does not report the test as ignored.

The Pen unit test at `src/path.rs:117` is tautological: it compares `p.flatten(5.0).len()` with the same expression. It cannot detect an unstable or wrong point count. The Pen integration test does not target a mask, exercise one-point operations, zero-length handles, undo overlay state, or malformed `--path` input.

The autosave test waits for its worker before discarding, so it cannot expose overlapping writes, a worker finishing after save or close, a failed write suppressing retries, a closed page retained in `App::pages`, or snapshots taken during Free Transform and Layer Style. The agent and model tests do not exercise the socket server, JSON-RPC errors, timeouts, response ids, job ownership, cancellation, malformed model config, wrong argument types, or any `App::agent_tool` implementation.

## Round 5 fix verification

All nine round 5 fixes still hold in the current code. Oversized Free Transform aborts, attached masks grow white, Layer Style preview state stays outside history and save output, duplicate style sessions are refused, mask histograms are refused, Fade uses `edit_serial`, styled cache keys include canvas size, Color Balance redistributes clipped luminosity, and transform handles are unparked on the required paths.

## Coverage limits

All twelve numbered areas in the Round 6 prompt were reviewed. GPU files were read only, and the application was not started with GPU or assistant flags. No network, model, or fal.ai operation was run. UI focus, window-manager, dma-buf consumer lifetime, and process shutdown findings are based on static lifecycle analysis; the build and nonignored automated suite were run locally.

## Status after fixes

| # | Finding | Status |
|---|---------|--------|
| 1 | Results land on whichever document is current | Fixed. A job remembers its document id, source layer and selection; the result lands there (a mask from the selection the model was given) or is refused with a message when that document is closed. |
| 2 | Tools consume an open Free Transform | Fixed. A mutating tool lands a floating transform first (as Return does), ends on-canvas typing, and is refused while a stroke, a warp or a Layer Style preview is open. |
| 3 | A second app steals the socket | Fixed. `serve` connects first; a live listener keeps its socket and the new window runs without an agent. |
| 4 | Reset lets the old turn control the new process | Fixed. Turns are numbered; a timer only acts while its number is current, clears only its own pid, and a reset or shutdown bumps the number and kills the child. |
| 5 | Empty model config recurses; malformed config overwritten | Fixed. `resolve_configured` falls back without recursion; a file that does not parse is left alone and the defaults serve. Unit tests. |
| 6 | Multi-gigabyte resizes | Fixed. `image_size` and `canvas_size` refuse more than 100 megapixels. Test. |
| 7 | Autosave resurrects a closed document | Partly a false alarm: `remove_page` fires page-removed, whose handler prunes `App::pages`. The worker races are fixed: one worker per document at a time, and a write that finishes after a discard removes itself. |
| 8 | Failed autosave counted; open edits serialized | Fixed. The serial is marked done from the worker only after a successful write; nothing is written while an edit is open (Free Transform, Layer Style preview, a stroke). |
| 9 | Fill Path and Stroke Path paint the image behind a mask | Fixed. Both target the mask when it is the target (`replay_mask_stroke`). Test. |
| 10 | Misspelled Layer Style key clears every effect | Fixed. Unknown keys, non-object values, unknown fields, bad colors and non-numbers are refused before anything changes. |
| 11 | Several undo steps per call; partial edits on error | Fixed. Every mutating tool runs inside one edit named for the tool and aborts it (restoring the state) on any error; set_layer validates first. |
| 12 | Waits forever; silent drops | Fixed. The client times out after eleven minutes and checks the response id; the server abandons a job after ten; malformed JSON-RPC gets a parse error, a call without a name an invalid-params error. |
| 13 | Cancelling the assistant does not cancel the fal job | Fixed. The socket thread watches for the client hanging up and sends `_cancel`, which flips the job's cancellation flag (fal's cancel URL is called) and drops the job. |
| 14 | Child survives the app; watchdog; voice focus | Fixed. Closing the window kills the running turn; the watchdog fires after five quiet minutes, not five total; the pop-out window counts as focused for dictation; a manual Send ends dictation; config write failures are reported. |
| 15 | Queued messages shown twice | Fixed. `submit` shows a message once; `send_now` no longer appends. |
| 16 | Non-PNG results stretched | Fixed. The result is decoded once with the general decoder and fitted from its real size; a zero-sized place falls back to the picture's own size. |
| 17 | GPU texture identity and buffer release | Identity fixed: surfaces carry a unique id in cairo user data instead of their pointer. Presentation buffer release is not addressed; the path stays parked and off by default. |
| 18 | open reports success on failure; empty type layers | Fixed. `open_path` returns whether it opened; an empty text is refused for text_layer and set_text. |
| 19 | A Pen handle parked on its anchor | Fixed. A handle dragged within a pixel of its anchor becomes no handle. |
| Tests | Tautological Pen assertion | Fixed (coarser step gives fewer points; one point flattens to itself). The GPU test still returns without a GPU, by design. |
