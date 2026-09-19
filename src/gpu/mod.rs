//! GPU compositing with wgpu. Layers and masks live on the GPU as textures (with mipmaps for the reduced
//! views); a frame is a chain of full-screen passes over a viewport-sized target, one or a few per layer,
//! that follow the CPU renderer's order exactly: place a layer through its own mask, put clipping stacks
//! together, apply folder masks, blend with the layer's mode and opacity, run adjustment lookups. The
//! result is read back as BGRA rows for the canvas. Anything the plan cannot express falls back to the CPU.

use anyhow::{Context as _, Result, bail};
use cairo::ImageSurface;
use std::collections::HashMap;
use uuid::Uuid;

/// How a layer or mask surface is addressed on the GPU: which store it came from, and the surface's own
/// identity (its pointer), so a replaced surface uploads again and an unchanged one does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SurfaceKey { pub id: Uuid, pub slot: u8, pub ptr: usize }

/// An affine map from device pixel centers to source texels (row-major 2x3).
#[derive(Clone, Copy, Debug)]
pub struct Affine(pub [f64; 6]);

impl Affine {
    pub fn from_cairo(m: &cairo::Matrix) -> Affine { Affine([m.xx(), m.xy(), m.x0(), m.yx(), m.yy(), m.y0()]) }
}

/// A layer's or mask's pixels, placed.
#[derive(Clone, Debug)]
pub struct Placed {
    pub key: SurfaceKey,
    pub surface: ImageSurface,
    pub to_texel: Affine,
    pub nearest: bool,
    /// The rows of the surface changed since the last frame (x0, y0, x1, y1), for previews mutated in place.
    pub dirty: Option<(i32, i32, i32, i32)>,
}

#[derive(Clone, Debug)]
pub struct MaskDraw { pub placed: Placed, pub outside: f32 }

/// The effects of a styled layer: document-space surfaces placed under and over it.
#[derive(Clone, Debug)]
pub struct EffectsDraw { pub below: Option<Placed>, pub inside: Option<Placed>, pub above: Option<Placed> }

#[derive(Clone, Debug)]
pub struct LayerDraw {
    pub image: Placed,
    pub mask: Option<MaskDraw>,
    /// Ancestor folder masks, nearest folder first (outside their rectangle they cover nothing).
    pub folders: Vec<MaskDraw>,
    pub opacity: f32,
    pub blend: u32,
    pub effects: Option<EffectsDraw>,
}

#[derive(Clone, Debug)]
pub struct AdjustDraw {
    /// 256 entries: per-channel tables in r, g, b (or a gradient map indexed by luminance).
    pub lut: Vec<u8>,
    pub by_luminance: bool,
    pub mask: Option<MaskDraw>,
    pub folders: Vec<MaskDraw>,
    pub opacity: f32,
    pub blend: u32,
}

#[derive(Clone, Debug)]
pub enum StackChild { Layer(LayerDraw), Adjust(AdjustDraw) }

#[derive(Clone, Debug)]
pub enum Item {
    Layer(LayerDraw),
    /// A clipping stack: the base with the layers clipped to it, then folder masks and the base's blend.
    Stack { base: LayerDraw, children: Vec<StackChild>, folders: Vec<MaskDraw>, blend: u32 },
    Adjust(AdjustDraw),
}

/// A frame to composite: the items in drawing order over a viewport of `width` x `height` device pixels,
/// `to_device` mapping document points to device pixels.
#[derive(Clone, Debug)]
pub struct Plan { pub items: Vec<Item>, pub width: u32, pub height: u32 }

struct Cached { texture: wgpu::Texture, view: wgpu::TextureView, width: u32, height: u32, mips: u32, frame: u64 }

struct Target { texture: wgpu::Texture, view: wgpu::TextureView }

pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    r8_pipeline_layout: wgpu::BindGroupLayout,
    lin: wgpu::Sampler,
    near: wgpu::Sampler,
    textures: HashMap<SurfaceKey, Cached>,
    /// Viewport-sized targets, reused between passes and frames.
    pool: Vec<Target>,
    pool_size: (u32, u32),
    blank: Cached,
    blank_mask: Cached,
    frame: u64,
    max_dimension: u32,
    pub name: String,
}

const OP_COMPOSITE: u32 = 0;
const OP_ATOP: u32 = 1;
const OP_MODULATE: u32 = 2;
const OP_UNPREMULTIPLY: u32 = 3;
const OP_RESTORE: u32 = 4;
const OP_ADJUST: u32 = 5;
const OP_DOWNSAMPLE: u32 = 6;
const OP_COPY: u32 = 7;

