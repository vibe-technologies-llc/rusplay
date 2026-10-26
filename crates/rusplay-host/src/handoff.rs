use rusplay_capture::{BufferLease, Frame, FrameBuffer, Plane};
use rusplay_config::Config;
use rusplay_core::{ColorSpace, Resolution, Timestamp};
use rusplay_encode::{
    Backend,
    device::{DeviceInput, DeviceMemory, DevicePlanes, check_device_layout, device_input},
};
use rusplay_scale::{DEFAULT_ROW_ALIGNMENT, ExportedFrame, FrameLayout, MemoryHandle, PlaneLayout};

use crate::HostError;

pub(crate) const HANDOFF_ROW_ALIGNMENT: u32 = DEFAULT_ROW_ALIGNMENT;
const DRM_FORMAT_MOD_LINEAR: u64 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Handoff {
    pub backend: Backend,
    pub input: DeviceInput,
}

impl Handoff {
    pub(crate) fn plan(config: &Config) -> Option<Self> {
        let video = &config.video;
        let backend = Backend::from(&video.encoder);

        video
            .encoder_gpu_handoff
            .then(|| device_input(backend, video.pixel_format))
            .flatten()
            .map(|input| Self { backend, input })
    }

    pub(crate) const fn layout(self) -> PlaneLayout {
        match self.input.planes {
            DevicePlanes::SemiPlanar => PlaneLayout::SemiPlanar,
            DevicePlanes::Planar => PlaneLayout::PlanarHigh,
        }
    }

    pub(crate) const fn handle(self) -> MemoryHandle {
        match self.input.memory {
            DeviceMemory::DmaBuf => MemoryHandle::DmaBuf,
            DeviceMemory::OpaqueFd => MemoryHandle::OpaqueFd,
        }
    }

    pub(crate) fn accepts(
        self,
        config: &Config,
        resolution: Resolution,
        layout: &FrameLayout,
    ) -> Result<(), HostError> {
        let planes = device_planes(layout)?;
        check_device_layout(self.backend, config.video.pixel_format, resolution, &planes)?;
        Ok(())
    }

    pub(crate) fn frame(
        self,
        exported: &ExportedFrame,
        resolution: Resolution,
        color: ColorSpace,
        timestamp: Timestamp,
    ) -> Result<Frame, HostError> {
        let planes = device_planes(exported.frame().layout())?;
        let fd = exported
            .fd()
            .try_clone_to_owned()
            .map_err(HostError::HandoffFdDup)?;
        let buffer = match self.input.memory {
            DeviceMemory::DmaBuf => FrameBuffer::DmaBuf {
                fd,
                modifier: DRM_FORMAT_MOD_LINEAR,
                planes,
                lease: BufferLease::detached(),
            },
            DeviceMemory::OpaqueFd => FrameBuffer::OpaqueFd {
                fd,
                allocation: exported.allocation(),
                size: exported.allocation_size(),
                dedicated: exported.dedicated(),
                planes,
            },
        };

        Ok(Frame {
            resolution,
            format: self.input.fourcc,
            color,
            timestamp,
            buffer,
        })
    }
}

fn device_planes(layout: &FrameLayout) -> Result<Vec<Plane>, HostError> {
    layout
        .planes
        .iter()
        .map(|plane| {
            Ok(Plane {
                offset: u32::try_from(plane.offset)
                    .map_err(|_| HostError::HandoffPlaneOffset(plane.offset))?,
                stride: plane.stride,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use rusplay_config::{NvencConfig, VaapiConfig, VideoEncoderConfig, X265Config};
    use rusplay_core::{BitDepth, ChromaSubsampling, PixelFormat};

    use super::*;

    const RESOLUTIONS: [Resolution; 4] = [
        Resolution::new(3840, 2160),
        Resolution::new(1920, 1080),
        Resolution::new(1366, 768),
        Resolution::new(401, 225),
    ];

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

    fn config(encoder: VideoEncoderConfig, format: PixelFormat) -> Config {
        let mut config = Config::default();
        config.video.encoder = encoder;
        config.video.pixel_format = format;
        config
    }

    #[test]
    fn hardware_encoders_take_every_device_layout_the_converter_writes() {
        let encoders = [
            VideoEncoderConfig::Vaapi(VaapiConfig::default()),
            VideoEncoderConfig::Nvenc(NvencConfig::default()),
        ];
        let compiled = if cfg!(feature = "nvenc") { 2 } else { 1 };

        for encoder in encoders.into_iter().take(compiled) {
            for format in formats() {
                let config = config(encoder.clone(), format);
                let Some(handoff) = Handoff::plan(&config) else {
                    continue;
                };

                for resolution in RESOLUTIONS {
                    let layout = FrameLayout::packed(
                        resolution,
                        format,
                        handoff.layout(),
                        HANDOFF_ROW_ALIGNMENT,
                    )
                    .unwrap();

                    assert_eq!(layout.planes.len(), handoff.input.plane_count());
                    assert!(
                        handoff.accepts(&config, resolution, &layout).is_ok(),
                        "{encoder:?} {format:?} {resolution}"
                    );
                    if format.depth != BitDepth::Eight {
                        assert_eq!(layout.sample_shift(), 16 - u32::from(format.depth.bits()));
                    }
                }
            }
        }
    }

    #[test]
    fn handoff_follows_the_backend_and_the_option() {
        let nv12 = PixelFormat {
            chroma: ChromaSubsampling::Yuv420,
            depth: BitDepth::Eight,
        };
        let yuv422 = PixelFormat {
            chroma: ChromaSubsampling::Yuv422,
            depth: BitDepth::Ten,
        };
        let vaapi = config(VideoEncoderConfig::Vaapi(VaapiConfig::default()), nv12);
        let nvenc = config(VideoEncoderConfig::Nvenc(NvencConfig::default()), nv12);
        let mut turned_off = vaapi.clone();
        turned_off.video.encoder_gpu_handoff = false;

        let vaapi_handoff = Handoff::plan(&vaapi).unwrap();
        let nvenc_handoff = Handoff::plan(&nvenc).unwrap();

        assert_eq!(vaapi_handoff.handle(), MemoryHandle::DmaBuf);
        assert_eq!(vaapi_handoff.layout(), PlaneLayout::SemiPlanar);
        assert_eq!(nvenc_handoff.handle(), MemoryHandle::OpaqueFd);
        assert_eq!(Handoff::plan(&turned_off), None);
        assert_eq!(
            Handoff::plan(&config(
                VideoEncoderConfig::X265(X265Config::default()),
                nv12
            )),
            None
        );
        assert_eq!(
            Handoff::plan(&config(
                VideoEncoderConfig::Vaapi(VaapiConfig::default()),
                yuv422
            )),
            None
        );
        assert_eq!(
            Handoff::plan(&config(
                VideoEncoderConfig::Nvenc(NvencConfig::default()),
                PixelFormat {
                    chroma: ChromaSubsampling::Yuv444,
                    depth: BitDepth::Ten,
                }
            ))
            .map(Handoff::layout),
            Some(PlaneLayout::PlanarHigh)
        );
    }
}
