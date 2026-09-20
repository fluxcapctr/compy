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