const FLAG_MASK: u32 = 1;
const FLAG_NEAREST: u32 = 2;
const FLAG_LAYER: u32 = 4;
const FLAG_MASK_DEVICE: u32 = 8;
const FLAG_LAYER_DEVICE: u32 = 16;

#[derive(Clone, Copy)]
struct Uniforms {
    to_layer: Affine,
    to_mask: Affine,
    layer_size: (f32, f32),
    mask_size: (f32, f32),
    op: u32,
    blend: u32,
    flags: u32,
    opacity: f32,
    target_size: (f32, f32),
    mask_outside: f32,
    lut_luma: f32,
}

impl Uniforms {
    fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(112);
        let f = |v: f64| v as f32;
        for row in [[self.to_layer.0[0], self.to_layer.0[1], self.to_layer.0[2], 0.0], [self.to_layer.0[3], self.to_layer.0[4], self.to_layer.0[5], 0.0], [self.to_mask.0[0], self.to_mask.0[1], self.to_mask.0[2], 0.0], [self.to_mask.0[3], self.to_mask.0[4], self.to_mask.0[5], 0.0]] {
            for v in row { out.extend_from_slice(&f(v).to_le_bytes()); }
        }
        for v in [self.layer_size.0, self.layer_size.1, self.mask_size.0, self.mask_size.1] { out.extend_from_slice(&v.to_le_bytes()); }
        for v in [self.op, self.blend, self.flags] { out.extend_from_slice(&v.to_le_bytes()); }
        out.extend_from_slice(&self.opacity.to_le_bytes());
        for v in [self.target_size.0, self.target_size.1, self.mask_outside, self.lut_luma] { out.extend_from_slice(&v.to_le_bytes()); }
        out
    }
}

fn identity() -> Affine { Affine([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]) }

