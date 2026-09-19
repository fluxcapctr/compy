# Bug review results

## Status (2026-09-18, after fixes)

All eleven findings are fixed; each has a regression in `tests/review.rs` (the malformed PSD inputs are
kept under `tests/fixtures/psd/`). The whole suite is 78 tests.

| # | Finding | Fix |
| --- | --- | --- |
| 1 | Painted blank layers not saved | Renderer names the image file whenever a surface is set or adopted |
| 2 | PSD rectangles allocated before validation | `rect_dims` checks every rectangle; channel lengths and a decoding budget are checked before any buffer |
| 3 | PSD missing channels panic | Channel counts validated against the color mode in the header |
| 4 | Hue/Saturation dictionaries in the wrong shape | Flat key/value arrays read and written (objects still read); `invertRange` carried and applied |
| 5 | Undo kept placed masks for the old geometry | `restore` drops placed masks whose transform or placement changed |
| 6 | Clone Stamp erased from transparent samples | Premultiplied source-over with the sample's alpha |
| 7 | Adjustment masks leaked past their rectangle | Coverage clipped to each mask's rectangle; own mask uses its placement |
| 8 | Failed Image Size left history open | `History::cancel` and `Document::abort_edit` roll the edit back |
| 9 | Adjustment settings not validated | `Adjustment::record_is_valid` ports `LayerAdjustment.isValid`; the loader calls it |
| 10 | Undo to the saved state read as modified | Entries carry the revisions they move between |
| 11 | Tautological adjustment round-trip test | Compares against the value that was set and checks the written shape |

The original report follows.

Reviewed on 2026-09-18 against the current working tree at
`/home/estevens/code/compositor-linux`, following `REVIEW.md`.

This is a report for handoff to Claude Code. No implementation fixes were made.
Read `CLAUDE.md` and `README.md` before making changes. The Swift source in
`reference/` is the specification; leave `reference/` and `csrc/` unchanged.
Line numbers below refer to the working tree at review time and may shift.

## Validation and scope

- `cargo build`: passed.
- `cargo test`: all 69 tests passed, with no failures.
- Additional small probes reproduced the failures described below.
- Swift behavior was compared by source inspection. The macOS app was not run.
- PSD allocation probes ran in child processes with a 256 MiB address-space
  limit and core dumps disabled.
- This review does not establish the absence of other bugs, including unsafe
  aliasing or surface lifetime problems.

Existing user changes were present in `Cargo.lock`, `Cargo.toml`, `README.md`,
`install.sh`, `src/document.rs`, `src/main.rs`, `src/ui/dialogs.rs`,
`src/ui/mod.rs`, and `tests/psd.rs`; `REVIEW.md` was untracked. Preserve those
changes. This report was added after the review at the user's request.

## Findings, ranked by severity

### 1. [P1] Saving drops pixels painted onto blank layers

**Locations:** `src/format/mod.rs:291`, the preview adoption paths in
`src/render/mod.rs`, and `Document::manifest` in `src/document.rs`.

Painting a blank layer creates its image surface but does not populate the
layer record's `image_file`. Saving writes images only for records with that
field. Consequently, saving succeeds and marks the document saved while
omitting the painted pixels from the package.

**Reproduction:** Create a 16x16 blank document, paint a red dab at (8, 8) with
diameter 4, finish the stroke, save as `.comp`, and reopen. The probe had 16
nontransparent pixels before save and zero after reopening. The original
layer's `image_file` was still `None`.

**Fix direction:** Keep layer asset metadata consistent with image surfaces,
including both whole-preview and cropped-preview adoption. Add a regression
covering painting a new blank layer, saving, and reopening with equal pixels.

### 2. [P1] PSD dimensions reach allocation before validation

**Locations:** `src/psd.rs:110`, `src/psd.rs:247`, `src/psd.rs:248`.

Layer and mask rectangle dimensions are subtracted as signed 32-bit integers,
then passed to channel allocation before validating dimensions, pixel budgets,
or the channel's declared data length. The later layer pixel budget is too late
to protect channel decoding.

**Reproduction:** A 102-byte PSD declaring a 30,000x30,000 channel attempted a
900,000,000-byte allocation. Under the probe's memory limit the process aborted.
A second 102-byte file with `top = -2147483648` and `bottom = 1` panicked at
line 248 with `attempt to subtract with overflow` in the debug build.

**Fix direction:** Use checked rectangle arithmetic and enforce dimension,
allocation, and cumulative decoding budgets before allocating channel buffers.
Bound decoding by the declared channel data region. Cover both pixel and mask
channels, as well as the merged-image path. The reproduced arithmetic panic is
specific to overflow-checked builds; release wrapping is not valid validation.

