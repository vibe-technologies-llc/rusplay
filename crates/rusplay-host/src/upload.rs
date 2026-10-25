use rusplay_capture::DrmFourcc;
use rusplay_core::ChromaSubsampling;
use rusplay_scale::FrameLayout;

use crate::HostError;

pub(crate) const UPLOAD_FORMATS: [(DrmFourcc, wgpu::TextureFormat); 4] = [
    (
        DrmFourcc::from_code(*b"XR24"),
        wgpu::TextureFormat::Bgra8UnormSrgb,
    ),
    (
        DrmFourcc::from_code(*b"AR24"),
        wgpu::TextureFormat::Bgra8UnormSrgb,
    ),
    (
        DrmFourcc::from_code(*b"XB24"),
        wgpu::TextureFormat::Rgba8UnormSrgb,
    ),
    (
        DrmFourcc::from_code(*b"AB24"),
        wgpu::TextureFormat::Rgba8UnormSrgb,
    ),
];

pub(crate) const UPLOAD_BYTES_PER_PIXEL: u32 = 4;

pub(crate) fn texture_format_for(fourcc: DrmFourcc) -> Result<wgpu::TextureFormat, HostError> {
    UPLOAD_FORMATS
        .iter()
        .find(|(candidate, _)| *candidate == fourcc)
        .map(|(_, format)| *format)
        .ok_or(HostError::CaptureFormatUnsupported { fourcc })
}

pub(crate) fn check_shm_frame(
    data: &[u8],
    stride: u32,
    width: u32,
    height: u32,
) -> Result<(), HostError> {
    let row_bytes = width as usize * UPLOAD_BYTES_PER_PIXEL as usize;
    if (stride as usize) < row_bytes {
        return Err(HostError::CaptureStrideTooSmall { stride, width });
    }

    let needed = (height as usize).saturating_sub(1) * stride as usize + row_bytes;
    if data.len() < needed {
        return Err(HostError::CaptureBufferTooShort {
            found: data.len(),
            needed,
        });
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EncoderPlanes {
    pub luma_stride: usize,
    pub chroma_stride: usize,
    pub size: usize,
}

impl EncoderPlanes {
    pub(crate) fn of(layout: &FrameLayout, chroma: ChromaSubsampling) -> Self {
        let sample = layout.bytes_per_sample as usize;
        let luma = &layout.planes[0];
        let luma_stride = (luma.width as usize).next_multiple_of(2) * sample;
        let chroma_stride = match chroma {
            ChromaSubsampling::Yuv444 => luma_stride,
            ChromaSubsampling::Yuv420 | ChromaSubsampling::Yuv422 => luma_stride / 2,
        };
        let chroma_rows = layout
            .planes
            .get(1)
            .map_or(0, |plane| plane.height as usize);

        Self {
            luma_stride,
            chroma_stride,
            size: luma_stride * luma.height as usize + 2 * chroma_stride * chroma_rows,
        }
    }

    pub(crate) fn repack(&self, layout: &FrameLayout, source: &[u8], out: &mut Vec<u8>) {
        if out.len() != self.size {
            out.clear();
            out.resize(self.size, 0);
        }

        let sample = layout.bytes_per_sample as usize;
        let mut destination = 0;

        for (index, plane) in layout.planes.iter().enumerate() {
            let stride = if index == 0 {
                self.luma_stride
            } else {
                self.chroma_stride
            };
            let row_bytes = plane.width as usize * sample;
            let offset = usize::try_from(plane.offset).unwrap_or(usize::MAX);

            for row in 0..plane.height as usize {
                let from = offset + row * plane.stride as usize;
                let to = destination + row * stride;
                out[to..to + row_bytes].copy_from_slice(&source[from..from + row_bytes]);
            }
            destination += stride * plane.height as usize;
        }
    }
}

#[cfg(test)]
mod tests {
    use rusplay_capture::{Frame, FrameBuffer};
    use rusplay_core::{BitDepth, PixelFormat, Resolution, Timestamp};
    use rusplay_encode::{
        Backend,
        input::{YuvFrame, fourcc_for},
    };
    use rusplay_scale::PlaneLayout;

    use super::*;

    const GPU_ROW_ALIGNMENT: u32 = 4;

    fn layout(resolution: Resolution, format: PixelFormat) -> FrameLayout {
        FrameLayout::packed(resolution, format, PlaneLayout::Planar, GPU_ROW_ALIGNMENT).unwrap()
    }

    fn numbered(layout: &FrameLayout) -> Vec<u8> {
        (0..layout.size)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect()
    }

    #[test]
    fn repacked_planes_satisfy_the_encoder_input_contract() {
        for chroma in [
            ChromaSubsampling::Yuv420,
            ChromaSubsampling::Yuv422,
            ChromaSubsampling::Yuv444,
        ] {
            for depth in [BitDepth::Eight, BitDepth::Ten] {
                for resolution in [Resolution::new(7, 5), Resolution::new(64, 4)] {
                    let format = PixelFormat { chroma, depth };
                    let layout = layout(resolution, format);
                    let source = numbered(&layout);
                    let planes = EncoderPlanes::of(&layout, chroma);
                    let mut packed = Vec::new();

                    planes.repack(&layout, &source, &mut packed);
                    let frame = Frame {
                        resolution,
                        format: fourcc_for(format),
                        timestamp: Timestamp::ZERO,
                        buffer: FrameBuffer::Shm {
                            data: packed,
                            stride: u32::try_from(planes.luma_stride).unwrap(),
                        },
                    };
                    let yuv = YuvFrame::from_frame(&frame, Backend::Vaapi, format).unwrap();

                    for (plane, expected) in yuv.planes.iter().zip(&layout.planes) {
                        let sample = layout.bytes_per_sample as usize;
                        let offset = usize::try_from(expected.offset).unwrap();
                        assert_eq!(plane.width, expected.width as usize);
                        assert_eq!(plane.rows, expected.height as usize);
                        for row in 0..plane.rows {
                            let got = &plane.data[row * plane.stride..][..plane.width * sample];
                            let want = &source[offset + row * expected.stride as usize..]
                                [..plane.width * sample];
                            assert_eq!(got, want, "{format:?} {resolution} row {row}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn only_formats_with_an_srgb_view_are_uploaded() {
        assert_eq!(
            texture_format_for(DrmFourcc::from_code(*b"XR24")).unwrap(),
            wgpu::TextureFormat::Bgra8UnormSrgb
        );
        assert!(matches!(
            texture_format_for(DrmFourcc::from_code(*b"XR30")),
            Err(HostError::CaptureFormatUnsupported { .. })
        ));
    }

    #[test]
    fn short_or_narrow_shared_memory_frames_are_refused() {
        assert!(matches!(
            check_shm_frame(&[0; 64], 12, 4, 2),
            Err(HostError::CaptureStrideTooSmall {
                stride: 12,
                width: 4
            })
        ));
        assert!(matches!(
            check_shm_frame(&[0; 20], 16, 4, 2),
            Err(HostError::CaptureBufferTooShort {
                found: 20,
                needed: 32
            })
        ));
        assert!(check_shm_frame(&[0; 32], 16, 4, 2).is_ok());
    }
}
