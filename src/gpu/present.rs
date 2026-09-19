//! Zero-copy presentation. Each frame is copied into a Vulkan image whose memory is exported as a dma-buf,
//! and GTK gets that dma-buf as a texture for the picture widget: no readback, no copy through the CPU.
//! Three buffers rotate so the one on screen is never the one being written.

use super::Gpu;
use anyhow::{Context as _, Result, bail};
use ash::vk;
use std::os::fd::{FromRawFd, OwnedFd};

/// DRM_FORMAT_ARGB8888: little-endian bytes B, G, R, A, as Cairo's ARGB32 and our BGRA textures.
const FOURCC_ARGB8888: u32 = 0x3432_5241;
const MOD_LINEAR: u64 = 0;

pub struct Buffer {
    /// Wrapped with a drop guard, so wgpu leaves the image to this struct.
    texture: Option<wgpu::Texture>,
    fd: OwnedFd,
    pub stride: u32,
    pub offset: u32,
    image: vk::Image,
    memory: vk::DeviceMemory,
    device: ash::Device,
}

impl Drop for Buffer {
    fn drop(&mut self) {
        // The wgpu texture goes first (it only runs its guard), then the image and memory it wrapped.
        self.texture.take();
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_image(self.image, None);
            self.device.free_memory(self.memory, None);
        }
    }
}

impl Buffer {
    pub fn raw_fd(&self) -> std::os::fd::RawFd { use std::os::fd::AsRawFd; self.fd.as_raw_fd() }
    pub fn texture(&self) -> &wgpu::Texture { self.texture.as_ref().expect("buffer texture") }
}

/// The extensions the device needs to export memory as dma-bufs.
pub fn extensions() -> [&'static std::ffi::CStr; 2] { [ash::khr::external_memory_fd::NAME, ash::ext::external_memory_dma_buf::NAME] }

impl Gpu {
    /// A `width` x `height` BGRA image in linear tiling whose memory is exported as a dma-buf, wrapped as
    /// a wgpu texture that frames can be copied into.
    pub fn export_buffer(&self, width: u32, height: u32) -> Result<Buffer> {
        if !self.export { bail!("the device was made without dma-buf export"); }
        let hal = unsafe { self.device.as_hal::<wgpu::hal::api::Vulkan>() }.context("not a Vulkan device")?;
        let instance = hal.shared_instance().raw_instance().clone();
        let dev = hal.raw_device().clone();
        let pd = hal.raw_physical_device();
        let (image, memory, fd, layout) = unsafe {
            let mut external = vk::ExternalMemoryImageCreateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
            let info = vk::ImageCreateInfo::default()
                .push_next(&mut external)
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::B8G8R8A8_UNORM)
                .extent(vk::Extent3D { width, height, depth: 1 })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::LINEAR)
                .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED);
            let image = dev.create_image(&info, None).context("creating the export image")?;
            let requirements = dev.get_image_memory_requirements(image);
            let properties = instance.get_physical_device_memory_properties(pd);
            let pick = |flags: vk::MemoryPropertyFlags| (0..properties.memory_type_count).find(|i| requirements.memory_type_bits & (1 << i) != 0 && properties.memory_types[*i as usize].property_flags.contains(flags));
            let Some(type_index) = pick(vk::MemoryPropertyFlags::DEVICE_LOCAL).or_else(|| pick(vk::MemoryPropertyFlags::empty())) else { dev.destroy_image(image, None); bail!("no memory type for the export image"); };
            let mut export = vk::ExportMemoryAllocateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
            let alloc = vk::MemoryAllocateInfo::default().allocation_size(requirements.size).memory_type_index(type_index).push_next(&mut export).push_next(&mut dedicated);
            let memory = match dev.allocate_memory(&alloc, None) { Ok(m) => m, Err(e) => { dev.destroy_image(image, None); bail!("allocating export memory: {e}"); } };
            if let Err(e) = dev.bind_image_memory(image, memory, 0) { dev.destroy_image(image, None); dev.free_memory(memory, None); bail!("binding export memory: {e}"); }
            let loader = ash::khr::external_memory_fd::Device::new(&instance, &dev);
            let fd = match loader.get_memory_fd(&vk::MemoryGetFdInfoKHR::default().memory(memory).handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)) { Ok(fd) => fd, Err(e) => { dev.destroy_image(image, None); dev.free_memory(memory, None); bail!("exporting the dma-buf: {e}"); } };
            let layout = dev.get_image_subresource_layout(image, vk::ImageSubresource::default().aspect_mask(vk::ImageAspectFlags::COLOR).mip_level(0).array_layer(0));
            (image, memory, OwnedFd::from_raw_fd(fd), layout)
        };
        let hal_texture = unsafe {
            hal.texture_from_raw(image, &wgpu::hal::TextureDescriptor {
                label: Some("present"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage: wgpu::TextureUses::COPY_DST,
                memory_flags: wgpu::hal::MemoryFlags::empty(),
                view_formats: vec![],
            }, Some(Box::new(|| {})), wgpu::hal::vulkan::TextureMemory::External)
        };
        drop(hal);
        let texture = unsafe {
            self.device.create_texture_from_hal::<wgpu::hal::api::Vulkan>(hal_texture, &wgpu::TextureDescriptor {
                label: Some("present"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage: wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            }, wgpu::TextureUses::UNINITIALIZED)
        };
        Ok(Buffer { texture: Some(texture), fd, stride: layout.row_pitch as u32, offset: layout.offset as u32, image, memory, device: dev })
    }
}

/// GTK's texture over an exported buffer. A fresh texture object per frame, so GTK's renderer never
/// shows a cached import of an older frame; the buffer's memory is what it samples.
pub fn gdk_texture(display: &gtk::gdk::Display, buffer: &Buffer, width: u32, height: u32) -> Result<gtk::gdk::Texture> {
    let builder = gtk::gdk::DmabufTextureBuilder::new()
        .set_display(display)
        .set_width(width)
        .set_height(height)
        .set_fourcc(FOURCC_ARGB8888)
        .set_modifier(MOD_LINEAR)
        .set_n_planes(1)
        .set_stride(0, buffer.stride)
        .set_offset(0, buffer.offset)
        .set_premultiplied(true);
    let builder = unsafe { builder.set_fd(0, buffer.raw_fd()) };
    unsafe { builder.build() }.map_err(|e| anyhow::anyhow!("GTK could not import the dma-buf: {e}"))
}
