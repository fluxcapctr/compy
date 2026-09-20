# GPU compositing: where it stands and what went wrong

Written 2026-09-19 for a fresh pair of eyes. Nothing here runs by default; the desktop path hung the
graphics card, so both GPU modes are opt-in through an environment variable and off otherwise.

## The goal

Composite the document on the GPU instead of the CPU, and get the frame onto the screen without
copying it back through system memory. The CPU renderer (`src/render/`) is correct and is the reference;
the GPU path must match it pixel for pixel and only then be faster.

## What exists

- `src/gpu/mod.rs` (581 lines): a wgpu 30 compositor. `Gpu::new` picks a Vulkan adapter (high
  performance), opens the device with the two dma-buf export extensions when it can, and keeps a
  texture cache keyed by `SurfaceKey` (layer id, slot, a stable surface identity). `compose` uploads
  every layer and mask that changed, then runs full-screen passes that mirror the CPU order: 13 blend
  modes, layer masks, folders, clipping stacks, adjustment layers (LUT based), effects buffers,
  mipmapped sampling for reductions. `render` reads the result back as bytes; `present` copies it into
  an exported buffer instead.
- `src/gpu/shader.wgsl` (246 lines): the passes.
- `src/gpu/present.rs` (128 lines): `export_buffer` makes a LINEAR-tiled `B8G8R8A8_UNORM` VkImage
  with dedicated memory exported as a dma-buf fd, wraps it as a wgpu texture (`COPY_DST`), and
  `gdk_texture` hands the fd to GTK through `GdkDmabufTextureBuilder` (fourcc ARGB8888, modifier
  LINEAR, premultiplied). `Buffer::drop` waits for the wgpu device to go idle, then destroys the image
  and frees the memory.
- `src/render/gpu_plan.rs` (157 lines): turns the renderer's layer tree into a `Plan` the GPU draws.
- `src/ui/canvas.rs`: `present_frame` (COMPOSITOR_GPU=present) rotates three exported buffers and sets
  the resulting GdkTexture on the picture widget; `gpu_frame` (COMPOSITOR_GPU=readback) renders on the
  GPU and paints the bytes through Cairo as the CPU path does. Anything else, or no variable, is CPU.
- `tests/gpu.rs`: a parity test that composes blends, masks, folders, clips and adjustments on both
  paths and compares. It needs COMPOSITOR_GPU set to run at all; without it, it returns early.

Commits: 801b8a1 (the compositor and the parity test) and 83f4fd1 (presentation).

## What works

- The readback path is correct: the parity test passes, and it found and fixed a real CPU bug on the
  way (clip stack children padded across their base).
- The readback path is not faster than the CPU. A 7 megapixel viewport composes in a few milliseconds
  but copying it back and re-uploading it as a GTK texture costs about what the CPU composite did.
- The presented path drew correct frames at 3 to 4 ms per 7 megapixel frame, then hung the card.

## What happened

Machine: AMD Ryzen 7 9800X3D with its integrated GPU (card0, PCI 74:00.0) and a Radeon RX 9070 XT
(Navi 48, card1, PCI 03:00.0). Mesa and vulkan-radeon 26.2.2, GTK 4.22.4, kernel 7.2.3. GTK 4.22 uses
its Vulkan renderer here. Hyprland is the compositor.

Testing COMPOSITOR_GPU=present on 2026-09-19 between 11:44 and 11:49 produced five gfx ring timeouts
on the 9070 (03:00.0) and a full GPU reset each time; the desktop froze for seconds and other apps
lost their GL and Vulkan contexts. `journalctl -k` shows, immediately before the first timeout, a burst
of page faults attributed to our process:

```
amdgpu 0000:03:00.0: [gfxhub] page fault (src_id:0 ring:157 vmid:5 pasid:1333)
amdgpu 0000:03:00.0:   in page starting at address 0x0000800101cef000 from client 10
amdgpu 0000:03:00.0: GCVM_L2_PROTECTION_FAULT_STATUS:0x0050113B
amdgpu 0000:03:00.0:          Faulty UTCL2 client ID: TCP (0x8)
amdgpu 0000:03:00.0:          MORE_FAULTS: 0x1
amdgpu 0000:03:00.0:          WALKER_ERROR: 0x5
amdgpu 0000:03:00.0:          PERMISSION_FAULTS: 0x3
amdgpu 0000:03:00.0:          MAPPING_ERROR: 0x1
amdgpu 0000:03:00.0:          RW: 0x0
... (a dozen more faults at neighbouring pages)
amdgpu 0000:03:00.0: ring gfx_0.0.0 timeout, signaled seq=18500856, emitted seq=18500859
amdgpu 0000:03:00.0:  Process compositor pid 1811026 thread compositor pid 1811026
```

