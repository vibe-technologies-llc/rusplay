use rusplay_capture::{DrmFourcc, Plane};
use rusplay_core::{BitDepth, ChromaSubsampling, PixelFormat, Resolution};

use crate::{Backend, EncodeError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceMemory {
    DmaBuf,
    OpaqueFd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DevicePlanes {
    SemiPlanar,
    Planar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceInput {
    pub memory: DeviceMemory,
    pub planes: DevicePlanes,
    pub fourcc: DrmFourcc,
}

impl DeviceInput {
    const fn new(memory: DeviceMemory, planes: DevicePlanes, code: [u8; 4]) -> Self {
        Self {
            memory,
            planes,
            fourcc: DrmFourcc::from_code(code),
        }
    }

    pub const fn plane_count(self) -> usize {
        match self.planes {
            DevicePlanes::SemiPlanar => 2,
            DevicePlanes::Planar => 3,
        }
    }
}

pub const fn device_input(backend: Backend, format: PixelFormat) -> Option<DeviceInput> {
    use DeviceMemory::{DmaBuf, OpaqueFd};
    use DevicePlanes::{Planar, SemiPlanar};

    match (backend, format.chroma, format.depth) {
        (Backend::Vaapi, ChromaSubsampling::Yuv420, BitDepth::Eight) => {
            Some(DeviceInput::new(DmaBuf, SemiPlanar, *b"NV12"))
        }
        (Backend::Vaapi, ChromaSubsampling::Yuv420, BitDepth::Ten) => {
            Some(DeviceInput::new(DmaBuf, SemiPlanar, *b"P010"))
        }
        (Backend::Vaapi, ChromaSubsampling::Yuv420, BitDepth::Twelve) => {
            Some(DeviceInput::new(DmaBuf, SemiPlanar, *b"P012"))
        }
        (Backend::Nvenc, ChromaSubsampling::Yuv420, BitDepth::Eight) => {
            Some(DeviceInput::new(OpaqueFd, SemiPlanar, *b"NV12"))
        }
        (Backend::Nvenc, ChromaSubsampling::Yuv420, BitDepth::Ten) => {
            Some(DeviceInput::new(OpaqueFd, SemiPlanar, *b"P010"))
        }
        (Backend::Nvenc, ChromaSubsampling::Yuv422, BitDepth::Eight) => {
            Some(DeviceInput::new(OpaqueFd, SemiPlanar, *b"NV16"))
        }
        (Backend::Nvenc, ChromaSubsampling::Yuv422, BitDepth::Ten) => {
            Some(DeviceInput::new(OpaqueFd, SemiPlanar, *b"P210"))
        }
        (Backend::Nvenc, ChromaSubsampling::Yuv444, BitDepth::Eight) => {
            Some(DeviceInput::new(OpaqueFd, Planar, *b"YU24"))
        }
        (Backend::Nvenc, ChromaSubsampling::Yuv444, BitDepth::Ten) => {
            Some(DeviceInput::new(OpaqueFd, Planar, *b"Q410"))
        }
        _ => None,
    }
}

pub fn check_device_layout(
    backend: Backend,
    format: PixelFormat,
    resolution: Resolution,
    planes: &[Plane],
) -> Result<(), EncodeError> {
    let Some(input) = device_input(backend, format) else {
        return Err(EncodeError::CpuFrameRequired { backend });
    };

    if backend == Backend::Nvenc {
        return check_nvenc_layout(format, resolution, planes);
    }
    if planes.len() != input.plane_count() {
        return Err(EncodeError::DmaBufPlaneCount {
            fourcc: input.fourcc,
            expected: input.plane_count(),
            actual: planes.len(),
        });
    }

    Ok(())
}

#[cfg(feature = "nvenc")]
fn check_nvenc_layout(
    format: PixelFormat,
    resolution: Resolution,
    planes: &[Plane],
) -> Result<(), EncodeError> {
    crate::nvenc::layout::dma_buf_layout(format, resolution, planes).map(drop)
}

#[cfg(not(feature = "nvenc"))]
fn check_nvenc_layout(
    _format: PixelFormat,
    _resolution: Resolution,
    _planes: &[Plane],
) -> Result<(), EncodeError> {
    Err(EncodeError::BackendNotCompiled {
        backend: Backend::Nvenc,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn formats() -> impl Iterator<Item = PixelFormat> {
        [
            ChromaSubsampling::Yuv420,
            ChromaSubsampling::Yuv422,
            ChromaSubsampling::Yuv444,
        ]
        .into_iter()
        .flat_map(|chroma| {
            [BitDepth::Eight, BitDepth::Ten, BitDepth::Twelve]
                .map(move |depth| PixelFormat { chroma, depth })
        })
    }

    #[test]
    fn software_backends_take_no_device_input() {
        for backend in [Backend::X264, Backend::X265, Backend::SvtAv1] {
            for format in formats() {
                assert_eq!(device_input(backend, format), None);
                assert!(matches!(
                    check_device_layout(backend, format, Resolution::new(64, 64), &[]),
                    Err(EncodeError::CpuFrameRequired { backend: refused }) if refused == backend
                ));
            }
        }
    }

    #[cfg(feature = "vaapi")]
    #[test]
    fn vaapi_device_input_is_its_dma_buf_format() {
        for format in formats() {
            let Some(input) = device_input(Backend::Vaapi, format) else {
                assert_ne!(crate::vaapi::format::dma_buf_planes(format), 2);
                continue;
            };

            assert_eq!(input.memory, DeviceMemory::DmaBuf);
            assert_eq!(input.fourcc, crate::vaapi::format::dma_buf_fourcc(format));
            assert_eq!(
                input.plane_count(),
                crate::vaapi::format::dma_buf_planes(format)
            );
        }
    }

    #[cfg(feature = "nvenc")]
    #[test]
    fn nvenc_device_input_is_every_format_it_imports() {
        for format in formats() {
            let Some(input) = device_input(Backend::Nvenc, format) else {
                assert_eq!(crate::nvenc::layout::buffer_format(format), None);
                continue;
            };

            assert!(crate::nvenc::layout::buffer_format(format).is_some());
            assert_eq!(input.memory, DeviceMemory::OpaqueFd);
            assert_eq!(input.fourcc, crate::nvenc::layout::dma_buf_fourcc(format));
        }
    }
}
