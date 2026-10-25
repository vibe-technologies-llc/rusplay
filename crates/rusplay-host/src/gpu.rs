use crate::HostError;

#[derive(Debug, Clone)]
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl Gpu {
    pub fn open() -> Result<Self, HostError> {
        pollster::block_on(request())
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
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("rusplay-host"),
            ..wgpu::DeviceDescriptor::default()
        })
        .await
        .map_err(HostError::GpuDevice)?;

    Ok(Gpu { device, queue })
}
