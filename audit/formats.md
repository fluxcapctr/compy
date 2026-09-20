# Format audit: .comp, PSD, images, brushes, patterns, export, autosave

Whole-project pass over every file the app reads or writes, treated as untrusted input.
Nothing already listed in REVIEW_RESULTS.md through REVIEW_RESULTS_8.md is repeated; where a
finding touches the same line as an earlier one, the relationship is called out.

Counts: 2 P1, 7 P2, 13 P3.

---

## P1

### 1. A ~500 KB PSD crashes the app: unbounded descriptor list recursion

`src/psd_desc.rs:90` (`item`), list arm `src/psd_desc.rs:94-100`.
Reached from `src/psd.rs:265` (`lfx2` → `read_lfx2` → `psd_desc::parse`) and
`src/psd.rs:264` (`TySh` → `read_tysh` → `psd_desc::parse`).

`descriptor()` guards nesting with `if depth > 32 { bail!(...) }` (`psd_desc.rs:76`), but that
guard only fires when an `Objc`/`GlbO` item is met. The `VlLs` (list) arm recurses straight back
into `item(r, depth + 1)` and `item` itself never inspects `depth`. A list whose single element is
another list therefore recurses once per 8 input bytes with no ceiling.

Trigger: any PSD with one layer whose `lfx2` additional-info block contains

```
00000000                  descriptor name, length 0
00000000 "null"           class
00000001                  one item
00000000 "test"           key
"VlLs" 00000001           repeated N times
"bool" 01                 terminator
```

At 8 bytes per frame, a few hundred KB of that block is enough to exhaust the 8 MB main-thread
stack. Rust's guard page turns it into `thread 'main' has overflowed its stack` and `SIGABRT`, so
the whole editor dies with every open document's unsaved work. `read_lfx2` is called during
`psd::read`, i.e. on File > Open of the PSD; no user interaction past the open is needed.

The same payload in a `TySh` block does the same thing.

Fix shape: give `item` the same `depth > 32` check the descriptor has, or thread a shared node
budget through both.

Confidence: confirmed by tracing (`item` has no depth test; `descriptor` is the only guard, and
the list path never reaches it).

### 2. A .comp save on a full disk reports success and destroys the previous package

`src/png_io.rs:141-153` (`encode`), `src/png_io.rs:85-95` (`encode_gray`), consumed by
`src/format/mod.rs:319-323`, with the destructive swap at `src/format/mod.rs:330-338`.

```rust
let file = File::create(path)?;
let mut encoder = png::Encoder::new(BufWriter::new(file), w as u32, h as u32);
...
let mut writer = encoder.write_header()?;
writer.write_image_data(&rgba)?;
Ok(())
```

`writer` is never `finish()`ed and the `BufWriter` is never flushed explicitly. png 0.18's
`impl Drop for Writer` (`png-0.18.1/src/encoder.rs:1115`) writes IEND with `let _ =`, and
`BufWriter::drop` swallows its flush error too. So the tail of the IDAT stream plus the entire IEND
chunk can fail to reach disk (ENOSPC, EDQUOT, EIO, a full tmpfs) while `encode` returns `Ok(())`.

`format::save` then treats the staging directory as complete: it renames the existing package to
`.<name>.previous-<pid>`, renames staging into place, and `remove_dir_all`s the backup
(`format/mod.rs:332-338`). The good copy is gone and the new package holds a PNG that
`png_io::decode` will reject on the next open with `ProjectError::MissingImage` ("An image inside
the project is missing or damaged").

The same swallowing applies to `export_png` and `export_layers`, where it is only a bad export
rather than data loss. `jpeg_bytes`/`webp_bytes`/`export_gif` go through `std::fs::write` and do
report the error; `psd::write` uses `write_all` and reports it.

Fix shape: `writer.finish()?` (which flushes) and, for the project save, `file.sync_all()` before
the rename.

Confidence: confirmed by tracing, including the png crate's `Drop` impl.

---

## P2

### 3. Debug-build panic on a malformed PSD `extra_len`, a second route to the REVIEW_RESULTS_2 #4 subtraction

`src/psd.rs:228` sets `let extra_end = r.pos + extra_len;` from an unclamped u32,
`src/psd.rs:272` does `r.pos = extra_end;` unconditionally, and `src/psd.rs:283` then evaluates
`if len > data.len() - start`.

REVIEW_RESULTS_2 #4 fixed the channel-skip route into that subtraction (`psd.rs:284` now bails
through `r.skip(len)?`). The `extra_len` route is still open:

