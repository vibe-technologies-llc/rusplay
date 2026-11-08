use rusplay_core::Resolution;
use rusplay_scale::{GpuFaults, check_extent};

use crate::HostError;

#[derive(Debug, Clone)]
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub faults: GpuFaults,
    pub pci_bus_id: Option<String>,
}

impl Gpu {
    pub fn open() -> Result<Self, HostError> {
        pollster::block_on(request())
    }

    pub fn guard<T>(&self, work: impl FnOnce() -> Result<T, HostError>) -> Result<T, HostError> {
        self.faults.guard(&self.device, work)
    }

    pub fn check_extent(&self, resolution: Resolution) -> Result<(), HostError> {
        Ok(check_extent(&self.device, resolution)?)
    }
}

async fn request() -> Result<Gpu, HostError> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..wgpu::RequestAdapterOptions::default()
        })
        .await
        .map_err(|_| HostError::NoGpuAdapter)?;
    let (device, queue) = rusplay_scale::request_fence_wait_device(&adapter, "rusplay-host")
        .await
        .map_err(HostError::GpuDevice)?;

    let faults = GpuFaults::install(&device);
    let pci_bus_id = Some(adapter.get_info().device_pci_bus_id).filter(|id| !id.is_empty());

    Ok(Gpu {
        device,
        queue,
        faults,
        pci_bus_id,
    })
}
