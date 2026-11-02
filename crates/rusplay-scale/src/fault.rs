use std::sync::Arc;

use parking_lot::Mutex;
use rusplay_core::Resolution;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GpuFault {
    #[error("the gpu ran out of memory")]
    OutOfMemory(#[source] wgpu::Error),
    #[error("the gpu rejected a command")]
    Validation(#[source] wgpu::Error),
    #[error("the gpu failed internally")]
    Internal(#[source] wgpu::Error),
    #[error("a {width}x{height} texture exceeds the device's limit of {limit} texels per side")]
    TextureTooLarge { width: u32, height: u32, limit: u32 },
}

impl From<wgpu::Error> for GpuFault {
    fn from(error: wgpu::Error) -> Self {
        match error {
            wgpu::Error::OutOfMemory { .. } => Self::OutOfMemory(error),
            wgpu::Error::Validation { .. } => Self::Validation(error),
            wgpu::Error::Internal { .. } => Self::Internal(error),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct GpuFaults {
    pending: Arc<Mutex<Option<GpuFault>>>,
}

impl GpuFaults {
    pub fn install(device: &wgpu::Device) -> Self {
        let faults = Self::default();
        let sink = faults.clone();
        device.on_uncaptured_error(Arc::new(move |error: wgpu::Error| {
            sink.record(GpuFault::from(error));
        }));

        faults
    }

    fn record(&self, fault: GpuFault) {
        self.pending.lock().get_or_insert(fault);
    }

    pub fn check(&self) -> Result<(), GpuFault> {
        self.pending.lock().take().map_or(Ok(()), Err)
    }

    pub fn guard<T, E: From<GpuFault>>(
        &self,
        device: &wgpu::Device,
        work: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        self.check()?;

        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);

        let outcome = work();

        let validation = pollster::block_on(validation.pop());
        let memory = pollster::block_on(memory.pop());
        let internal = pollster::block_on(internal.pop());
        if let Some(error) = memory.or(validation).or(internal) {
            return Err(GpuFault::from(error).into());
        }

        self.check()?;
        outcome
    }
}

pub fn check_extent(device: &wgpu::Device, resolution: Resolution) -> Result<(), GpuFault> {
    check_extent_against(device.limits().max_texture_dimension_2d, resolution)
}

fn check_extent_against(limit: u32, resolution: Resolution) -> Result<(), GpuFault> {
    if resolution.width > limit || resolution.height > limit {
        return Err(GpuFault::TextureTooLarge {
            width: resolution.width,
            height: resolution.height,
            limit,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_device() -> Option<wgpu::Device> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;

        pollster::block_on(adapter.request_device(&crate::device_descriptor(&adapter)))
            .ok()
            .map(|(device, _)| device)
    }

    fn oversized(device: &wgpu::Device) -> wgpu::Texture {
        let side = device.limits().max_texture_dimension_2d + 1;

        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rusplay-test-oversized"),
            size: wgpu::Extent3d {
                width: side,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    }

    #[test]
    fn an_extent_past_the_limit_is_refused() {
        let wide = Resolution::new(8193, 100);
        let tall = Resolution::new(100, 8193);
        let fits = Resolution::new(8192, 8192);

        assert!(matches!(
            check_extent_against(8192, wide),
            Err(GpuFault::TextureTooLarge {
                width: 8193,
                height: 100,
                limit: 8192,
            })
        ));
        assert!(matches!(
            check_extent_against(8192, tall),
            Err(GpuFault::TextureTooLarge { .. })
        ));
        assert!(check_extent_against(8192, fits).is_ok());
    }

    #[test]
    fn a_recorded_fault_surfaces_once_and_the_first_one_wins() {
        let faults = GpuFaults::default();

        faults.record(GpuFault::TextureTooLarge {
            width: 1,
            height: 1,
            limit: 0,
        });
        faults.record(GpuFault::TextureTooLarge {
            width: 2,
            height: 2,
            limit: 0,
        });

        assert!(matches!(
            faults.check(),
            Err(GpuFault::TextureTooLarge { width: 1, .. })
        ));
        assert!(faults.check().is_ok());
    }

    #[test]
    fn a_guarded_oversized_texture_is_an_error_instead_of_a_panic() {
        let Some(device) = fresh_device() else {
            return;
        };
        let faults = GpuFaults::install(&device);

        let result = faults.guard(&device, || {
            drop(oversized(&device));
            Ok::<_, GpuFault>(())
        });

        assert!(matches!(result, Err(GpuFault::Validation(_))));
        assert!(faults.check().is_ok());
    }

    #[test]
    fn an_unguarded_oversized_texture_reaches_the_installed_handler() {
        let Some(device) = fresh_device() else {
            return;
        };
        let faults = GpuFaults::install(&device);

        drop(oversized(&device));

        assert!(matches!(faults.check(), Err(GpuFault::Validation(_))));
    }

    #[test]
    fn a_clean_guarded_run_returns_its_value() {
        let Some(device) = fresh_device() else {
            return;
        };
        let faults = GpuFaults::install(&device);

        let result = faults.guard(&device, || Ok::<_, GpuFault>(7));

        assert_eq!(result.unwrap(), 7);
    }
}