1. Layer count `1`.
2. A well-formed rect, channel spec list, `8BIM` blend signature, mask length and ranges length.
3. `extra_len = 0xFFFFFFFF`.
4. Four bytes after the Pascal name that are **not** `8BIM`/`8B64`, so the additional-info loop at
   `psd.rs:248-271` takes the `break` at line 250 instead of erroring out of `r.bytes(4)?`.

Line 272 then parks `r.pos` about 4 GB past EOF, the record loop ends (count was 1), and the
channel-data loop at line 276-283 computes `data.len() - start` with `start > data.len()`.

In the installed binary this is benign: `install.sh:7` builds `--release`, `Cargo.toml` sets no
`overflow-checks`, the subtraction wraps, the comparison falls through, and the very next
`r.u16()?`/`r.skip(len)?` bails cleanly. In a debug build (which is what `cargo test` and a
development run use, and which is where REVIEW_RESULTS_2 observed the original) it is
`attempt to subtract with overflow`.

Fix shape: clamp at the source, `let extra_end = (r.pos + extra_len).min(data.len());` and likewise
for `resources_end` (`psd.rs:182`) and `layer_mask_end` (`psd.rs:201`).

Confidence: confirmed by tracing.

### 4. PSD clipping base is keyed by nesting depth, so clipping leaks between sibling groups

`src/psd.rs:394-400`.

```rust
let parent_key = pending_parent.len() - 1;
if raw.clipping {
    let base = base_by_parent.get(&Some(Uuid::from_u128(parent_key as u128))).copied().flatten();
    ...
} else if !is_adjustment {
    base_by_parent.insert(Some(Uuid::from_u128(parent_key as u128)), Some(id));
}
```

`parent_key` is the current nesting *depth*, not the identity of the enclosing folder, and
`base_by_parent` is never cleared when a folder closes at `psd.rs:341-353`. Every group at depth 1
shares one slot.

Trigger: a Photoshop file with two sibling groups, `Group A` holding a plain layer then `Group B`
whose bottom-most layer is clipped (Photoshop allows the bottom member of a group to be clipped;
it then clips to nothing and Photoshop simply shows it normally). On import, that layer gets
`mask_source_id` pointing at the last unclipped layer of `Group A`. `validate::live_mask_graph`
accepts it (it only rejects folders, adjustments, cycles and long chains), so the file opens with a
layer silently masked by the alpha of a layer in a different, unrelated folder.

The same aliasing makes the "clips to nothing that was opened" warning at `psd.rs:397` not fire
when it should.

Fix shape: key `base_by_parent` on the folder's own `Uuid` (it is known when the folder record is
built) or push/pop a base slot alongside `pending_parent`.

Confidence: confirmed by tracing.

### 5. `decode_image_bytes` drops EXIF orientation that `decode_image` applies

`src/document.rs:3105-3114` against `src/document.rs:3076-3090`.

`decode_image` (the File > Open path) reads `decoder.orientation()` and calls
`decoded.apply_orientation(orientation)`. `decode_image_bytes` does neither. The two are otherwise
line-for-line the same.

`decode_image_bytes` is what `open_image_bytes`, `import_image_bytes`, `add_image_layer` and
`patterns::load` use, so the same phone JPEG (EXIF orientation 6, the common portrait case) is
upright when opened from disk and rotated 90 degrees when dropped onto the canvas, pasted, placed
by the agent, or loaded as a pattern. `decode_image_bytes` also skips the post-orientation size
recheck, which is harmless only because it never rotates.

Confidence: confirmed by reading the two functions side by side.

### 6. A .comp this port writes can be wholly undecodable on macOS, not partly

