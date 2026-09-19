# Second bug review

## Status (2026-09-18, after fixes)

All nine findings and the raw-access soundness note are fixed; each has a regression in `tests/review2.rs`
(the two malformed PSDs are kept under `tests/fixtures/psd/`). The suite is 85 tests.

| # | Finding | Fix |
| --- | --- | --- |
| 1 | Tab close button and window close skipped the unsaved prompt | `close_page` is the one path; the window's close request walks modified pages through it |
| 2 | Images decoded before the size budget | The decoder's header dimensions are checked before any pixel buffer |
| 3 | Brush tip unbounded on downscaled layers | Tips past 3,000 layer pixels (the reference's `gridTipLimit`) refuse the stroke with a message |
| 4 | PSD channel skips past EOF | Every skip is bounds-checked (`Reader::skip`), the length check comes first |
| 5 | PSD Unicode name count unbounded | The count must fit the block; reads are fallible |
| 6 | Adjustment dialog on a deleted layer panicked | `Document::set_adjustment` returns false for a missing layer; the dialog reports it |
| 7 | Visibility, opacity, blend not edits | `Document::set_visible/set_opacity/set_blend_mode` are undo steps; slider steps merge within 1.5 s |
| 8 | Non-UTF-8 command-line paths panicked | `args_os`; only option values are matched as text |
| 9 | Adjustment settings of the wrong JSON type accepted | Present containers and values must be the right type; only absent ones default |
| note | Raw accessors could alias | A per-thread ledger in `raster.rs`: readers share, a writer is alone, aliasing is refused |

The original report follows.

Reviewed 2026-09-18 at commit `53e1fac9838bacfe85eea8257e4588c31af1ecaa`.
Report only. No application code, tests, Swift reference files, or C sources were changed.
The existing user modification to `REVIEW.md` was preserved.

`cargo build` passed. `cargo test` passed all 78 tests. Additional standalone probes
were compiled against the current debug library. GUI findings below are based on
callback inspection; the GTK interactions themselves were not exercised. The
Swift implementation was read, not executed.

## Findings, ranked by severity

### 1. [P1] Closing a tab bypasses the unsaved-changes check

Locations: `src/ui/mod.rs:483`, `src/ui/mod.rs:534`.

Paint in a document, then click its tab's close button. The callback calls
`notebook.remove_page` directly. The unsaved confirmation lives in `close_current`,
which this callback never calls. The tab disappears without Save/Discard/Cancel,
losing access to the unsaved work. Main window construction also has no close-request
handler to protect modified documents when the window is closed.

Evidence: static callback tracing. Route all document-closing entry points through
one document-specific save/discard flow, including main-window close.

### 2. [P1] Large image opening allocates before checking the size budget

Location: `src/document.rs:1070` through `src/document.rs:1076`.

The ordinary image loader decodes the image before checking its 30,000-pixel side
and 100-megapixel limits. A PNG advertising 30,000 by 30,000 RGBA pixels requests a
3,600,000,000-byte allocation before the application can reject it. This path is
shared by ordinary image opening, including the GUI's `open_document`.

Reproduced by calling `ui::open_document` on a tiny PNG with a valid IHDR declaring
those dimensions and a zlib-compressed single zero byte as IDAT. In a child with a
256 MiB address-space limit, the process aborted with:

```text
memory allocation of 3600000000 bytes failed
```

This is an allocation-order bug, not a requirement to decode a complete 900-megapixel
file. Check decoder dimensions and the budget before materializing pixel buffers.
The `.comp` PNG loader's header-check callback already follows that ordering.

### 3. [P1] A small scaled layer can make a brush allocate hundreds of terabytes

Locations: `src/brush.rs:183` through `src/brush.rs:185`, `src/brush.rs:82` through
`src/brush.rs:87`. Reference: `reference/Compositor/Document/BrushStroke.swift:111`
and `reference/Compositor/Document/BrushStroke.swift:183`.

Brush construction divides the document-space diameter by the layer's X scale and
unconditionally allocates a square tip at that resulting pixel-grid diameter.
The limits on the preview grid do not limit the brush tip.

Reproduced with a valid 1 by 1 document, a 10,000 by 1 pixel layer transformed to
1 by 1 document units, and a brush diameter of 2,000. Beginning a stroke requested
400,000,000,000,000 bytes and aborted in a child limited to 256 MiB. The document
and source image themselves are small.

The Swift implementation limits its grid tip to 3,000 pixels and uses other stamp
or procedural paths when it cannot use that tip. Bound allocation and provide the
corresponding fallback before calling `tip`.

### 4. [P1] PSD channel skipping can still panic past EOF

Location: `src/psd.rs:271` through `src/psd.rs:272`.

The branch for channels shorter than two bytes, or empty rectangles, advances
`r.pos` without checking the remaining file length. A following ordinary channel
then evaluates `data.len() - start` with `start` beyond EOF.

Reproduced with a 102-byte PSD: RGB 8-bit, 1 by 1 canvas, one 1 by 1 layer, two
channel declarations `(id=0, length=1)` and `(id=1, length=2)`, and no channel
payload after the layer record. Both lengths individually pass the new
`length <= data.len()` check. The first skip passes EOF; the next channel panics:

```text
thread 'main' panicked at src/psd.rs:272:30:
attempt to subtract with overflow
```

Observed in the required debug build. Release arithmetic can behave differently;
this is not a claim that this exact input panics in release. Validate each channel's
range before every skip/read, using checked bounds. This is an incomplete portion
of the first review's PSD validation fix.

### 5. [P1] A tiny PSD Unicode-name block can occupy the UI for billions of iterations

Location: `src/psd.rs:249`, the `luni` handler.

The Unicode name's declared character count drives `(0..n).filter_map(...)`.
Failed reads are ignored, so EOF does not end the loop. Neither the count nor the
read is bounded by the additional-info block's declared payload length.

Reproduced with a 106-byte PSD containing one layer and an `8BIM/luni` block whose
payload length is four and whose character count is `0xffffffff`. The importer
kept consuming CPU until the child process's two-second CPU limit killed it
(exit signal 9). A truncated name should return an error immediately. GUI imports
run synchronously, so the same parsing loop blocks interaction.

Validate that the count fits inside the block, then read it fallibly rather than
swallowing EOF. This is another remaining PSD validation gap.

### 6. [P1] An adjustment dialog can call into a deleted layer and panic

Locations: `src/ui/filter_dialog.rs:70`, `src/ui/filter_dialog.rs:89`,
`src/ui/filter_dialog.rs:346`, `src/render/mod.rs:467`.

Adjustment dialogs are nonmodal and capture the target layer UUID. Open an
adjustment editor, then delete its layer or undo its creation in the main window.
Moving a slider, toggling preview, or cancelling the dialog still calls
`set_adjustment` with the old UUID. The renderer indexes `self.index[&id]` without
checking whether the layer exists.

The underlying callback operation was reproduced by creating a document, retaining
its layer ID, deleting that layer, and calling `Document::set_adjustment` with the
retained ID. It panicked at `src/render/mod.rs:467` with `no entry found for key`.
The full GTK interaction was not run. A panic escaping a GTK callback can terminate
the application and lose work in other tabs.

Cancel or invalidate dialogs when their targets disappear, and check the target
before applying or restoring adjustments. The Swift adjustment update path checks
that the layer still exists (`LayerAdjustment.swift:108`).

### 7. [P2] Visibility, opacity, and blend changes do not become document edits

Locations: `src/ui/layers.rs:110`, `src/ui/layers.rs:122`,
`src/ui/layers.rs:166`, `src/document.rs:866`.

These controls call renderer setters directly without a document history
transaction. The renderer's redraw revision changes, but `Document::is_modified`
uses the history revision. On an otherwise clean document, these changes therefore
leave the document marked clean and cannot be undone. Even the guarded Ctrl+W
close path skips its save prompt.

A probe performing the same opacity setter on a clean document printed:

```text
dirty before false
dirty after opacity false undo false
```

The other two callbacks have the same transaction omission. Use document edit
operations, coalescing slider changes into a sensible undo step.

### 8. [P2] Non-UTF-8 command-line paths panic before opening

Location: `src/main.rs:11`.

`std::env::args().collect::<Vec<String>>()` panics on Linux arguments containing
non-UTF-8 bytes. A valid filesystem path does not have to be UTF-8.

Reproduced by launching the binary with the byte argument
`b"/tmp/nonutf8-\xff.png"`. It exited 101 with a panic in `std::env` converting the
argument to a string. The file need not exist: the panic happens before file access.
Use `args_os`/`PathBuf` for file arguments and decode only textual options.

### 9. [P2] Adjustment validation still accepts settings with the wrong JSON type

Locations: `src/filters.rs:612` through `src/filters.rs:643`, particularly
`src/filters.rs:622` and the numeric defaulting at `src/filters.rs:625`.
Reference: `reference/Compositor/Document/LayerAdjustment.swift:28`.

The validator checks nested members only when lookup finds them. For example,
`{"kind":"Levels","levels":42}` deserializes into the Rust adjustment record,
and a probe confirmed that `Adjustment::record_is_valid` returns `true`.
A number has no `ranges` member, so validation skips the whole Levels check and
the reader substitutes defaults. Explicit nonnumeric range values can likewise
turn into valid numeric defaults through `as_f64().unwrap_or(...)`.

Swift decodes these fields into typed settings and rejects a number where a Levels
settings object is required. Rust can silently accept damaged or incompatible
metadata and render an identity adjustment instead of reporting the bad input.
This is an incomplete part of first-review finding 9, rather than a re-report of
the numeric bounds already fixed. Validate present containers and values for type
before applying backwards-compatibility defaults to genuinely absent fields.

## Additional audit notes and limits

- The first-round regression suite passes. I did not establish another concrete
  failure in placed-mask restoration, transparent Clone Stamp source-over, the
  adjustment coverage rectangle, or the Image Size rollback fix. Passing those
  examples does not establish parity for every combination.
- The surface-access audit covered the raw accessors and their call sites in
  raster, renderer, document, brush, warp, filters, selection, PNG, and PSD code.
  Raw writes call `mark_dirty` on normal return; the checked mutable accessor uses
  Cairo's data guard. Existing read-only Context/pattern references alone do not
  demonstrate a use-after-free. No specific live-edit mutation of a history-owned
  surface was reproduced.
- The public safe raw-access API remains insufficiently constrained: nesting
  `with_bytes_raw_mut(&surface, ...)` inside `with_bytes(&surface, ...)`, then using
  the outer slice, can invalidate Rust's aliasing assumptions. Both calls accept
  shared surface references and neither tracks active borrows. This is an API
  soundness concern, not a demonstrated GUI execution path. Restrict the interface
  or enforce its access contract; ordinary refcount sharing is not itself proof
  that simultaneous byte access is safe. No Miri or sanitizer run was performed.
- One suspected stale healing-preview cache issue was probed and did not reproduce;
  it is deliberately excluded from the findings.
- Calls to the shared `ui::open_document` function returned ordinary errors for
  zero-byte PNG and PSD files and a `.comp` manifest consisting of `[]`. The
  oversized PNG and non-UTF-8 CLI failures are described above.
- Open-dialog and drag-and-drop interactions, symlink handling, extensionless paths,
  missing image assets, screenshot automation, dialog overlap, and closed-tab
  resource reclamation were inspected only to varying degrees and were not all
  exercised end to end. This report does not claim that entire input matrix passed.
- Brush/warp and test assertions were reviewed, but no additional warp-specific bug
  or demonstrably wrong assertion is claimed here. The existing 78 tests do not
  cover the new failures above.

## Handoff and reproductions

Temporary probe artifacts from this session are in `/tmp/compositor-review-2/`:
`probes.rs`, its compiled binary, `skip-past-eof.psd`, `unicode-count.psd`, and
`large.png`. These are temporary local artifacts, not committed fixtures. Preserve
or recreate them before relying on them in a later environment.

PSD import reproduction:

```sh
target/debug/compositor psd /tmp/compositor-review-2/skip-past-eof.psd /tmp/compositor-review-2/result.comp
```

Run the Unicode-count and allocation probes only in child processes with CPU or
address-space limits and core dumps disabled, as in this review. Add regression
coverage alongside each eventual fix, including the actual GUI close/edit paths
where feasible. Keep `reference/` and `csrc/` unchanged.
