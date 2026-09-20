# Third bug review

## Status (2026-09-18, after fixes)

All eleven findings and the follow-up note are fixed; regressions are in `tests/review3.rs` and the corrected
`tests/features_d.rs`. The suite is 124 tests.

| # | Finding | Fix |
| --- | --- | --- |
| 1 | Malformed ABR rows decoded as blanks, no file-wide budget | Rows must decode to their exact width, compression is checked, reads stay inside each record, a 100-megapixel budget is charged before allocation |
| 2 | Merging a folder with its child left a dangling parent | The result takes the topmost selected root's place; the arrangement is validated before anything changes |
| 3 | Panel rebuilds collapsed the multi-selection | The row-selected signal is ignored while rows are rebuilt |
| 4 | Free distort reflected a placed mask on flipped layers | The carry uses the box geometry without pixel flips, as the reference does |
| 5 | Remove Background dropped a separately placed mask | The old mask is rasterized onto the layer's grid at its placement and multiplied in |
| 6 | Dragging a folder and its child flattened them | Only the top of each dragged block takes the new parent |
| 7 | Provisional tails advanced the jitter sequence | The dab count is saved and restored with the tail |
| 8 | Distort preview showed the old mask | The preview warps or carries the mask the same way the commit will (`mask_preview_placements`) |
| 9 | A corrupt model counted as ready | The default model must be its exact size; a failed load discards it and shows the download button again |
| 10 | Loading a set removed same-named tips from other sets | Replacement matches on set and name |
| 11 | Tautological clipping assertion | The test builds a real clipped pair, moves it, and checks the link and its remap on copy |
| note | New layers left the old selection | Layer creation goes through `select_layer` |

The original report follows.

Reviewed on 2026-09-18 against commit
`0027b5fa646ff67df82c139308d069c007fedc81`.

The working tree initially contained brush-related changes on top of `dc71b02`.
Those changes were updated and committed during this review. The final checks and
line references below use `0027b5f`, including the new GIMP brush and hose support.
No application code, tests, reference files, or C sources were changed by this audit.
This report covers new work since round 2 and does not repeat its fixed findings.

## Validation

- `cargo build`: passed.
- `cargo test`: **116 passed, 0 failed, 1 ignored**. The ignored test requires the
  downloaded segmentation model. The count differs from the older review prompt
  because additional tests were added.
- Existing warnings: unused `w` in `tests/features_c.rs:50`, and unnecessary
  parentheses in `src/matte.rs:241`.
- Standalone probes exercised the current library with small generated projects
  and brush files. Allocation probes ran in child processes with a 256 MiB
  address-space limit and core dumps disabled.
- Source hashes were unchanged across the final build and test run.
- GTK interactions were inspected in code, not driven in a running window. No
  model was downloaded or inference run. Each finding states its evidence.

## Findings, ranked by severity

### 1. [P1] Malformed ABR rows can produce arbitrarily large retained brush collections

**Locations:** `src/abr.rs:142`, `src/abr.rs:147`, `src/abr.rs:153`, and the
per-brush accumulation in `parse`.

Each brush allocates its full decoded rectangle before its payload is validated.
PackBits decoding then accepts rows that produce fewer than the required number
of bytes, including zero-length rows. The resulting zero-filled tips are retained
in the preset list. The 50-megapixel limit is per tip; there is no aggregate decoded
budget for the file.

**Reproduced:** a 52-byte version-2 file declaring one 4 by 2 brush with two empty
compressed rows was accepted as eight decoded bytes. A 112,356-byte file containing
eight 7,000 by 7,000 brushes, again with empty rows, exhausted the child process's
memory and aborted with `memory allocation of 49000000 bytes failed`. These tips
collectively request 392 MB from a roughly 110 KB malformed file. More entries can
increase that without an overall bound. Load Brushes runs the parser in the UI
process, so an allocation abort terminates the editor rather than reporting an error.

Require each row to decode exactly its expected width, reject unsupported
compression values, confine reads to each brush record, and enforce an aggregate
budget before allocation. Add truncated and empty-row cases alongside valid
PackBits samples.

### 2. [P1] Merging a selected folder and its child leaves a dangling parent

**Locations:** `src/document.rs:711`, `src/document.rs:768`,
`src/document.rs:779`.

For a multiple-layer merge, the result inherits the top selected record's
`parent_id`. If the selection includes a folder and its child, that parent is the
folder being removed. The new merged record is inserted with a reference to a
nonexistent parent. There is no final hierarchy validation before changing the
document.

**Reproduced:** an 8 by 8 document with a folder containing one opaque red child;
select both through `select_layer` and `toggle_layer_selected`, then call
`merge_layers`. The call succeeds, but the only remaining layer has a missing
parent, the flattened result is completely transparent, and serializing its
manifest then calling `Manifest::parse` fails. The document no longer meets the
project format's own invariants. The panel issue below can currently obscure this
core merge failure, so test the operation independently as well as through the UI.

Choose a surviving parent or reject the merge before mutation. The Swift reference
performs `LayerHierarchy.validate` on the proposed result before committing it
(`reference/Compositor/Document/LayerMerge.swift`).

