use std::{
    fmt,
    os::fd::{BorrowedFd, FromRawFd, IntoRawFd, OwnedFd},
};

use ash::{ext, khr, vk};
use wgpu::hal::{self, api::Vulkan};

use crate::ScaleError;

const DMA_BUF: vk::ExternalMemoryHandleTypeFlags = vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT;
const REQUIRED_EXTENSIONS: [&std::ffi::CStr; 3] = [
    khr::external_memory_fd::NAME,
    ext::external_memory_dma_buf::NAME,
    ext::image_drm_format_modifier::NAME,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ModifierProperties {
    pub modifier: u64,
    pub plane_count: usize,
    pub copyable: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ImageRequest<'a> {
    pub format: vk::Format,
    pub view_formats: &'a [vk::Format],
    pub modifier: u64,
    pub copyable: bool,
}

impl ImageRequest<'_> {
    fn usage(&self) -> vk::ImageUsageFlags {
        if self.copyable {
            vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC
        } else {
            vk::ImageUsageFlags::SAMPLED
        }
    }

    fn flags(&self) -> vk::ImageCreateFlags {
        if self.view_formats.is_empty() {
            vk::ImageCreateFlags::empty()
        } else {
            vk::ImageCreateFlags::MUTABLE_FORMAT
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MemoryPlane {
    pub offset: u64,
    pub stride: u64,
}

pub(crate) struct VulkanImporter {
    instance: ash::Instance,
    device: ash::Device,
    physical: vk::PhysicalDevice,
    memory_fd: khr::external_memory_fd::Device,
}

impl fmt::Debug for VulkanImporter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VulkanImporter")
            .field("device", &self.device.handle())
            .field("physical", &self.physical)
            .finish_non_exhaustive()
    }
}

impl VulkanImporter {
    pub fn new(device: &wgpu::Device) -> Result<Self, ScaleError> {
        if !device
            .features()
            .contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF)
        {
            return Err(ScaleError::DmaBufImportUnavailable);
        }

        let hal = unsafe { device.as_hal::<Vulkan>() }.ok_or(ScaleError::DmaBufNeedsVulkan)?;
        let enabled = hal.enabled_device_extensions();
        if !REQUIRED_EXTENSIONS
            .iter()
            .all(|extension| enabled.contains(extension))
        {
            return Err(ScaleError::DmaBufImportUnavailable);
        }

        let instance = hal.shared_instance().raw_instance().clone();
        let raw = hal.raw_device().clone();
        let memory_fd = khr::external_memory_fd::Device::new(&instance, &raw);

        Ok(Self {
            physical: hal.raw_physical_device(),
            instance,
            device: raw,
            memory_fd,
        })
    }

    pub fn modifiers(&self, format: vk::Format) -> Vec<ModifierProperties> {
        let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
        self.format_properties(format, &mut list);

        let mut modifiers = vec![
            vk::DrmFormatModifierPropertiesEXT::default();
            list.drm_format_modifier_count as usize
        ];
        let mut list = vk::DrmFormatModifierPropertiesListEXT::default()
            .drm_format_modifier_properties(&mut modifiers);
        self.format_properties(format, &mut list);
        let written = list.drm_format_modifier_count as usize;

        modifiers
            .iter()
            .take(written)
            .filter(|properties| {
                properties
                    .drm_format_modifier_tiling_features
                    .contains(vk::FormatFeatureFlags::SAMPLED_IMAGE)
            })
            .map(|properties| ModifierProperties {
                modifier: properties.drm_format_modifier,
                plane_count: properties.drm_format_modifier_plane_count as usize,
                copyable: properties
                    .drm_format_modifier_tiling_features
                    .contains(vk::FormatFeatureFlags::TRANSFER_SRC),
            })
            .collect()
    }

    fn format_properties(
        &self,
        format: vk::Format,
        list: &mut vk::DrmFormatModifierPropertiesListEXT<'_>,
    ) {
        let mut properties = vk::FormatProperties2::default().push_next(list);
        unsafe {
            self.instance.get_physical_device_format_properties2(
                self.physical,
                format,
                &mut properties,
            );
        }
    }

    pub fn max_extent(&self, request: &ImageRequest<'_>) -> Option<(u32, u32)> {
        let mut modifier_info = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default()
            .drm_format_modifier(request.modifier)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let mut external_info =
            vk::PhysicalDeviceExternalImageFormatInfo::default().handle_type(DMA_BUF);
        let mut format_list =
            vk::ImageFormatListCreateInfo::default().view_formats(request.view_formats);
        let mut info = vk::PhysicalDeviceImageFormatInfo2::default()
            .format(request.format)
            .ty(vk::ImageType::TYPE_2D)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(request.usage())
            .flags(request.flags())
            .push_next(&mut modifier_info)
            .push_next(&mut external_info);
        if !request.view_formats.is_empty() {
            info = info.push_next(&mut format_list);
        }

        let mut external = vk::ExternalImageFormatProperties::default();
        let mut properties = vk::ImageFormatProperties2::default().push_next(&mut external);
        let supported = unsafe {
            self.instance.get_physical_device_image_format_properties2(
                self.physical,
                &info,
                &mut properties,
            )
        };
        let extent = properties.image_format_properties.max_extent;
        let importable = external
            .external_memory_properties
            .external_memory_features
            .contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE);

        (supported.is_ok() && importable).then_some((extent.width, extent.height))
    }

    pub fn import(
        &self,
        device: &wgpu::Device,
        request: &ImageRequest<'_>,
        fd: BorrowedFd<'_>,
        planes: &[MemoryPlane],
        desc: &wgpu::TextureDescriptor<'_>,
    ) -> Result<wgpu::Texture, ScaleError> {
        let layouts: Vec<vk::SubresourceLayout> = planes
            .iter()
            .map(|plane| vk::SubresourceLayout {
                offset: plane.offset,
                size: 0,
                row_pitch: plane.stride,
                array_pitch: 0,
                depth_pitch: 0,
            })
            .collect();
        let mut external_info = vk::ExternalMemoryImageCreateInfo::default().handle_types(DMA_BUF);
        let mut modifier_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
            .drm_format_modifier(request.modifier)
            .plane_layouts(&layouts);
        let mut format_list =
            vk::ImageFormatListCreateInfo::default().view_formats(request.view_formats);
        let mut info = vk::ImageCreateInfo::default()
            .flags(request.flags())
            .image_type(vk::ImageType::TYPE_2D)
            .format(request.format)
            .extent(vk::Extent3D {
                width: desc.size.width,
                height: desc.size.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(request.usage())
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut external_info)
            .push_next(&mut modifier_info);
        if !request.view_formats.is_empty() {
            info = info.push_next(&mut format_list);
        }

        let image = unsafe { self.device.create_image(&info, None) }
            .map_err(ScaleError::DmaBufImageCreate)?;
        let mut pending = PendingImage {
            device: &self.device,
            image,
            memory: vk::DeviceMemory::null(),
        };
        pending.memory = self.import_memory(image, fd)?;
        unsafe { self.device.bind_image_memory(image, pending.memory, 0) }
            .map_err(ScaleError::DmaBufMemoryBind)?;

        let (image, memory) = pending.release();
        let hal_desc = hal::TextureDescriptor {
            label: desc.label,
            size: desc.size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: desc.format,
            usage: hal_usage(desc.usage),
            memory_flags: hal::MemoryFlags::empty(),
            view_formats: desc.view_formats.to_vec(),
        };
        let hal_texture = {
            let hal_device = unsafe { device.as_hal::<Vulkan>() };
            let Some(hal_device) = hal_device else {
                unsafe {
                    self.device.destroy_image(image, None);
                    self.device.free_memory(memory, None);
                }
                return Err(ScaleError::DmaBufNeedsVulkan);
            };
            unsafe {
                hal_device.texture_from_raw(
                    image,
                    &hal_desc,
                    None,
                    hal::vulkan::TextureMemory::Dedicated(memory),
                )
            }
        };

        Ok(unsafe {
            device.create_texture_from_hal::<Vulkan>(
                hal_texture,
                desc,
                wgpu::TextureUses::UNINITIALIZED,
            )
        })
    }

    fn import_memory(
        &self,
        image: vk::Image,
        fd: BorrowedFd<'_>,
    ) -> Result<vk::DeviceMemory, ScaleError> {
        let requirements = unsafe { self.device.get_image_memory_requirements(image) };
        let owned = rustix::io::fcntl_dupfd_cloexec(fd, 0).map_err(ScaleError::DmaBufFdDup)?;
        let raw = owned.into_raw_fd();
        let close = || drop(unsafe { OwnedFd::from_raw_fd(raw) });

        let mut fd_properties = vk::MemoryFdPropertiesKHR::default();
        if let Err(result) = unsafe {
            self.memory_fd
                .get_memory_fd_properties(DMA_BUF, raw, &mut fd_properties)
        } {
            close();
            return Err(ScaleError::DmaBufMemoryProperties(result));
        }

        let types = requirements.memory_type_bits & fd_properties.memory_type_bits;
        if types == 0 {
            close();
            return Err(ScaleError::DmaBufNoMemoryType);
        }

        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
        let mut import = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(DMA_BUF)
            .fd(raw);
        let info = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(types.trailing_zeros())
            .push_next(&mut import)
            .push_next(&mut dedicated);

        unsafe { self.device.allocate_memory(&info, None) }.map_err(|result| {
            close();
            ScaleError::DmaBufMemoryImport(result)
        })
    }
}

fn hal_usage(usage: wgpu::TextureUsages) -> wgpu::TextureUses {
    let mut uses = wgpu::TextureUses::RESOURCE;
    if usage.contains(wgpu::TextureUsages::COPY_SRC) {
        uses |= wgpu::TextureUses::COPY_SRC;
    }
    uses
}

struct PendingImage<'a> {
    device: &'a ash::Device,
    image: vk::Image,
    memory: vk::DeviceMemory,
}

impl PendingImage<'_> {
    fn release(mut self) -> (vk::Image, vk::DeviceMemory) {
        let parts = (self.image, self.memory);
        self.image = vk::Image::null();
        self.memory = vk::DeviceMemory::null();
        parts
    }
}

impl Drop for PendingImage<'_> {
    fn drop(&mut self) {
        unsafe {
            if self.image != vk::Image::null() {
                self.device.destroy_image(self.image, None);
            }
            if self.memory != vk::DeviceMemory::null() {
                self.device.free_memory(self.memory, None);
            }
        }
    }
}
