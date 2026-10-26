use std::{
    fmt,
    os::fd::{FromRawFd, OwnedFd},
};

use ash::{khr, vk};
use wgpu::hal::{self, api::Vulkan};

use crate::{MemoryHandle, ScaleError};

const BUFFER_USAGE: vk::BufferUsageFlags = vk::BufferUsageFlags::from_raw(
    vk::BufferUsageFlags::STORAGE_BUFFER.as_raw()
        | vk::BufferUsageFlags::TRANSFER_SRC.as_raw()
        | vk::BufferUsageFlags::TRANSFER_DST.as_raw(),
);

const fn handle_type(handle: MemoryHandle) -> vk::ExternalMemoryHandleTypeFlags {
    match handle {
        MemoryHandle::DmaBuf => vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
        MemoryHandle::OpaqueFd => vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
    }
}

pub(crate) struct ExportedBuffer {
    pub buffer: wgpu::Buffer,
    pub fd: OwnedFd,
    pub allocation_size: u64,
    pub dedicated: bool,
}

pub(crate) struct Exporter {
    instance: ash::Instance,
    device: ash::Device,
    physical: vk::PhysicalDevice,
    memory_fd: khr::external_memory_fd::Device,
    handle: MemoryHandle,
}

impl fmt::Debug for Exporter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Exporter")
            .field("device", &self.device.handle())
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

impl Exporter {
    pub fn new(device: &wgpu::Device, handle: MemoryHandle) -> Result<Self, ScaleError> {
        let unavailable = ScaleError::MemoryExportUnavailable(handle);
        if handle == MemoryHandle::DmaBuf
            && !device
                .features()
                .contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF)
        {
            return Err(unavailable);
        }

        let hal =
            unsafe { device.as_hal::<Vulkan>() }.ok_or(ScaleError::MemoryExportNeedsVulkan)?;
        if !hal
            .enabled_device_extensions()
            .contains(&khr::external_memory_fd::NAME)
            || hal.shared_instance().instance_api_version() < vk::API_VERSION_1_1
        {
            return Err(unavailable);
        }

        let instance = hal.shared_instance().raw_instance().clone();
        let physical = hal.raw_physical_device();
        let properties = unsafe { instance.get_physical_device_properties(physical) };
        if properties.api_version < vk::API_VERSION_1_1 {
            return Err(unavailable);
        }
        let raw = hal.raw_device().clone();
        let memory_fd = khr::external_memory_fd::Device::new(&instance, &raw);

        Ok(Self {
            instance,
            device: raw,
            physical,
            memory_fd,
            handle,
        })
    }

    pub fn exportable(&self) -> bool {
        let info = vk::PhysicalDeviceExternalBufferInfo::default()
            .usage(BUFFER_USAGE)
            .handle_type(handle_type(self.handle));
        let mut properties = vk::ExternalBufferProperties::default();
        unsafe {
            self.instance
                .get_physical_device_external_buffer_properties(
                    self.physical,
                    &info,
                    &mut properties,
                );
        }

        properties
            .external_memory_properties
            .external_memory_features
            .contains(vk::ExternalMemoryFeatureFlags::EXPORTABLE)
    }

    pub fn export(&self, device: &wgpu::Device, size: u64) -> Result<ExportedBuffer, ScaleError> {
        if !self.exportable() {
            return Err(ScaleError::MemoryExportUnavailable(self.handle));
        }

        let handle = handle_type(self.handle);
        let mut external = vk::ExternalMemoryBufferCreateInfo::default().handle_types(handle);
        let info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(BUFFER_USAGE)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external);
        let buffer = unsafe { self.device.create_buffer(&info, None) }
            .map_err(ScaleError::MemoryExportBufferCreate)?;
        let mut pending = PendingBuffer {
            device: &self.device,
            buffer,
            memory: vk::DeviceMemory::null(),
        };

        let requirements = unsafe { self.device.get_buffer_memory_requirements(buffer) };
        let memory_type = self
            .memory_type(requirements.memory_type_bits)
            .ok_or(ScaleError::MemoryExportNoMemoryType)?;
        let mut export = vk::ExportMemoryAllocateInfo::default().handle_types(handle);
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().buffer(buffer);
        let allocate = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(memory_type)
            .push_next(&mut export)
            .push_next(&mut dedicated);
        pending.memory = unsafe { self.device.allocate_memory(&allocate, None) }
            .map_err(ScaleError::MemoryExportAllocate)?;
        unsafe { self.device.bind_buffer_memory(buffer, pending.memory, 0) }
            .map_err(ScaleError::MemoryExportBind)?;

        let fd_info = vk::MemoryGetFdInfoKHR::default()
            .memory(pending.memory)
            .handle_type(handle);
        let raw_fd = unsafe { self.memory_fd.get_memory_fd(&fd_info) }
            .map_err(ScaleError::MemoryExportFd)?;
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };

        let (buffer, memory) = pending.release();
        let hal_buffer =
            unsafe { hal::vulkan::Buffer::from_raw_managed(buffer, memory, 0, requirements.size) };
        let wrapped = unsafe {
            device.create_buffer_from_hal::<Vulkan>(
                hal_buffer,
                &wgpu::BufferDescriptor {
                    label: Some("rusplay-exported-yuv-frame"),
                    size,
                    usage: wgpu::BufferUsages::STORAGE
                        | wgpu::BufferUsages::COPY_SRC
                        | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            )
        };

        Ok(ExportedBuffer {
            buffer: wrapped,
            fd,
            allocation_size: requirements.size,
            dedicated: true,
        })
    }

    fn memory_type(&self, allowed: u32) -> Option<u32> {
        let properties = unsafe {
            self.instance
                .get_physical_device_memory_properties(self.physical)
        };
        let types = &properties.memory_types[..properties.memory_type_count as usize];
        let allowed_index = |index: &usize| allowed & (1 << index) != 0;

        (0..types.len())
            .filter(allowed_index)
            .find(|&index| {
                types[index]
                    .property_flags
                    .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            })
            .or_else(|| (0..types.len()).find(allowed_index))
            .and_then(|index| u32::try_from(index).ok())
    }
}

struct PendingBuffer<'a> {
    device: &'a ash::Device,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
}

impl PendingBuffer<'_> {
    fn release(mut self) -> (vk::Buffer, vk::DeviceMemory) {
        let parts = (self.buffer, self.memory);
        self.buffer = vk::Buffer::null();
        self.memory = vk::DeviceMemory::null();
        parts
    }
}

impl Drop for PendingBuffer<'_> {
    fn drop(&mut self) {
        unsafe {
            if self.buffer != vk::Buffer::null() {
                self.device.destroy_buffer(self.buffer, None);
            }
            if self.memory != vk::DeviceMemory::null() {
                self.device.free_memory(self.memory, None);
            }
        }
    }
}