### 3. [P1] Missing PSD channels cause an indexing panic

**Location:** `src/psd.rs:283`.

The merged-image path indexes the required color planes without first checking
that the header declared enough channels for its color mode.

**Reproduction:** A 40-byte, 1x1 RGB PSD declaring zero channels reaches
`planes[0]` and panics with `index out of bounds: the len is 0 but the index is
0`. RGB files with fewer than three channels can reach the same unchecked
indexing. Reproduced through the CLI converter, with exit code 101.

**Fix direction:** Validate channel counts for the selected color mode before
decoding or indexing, and return an ordinary file-format error.

### 4. [P1] Hue/Saturation settings are incompatible with Swift files

**Locations:** `src/filters.rs:582`, `src/filters.rs:585`,
`src/filters.rs:614`, `src/filters.rs:616`.

The reference defines `adjustments` and `bands` as dictionaries keyed by the
`ColorRange` enum, in
`reference/Compositor/Document/HueSaturation.swift:176`. That enum is Codable
but does not conform to `CodingKeyRepresentable`. Swift encodes these
dictionaries as arrays of alternating keys and values. Rust instead expects
JSON objects and writes JSON objects.

**Reproduction:** Feed `Adjustment::from_record` settings containing:

```json
{
  "kind": "Hue/Saturation",
  "hue": 120,
  "saturation": 0,
  "lightness": 0,
  "colorize": false,
  "hsvSettings": {
    "range": "Master",
    "colorize": false,
    "invertRange": false,
    "adjustments": ["Master", {"hue": 120, "saturation": 0, "lightness": 0}],
    "bands": []
  }
}
```

The decoded adjustment had empty adjustments and bands. Applying it to a red
pixel left the pixel red rather than shifting it to green. Conversely,
Rust-written object-shaped dictionaries cannot decode into the reference's
enum-keyed dictionaries. The latter conclusion follows from the Swift types
and standard-library implementation; it was not tested by running the Mac app.