`src/document.rs:3524` (path shape record), `src/format/mod.rs:206` (`ADJUSTMENT_KINDS`), against
`reference/Compositor/Document/ShapeTool.swift:3-5` and
`reference/Compositor/Document/LayerAdjustment.swift:4-6`.

The reference's `ShapeKind` has exactly two cases, `"Rectangle"` and `"Ellipse"`. The reference's
`AdjustmentKind` has exactly six, `"Hue/Saturation" "Levels" "Curves" "Exposure" "Gradient Map"
"Grain"`. This port writes `{"kind": "Path", ...}` for a Pen shape layer and nine further
adjustment kinds (`Brightness/Contrast`, `Vibrance`, `Black & White`, `Photo Filter`, `Threshold`,
`Posterize`, `Shadows/Highlights`, `Selective Color`, `Channel Mixer`).

Swift's synthesized `Decodable` throws `DataCorrupted` on an unknown `String` raw value, and
`ProjectLayerRecord.shape` / `.adjustment` are `Optional<T>` decoded with `decodeIfPresent`, which
throws when the key is present but its contents do not decode. `JSONDecoder` has no partial
recovery, so the *entire* `manifest.json` fails and the Mac app reports "This is not a valid
Compositor project, or its metadata is damaged" for a project that is merely using a newer
feature. One Threshold layer loses the user the whole file on the Mac.

By contrast the reverse direction is safe: `filters::Adjustment::to_record`
(`src/filters.rs:1154-1160`) deliberately emits the legacy `hue`/`saturation`/`lightness`/
`colorize`/`levels`/`curves` keys the Swift requires, and Swift ignores the unknown `text`,
`effects` and `artboard` keys. Only `kind` is the breaking field.

Fix shape: for a Mac-compatible save, either bump the declared version past 7 so the Mac rejects it
on the version check with an honest message, or map unknown kinds to a spelling the Mac tolerates.

Confidence: confirmed by comparing the Rust writers against the reference enums. The Swift decoder
behaviour is standard `Codable` semantics, not verified by running it.

### 7. PSD export of an artboard document produces layers that contradict its own merged image

`src/psd.rs:487-507` (`emit`), with the composite at `src/psd.rs:606`.

`render_flat` draws artboard backgrounds (`render/mod.rs:676`) and clips each layer to its owning
board (`render/mod.rs:679` → `draw_within_artboard`). `psd::write` does neither: an artboard is
`layer.is_group()`, so it emits a plain `lsct` folder pair (`psd.rs:490-494`) whose children are
rasterized at their own unclipped bounds (`psd.rs:625-646`). The board's frame, its clipping and
its background colour are all dropped, and unlike every other lossy case here
(`psd.rs:496`, `:498`, `:499`, `:502`) no warning is pushed.

Result: Photoshop shows the flattened preview correctly (that comes from the merged image) but the
moment layers are turned on, content spills across board boundaries and the board backgrounds are
missing. Photoshop's own artboards are `lsct` type 3 with an `artb`/`artd` descriptor, which this
writer does not emit.

Confidence: confirmed by tracing; the visual claim about Photoshop follows from the missing
`artb` block rather than from opening a file there.

### 8. ZIP-compressed channels are refused, which is how Photoshop CC writes most 16- and 32-bit PSDs

`src/psd.rs:125` (`other => bail!("compression {other} (zip) is not supported yet")`), also
`src/psd.rs:323` for the merged image.

The module header at `src/psd.rs:4` promises "16-bit files read at 8 bits", and `psd.rs:170`
accepts depth 8/16/32 with a warning at `psd.rs:177`. But Photoshop writes compression 2 (ZIP
without prediction) or 3 (ZIP with prediction) for deep-bit layer channels as a matter of course.
Such a file passes every header check, emits the "opened at 8 bits" warning, and then fails at the
first layer channel with "compression 2 (zip) is not supported yet", after the user has been told
the file is fine. Compression 3 additionally reports itself as "zip" in a message that only makes
sense for 2.

Affinity Photo and GIMP write RLE and are unaffected; this is specifically the Photoshop CC 16/32-bit
path the module claims to support.

Confidence: confirmed by tracing the reader; the claim about what Photoshop emits is from the PSD
spec and general practice, not from a sample file.