impl Gpu {
    /// A device on the best adapter, or None when there is no usable GPU (the CPU path stays).
    pub fn new() -> Option<Gpu> {
        // Opt in while the frame still comes back through a readback: set COMPOSITOR_GPU=1.
        if !std::env::var("COMPOSITOR_GPU").is_ok_and(|v| v == "1") { return None; }
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        descriptor.backends = wgpu::Backends::VULKAN | wgpu::Backends::GL;
        let instance = wgpu::Instance::new(descriptor);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() })).ok()?;
        let info = adapter.get_info();
        let limits = adapter.limits();
        let max_dimension = limits.max_texture_dimension_2d.min(16384);
        let mut required = wgpu::Limits::default();
        required.max_texture_dimension_2d = max_dimension;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { label: Some("compy"), required_limits: required, ..Default::default() })).ok()?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("composite"), source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()) });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("composite"),
            entries: &[
                wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 3, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 4, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
                wgpu::BindGroupLayoutEntry { binding: 5, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering), count: None },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("composite"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("composite"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), buffers: &[], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs"), targets: &[Some(wgpu::ColorTargetState { format: wgpu::TextureFormat::Bgra8Unorm, blend: None, write_mask: wgpu::ColorWrites::ALL })], compilation_options: Default::default() }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let lin = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("linear"), address_mode_u: wgpu::AddressMode::ClampToEdge, address_mode_v: wgpu::AddressMode::ClampToEdge, address_mode_w: wgpu::AddressMode::ClampToEdge, mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, mipmap_filter: wgpu::MipmapFilterMode::Linear, ..Default::default() });
        let near = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("nearest"), address_mode_u: wgpu::AddressMode::ClampToEdge, address_mode_v: wgpu::AddressMode::ClampToEdge, address_mode_w: wgpu::AddressMode::ClampToEdge, mag_filter: wgpu::FilterMode::Nearest, min_filter: wgpu::FilterMode::Nearest, mipmap_filter: wgpu::MipmapFilterMode::Nearest, ..Default::default() });
        let blank = Self::make_texture(&device, 1, 1, wgpu::TextureFormat::Bgra8Unorm, 1);
        queue.write_texture(blank.texture.as_image_copy(), &[0, 0, 0, 0], wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: None }, wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 });
        let blank_mask = Self::make_texture(&device, 1, 1, wgpu::TextureFormat::R8Unorm, 1);
        queue.write_texture(blank_mask.texture.as_image_copy(), &[255], wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(1), rows_per_image: None }, wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 });
        Some(Gpu { device, queue, pipeline, r8_pipeline_layout: layout, lin, near, textures: HashMap::new(), pool: Vec::new(), pool_size: (0, 0), blank, blank_mask, frame: 0, max_dimension, name: format!("{} ({:?})", info.name, info.backend) })
    }

    pub fn max_dimension(&self) -> u32 { self.max_dimension }

    fn make_texture(device: &wgpu::Device, width: u32, height: u32, format: wgpu::TextureFormat, mips: u32) -> Cached {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: mips,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Cached { texture, view, width, height, mips, frame: 0 }
    }

    /// The texture for a surface, uploaded when new or changed, with its reduced levels built.
    fn upload(&mut self, placed: &Placed, mask: bool) -> Result<()> {
        let (w, h) = (placed.surface.width() as u32, placed.surface.height() as u32);
        if w == 0 || h == 0 || w > self.max_dimension || h > self.max_dimension { bail!("surface {w}x{h} is outside the GPU's texture limits"); }
        let format = if mask { wgpu::TextureFormat::R8Unorm } else { wgpu::TextureFormat::Bgra8Unorm };
        let mips = if mask { 1 } else { (32 - w.max(h).leading_zeros()).max(1) };
        let fresh = !self.textures.contains_key(&placed.key);
        if fresh {
            let cached = Self::make_texture(&self.device, w, h, format, mips);
            self.textures.insert(placed.key, cached);
        }
        let dirty = if fresh { Some((0, 0, w as i32, h as i32)) } else { placed.dirty };
        if let Some((x0, y0, x1, y1)) = dirty {
            let (x0, y0) = (x0.clamp(0, w as i32) as u32, y0.clamp(0, h as i32) as u32);
            let (x1, y1) = (x1.clamp(0, w as i32) as u32, y1.clamp(0, h as i32) as u32);
            if x1 > x0 && y1 > y0 {
                let bpp = if mask { 1 } else { 4 };
                let cached = &self.textures[&placed.key];
                crate::raster::with_bytes(&placed.surface, |data, stride| {
                    let rows: Vec<u8> = (y0..y1).flat_map(|y| data[y as usize * stride + x0 as usize * bpp..y as usize * stride + x1 as usize * bpp].iter().copied()).collect();
                    self.queue.write_texture(
                        wgpu::TexelCopyTextureInfo { texture: &cached.texture, mip_level: 0, origin: wgpu::Origin3d { x: x0, y: y0, z: 0 }, aspect: wgpu::TextureAspect::All },
                        &rows,
                        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some((x1 - x0) * bpp as u32), rows_per_image: None },
                        wgpu::Extent3d { width: x1 - x0, height: y1 - y0, depth_or_array_layers: 1 },
                    );
                })?;
                if mips > 1 { self.build_mips(placed.key)?; }
            }
        }
        if let Some(c) = self.textures.get_mut(&placed.key) { c.frame = self.frame; }
        Ok(())
    }

    /// Each level is the box average of the one above (the CPU halves with a wider filter; views far
    /// below 1:1 differ by a little smoothing).
    fn build_mips(&mut self, key: SurfaceKey) -> Result<()> {
        let cached = &self.textures[&key];
        let (mut w, mut h) = (cached.width, cached.height);
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("mips") });
        for level in 1..cached.mips {
            let src = cached.texture.create_view(&wgpu::TextureViewDescriptor { base_mip_level: level - 1, mip_level_count: Some(1), ..Default::default() });
            let dst = cached.texture.create_view(&wgpu::TextureViewDescriptor { base_mip_level: level, mip_level_count: Some(1), ..Default::default() });
            w = (w / 2).max(1);
            h = (h / 2).max(1);
            let u = Uniforms { to_layer: identity(), to_mask: identity(), layer_size: (w as f32, h as f32), mask_size: (1.0, 1.0), op: OP_DOWNSAMPLE, blend: 0, flags: 0, opacity: 1.0, target_size: (w as f32, h as f32), mask_outside: 0.0, lut_luma: 0.0 };
            let bind = self.bind_group(&u, &self.blank.view, &src, &self.blank_mask.view);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor { label: Some("mip"), color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &dst, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store }, depth_slice: None })], depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        Ok(())
    }

    fn bind_group(&self, u: &Uniforms, dest: &wgpu::TextureView, layer: &wgpu::TextureView, mask: &wgpu::TextureView) -> wgpu::BindGroup {
        use wgpu::util::DeviceExt;
        let buffer = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("uniforms"), contents: &u.bytes(), usage: wgpu::BufferUsages::UNIFORM });
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pass"),
            layout: &self.r8_pipeline_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(dest) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(layer) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(mask) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&self.lin) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Sampler(&self.near) },
            ],
        })
    }

    fn take_target(&mut self, width: u32, height: u32) -> Target {
        if self.pool_size != (width, height) { self.pool.clear(); self.pool_size = (width, height); }
        if let Some(t) = self.pool.pop() { return t; }
        let c = Self::make_texture(&self.device, width, height, wgpu::TextureFormat::Bgra8Unorm, 1);
        Target { texture: c.texture, view: c.view }
    }

    fn give_back(&mut self, t: Target) { if self.pool.len() < 12 { self.pool.push(t); } }

    /// Composites `plan` and returns the frame as tightly packed BGRA premultiplied rows.
    pub fn render(&mut self, plan: &Plan) -> Result<Vec<u8>> {
        self.frame += 1;
        let (w, h) = (plan.width, plan.height);
        if w == 0 || h == 0 { return Ok(Vec::new()); }
        if w > self.max_dimension || h > self.max_dimension { bail!("viewport {w}x{h} is larger than the GPU allows"); }
        // Upload everything first, so the passes only bind.
        for item in &plan.items {
            match item {
                Item::Layer(l) => self.upload_layer(l)?,
                Item::Stack { base, children, folders, .. } => {
                    self.upload_layer(base)?;
                    for c in children { match c { StackChild::Layer(l) => self.upload_layer(l)?, StackChild::Adjust(a) => self.upload_adjust(a)? } }
                    for f in folders { self.upload(&f.placed, true)?; }
                }
                Item::Adjust(a) => self.upload_adjust(a)?,
            }
        }
        let mut frame = Frame { gpu: self, width: w, height: h, encoder: None };
        let mut canvas = frame.gpu.take_target(w, h);
        frame.clear(&canvas);
        for item in &plan.items {
            match item {
                Item::Layer(l) => { let g = frame.layer_group(l)?; canvas = frame.composite_group(canvas, &g, l.opacity, l.blend); frame.gpu.give_back(g); }
                Item::Adjust(a) => { canvas = frame.adjust(canvas, a)?; }
                Item::Stack { base, children, folders, blend } => {
                    // The base at its own opacity, its alpha kept aside; children onto its straight colors.
                    let base_group = frame.layer_group(base)?;
                    let mut b = frame.gpu.take_target(w, h);
                    frame.clear(&b);
                    b = frame.composite_group(b, &base_group, base.opacity, 0);
                    let mut straight = frame.gpu.take_target(w, h);
                    let (blank, blank_mask) = (frame.gpu.blank.view.clone(), frame.gpu.blank_mask.view.clone());
                    let unpremultiply = Uniforms { op: OP_UNPREMULTIPLY, ..frame.base_uniforms() };
                    frame.pass(&b.view, &straight.view, &unpremultiply, &blank, &blank_mask);
                    for child in children {
                        match child {
                            StackChild::Layer(l) => { let g = frame.layer_group(l)?; straight = frame.composite_group(straight, &g, l.opacity, l.blend); frame.gpu.give_back(g); }
                            StackChild::Adjust(a) => { straight = frame.adjust(straight, a)?; }
                        }
                    }
                    let mut restored = frame.gpu.take_target(w, h);
                    let restore = Uniforms { op: OP_RESTORE, ..frame.base_uniforms() };
                    frame.pass(&straight.view, &restored.view, &restore, &b.view, &blank_mask);
                    frame.gpu.give_back(b);
                    frame.gpu.give_back(straight);
                    frame.gpu.give_back(base_group);
                    restored = frame.through_masks(restored, folders);
                    canvas = frame.composite_group(canvas, &restored, 1.0, *blend);
                    frame.gpu.give_back(restored);
                }
            }
        }
        frame.flush();
        let bytes = frame.gpu.read_back(&canvas, w, h)?;
        let gpu = frame.gpu;
        gpu.give_back(canvas);
        // Forget textures no frame has used for a while.
        let current = gpu.frame;
        gpu.textures.retain(|_, c| current - c.frame < 120);
        Ok(bytes)
    }

    fn upload_layer(&mut self, l: &LayerDraw) -> Result<()> {
        self.upload(&l.image, false)?;
        if let Some(m) = &l.mask { self.upload(&m.placed, true)?; }
        for f in &l.folders { self.upload(&f.placed, true)?; }
        if let Some(e) = &l.effects { for p in [&e.below, &e.inside, &e.above].into_iter().flatten() { self.upload(p, false)?; } }
        Ok(())
    }

    fn upload_adjust(&mut self, a: &AdjustDraw) -> Result<()> {
        if let Some(m) = &a.mask { self.upload(&m.placed, true)?; }
        for f in &a.folders { self.upload(&f.placed, true)?; }
        Ok(())
    }

    fn lut_texture(&mut self, a: &AdjustDraw) -> Cached {
        let c = Self::make_texture(&self.device, 256, 1, wgpu::TextureFormat::Bgra8Unorm, 1);
        // The table arrives as RGBA per entry; the texture is BGRA.
        let mut bytes = Vec::with_capacity(1024);
        for e in a.lut.chunks_exact(4) { bytes.extend_from_slice(&[e[2], e[1], e[0], e[3]]); }
        self.queue.write_texture(c.texture.as_image_copy(), &bytes, wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(1024), rows_per_image: None }, wgpu::Extent3d { width: 256, height: 1, depth_or_array_layers: 1 });
        c
    }

    fn read_back(&mut self, target: &Target, w: u32, h: u32) -> Result<Vec<u8>> {
        let padded = (w * 4).div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("readback"), size: (padded * h) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
        encoder.copy_texture_to_buffer(target.texture.as_image_copy(), wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: None } }, wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 });
        self.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        self.device.poll(wgpu::PollType::wait_indefinitely()).context("waiting for the GPU")?;
        rx.recv().context("readback")?.context("mapping the readback buffer")?;
        let data = slice.get_mapped_range().context("reading the readback buffer")?;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize { out.extend_from_slice(&data[y * padded as usize..y * padded as usize + (w * 4) as usize]); }
        drop(data);
        buffer.unmap();
        Ok(out)
    }
}