### 3. [P2] Rebuilding the layer panel immediately clears multiple selection

**Locations:** `src/ui/layers.rs:110`, `src/ui/layers.rs:342`,
`src/ui/layers.rs:360`.

Ctrl-click and Shift-click update `Document::selected`, then call `rebuild`.
Rebuilding removes the old rows and programmatically selects the active row in a
new single-selection GTK ListBox. The `row_selected` callback treats that signal
as a user selection and calls `select_layer` whenever the document has more than
one selected layer. That reduces the set back to one. Removing the previously
selected row can also emit an intermediate deselection.

**Evidence:** callback tracing, not a driven GTK reproduction. The trigger is
Ctrl-clicking or Shift-clicking another layer in the panel. The document methods
can pass their unit tests while the panel prevents the user from retaining the
selection needed for grouped transforms, deletes, merges, and drags.

Guard selection signals during rebuilds and distinguish synchronizing the active
row from replacing the document's selected set. Test the actual panel interaction.

### 4. [P2] Free distort reflects a separately placed mask on a flipped layer

**Locations:** `src/distort.rs:61`, `src/document.rs:1263`.

`carried` maps document coordinates back into the layer's unit square using
`unit_to_document`, which includes pixel flips. The destination homography uses
geometric handle corners, which do not include those flips. Mixing these coordinate
systems reflects the mask placement even when the geometric warp is an identity.

**Reproduced:** an 8 by 8 horizontally flipped layer with a linked 2 by 8 mask
placed at x=1. Committing its unchanged corner coordinates moves the mask to x=5
and changes 32 flattened image bytes. The otherwise identical unflipped control
changes zero bytes. A user-applied corner distortion on a flipped layer goes
through this same carry calculation and introduces the unwanted reflection.

Use the inverse geometric box transform without pixel flips. Swift explicitly
constructs that unflipped transform in
`reference/Compositor/Document/Distort.swift:114`.

### 5. [P2] Remove Background discards an existing separately placed mask

**Locations:** `src/document.rs:435`, `src/document.rs:444`.

The commit path combines the subject mask with the old mask only when
`mask_placement.is_none()`. If a mask has its own placement, the combination is
skipped, but the code still replaces the mask and clears its placement. Previously
hidden subject pixels can therefore reappear on Apply. The preview continues to
render through the old mask, making this a preview/commit discrepancy too.

**Evidence:** direct inspection of the commit and preview branches. To exercise
it with a model installed, move an enabled layer mask independently, hide part of
the foreground subject with it, then apply Remove Background. This model-dependent
scenario was not run during the audit.

Rasterize the existing mask into the layer's grid before multiplying it with the
new subject coverage. Preserve its effective coverage rather than dropping it
because it has a placement. The reference's `SubjectRemoval.subjectMask` also
requires that what either mask hides stays hidden.

### 6. [P2] Dragging a selected folder and child reparents the child out of the folder

**Location:** `src/document.rs:1801`.

`move_layers` collects selected folders with their descendants, but it assigns the
destination parent to every record explicitly listed in `ids`. When both a folder
and its child are selected, they both become siblings at the destination. The
operation silently changes their nesting instead of moving the existing subtree.
That can also remove inherited folder visibility or mask coverage from the child.

**Reproduced:** folder F with child C and a separate root layer A; call
`move_layers(&[F, C], Place::Above(A))`. The call succeeds and C's parent becomes
`None`, rather than remaining F. This is independent of the panel selection issue.

Reduce selected IDs to top-level selected roots before changing parents. Descendants
should keep their parent relationships when their selected ancestor moves with them.

### 7. [P2] Provisional brush tails consume jitter state that replay never consumes

**Locations:** `src/brush.rs:321`, `src/brush.rs:326`, `src/brush.rs:411`.

`draw_tail` saves and restores the path position and spacing accumulator, but not
`dabs`. Drawing a provisional tail increments `dabs`, which selects the angle or
hose-frame variant. Removing that tail restores coverage without restoring the
variant sequence. `Stroke::replay` skips provisional tails entirely, so it selects
different variants for the same finished path.

**Reproduced:** an opaque 8 by 2 bar preset, diameter 14, spacing 0.5, full angle
jitter, and points `(20,32), (40,32), (60,32), (90,32)` on a 128 by 64 grid.
Appending those points and flushing differs from replaying them by **673 bytes**.
This was a straight path, so the difference is not explained by final curve shape.
It affects the newly added hose-frame selection for the same reason.

Restore all dab-generation state after drawing a provisional tail. Add an exact
live-versus-replay comparison using an asymmetric jittered tip and a multi-frame
preset; the existing ordinary-brush preview comparison does not cover this.

### 8. [P2] Distortion preview warps the image but not its mask

**Locations:** `src/document.rs:1235`, `src/document.rs:1255`.

`preview_distort` only creates and places a warped image. The commit path separately
warps linked masks, including those with independent placement. During a non-affine
corner drag, the old mask shown by the renderer does not represent the mask that
will be committed, so the visible cutout jumps on release.