### 9. Imported images always open at 72 ppi; pHYs and JFIF density are discarded

`src/document.rs:3093-3102` (`open_image` → `Document::blank(w, h, 72.0)`), same at
`src/document.rs:3116-3123`.

`png_io::encode` writes a `pHYs` chunk from the document resolution (`png_io.rs:148-149`) and the
PSD reader honours resource `0x03ED` (`psd.rs:190-194`), so the app clearly cares about resolution.
But opening a PNG or JPEG throws away the file's own density and hard-codes 72. A 300 ppi scan
opens as a 72 ppi document; Image Size shows the wrong physical dimensions, print-size presets in
Export Sizes are wrong, and re-exporting writes 72 ppi back, permanently losing the original
metadata.

`decode_image` already holds the `ImageDecoder`, from which the PNG pHYs and JPEG JFIF density are
reachable, so nothing structural stands in the way.

Confidence: confirmed by tracing.

---

## P3

### 10. `patterns::save` silently overwrites a user's pattern, and is not atomic

`src/patterns.rs:155-163`.

```rust
let path = path_for(&name);
let bytes = crate::png_io::png_bytes(surface)?;
std::fs::write(&path, bytes)...
```

No existence check. Define Pattern with a name the user already used replaces the old pattern with
no prompt. Worse, `clean` (`patterns.rs:134`) maps every non-alphanumeric to `_`, so `"Brick/2"`,
`"Brick 2"` (no, space is kept) and `"Brick.2"` and `"Brick#2"` all collapse to the same file. The
CLI path `compositor patterns import file.abr` (`src/main.rs:44`) runs `save` once per pattern in
the file with no `unique_file`, so importing a `.pat` whose entries share a name keeps only the
last, and importing over an existing library wipes hand-made patterns without a word.

`fs::write` truncates then writes, so a crash mid-write leaves a zero- or part-length PNG where the
pattern was. Every other writer in the project stages and renames.

Path traversal is *not* possible: `clean` turns `/` and `.` into `_`, so `"../../evil"` becomes
`"______evil"` and `load`/`path_for`/`save` all run the same `clean`. Confirmed sound.

### 11. Export batches de-duplicate only within the batch, never against the disk

`src/document.rs:3704-3711` (`unique_file`), used by `export_sizes::export_all`
(`export_sizes.rs:128`) and `export_artboards` (`document.rs:2777`, `:2786`).

`used` is a fresh `HashSet` per call, so running Export Sizes twice into the same folder overwrites
the first run's files. The doc comment on `unique_file` claims it exists "so two names that clean
to the same text never overwrite each other", which is true only inside one run.
`export_layers` (`document.rs:1294`) avoids this by accident, through its `{:02}-` index prefix.

### 12. A missing manifest or missing layer PNG reports "exceeds the supported limits"

`src/format/mod.rs:404-410` (`check_file`).

```rust
let resolved = file.canonicalize().map_err(|_| ProjectError::TooLarge)?;
```

`canonicalize` fails with `ENOENT` for a file that is simply absent, and the error is mapped to
`TooLarge`. Opening a `.comp` whose `images/<uuid>.png` was deleted or whose `manifest.json` is
missing tells the user "This project exceeds the supported canvas, layer, file-size, or
100-megapixel image limit" instead of `MissingImage`'s "An image inside the project is missing or
damaged". `ProjectError::MissingImage` exists for exactly this and is only reachable from inside
the PNG decoder.

### 13. GIMP tips bypass `shrink_tip` and keep up to 50 megapixels resident

`src/gbr.rs:52` builds a `Preset` directly from the decoded pixels; `abr::read_plane`
(`src/abr.rs:190`) funnels `.abr` tips through `shrink_tip` first.

`gbr::parse_one` admits any tip up to 16,384 per side and 50 MP (`gbr.rs:44`). The brush never
paints wider than 2,000 pixels, which is precisely why `TIP_LIMIT = 2048` and `shrink_tip` exist
(`abr.rs:193-209`). A legitimate large `.gbr` therefore keeps ~50 MB resident for good, and a
`.gih` hose keeps one such buffer per cell in `Preset::frames` (up to 512 cells, `gbr.rs:67`).
The thumbnail drawer (`src/ui/brushes.rs:92-95`) then reallocates `stride * height` from those
dimensions on every redraw of the brush picker.