/// One frame's passes, batched into a command encoder.
struct Frame<'a> { gpu: &'a mut Gpu, width: u32, height: u32, encoder: Option<wgpu::CommandEncoder> }

impl<'a> Frame<'a> {
    fn base_uniforms(&self) -> Uniforms {
        Uniforms { to_layer: identity(), to_mask: identity(), layer_size: (1.0, 1.0), mask_size: (1.0, 1.0), op: OP_COMPOSITE, blend: 0, flags: 0, opacity: 1.0, target_size: (self.width as f32, self.height as f32), mask_outside: 0.0, lut_luma: 0.0 }
    }

    fn encoder(&mut self) -> &mut wgpu::CommandEncoder {
        if self.encoder.is_none() { self.encoder = Some(self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") })); }
        self.encoder.as_mut().unwrap()
    }

    fn flush(&mut self) { if let Some(e) = self.encoder.take() { self.gpu.queue.submit([e.finish()]); } }

    fn clear(&mut self, t: &Target) {
        let encoder = self.encoder();
        let _ = encoder.begin_render_pass(&wgpu::RenderPassDescriptor { label: Some("clear"), color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &t.view, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store }, depth_slice: None })], depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None });
    }

    /// One full-screen pass reading `dest` and writing `out`.
    fn pass(&mut self, dest: &wgpu::TextureView, out: &wgpu::TextureView, u: &Uniforms, layer: &wgpu::TextureView, mask: &wgpu::TextureView) {
        let bind = self.gpu.bind_group(u, dest, layer, mask);
        let pipeline = self.gpu.pipeline.clone();
        let encoder = self.encoder();
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor { label: Some("pass"), color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: out, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store }, depth_slice: None })], depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// `dest` with a source placed over it; the previous target goes back to the pool.
    fn step(&mut self, dest: Target, u: &Uniforms, layer: &wgpu::TextureView, mask: &wgpu::TextureView) -> Target {
        let out = self.gpu.take_target(self.width, self.height);
        self.pass(&dest.view, &out.view, u, layer, mask);
        self.gpu.give_back(dest);
        out
    }

    fn placed_uniforms(&self, p: &Placed) -> Uniforms {
        let mut u = self.base_uniforms();
        u.to_layer = p.to_texel;
        u.layer_size = (p.surface.width() as f32, p.surface.height() as f32);
        u.flags |= FLAG_LAYER;
        if p.nearest { u.flags |= FLAG_NEAREST; }
        u
    }

    fn with_mask(&self, mut u: Uniforms, m: &MaskDraw) -> Uniforms {
        u.to_mask = m.placed.to_texel;
        u.mask_size = (m.placed.surface.width() as f32, m.placed.surface.height() as f32);
        u.mask_outside = m.outside;
        u.flags |= FLAG_MASK;
        u
    }

    fn view_of(&self, key: SurfaceKey) -> wgpu::TextureView { self.gpu.textures[&key].view.clone() }

    /// The layer through its own mask and effects, alone on a cleared target (opacity and blend apply
    /// when it lands), as `draw_own` paints it into a group.
    fn layer_group(&mut self, l: &LayerDraw) -> Result<Target> {
        let mut g = self.gpu.take_target(self.width, self.height);
        self.clear(&g);
        let blank_mask = self.gpu.blank_mask.view.clone();
        if let Some(e) = &l.effects {
            if let Some(below) = &e.below { let u = self.placed_uniforms(below); let v = self.view_of(below.key); g = self.step(g, &u, &v, &blank_mask); }
        }
        // The layer itself, with its inside effects composited Atop within a group of its own.
        let mut u = self.placed_uniforms(&l.image);
        let mask_view = match &l.mask { Some(m) => { u = self.with_mask(u, m); self.view_of(m.placed.key) } None => blank_mask.clone() };
        let image_view = self.view_of(l.image.key);
        match l.effects.as_ref().and_then(|e| e.inside.as_ref()) {
            Some(inside) => {
                let mut n = self.gpu.take_target(self.width, self.height);
                self.clear(&n);
                n = self.step(n, &u, &image_view, &mask_view);
                let iu = Uniforms { op: OP_ATOP, ..self.placed_uniforms(inside) };
                let iv = self.view_of(inside.key);
                n = self.step(n, &iu, &iv, &blank_mask);
                let cu = Uniforms { flags: FLAG_LAYER | FLAG_LAYER_DEVICE, ..self.base_uniforms() };
                g = self.step(g, &cu, &n.view, &blank_mask);
                self.gpu.give_back(n);
            }
            None => { g = self.step(g, &u, &image_view, &mask_view); }
        }
        if let Some(e) = &l.effects {
            if let Some(above) = &e.above { let u = self.placed_uniforms(above); let v = self.view_of(above.key); g = self.step(g, &u, &v, &blank_mask); }
        }
        g = self.through_masks(g, &l.folders);
        Ok(g)
    }

    /// Multiplies a group's coverage by each folder mask (nothing beyond a folder's rectangle).
    fn through_masks(&mut self, mut g: Target, folders: &[MaskDraw]) -> Target {
        for f in folders {
            let u = Uniforms { op: OP_MODULATE, ..self.with_mask(self.base_uniforms(), f) };
            let v = self.view_of(f.placed.key);
            let blank = self.gpu.blank.view.clone();
            g = self.step(g, &u, &blank, &v);
        }
        g
    }

    /// A group (device-sized) blended onto `dest` at `opacity` with `blend`.
    fn composite_group(&mut self, dest: Target, g: &Target, opacity: f32, blend: u32) -> Target {
        let u = Uniforms { flags: FLAG_LAYER | FLAG_LAYER_DEVICE, opacity, blend, ..self.base_uniforms() };
        let blank_mask = self.gpu.blank_mask.view.clone();
        self.step(dest, &u, &g.view, &blank_mask)
    }

    /// An adjustment layer over everything drawn so far, through its own and its folders' masks.
    fn adjust(&mut self, dest: Target, a: &AdjustDraw) -> Result<Target> {
        // Coverage: ones, modulated by every mask, kept in a device-sized target's red channel.
        let mut cov = self.gpu.take_target(self.width, self.height);
        {
            let encoder = self.encoder();
            let _ = encoder.begin_render_pass(&wgpu::RenderPassDescriptor { label: Some("ones"), color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &cov.view, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::WHITE), store: wgpu::StoreOp::Store }, depth_slice: None })], depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None });
        }
        let mut masks: Vec<MaskDraw> = a.folders.clone();
        if let Some(m) = &a.mask { masks.push(m.clone()); }
        cov = self.through_masks(cov, &masks);
        let lut = self.gpu.lut_texture(a);
        let u = Uniforms { op: OP_ADJUST, blend: a.blend, opacity: a.opacity, flags: FLAG_MASK | FLAG_MASK_DEVICE, lut_luma: if a.by_luminance { 1.0 } else { 0.0 }, ..self.base_uniforms() };
        let out = self.step(dest, &u, &lut.view, &cov.view);
        self.gpu.give_back(cov);
        Ok(out)
    }
}