Read: a shader texture fetch (client TCP is the texture cache) read pages that were not mapped
(MAPPING_ERROR, RW 0 means a read), in our process. Something sampled a texture whose memory had been
freed or never bound. The faults come in runs of consecutive 4 KB pages, the shape of a texture being
walked. GTK's own Vulkan renderer runs inside our process too, so "our process" covers both the wgpu
device and GTK's device.

## Hypotheses, most likely first

1. **The exported buffers are destroyed while GTK still samples them.** `present_frame` clears and
   rebuilds all three buffers whenever the viewport size changes (`gpu.buffers.clear()`), and
   `Buffer::drop` waits only on the wgpu device, not on GTK's. GTK may still have a frame in flight
   that samples the imported image on its own VkDevice and queue. Three rotating buffers also assume
   GTK is done with a buffer by the time it comes round again, with no release tracking at all. A
   window resize during the test would fit the timing. The fix shape: never free an exported buffer
   until GTK says it is done (`GdkDmabufTextureBuilder::build_with_release_func`), and keep a pool
   rather than three fixed slots.
2. **Layout and ownership of the external image.** The image is created with `initial_layout
   UNDEFINED` and handed to wgpu as `TextureUses::UNINITIALIZED`; wgpu's copy transitions it to
   TRANSFER_DST and leaves it there, while GTK imports the dma-buf and samples it as an external image
   with its own barriers. On RADV a LINEAR image with DCC or with a mismatched layout can read as
   garbage rather than fault, so this is less likely to be the page fault, but worth checking with the
   validation layer.
3. **A wgpu texture freed under a submitted command buffer.** The layer texture cache evicts by frame
   age; `present` waits with `poll(wait_indefinitely)` after the copy, so this should be safe, but the
   readback path (which never faulted) uses the same cache, which argues against this one.
4. **Two GPUs.** wgpu asks for the high performance adapter (the 9070). Which device GTK's Vulkan
   renderer opened is not known; if it is the integrated GPU, the dma-buf crosses devices and a
   LINEAR modifier is the only thing that could work, which is what is used. Worth confirming with
   `GDK_DEBUG=vulkan` or `vulkaninfo` (not installed; `omarchy pkg add vulkan-tools`).

## What was tried

- Double vkDestroyImage on buffer drop: fixed with a drop guard (the wgpu texture only runs a no-op
  release).
- Thread-local drop order: buffers are owned by `Gpu`, which lives in a thread local, so they die
  with the device, not after it.
- A fresh GdkTexture object per frame, so GTK does not show a cached import of an older frame.
- `device_wait_idle` in `Buffer::drop`.

None of these prevented the hang. The path was parked rather than risk more resets on the working
desktop.

## Rules for working on this

- Do not run COMPOSITOR_GPU=present on the desktop casually. A hang resets the 9070 for the whole
  session and can take other apps down with it. If you must test presentation, do it with everything
  else saved and closed, and read `journalctl -k -f` in a terminal beside it.
- COMPOSITOR_GPU=readback is safe and exercises everything except export and import. Use it and
  `cargo test --release --test gpu` (with the variable set) for parity.
- `VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation` (package vulkan-validation-layers) will flag
  layout and lifetime mistakes on the wgpu side before the hardware does. GTK's device is a separate
  VkDevice and the layer sees it too.
- Consider forcing both wgpu and GTK onto the integrated GPU for experiments (wgpu: prefer
  `PowerPreference::LowPower` behind a variable; GTK: check its device selection), since a fault
  there does not take the display down if Hyprland is on the 9070. Confirm first which card Hyprland
  drives (`cat /proc/$(pgrep -x Hyprland)/environ | tr '\0' '\n' | grep -i drm`, or its log).
- Read-only review is welcome; the previous review round (REVIEW_RESULTS_6.md, finding 17) already
  noted the missing release tracking.

## A prompt for the review assistant

---

The Rust project at /home/estevens/code/compositor-linux has a parked GPU compositor. Read
GPU_HANDOFF.md first, then src/gpu/mod.rs, src/gpu/present.rs, src/gpu/shader.wgsl,
src/render/gpu_plan.rs and present_frame/gpu_frame in src/ui/canvas.rs. Do not run the app with
COMPOSITOR_GPU=present: it has hung this machine's graphics card five times. COMPOSITOR_GPU=readback
and the parity test in tests/gpu.rs are safe to run.

Find the cause of the page faults and gfx ring timeouts described in the handoff, or the most
probable causes ranked with the evidence for each, citing file:line. Confirm from the code which
VkDevice GTK 4.22's Vulkan renderer would open on this machine (the 9070 XT drives the display; the
Ryzen's integrated GPU is also present) and whether the dma-buf crosses devices.

Then write, do not implement, the design for a presentation path that cannot free or overwrite a
buffer GTK is still reading: release callbacks from GdkDmabufTextureBuilder, a buffer pool with
explicit ownership, the image layouts and barriers for an image shared between two VkDevices in one
process, and whatever synchronization GDK 4.22 offers for dma-buf import. Be specific enough that
another engineer can implement it from your notes without rediscovering anything: which functions
change, what they do, in what order, and how to verify each step without hanging the card.