Bounded by file size in every case (the pixels must actually be present), so this is waste rather
than an amplification attack.

### 14. `.gbr` version 1 is accepted by the version test and rejected by the magic test

`src/gbr.rs:42`: `if !(1..=3).contains(&version) || data.get(20..24) != Some(b"GIMP")`.

The `GIMP` magic at offset 20 and the spacing field at 24 arrived in `.gbr` version 2. A genuine
version 1 file has its name text at offset 20, so it always fails the second half of the condition
and reports "This is not a GIMP brush file." Either the version range should be `2..=3` (honest
message) or version 1 should be handled with its shorter header.

### 15. A PackBits tip is allocated from a length table alone, so a small `.abr` expands ~180x

`src/abr.rs:153-155`.

```rust
if (compression == 0 && room < w * h * bytes) || (compression == 1 && room < h * 3) { bail!(...) }
*budget = budget.checked_sub(w * h)...;
let mut raw = vec![0u8; w * h * bytes];
```

For compressed tips the only up-front requirement is three bytes per row, while the allocation is
`w * h * bytes`. The budget is debited `w * h` but the buffer is `w * h * bytes`, so a 16-bit tip
costs twice its budget share, plus another `w * h` for the `chunks_exact(2)` copy at `abr.rs:189`.
A 50 MP 16-bit tip is a 150 MB peak; the 400 MP budget lets eight of them through one file, though
sequentially, so the peak stays around 150 MB rather than 1.2 GB.

Because the rows must genuinely decode to exactly `w` bytes each (`abr.rs:186`), the real
amplification is bounded at roughly 60x by PackBits' 2-bytes-per-128 run: about 800 KB of file for
a 50 MP tip. Not remotely fatal, but given that systemd-oomd on this machine reaps the whole
terminal cgroup, sizing the up-front requirement from `w` as well as `h` would be cheap insurance.

The *good* news, and it is the part the question asked about: the budget is debited **before** the
allocation, so a single huge tip is checked first. `patterns()` does the same at `abr.rs:286`,
debiting `w * h * (wanted + 4)` before touching a buffer. Both are correct.

### 16. Guides are never saved

`src/document.rs:38-41` ("Not saved"). Rulers, double-click-to-add guides and guide snapping are
this port's own feature (the reference has no persistent guides at all, only transient snap
indicators in `EditorSession.swift:165`). A user who lays out a poster against guides, saves and
reopens finds them gone. Since the guides are purely this port's, an extra top-level
`guides` key in the manifest would be ignored by the Mac exactly as `text`/`effects`/`artboard`
already are.

### 17. A PSD folder's opacity and blend mode are dropped without a warning

`src/psd.rs:346`: `record(id, &raw.name, ..., 255, BlendMode::Normal)`.

`raw.opacity` and `raw.blend` were parsed for the folder record and are then discarded, because
`validate::manifest` requires groups to be plain (`format/validate.rs:40`, matching the reference's
version 3 rule). That is the right call, but every other lossy import in this reader pushes a
warning and this one does not, so a Photoshop file with a 50%-opacity group opens looking wrong
with no explanation.

### 18. A recovery file from a newer format version is offered forever and never opens

`src/autosave.rs:363-376` (`recoverable`) only checks that `manifest.json` exists. A `.comp` in the
autosave folder declaring version 8 (a future build, or a corrupted byte) is listed on the start
page, fails `Manifest::parse` with `ProjectError::Version(8)` on every attempt, and is never
discarded, so the entry stays on the start page permanently.

Everything else about autosave recovery is sound and is listed below.

### 19. WebP export over 16,384 pixels fails with an opaque encoder error

`src/document.rs:1236-1246`. The canvas limit is 30,000 per side; `image-webp`'s VP8L encoder
rejects anything over 16,384 with `EncodingError::InvalidDimensions`
(`image-webp-0.2.4/src/encoder.rs:399`), which surfaces to the user as a generic encoding error
rather than "WebP tops out at 16,384 pixels per side". Cheap to pre-check next to the existing
per-format guards.

