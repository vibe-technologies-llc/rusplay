use std::{
    ffi::c_void,
    fmt,
    os::fd::{BorrowedFd, FromRawFd, IntoRawFd, OwnedFd},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use ash::{khr, vk};
use rustix::ioctl::{Ioctl, IoctlOutput, Opcode, opcode};
use wgpu::hal::{self, api::Vulkan};

use crate::{ScaleError, device_descriptor};

const SYNC_FD: vk::ExternalSemaphoreHandleTypeFlags = vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD;
const WAIT_STAGE: vk::PipelineStageFlags = vk::PipelineStageFlags::ALL_COMMANDS;
const DMA_BUF_BASE: u8 = b'b';
const DMA_BUF_EXPORT_SYNC_FILE: u8 = 2;
const DMA_BUF_SYNC_READ: u32 = 1;

#[repr(C)]
struct ExportSyncFile {
    flags: u32,
    fd: i32,
}

struct ExportWriteFences(ExportSyncFile);

unsafe impl Ioctl for ExportWriteFences {
    type Output = OwnedFd;

    const IS_MUTATING: bool = true;

    fn opcode(&self) -> Opcode {
        opcode::read_write::<ExportSyncFile>(DMA_BUF_BASE, DMA_BUF_EXPORT_SYNC_FILE)
    }

    fn as_ptr(&mut self) -> *mut c_void {
        (&raw mut self.0).cast()
    }

    unsafe fn output_from_ptr(
        _out: IoctlOutput,
        extract_output: *mut c_void,
    ) -> rustix::io::Result<OwnedFd> {
        let exported = unsafe { extract_output.cast::<ExportSyncFile>().read() };
        Ok(unsafe { OwnedFd::from_raw_fd(exported.fd) })
    }
}

fn export_write_fences(dmabuf: BorrowedFd<'_>) -> Result<OwnedFd, ScaleError> {
    let request = ExportWriteFences(ExportSyncFile {
        flags: DMA_BUF_SYNC_READ,
        fd: -1,
    });

    unsafe { rustix::ioctl::ioctl(dmabuf, request) }.map_err(ScaleError::DmaBufFenceExport)
}

fn with_hal_queue<R>(
    queue: &wgpu::Queue,
    use_queue: impl FnOnce(&hal::vulkan::Queue) -> R,
) -> Option<R> {
    let hal_queue = unsafe { queue.as_hal::<Vulkan>() }?;

    Some(use_queue(&hal_queue))
}

struct LiveSemaphore {
    _device: wgpu::Device,
    raw: ash::Device,
    semaphore: vk::Semaphore,
    live: Arc<AtomicUsize>,
}

impl LiveSemaphore {
    fn create(waiter: &GpuFenceWaiter) -> Result<Self, ScaleError> {
        let semaphore = unsafe {
            waiter
                .raw
                .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
        }
        .map_err(ScaleError::GpuFenceSemaphoreCreate)?;
        waiter.live.fetch_add(1, Ordering::Relaxed);

        Ok(Self {
            _device: waiter.device.clone(),
            raw: waiter.raw.clone(),
            semaphore,
            live: waiter.live.clone(),
        })
    }
}

impl Drop for LiveSemaphore {
    fn drop(&mut self) {
        unsafe { self.raw.destroy_semaphore(self.semaphore, None) };
        self.live.fetch_sub(1, Ordering::Relaxed);
    }
}

pub struct GpuFenceWaiter {
    device: wgpu::Device,
    raw: ash::Device,
    semaphore_fd: khr::external_semaphore_fd::Device,
    live: Arc<AtomicUsize>,
}

impl fmt::Debug for GpuFenceWaiter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GpuFenceWaiter")
            .field("device", &self.raw.handle())
            .field("live", &self.live)
            .finish_non_exhaustive()
    }
}