Do not change any source file. Append your findings and the design to GPU_HANDOFF.md under a heading
"Diagnosis" and a heading "Proposed design". No em dashes in your output.

## Diagnosis

Reviewed 2026-09-19 at commit `080cdb129386c483ce73ee26bb435cfe7fef6380`.
This is a source audit, not a reproduction of the hardware hang. The page-fault excerpt does not
identify the responsible VkImage, VkDevice, or submission. The faults therefore cannot be assigned
to one defect conclusively. The strongest application defect is a borrowed-fd lifetime violation,
followed by unconditional reuse of buffers still owned by consumers. External image layout and
ownership are also incomplete. All three must be addressed before presentation is enabled again.

The GTK implementation was checked against the official
[GTK 4.22.4 source archive](https://download.gnome.org/sources/gtk/4.22/gtk-4.22.4.tar.xz),
extracted during this audit at `/tmp/compositor-gpu-review/gtk-4.22.4`.
Archive SHA-256: `51bd9f60c7d23a665a556c7364c21fb2e4e282566b3e7e092455e8f910330893`.
Below, paths prefixed `GTK:` refer to that version, not current GTK main.

### Ranked causes and evidence

1. **Borrowed dma-buf descriptors become invalid, or refer to different allocations. Confirmed bug;
   strongest concrete route to an invalid import.** `src/gpu/present.rs:115-127` passes the buffer's
   raw fd to `DmabufTextureBuilder::set_fd` and calls `build()` without a release callback or an
   owning reference. `src/ui/canvas.rs:1701-1705` destroys the whole pool before allocating the next
   size. `src/gpu/present.rs:25-34` destroys the image and allocation; its `OwnedFd` then closes.
   The previous texture is still the picture's paintable until `src/ui/canvas.rs:224` replaces it.
   Other canvases can retain textures indefinitely because the pool is shared by the thread-local
   GPU at `src/ui/canvas.rs:1657-1670`. Switching between differently sized canvases is sufficient;
   this does not require an interactive window resize.

   GDK stores the plane descriptors without duplicating them when constructing the texture
   (`GTK:gdk/gdkdmabuftexture.c:194-284`). Actual Vulkan import duplicates the descriptor later
   (`GTK:gsk/gpu/gskvulkanimage.c:1273-1284`). Between those operations, or before a later reimport,
   the application can close fd N and reopen a new, smaller buffer as fd N. GTK still has the old
   width, height, offset and stride. It can consequently import the new allocation using the old
   image description. An invalid fd may simply fail import; a reused valid fd is more dangerous.
   This is a plausible explanation for invalid texture accesses, not proof that it occurred in
   the recorded crash. GDK explicitly requires the caller to retain the fds until texture release.
   See [GDK's builder contract](https://docs.gtk.org/gdk4/method.DmabufTextureBuilder.build.html).

   **Correction to the original hypothesis:** once GTK has successfully imported the allocation,
   its duplicated/imported memory reference can keep the backing dma-buf alive independently of
   the producer's allocation. `vkFreeMemory` on the producer alone is not evidence that GTK's
   already-imported memory became unmapped. The definite violation is the lost lifetime of the
   borrowed fd and its metadata association. Do not describe this as a proven GPU use-after-free.

2. **A slot is overwritten while an older GTK texture can still read it. Confirmed race.**
   `src/ui/canvas.rs:1707-1713` picks the next slot modulo three regardless of consumer ownership.
   Waiting in `src/gpu/mod.rs:338` only finishes producer work. Waiting on the producer's device
   in `Buffer::drop` cannot finish GTK's independent device. A cached paintable in another tab
   makes even an arbitrarily long rotation unsafe. A fresh GdkTexture does not make the underlying
   allocation fresh. Concurrent copy and sampling can corrupt a frame; by itself this does not
   establish why a mapping fault occurred. There is no evidence that three frames is a bound on
   GTK's use, and adding more fixed slots would not establish one.

3. **Missing external layout and queue ownership protocol. Confirmed synchronization gap;
   an independent candidate for undefined GPU behavior.** The producer creates an EXCLUSIVE image
   (`src/gpu/present.rs:55-67`), wraps it as UNINITIALIZED (`:83-107`), copies into it, and publishes
   it without a release barrier (`src/gpu/mod.rs:333-340`). wgpu-hal 30.0.1 maps COPY_DST to
   TRANSFER_DST_OPTIMAL (`src/vulkan/conv.rs:241-258` in the cached crate). GTK imports the image
   with a tracked initial layout of GENERAL (`GTK:gsk/gpu/gskvulkanimage.c:1180-1201`). Producer
   completion is not a substitute for publishing the correct external layout or ownership.

   There is a consumer-side limitation too: GTK's transition helper uses QUEUE_FAMILY_IGNORED on
   both sides (`GTK:gsk/gpu/gskvulkanimage.c:2022-2044`). This version has no explicit EXTERNAL or
   FOREIGN ownership transfer in its GDK/GSK source. Thus adding a producer release barrier and
   claiming a fully paired, portable Vulkan ownership protocol would overstate what stock GTK
   implements. External sharing requires ownership handling even across instances on the same
   physical GPU. See [Vulkan synchronization rules](https://docs.vulkan.org/spec/latest/chapters/synchronization.html#synchronization-queue-transfers).

4. **Export/import capability and memory-layout assumptions are unchecked. Confirmed validation
   gap; hardware-specific contribution unproven.** `src/gpu/present.rs:48-109` does not query
   external image format properties before requesting LINEAR plus TRANSFER_DST plus SAMPLED.
   It advertises modifier zero unconditionally at `:120-124`; GTK creates a modifier-explicit
   image at `GTK:gsk/gpu/gskvulkanimage.c:1197-1227`. No query proves that this exact format,
   tiling, usage, handle type and extent is exportable and compatible with the consumer. Nor is
   DEVICE_LOCAL allocation a guarantee of importability by a different GPU. Missing checks do
   not establish that RADV rejected or misinterpreted these particular allocations. There is no
   evidence here of DCC actually being enabled on these LINEAR images.

5. **A driver defect or another imported-image failure remains possible.** Correct dma-buf
   reference management should protect an already-imported backing allocation. If the lifetime,
   ownership and capability defects are removed and faults persist, capture validation output,
   actual device identities and the first failing submission before blaming the layer cache.
   The ordinary wgpu texture cache is used by readback too; it does not manually free Vulkan
   resources like the export path. Its surface identity now uses a monotonic Cairo user-data id
   (`src/render/gpu_plan.rs:21-29`), so round 6's pointer-reuse warning is no longer applicable to
   this checkout. Cache eviction at `src/gpu/mod.rs:404-407` is not the leading suspect.

### Which GPU GTK selects

The selection algorithm is confirmed; the actual desktop selection is not observable here.
`GTK:gdk/gdkvulkancontext.c:1704-1727` enumerates physical devices. At `:1779-1794` it walks them in
loader order and chooses a graphics queue, returning after successful device creation at
`:1856-1917`. It does not match the display GPU, consult Wayland dma-buf feedback for this choice,
or prefer the discrete GPU. `GDK_DEBUG=vulkan` prints the enumeration and selected device/queue.
See the [versioned device-selection source](https://gitlab.gnome.org/GNOME/gtk/-/blob/4.22.4/gdk/gdkvulkancontext.c).

The application independently requests HighPerformance at `src/gpu/mod.rs:172`; its Vulkan
device creation is at `:181-188`. Assuming that chooses the 9070 as in the original test:

| GTK's first usable enumerated device | Relationship to the producer |
| --- | --- |
| RX 9070 XT, PCI 03:00.0 | Same physical GPU, separate VkInstances, VkDevices and queues |
| Ryzen integrated GPU, PCI 74:00.0 | Different physical GPUs as well as separate logical devices |

The fact that Hyprland drives the 9070 does not decide which row applies. Loader configuration,
ICDs, device-selection layers and process environment can change enumeration. There is no
`GDK_VULKAN_DEVICE` selection branch in this GTK source. Do not invent that override.

This execution environment has no `/dev/dri`, no visible compositor or Hyprland process, and no
running desktop state to inspect. It cannot confirm the original process's enumeration order or
whether its dma-buf crossed physical GPUs. A future standalone, non-exporting GTK diagnostic
should log `GDK_DEBUG=vulkan`, and a Vulkan enumeration helper should record deviceUUID,
driverUUID and PCI/DRM identity in the same launch environment. Record the producer's identities
too. Check these before any import experiment. GTK's raw-device getters are private headers in
4.22.4, so do not plan around a public gtk-rs getter for them. A separate GTK diagnostic avoids
starting a second application instance or disturbing its agent socket.

### What GTK 4.22.4 actually synchronizes

- `DmabufTextureBuilder` has no acquire-fence property and returns no release-fence fd. Its destroy
  callback reports texture release. `GTK:gdk/gdkdmabuftexture.c:77-89` invokes it at disposal.
- Vulkan import optionally extracts existing dma-buf writer fences using
  `gdk_dmabuf_export_sync_file(fd, DMA_BUF_SYNC_READ)` and imports the result as a temporary
  SYNC_FD semaphore (`GTK:gsk/gpu/gskvulkanimage.c:1295-1319`). The transition helper adds it to
  the submission wait list (`:2013-2019`). The test there compares a stage value with a layout
  enum; the initial TOP_OF_PIPE and GENERAL values both happen to be 1. Do not copy that idiom.
- This waits on fences already attached to the dma-buf. It does not publish the application's
  Vulkan work automatically. The current host wait prevents an unfinished producer copy from
  reaching GTK, but does not solve layouts, ownership or the reverse dependency.
- The Vulkan import path takes a texture toggle reference (`GTK:gsk/gpu/gskvulkanframe.c:223-236`;
  `gskgpuimage.c:97-121`). Frame cleanup waits for its VkFence before releasing operation resources
  (`gskvulkanframe.c:135-158`; `gskgpuframe.c:86-107`). This supports using texture release as the
  lifetime/reuse boundary for this consumer. Merely replacing the picture's paintable is earlier
  than that boundary. The pool must also respect references retained by snapshots and caches.
- No read-completion sync-file publication was found in this Vulkan import path. The
  `gdk_dmabuf_import_sync_file` call in `gskgpudownloadop.c:192` concerns a GL-produced download,
  not a release fence for these Vulkan-sampled input textures. Polling the dma-buf alone is not
  a substitute for waiting for the texture's release callback.

### Other correctness defects found in the requested files

- **Background uniforms are packed incorrectly.** `src/gpu/mod.rs:150` pads each affine row to
  four floats. The background assignment at `:366` therefore produces `(x,y,width,0)` and
  `(height,tile,shadow,0)`. `src/gpu/shader.wgsl:213-215` reads those as a rectangle, then tile
  and shadow. The rectangle height is zero, tile becomes document height, and shadow becomes
  tile size. The current parity test never calls `present` or `compose` with a background.
- **Presentation composites the document against the surround too early.**
  `src/gpu/mod.rs:363-376` paints the background before the document stack and adjustment passes.
  Adjustment layers can then modify the checkerboard/surround, and blend modes see the background
  as part of the document. The presentation plan has no document-rectangle clip, so layers and
  effects outside the document can appear in the surround. The CPU clips its drawing at
  `src/ui/canvas.rs:1758-1760`. Composite the transparent document first, then clip and place that
  result over the background in a final pass. Include display scale explicitly for the shadow;
  the shader's `u.mask_size.x` is currently left at the default 1.
  There is a CPU reference inconsistency to settle before changing blend semantics: crisp zoom
  and GPU readback composite a transparent document first (`src/ui/canvas.rs:1780-1818`), whereas
  the normal-zoom CPU fallback calls `renderer.draw(cr)` directly on the background at `:1824`.
  `src/render/mod.rs:649-675` does not isolate that draw when the target is already an image surface.
  Compare both with the Swift specification; do not claim the current CPU viewport paths agree.
- **Rectangular mip tails need coverage tests.** `src/gpu/mod.rs:278-289` clamps each mip dimension
  to at least one, while `src/gpu/shader.wgsl:237-240` always reads a 2x2 footprint. Once a source
  dimension is one, some reads are outside it. This is a sampling/parity problem, not evidence
  of a Vulkan mapping fault through wgpu's bounds-protected shader path. Test 1xN, Nx1 and odd
  extents, and define edge weighting to match the CPU reference.

### Validation performed

Read all requested application files, the parity test, gtk-rs 0.11.4's builder implementation,
wgpu/wgpu-hal 30.0.1 interop code, and the GTK source above. No presentation command or application
instance was run. No system packages or source files were changed.

The permitted command `COMPOSITOR_GPU=readback cargo test --release --test gpu -- --nocapture`
built successfully but **failed before any parity assertions**, while creating the `composite`
pipeline: `Internal error in ShaderStages(FRAGMENT) shader: A image was used with multiple samplers`.
That diagnostic comes from Naga 30.0.1's GLSL backend (`src/back/glsl/mod.rs:438-440`) and indicates
the GL fallback's shader translation restriction; the adapter
name is not printed because construction fails first. No claim is made that this reproduces the
9070 Vulkan behavior. The test must log backend/device before pipeline creation and fail or report
an explicit skip when the required Vulkan adapter is unavailable. Its present early-return path
at `tests/gpu.rs:37` can otherwise report success without exercising a GPU. Validation layers are
not installed here. The historical parity pass in the handoff was not reproduced by this audit.

## Proposed design

This is an implementation plan only. Keep the default CPU path and the presentation opt-in disabled
until its gates below pass. The ownership design can eliminate application-side premature free and
overwrite. It cannot guarantee against driver defects, and stock GTK 4.22.4's missing external
ownership transfers prevent claiming a complete portable Vulkan protocol merely by changing the
producer. Treat that consumer compatibility issue as an explicit gate, not an assumed fix.

### 1. Replace rotating slots with owned publication leases

Change `src/gpu/present.rs` to separate allocation lifetime from pool availability:

- `ExportAllocation` owns the image, VkDeviceMemory, dma-buf `OwnedFd`, immutable dimensions,
  allocation size, format, modifier, plane stride/offset, and a strong device-owner reference.
  An `ash::Device` clone is only a function table/handle; it does not keep the wgpu device alive.
  Hold a `wgpu::Device` clone or an explicit shared device owner until raw resources are destroyed.
- `PresentationPool` owns slots indexed by a monotonic id. Each records a size/generation key,
  a publication serial and a state: `Free`, `Producing`, `Published`, `Retired` or `Quarantined`.
  `Retired` also records whether a producer operation or publication is outstanding. Do not reuse
  numeric ids after resize. A pool entry must not hold a strong GdkTexture reference.
- A publication owns a strong `Arc<ExportAllocation>`. The GTK release closure retains this Arc
  and the exact `(slot_id, generation, publication_serial)` until release. It sends a completion
  event through a thread-safe channel. It must not touch GTK widgets, the thread-local RefCell,
  or submit Vulkan work. Keep normal pool cleanup on the owning thread. If the pool has already
  gone away, the lease still keeps the allocation and device alive until release.
- No reader can access a `Free` slot; only `Free` can become `Producing`. Publish only after
  successful producer completion and external release. A valid callback is the only transition
  from `Published` to reusable or retired storage. Reject stale/duplicate callback serials.
  An allocation cannot be destroyed while either producer work or a publication lease exists.

Replace `Gpu::{buffers,buffer_size,next_buffer}` in `src/gpu/mod.rs:91-95` with this pool. In
`present_frame`, drain release events, request a free slot of the correct size, then render and
publish it. On resize retire the previous size generation; do not clear published allocations.
On document/tab changes, retained old paintables remain leases regardless of which canvas draws.

Bound total allocated bytes, including retired and quarantined slots. If no free slot exists,
retain the previous frame or use CPU fallback and schedule a retry on a release event. Do not
block the GTK main loop waiting for a callback that needs it, and do not allocate without a bound.
CPU fallback must replace the old picture paintable so it does not retain an otherwise unnecessary
lease. Update presented revision/size only after successful publication. Requeue drawing when a
release makes a deferred frame possible.

Change `gdk_texture` to take a publication lease rather than `&Buffer`. Keep the fd open through
`build_with_release_func`; a separate per-publication dup is optional if the lease already owns
the allocation. Never close/recycle it when `set_paintable` returns. Leave `update_texture` unset
initially so old/new texture references cannot accidentally prolong a chain of publications.

**Handle builder failure explicitly.** In cached gtk-rs 0.11.4,
`src/dmabuf_texture_builder.rs`, `build_with_release_func` boxes its closure before calling C and
does not reclaim that box on error. GTK only installs the destroy callback after successful
construction (`GTK:gdk/gdkdmabuftexture.c:281-284`). A closure capturing an allocation can therefore
leak it on failed build. Use a corrected binding or a small audited FFI wrapper around
`gdk_dmabuf_texture_builder_build`: pass a boxed lease, transfer it to C only on success, reclaim
it exactly once on failure. Check both error and null-result cases. After a failed publication,
retire the producer-completed allocation until its native ownership state is reconciled; do not
silently mark externally released storage `Free`.

### 2. Negotiate the exact external image before allocating

In `Gpu::new`, keep ordinary rendering separate from export capability. Enumerate the available
extensions before enabling them. Record adapter backend, UUIDs and PCI/DRM identity before creating
the pipeline. For the first supported presentation configuration require Vulkan and matching
producer/consumer physical-device and driver identities. A mismatch or unverified identity means
CPU/readback fallback, not a cross-device experiment. GTK's selected device needs the diagnostic
or a reviewed consumer integration described above; its display GPU is not an identity check.

In `export_buffer`, negotiate BGRA8/ARGB8888 with `display.dmabuf_formats()`. Use
`VK_EXT_image_drm_format_modifier` and initially offer only DRM_FORMAT_MOD_LINEAR. Query
`vkGetPhysicalDeviceFormatProperties2` for modifier zero and its memory-plane count; require one
plane. Query `vkGetPhysicalDeviceImageFormatProperties2` with
`VkPhysicalDeviceExternalImageFormatInfo(DMA_BUF_EXT)` and
`VkPhysicalDeviceImageDrmFormatModifierInfoEXT`, the exact usage/flags/extent, and inspect
`VkExternalImageFormatProperties` for exportability, compatible handles and dedicated requirements.
The consumer must independently support import and sampling for that format/modifier.

Create a modifier-tiled image using `VkImageDrmFormatModifierListCreateInfoEXT([LINEAR])`, dedicated
exportable memory and the returned memory-type requirements. Query the actual selected modifier
with `vkGetImageDrmFormatModifierPropertiesEXT`, then query plane zero using
`VK_IMAGE_ASPECT_MEMORY_PLANE_0_BIT_EXT`. Feed the returned offset and row pitch to GDK, checking
u32 conversions and that the full plane range fits the allocation. Keep every partial construction
step under RAII cleanup. Do not fall back to a guessed modifier or continue after capability failure.
See [the modifier extension](https://docs.vulkan.org/refpages/latest/refpages/source/VK_EXT_image_drm_format_modifier.html)
and [external image properties](https://docs.vulkan.org/refpages/latest/refpages/source/VkExternalImageFormatProperties.html).

### 3. Keep native export-image state out of wgpu's texture tracker

Replace the export image's `texture_from_raw`/no-op drop wrapper with a raw Vulkan destination
owned entirely by `ExportAllocation`. Keep ordinary compositing targets managed by wgpu. This
avoids modifying the export image to GENERAL behind a tracker that still believes COPY_DST.

In `Gpu::present`, compose the document and final background into an ordinary wgpu target. Create
an encoder, then use `CommandEncoder::transition_resources` to establish COPY_SRC for that source.
Use `CommandEncoder::as_hal_mut::<Vulkan, ...>` and the HAL encoder's `raw_handle()` to record the
native destination barrier, copy and external release in the same command stream. Read the source
image handle through `Texture::as_hal`; do not destroy it or change its tracked source layout.
These APIs are present in wgpu 30.0.1 (`src/api/command_encoder.rs:277,431`) and wgpu-hal 30.0.1
(`src/vulkan/mod.rs:835,1126`). Do not call wgpu encoder methods while inside its HAL callback.

Keep the source target and destination lease alive through submission completion. Submit through
wgpu's queue so queue serialization remains under one owner. Initially wait for that submission
on the host with a bounded timeout, including the release barrier. Do not publish on timeout,
device loss or any validation error. Mark uncertain allocations `Quarantined`; a timeout does
not authorize destroying resources that might still be executing. Do not run raw `vkQueueSubmit`
beside wgpu submissions without a separate, audited queue synchronization design.

### 4. Define both halves of the Vulkan handoff

For the initial same-physical-device, same-driver configuration, let `Qp` be the producer queue
family, `Qc` the consumer queue family and `E = VK_QUEUE_FAMILY_EXTERNAL`. Queue family numbers
belong to each device's context; matching numbers do not remove the external-instance boundary.
All barriers cover color aspect, mip 0, layer 0. The following is the required protocol:

| Step | Layout and ownership | Access dependency |
| --- | --- | --- |
| First producer use | UNDEFINED to TRANSFER_DST_OPTIMAL; local Qp, no prior external acquire | no previous contents; destination COPY/TRANSFER_WRITE |
| Producer copy | source TRANSFER_SRC_OPTIMAL, destination TRANSFER_DST_OPTIMAL | write the entire published extent |
| Prepare external layout | TRANSFER_DST_OPTIMAL to GENERAL on Qp | COPY/TRANSFER_WRITE made available |
| Producer release | GENERAL to GENERAL, Qp to E | prior writes to external release; signal completion after this |
| Consumer acquire | GENERAL to GENERAL, E to Qc | wait producer completion, make content visible to consumer reads |
| Consumer use | GENERAL to SHADER_READ_ONLY_OPTIMAL, or keep GENERAL consistently | FRAGMENT_SHADER/SHADER_SAMPLED_READ; include transfer reads if used |
| Consumer end of use | return to GENERAL, then GENERAL-to-GENERAL release Qc to E | all consumer reads complete before release notification |
| Producer reuse | GENERAL-to-GENERAL acquire E to Qp, then transition to TRANSFER_DST_OPTIMAL | release notification first; destination COPY/TRANSFER_WRITE |

Use synchronization2 NONE for the unused side of each release/acquire barrier, and ALL_COMMANDS
where needed to conservatively order the external handoff; do not limit a release signal to a
stage that can precede its ownership transfer. Separate layout transitions from ownership barriers
as above to make the external boundary consistently GENERAL. A future cross-device or non-Vulkan
consumer requires FOREIGN_EXT with its extension and independently negotiated support, not an
unverified substitution. The relevant external family definitions are in
[VK_QUEUE_FAMILY_EXTERNAL](https://docs.vulkan.org/refpages/latest/refpages/source/VK_QUEUE_FAMILY_EXTERNAL.html)
and [VK_QUEUE_FAMILY_FOREIGN_EXT](https://docs.vulkan.org/refpages/latest/refpages/source/VK_QUEUE_FAMILY_FOREIGN_EXT.html).

**Stock GTK compatibility gate:** the consumer acquire/release rows are not implemented as such
in the audited GTK 4.22.4 path. There is no public builder option for injecting them. A producer-only
patch cannot honestly promise this complete protocol. Before zero-copy production use, either
obtain a verified driver/GTK interop contract for this exact configuration, or use a consumer
implementation with explicit external ownership support. Until then keep the application on
CPU/readback. A successful short run or clean producer-only validation is not that contract.

For an explicit consumer implementation, changes belong in GTK's
`gsk_vulkan_image_new_for_dmabuf`, its transition handling and `gsk_vulkan_frame_submit` lifecycle:
mark imported images as externally owned; collect them per frame; acquire before first use;
return them to GENERAL and release at the end of each submission that uses them; reacquire on
later submissions; retain texture references until the submission's fence signals. Include
download/transfer uses, not only fragment sampling. Defer the release callback until all uses and
the ownership-release submission have completed. Disable offload to another consumer in this first
configuration unless it follows the same ownership/lifetime contract. These are dependency changes
for later engineering review, not changes performed by this audit.

The initial producer host wait makes its output ready before construction of a GdkTexture. For a
later asynchronous implementation, export a binary Vulkan semaphore as a SYNC_FD, attach it to the
dma-buf using `DMA_BUF_IOCTL_IMPORT_SYNC_FILE` with `DMA_BUF_SYNC_WRITE` before exposing the texture,
and verify GTK's semaphore-import feature is enabled. Close the exported sync fd after the ioctl
has taken its reference. Importing a fence is separate from image ownership transfer. Reuse still
requires the texture lease release, or a future explicit consumer release fence. CPU cache-sync
ioctls are not GPU completion fences. See [Linux dma-buf synchronization](https://docs.kernel.org/driver-api/dma-buf.html#dma-buffer-ioctls).

### 5. Shutdown, error handling and rendering correctness

Stop producing before tearing down canvases. Replace picture paintables and drop application
texture references, retire all slots, and let outstanding consumer leases release naturally.
Keep device ownership with allocations even after the thread-local GPU is gone. Destroy native
images/memory only after producer completion and consumer release. Remove the global idle wait
from ordinary `Buffer::drop`; a destructor should not try to discover whether reuse was safe.
Keep a diagnostic count of live allocations, publications, generations, quarantines and callbacks.
On device loss, disable presentation and avoid further submissions rather than cycling the pool.

In `compose`, split document rendering from final canvas composition. Add explicit background
uniform fields instead of reusing padded affine rows. Clip the completed transparent document to
  its rectangle and source-over it onto the checkerboard/surround only after layer blending and
adjustments. Fix mip edge sampling against the CPU reference. Preserve preview dirty state until
uploads succeed; `src/render/gpu_plan.rs:73-74` currently clears it during plan construction, before
a failed GPU frame can acknowledge it. These correctness changes can be developed and tested
through readback without touching dma-buf presentation.

### 6. Verification order and acceptance criteria

1. **No GPU required:** unit-test the pool with fake producer fences and delayed release callbacks.
   Cover more than three publications, two canvases, resize to smaller sizes, close while published,
   duplicate/stale events, budget exhaustion, failed builder construction, device loss and shutdown
   before callback. Assert that no published/quarantined slot is selected or destroyed and that
   each fd/allocation/device owner is released exactly once. Test uniform byte offsets separately.
2. **Readback only:** resolve the observed backend/pipeline failure and log the actual adapter before
   pipeline construction. Run the existing parity test on the intended Vulkan adapter, with an
   explicit non-skip result. Extend coverage for background/preview/HiDPI, translucent blend modes
   over checkerboard, adjustments and effects outside document bounds, rectangular mip tails,
   repeated preview updates and two documents. Expose the final canvas pass through readback for
   these tests. The existing tolerance-based test does not establish literal pixel-for-pixel parity.
3. **Enumeration only:** in the real session, use a separate diagnostic with no exported images to
   capture GTK's selected device and producer/consumer UUIDs, modifiers and external-memory
   capabilities. Do not start `COMPOSITOR_GPU=present` just to learn device identities. Refuse
   presentation if the supported configuration cannot be established.
4. **Isolated interop harness:** after the consumer ownership gate is resolved, exercise one tiny
   immutable allocation and two explicit Vulkan devices in a disposable session or dedicated test
   system with Khronos validation and synchronization validation. Then test the actual GTK import
   with its release callback. Verify content via readback, release ordering and allocation counts.
   Validation across external-memory aliases has limits; a clean run is supporting evidence, not
   proof of all cross-device dependencies. Do not assume using the iGPU isolates desktop resets.
5. **Stress in that test environment:** delayed consumer completion, repeated smaller/larger
   resizes, multiple retained paintables, hide/show, tab switching, close, minimize, failed imports
   and shutdown. Instrument allocation ids and publication serials so assertions detect illegal
   reuse before submission. Require no validation errors, fd/allocation leaks or kernel GPU faults.
   If any first fault occurs, stop the experiment rather than collecting more resets.
6. **Only then test the working desktop deliberately:** save work and arrange kernel-log capture
   first. Presentation stays opt-in until sustained stress passes. Do not remove host completion
   waits for performance until the sync-file path has its own tests. Measure compositing, copy,
   waiting and GTK import separately before claiming a speedup.

No test sequence can promise that buggy kernel/driver code will never hang a GPU. This order catches
application lifetime violations without submitting dangerous work and reserves hardware interop
testing for an environment where a failure will not take the working desktop down.