### 20. `clean_file_name` has no length cap

`src/document.rs:3696-3700`. Layer names are validated up to 16,384 bytes
(`format/validate.rs:49`). `export_layers` builds `format!("{:02}-{}.png", n, clean_file_name(name))`
(`document.rs:1294`), so a long-named layer makes `File::create` fail with `ENAMETOOLONG` (255 bytes
on ext4/btrfs) and aborts the whole export part-way, leaving the earlier files behind. Truncating
to ~180 bytes on a char boundary would settle it.

### 21. HEIC re-checks nothing about the plane the decoder actually returns

`src/heic.rs:15-22`. The 30,000-side and 100 MP guards are applied to `handle.width()/height()`;
the buffer is then allocated from `plane.width`/`plane.height`, which libheif derives after
applying `irot`/`clap`/grid assembly. For a well-formed file these agree or shrink, but a tiled
("grid") HEIC whose handle reports the tile size rather than the assembled size would allocate past
the budget unchecked. Repeating the two-line guard on `(pw, ph)` before `vec![0u8; pw * ph * 4]`
costs nothing.

Confidence: plausible, not confirmed (depends on libheif's grid handling, which was not traced).

### 22. `shape`, `text` and `effects` records are accepted from a file without any validation

`src/format/validate.rs:8-59` validates `adjustment`, `mask_file`, `mask_placement`, `opacity`,
`blend_mode` and `artboard`, but `shape`, `text` and `effects` are opaque `serde_json::Value`s that
pass straight through. A hand-edited `.comp` can therefore carry a `"kind": "Path"` shape with
tens of thousands of anchors (bounded only by the 4 MB manifest cap), or a `text` record with a
2,000-pixel size on a 1x1 layer.

Nothing crashes: `Effects::reach()` clamps to 4,000 (`effects.rs:184`), `TextStyle::size` is clamped
on the PSD path, `anchors_from_json` (`document.rs:3676`) discards malformed anchors, and
`Effects::from_record`/`TextStyle::from_record` return `None` on any decode error so a bad record
degrades to "no effects"/"plain pixels". It is a gap in the "a file can never make an invalid
project" principle rather than a live bug.

### 23. `avifenc` is handed the target path positionally

`src/document.rs:1270-1272`. Arguments go through `Command`, so spaces, unicode and shell
metacharacters in the path are all safe (no shell is involved), and that part of the question is
clean. The one edge is a path beginning with `-`, which `avifenc` reads as an option. Prefixing
with `--` or `./` would close it.

---

## Checked and found sound

**.comp package**

- `check_file` (`format/mod.rs:404-411`) canonicalizes, requires the result to stay under the
  package root, rejects symlinks via `symlink_metadata`, rejects non-regular files and enforces the
  4 MB manifest / 512 MB asset caps. Asset names cannot traverse anyway: `validate::manifest`
  requires `image_file == "<UPPER-UUID>.png"` and `mask_file == "<UPPER-UUID>.mask.png"` exactly
  (`validate.rs:29`, `:53`).
- `check_size` (`format/mod.rs:395-401`) works in `i64` with separate image and mask budgets, which
  matches `project-format.md`'s "100 million mask pixels in addition to the existing 100 million".
  The header is checked before any pixel buffer exists (`png_io.rs:42`), so a 30,000 x 30,000 PNG
  header is rejected at 900 MP without allocating.
- `format::save` stages into `.<name>.saving-<pid>`, builds `images/` from scratch (so stale PNGs
  from deleted layers cannot survive), removes staging on any error, and swaps with
  rename-aside/rename-in/remove-backup with a rollback on failure. Saving into a sub-path of the
  package is odd but not destructive.
- `entries_ordered` (`format/mod.rs:428`) caps recursion at depth 64; `hierarchy` and
  `live_mask_graph` both reject cycles, missing parents, non-group parents, image-bearing groups and
  over-long chains, matching `ProjectStore.swift:175-200`.
- Round trip of every layer kind: `Document::manifest()` (`document.rs:2526`) clones
  `renderer.layers()` verbatim, and `Layer`'s serde attributes are symmetric
  (`skip_serializing_if = "Option::is_none"` paired with `#[serde(default)]`). Pixel, group,
  adjustment, type (including `width` for paragraph text), rectangle/ellipse/path shape, mask with
  `maskPlacement` and `maskLinked`, clipping via `maskSourceID`, artboard frame and background,
  effects including Blend If and both overlays, blend mode, opacity, hidden and resolution all
  survive byte-for-byte. No field is written-but-not-read and no default differs between the two
  directions.
- Collapsed-folder state is deliberately session-only, exactly as `project-format.md` states
  ("Collapse state is not serialized"), so it is not a divergence from the reference.
- Unknown keys in a macOS-written manifest are silently ignored by serde, which is the right
  behaviour for forward compatibility.

**PSD writer byte layout**, checked against the spec:

- Channel lengths at `psd.rs:555` include the 2-byte compression word, because `c` is built as
  `vec![0, 1]` plus counts plus data (`psd.rs:543`). Zero-size records (dividers, folders) write a
  2-byte channel of compression 0 with no data (`psd.rs:541`), which is what Photoshop itself does.
- Extra-data length at `psd.rs:591` is written after the block is complete.
- Layer name is a Pascal string padded to 4 (`Writer::pascal`, `psd.rs:454-460`), correct for the
  empty-name case too.
- `luni` block length (`psd.rs:579-580`) is `4 + 2n` plus a 2-byte pad for odd `n`, which is always
  a multiple of 4.
- `lsct` is length 12 with type, `8BIM`, `pass` (`psd.rs:581`), and the record order is divider
  (type 3) then contents then the folder record (type 1), bottom-up, which is correct.
- Mask record is written as exactly 20 bytes: rect(16), default(1), flags(1), pad(2)
  (`psd.rs:567-571`). The reader tolerates both 20 and 36 by seeking with
  `r.pos = mask_start + mask_len` (`psd.rs:238`), and channel `-2` correctly maps to the first rect
  in both cases.
- Blending ranges length 0 (`psd.rs:575`), flags bit 3 set to mark bit 4 meaningful (`psd.rs:562`),
  `layer_info` padded to even (`psd.rs:598`), global layer mask length 0 (`psd.rs:602`).
- `lfx2` body is version 0, descriptor version 16, descriptor, padded to 4 (`psd.rs:584-589`),
  which the reader mirrors at `psd.rs:762-767`.
- `records.i16(recs.len())` cannot wrap: `MAX_LAYERS` is 10,000 and a group costs 2 records, so the
  worst case is 20,000, well under 32,767.

**PSD reader tolerance**

- A negative layer count is handled with `unsigned_abs()` (`psd.rs:206`). Discarding the sign is
  safe here, because its only meaning concerns the merged image, and the merged image is read only
  when there are no layer records at all.
- Grayscale (mode 1) and RGB (mode 3) both read; CMYK, Lab, indexed, bitmap, duotone, multichannel
  and PSB are each refused with a specific message (`psd.rs:169`, `:159`).
- ImageMagick's byte-reversed `mron` blend key is special-cased (`psd.rs:222`); unknown keys warn
  and fall back to Normal rather than failing.
- Huge additional-info blocks are bounded: the loop condition is `r.pos + 12 <= extra_end` and every
  read goes through `need()`.
- `lfx2` keys this port does not write are simply absent from the `Descriptor` and all readers use
  `.unwrap_or(default)` (`psd.rs:728-758`), so a Photoshop style with Satin, Pattern Overlay or
  contour curves imports the parts that are understood without failing.
- `unpack_bits` (`psd.rs:60-82`) clamps repeats to the remaining output and errors rather than
  over-running; `read_channel` sizes the buffer from `rect_dims`, which rejects inverted rects,
  rects over 30,000 and coordinates past +/- 2^24 (`psd.rs:136-142`).
- The per-file 400 megasample budget is debited before every channel allocation (`psd.rs:289`,
  `:310`), and the result of `psd::read` is round-tripped through `Manifest::parse` at `psd.rs:408`
  so a PSD can never produce a project the validator would reject.
- `build_mask` (`psd.rs:424-442`) indexes the `-2` plane with the same `mw`/`mh` the plane was sized
  from, and clips every write to the target rect.

**Images**

- `decode_image` checks header dimensions before any pixel buffer exists, applies EXIF orientation,
  then re-checks the post-rotation size (`document.rs:3081-3088`).
- 16-bit PNG is refused inside a `.comp` (`png_io.rs:41`, matching the reference) and down-converted
  by `to_rgba8` on import. Indexed PNG is expanded by `Transformations::EXPAND` before the
  `ColorType::Indexed` rejection ever fires, so indexed-with-tRNS works. APNG and animated
  GIF/WebP yield the first frame; APNG inside a `.comp` is rejected outright via `frames != 1`.
- No half-created layer on a decode failure: `import_surface`, `add_image_surface` and `open_image`
  all decode fully before `begin_edit`, so a failed decode leaves no open edit and no orphan record.
- Colour profiles are ignored throughout, which is a documented limitation of the working space
  rather than a bug.

**Brushes and patterns**

- `.abr` version dispatch covers 1, 2, 6, 7 and 10 with a clear message for anything else
  (`abr.rs:66-131`); 6/7/10 additionally validate the subversion. Both the sample-section walk and
  the `8BPT` container bound every offset with `checked_add(...).filter(|e| *e <= data.len())`
  (`abr.rs:111`, `:246`, `:250`), and every loop provably advances at least four bytes per
  iteration, so no malformed file spins.
- Sample section offsets, the 37-byte key and the subversion-dependent 10/264-byte skip are all
  read through the bounds-checked `Reader`.
- Both budgets are debited before allocation, and per-pattern channel decoding gets its own
  200 MP sub-budget (`abr.rs:288`).
- An `.abr` renamed `.gbr` is dispatched to `gbr::parse` (`ui/brushes.rs:60`) and refused cleanly:
  the `GIMP` magic test fails, the hose path then fails to parse a cell count from the binary
  header, and it bails with "This is not a GIMP brush file." The reverse (`.gbr` renamed `.abr`)
  reads the header length's high bytes as version 0 and bails too.
- `patterns::clean`/`path_for`/`load`/`save` all run the same sanitizer, so a name containing `/`
  or `..` cannot escape `~/.local/share/compositor/patterns`.

**Export**

- The JPEG alpha matte composites straight-alpha colour over the background correctly
  (`document.rs:3036-3039`), as does the artboard variant.
- `export_layers` numbers files from the bottom, so names cannot collide, and it draws from
  `visible_layers`, whose `entries` walk already excludes the children of a hidden group
  (`format/mod.rs:433`). Adjustment layers and clipped layers are filtered out at
  `document.rs:1287`.
- `export_sizes::remake` rejects out-of-range targets before duplicating the document
  (`export_sizes.rs:64`), excludes adjustment layers from the reframe element set
  (`export_sizes.rs:111`), and measures a clipped element by what actually shows through its mask
  via `layer_coverage`. Unicode in `file_name` survives `clean_file_name`, which keeps every
  `char::is_alphanumeric`.
- `export_avif` passes arguments without a shell, cleans up its temporary PNG on both the success
  and failure paths, and names the temp file with the pid so two exports cannot collide.
- GIF export's speed 10 is inside the valid 1..=30 range and is a deliberate quality/time trade;
  the 30,000-pixel canvas limit is well under GIF's 65,535.

**Autosave**

- Recovery files are named by document UUID (`autosave.rs:69`), so two documents with the same title
  never collide; the title lives in a sibling `.title` file.
- The write is atomic in the sense that matters: `Snapshot::write` goes through `format::save`,
  which stages and renames, so a reader never sees a half-written package and a crash mid-write
  leaves the previous autosave intact.
- The discard/write race is handled properly with a generation counter: a write that finishes after
  the user saved or closed re-discards what it just wrote (`autosave.rs:339-343`), and
  `autosave_all` holds a `writing` flag so one document never has two workers
  (`ui/mod.rs:1171-1179`).
- A recovered document is re-opened untitled with its path cleared and history marked unsaved
  (`ui/mod.rs:1038-1043`), so Save cannot write back over the autosave.