impl GpuFenceWaiter {
    pub fn new(device: &wgpu::Device) -> Result<Self, ScaleError> {
        let hal = unsafe { device.as_hal::<Vulkan>() }.ok_or(ScaleError::DmaBufNeedsVulkan)?;
        if !hal
            .enabled_device_extensions()
            .contains(&khr::external_semaphore_fd::NAME)
        {
            return Err(ScaleError::GpuFenceWaitUnavailable);
        }

        let instance = hal.shared_instance().raw_instance();
        let physical = hal.raw_physical_device();
        if !imports_sync_files(instance, physical) {
            return Err(ScaleError::GpuFenceWaitUnavailable);
        }

        let raw = hal.raw_device().clone();

        Ok(Self {
            device: device.clone(),
            semaphore_fd: khr::external_semaphore_fd::Device::new(instance, &raw),
            raw,
            live: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub fn live_semaphores(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }

    pub fn stage(
        &self,
        queue: &wgpu::Queue,
        dmabuf: BorrowedFd<'_>,
    ) -> Result<GpuFenceWait, ScaleError> {
        let sync_file = export_write_fences(dmabuf)?;
        let semaphore = LiveSemaphore::create(self)?;
        self.import_sync_file(&semaphore, sync_file)?;

        let staged = with_hal_queue(queue, |hal_queue| {
            hal_queue.add_wait_semaphore(semaphore.semaphore, None, WAIT_STAGE);
        });
        if staged.is_none() {
            return Err(ScaleError::DmaBufNeedsVulkan);
        }

        Ok(GpuFenceWait {
            queue: queue.clone(),
            semaphore: Some(semaphore),
        })
    }

    fn import_sync_file(
        &self,
        semaphore: &LiveSemaphore,
        sync_file: OwnedFd,
    ) -> Result<(), ScaleError> {
        let raw = sync_file.into_raw_fd();
        let info = vk::ImportSemaphoreFdInfoKHR::default()
            .semaphore(semaphore.semaphore)
            .flags(vk::SemaphoreImportFlags::TEMPORARY)
            .handle_type(SYNC_FD)
            .fd(raw);

        unsafe { self.semaphore_fd.import_semaphore_fd(&info) }.map_err(|result| {
            drop(unsafe { OwnedFd::from_raw_fd(raw) });
            ScaleError::GpuFenceSemaphoreImport(result)
        })
    }
}

fn imports_sync_files(instance: &ash::Instance, physical: vk::PhysicalDevice) -> bool {
    let properties = unsafe { instance.get_physical_device_properties(physical) };
    if properties.api_version < vk::API_VERSION_1_1 {
        return false;
    }

    let info = vk::PhysicalDeviceExternalSemaphoreInfo::default().handle_type(SYNC_FD);
    let mut support = vk::ExternalSemaphoreProperties::default();
    unsafe {
        instance.get_physical_device_external_semaphore_properties(physical, &info, &mut support);
    }

    support
        .external_semaphore_features
        .contains(vk::ExternalSemaphoreFeatureFlags::IMPORTABLE)
}

#[derive(Debug)]
pub struct GpuFenceWait {
    queue: wgpu::Queue,
    semaphore: Option<LiveSemaphore>,
}

impl fmt::Debug for LiveSemaphore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveSemaphore")
            .field("semaphore", &self.semaphore)
            .finish_non_exhaustive()
    }
}

impl Drop for GpuFenceWait {
    fn drop(&mut self) {
        let Some(semaphore) = self.semaphore.take() else {
            return;
        };

        let unsubmitted = with_hal_queue(&self.queue, |hal_queue| {
            hal_queue.remove_wait_semaphore(semaphore.semaphore)
        });
        if unsubmitted == Some(false) {
            self.queue.on_submitted_work_done(move || drop(semaphore));
        }
    }
}

pub async fn request_fence_wait_device(
    adapter: &wgpu::Adapter,
    label: &'static str,
) -> Result<(wgpu::Device, wgpu::Queue), wgpu::RequestDeviceError> {
    let descriptor = wgpu::DeviceDescriptor {
        label: Some(label),
        ..device_descriptor(adapter)
    };

    if let Some(opened) = open_with_fence_wait(adapter, &descriptor) {
        return Ok(opened);
    }
    adapter.request_device(&descriptor).await
}

fn open_with_fence_wait(
    adapter: &wgpu::Adapter,
    descriptor: &wgpu::DeviceDescriptor<'_>,
) -> Option<(wgpu::Device, wgpu::Queue)> {
    let open = {
        let hal_adapter = unsafe { adapter.as_hal::<Vulkan>() }?;
        let physical = hal_adapter.raw_physical_device();
        let instance = hal_adapter.shared_instance().raw_instance();
        let supported = hal_adapter
            .physical_device_capabilities()
            .supports_extension(khr::external_semaphore_fd::NAME);
        if !supported || !imports_sync_files(instance, physical) {
            return None;
        }

        let add_extension: Box<hal::vulkan::CreateDeviceCallback<'_>> = Box::new(|args| {
            if !args.extensions.contains(&khr::external_semaphore_fd::NAME) {
                args.extensions.push(khr::external_semaphore_fd::NAME);
            }
        });
        unsafe {
            hal_adapter.open_with_callback(
                descriptor.required_features,
                &descriptor.required_limits,
                &descriptor.memory_hints,
                Some(add_extension),
            )
        }
        .ok()?
    };

    unsafe { adapter.create_device_from_hal::<Vulkan>(open, descriptor) }.ok()
}