**Source:** [Swift Dictionary Codable implementation](https://github.com/swiftlang/swift/blob/main/stdlib/public/core/Codable.swift#L5905).

**Fix direction:** Match the reference's serialized dictionary representation
and test with independently constructed Swift-format records. Do not rely only
on a Rust writer/reader round trip.

### 5. [P2] Undo retains masks cached for the wrong transform

**Location:** `src/render/mod.rs:408`.

`Renderer::restore` invalidates caches only when image or mask surface identity
changes. Transform and mask-placement changes can leave the same surfaces in
place but invalidate the `placed` mask cache, which is keyed only by layer ID
and preview status.

**Reproduction:** On a 12x4 canvas, create an 8x4 red layer with a mask whose
left half is black and right half white. Give the mask its own placement at
(1, 0), unlink it, render, move the layer two pixels right, render, then undo.
The restored rendering differed from the initial rendering in 12 pixels.

**Reference:** `reference/Compositor/Document/LayerMask.swift:145` includes both
mask placement and layer transform in the placement cache lookup.

**Fix direction:** Invalidate placed masks when restoring relevant geometry,
or include that geometry in the cache key. Exercise rendering between edits
and undo/redo in the regression.

### 6. [P2] Clone Stamp erases when sampling transparency

**Locations:** `src/brush.rs:397`, `src/brush.rs:411`.

The ordinary Clone Stamp branch attenuates the destination by brush coverage,
without accounting for sampled source alpha. It behaves like interpolation
toward the sampled pixel, including its alpha, rather than source-over.

**Reproduction:** Use an 8x4 layer with a transparent left half and opaque blue
right half. Clone from the transparent half onto (6, 2), with diameter 2,
opacity 1, and offset (-4, 0). At the inspected destination pixel, premultiplied
BGRA changed from `[255, 0, 0, 255]` to `[53, 0, 0, 53]`.

**Reference:** `reference/Compositor/Document/BrushStroke.swift:450` uses normal
source-over unless `replacesWithClone` explicitly requests replacement.
Transparent source pixels should leave the destination intact.

**Fix direction:** Use premultiplied source-over with source alpha multiplied
by brush coverage for ordinary cloning; retain replacement behavior for the
explicit replacement path. Test transparent and partially transparent samples.

### 7. [P2] Adjustment masks affect pixels outside their bounds

**Location:** `src/render/mod.rs:613`.

Adjustment coverage paints a mask pattern with padded edges over the entire
coverage surface, without limiting that mask to its placement rectangle.
White mask edges therefore extend the adjustment beyond the mask's bounds.

**Reproduction:** Start with an 8x4 image of opaque gray 192. Add a Levels
adjustment with input black 128 and a white mask placed by a 2x4 transform at
the origin. Pixel (6, 2), outside the mask rectangle, becomes gray 129 instead
of remaining 192.

**Reference:** `FolderMaskClip.apply` in
`reference/Compositor/Document/LayerMask.swift:172` clips to the mask rectangle.

**Fix direction:** Make coverage outside each mask rectangle zero, while
preserving multiplication of nested masks. Test non-canvas-sized masks.

### 8. [P2] Failed Image Size leaves an open history transaction

**Locations:** `src/document.rs:1161`, `src/document.rs:1171`.

Image Size starts an edit and then performs fallible rendering and allocation.
An error returns through `?` without ending or rolling back the transaction.
History remains at nonzero depth, disabling undo and redo. Earlier layers in
the loop can also have already been resized.

**Reproduction:** On a 1x1 canvas, use a 1x1 image stretched by its layer
transform to 30,000x1. Rename it to create an undo entry. Request Image Size
2x1. Cairo returns `Invalid Size` when creating the 60,000-pixel-wide layer
surface. `can_undo()` was true before the request and false afterward.

**Fix direction:** Stage the operation before committing, or reliably restore
the prior state and close the transaction on error. Test failure after some
layers have already been processed.

### 9. [P2] Project validation omits adjustment settings constraints

**Location:** `src/format/validate.rs:17`.

The validator checks the adjustment kind and structural restrictions but not
the settings constraints in the reference's `LayerAdjustment.isValid`.

**Reproduction:** A Curves adjustment with only one channel and points
`[{"x":0,"y":0},{"x":0,"y":255}]` loads successfully. Swift requires four
channels, bounded point counts and coordinates, endpoints at x=0 and x=255,
and strictly increasing x values. Invalid settings reach rendering instead
of being rejected.

**References:** `reference/Compositor/IO/ProjectStore.swift:184`,
`reference/Compositor/Document/LayerAdjustment.swift`, and
`reference/Compositor/Document/Curves.swift:11`.

**Fix direction:** Port settings validation for all supported adjustment kinds.
Silent normalization during rendering is not equivalent to rejecting invalid
project metadata.

### 10. [P2] Undoing to the saved state still marks the document modified

**Locations:** `src/history.rs:61`, `src/history.rs:71`.

Undo and redo assign fresh revision numbers instead of restoring the revision
associated with the target snapshot. Once either operation runs, returning to
the saved state cannot match the saved revision.

**Reproduction:** Start with clean history, perform one edit, then undo it.
`is_modified()` remains true. Saving a state, undoing away from it, and redoing
back has the same issue. This causes incorrect dirty indicators and unsaved
changes prompts.

**Reference:** `reference/Compositor/Document/DocumentHistory.swift` stores
revision IDs in snapshots and restores them on undo and redo.

**Fix direction:** Track the before/after revision identity in history entries.
Test both undo and redo returning to a saved state.

### 11. [Test] Adjustment round-trip assertion is tautological

**Location:** `tests/phase6.rs:210`.

The assertion compares `Adjustment::from_record(&record)` with
`Document::adjustment(id)`. The latter calls the same parser on the same stored
record. Both sides can discard settings identically and the test still passes.

**Fix direction:** Compare the decoded result against the original adjustment
used to construct the record. Add independent Swift-format fixtures, especially
for Hue/Saturation dictionaries.

## Local reproduction artifacts

The review's temporary artifacts remain at `/tmp/compositor-review` on this
machine. Temporary files may not survive reboot or transfer to another machine.

- `probes.rs`: Rust probes for painted-layer persistence, mask undo, saved-state
  tracking, invalid Curves acceptance, Clone Stamp alpha, adjustment mask bounds,
  Swift-format Hue/Saturation decoding, and failed Image Size history handling.
- `probes`: compiled probe executable, linked against the reviewed debug build.
- `missing-channels.psd`: 40-byte channel-count panic input.
- `overflow-rectangle.psd`: 102-byte rectangle overflow input.
- `unbounded-channel.psd`: 102-byte oversized allocation input.
- `painted.comp`: saved package demonstrating omitted blank-layer paint.

The Rust probe source imports `tests/common/mod.rs` using this checkout's
absolute path. Rebuild it against the current library before using it to check
fixes; the existing executable contains the old implementation.

Do not run the oversized allocation fixture without a process memory limit.
The original allocation probe set `RLIMIT_AS` to 256 MiB and `RLIMIT_CORE` to
zero before invoking the converter. It observed allocation failure rather than
allowing the process to consume the requested memory.