**Reproduced:** an opaque 8 by 8 layer with an 8 by 8 mask revealing its left half;
move the top-right corner from `(8,0)` to `(16,0)`. Rendering after
`preview_distort` and after `commit_distort` differs by **136 bytes**, even though
this example is far below the 2,048-pixel preview limit.

Preview the warped mask with the same linked/unlinked placement rules used at
commit. The reference's `distortPreview` explicitly computes `warpedMask`
(`reference/Compositor/Document/Distort.swift:210`).

### 9. [P2] Model readiness accepts corrupt files and removes the recovery button

**Locations:** `src/matte.rs:43`, `src/matte.rs:65`,
`src/ui/filter_dialog.rs:307`.

`model_ready` checks only that a file is larger than one million bytes. The download
path has the same minimum-size threshold before promoting the staging file; it
does not compare the completed length with its expected total or validate the model.
The dialog only creates its download button when `model_ready` is false.

**Reproduced:** pointing `COMPOSITOR_MODEL` at a file containing 1,000,001 zero
bytes makes `model_ready()` return `true`. Model loading would fail, but the dialog
treats that file as installed and offers no download/retry control. This demonstrates
the readiness issue without downloading a model. Ordinary network read errors do
return an error; this finding is not a claim that every interrupted transfer is
promoted successfully.

Validate the published model and provide recovery when loading fails. Check the
completed download length against the expected response/model size before rename,
with appropriate handling for explicitly configured alternative models.

### 10. [P2] Loading a brush set silently removes same-named brushes from other sets

**Location:** `src/ui/brushes.rs:64`.

The global preset registry removes existing presets by display name alone before
adding the newly loaded file. Set identity and source path are ignored. Names such
as "Soft", "Chalk", or "Shared" are not unique across brush packs, so loading one
pack can silently remove entries from another pack or the bundled set.

**Reproduced:** load `set-a.abr` with a brush named `Shared` at coverage 255, then
`set-b.abr` with a different brush named `Shared` at coverage 128. The public preset
registry retains only `("set-b", 128)`. Both files loaded successfully. The same
collision happens during startup when remembered files are loaded in order.

Use a source/set-aware identity for replacement and retain distinct presets that
merely share a display name.

### 11. [P3] The folder-drag clipping assertion is tautological

**Location:** `tests/features_d.rs:58`.

The test asserts:

```rust
mask_source_id.is_some() || mask_source_id.is_none()
```

That is true for every `Option`, including one whose link was lost or changed. In
addition, its fixture's child B has no lower sibling inside F, so calling
`toggle_clipping(b)` does not establish the meaningful internal clipping link that
the test's title promises to preserve.

Create a base and clipped child in the folder, assert the source UUID before the
move, and compare that same UUID afterward. Also check remapped clipping IDs when
copying the folder between documents.

## Coverage and follow-up details

- Reviewed the new ABR/GIMP parsers, HEIC wrapper, brush shape/variant paths,
  distortion, pixel moves, multi-selection and layer operations, gradient/shape/
  crop/eyedropper paths, matte code, and the associated UI and test changes.
  `src/psd.rs` has no changes after the round-2 fix commit `8289e2e`; its regression
  tests still pass. No previous PSD finding is repeated here.
- No additional concrete HEIC wrapper, guided-filter numeric, ruler, color-wheel,
  or theme-watcher failure was established. Source inspection and the existing
  samples do not establish safety for every malformed native-code input or GUI
  event sequence. Native decoder fuzzing and sanitizer runs were not performed.
- A suspected translated-layer gradient coverage issue was tested and did not
  reproduce. It is not a finding.
- A related document-selection invariant deserves a regression when fixing the
  panel: select A and B, then `add_blank_layer`. The new layer becomes active but
  `selected` still contains only A and B. A direct `delete_layer` then deletes A
  and B and leaves the new layer. This was reproduced through the Document API;
  the current panel rebuild can hide it by resetting selection. Creation/import
  operations should use `select_layer` consistently rather than assigning only
  `active` (`src/document.rs:1671`, with analogous assignments elsewhere).
- No GUI was launched and no full model inference was performed. In particular,
  the placed-mask background-removal finding is established from its unconditional
  replacement branch, not from a claim to have tested segmentation quality.

## Reproduction artifacts

Local artifacts are in `/tmp/compositor-review-3/`:

- `probes.rs` and `probes`: standalone core reproductions, with modes `abr`,
  `merge`, `move`, `jitter`, `selected`, `distort`, `distortpreview`, `modelready`,
  `registry`, and the non-failing `gradient` check.
- `empty-row.abr`, `allocation.abr`, `set-a.abr`, `set-b.abr`, and
  `not-a-model.onnx`: generated input files.
- `build.log`, `test.log`, and `reviewed-files.sha256`: final validation evidence.

These are temporary local artifacts, not committed regression fixtures. For
example, `/tmp/compositor-review-3/probes jitter` reruns the brush comparison.
Preserve or recreate the probes for a different machine, and keep allocation
reproductions in resource-limited child processes. No source changes or fixes are
included in this report. Leave `reference/` and `csrc/` unchanged when addressing it.
