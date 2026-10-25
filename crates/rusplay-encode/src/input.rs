use rusplay_capture::{DrmFourcc, Frame, FrameBuffer};
use rusplay_core::{BitDepth, ChromaSubsampling, PixelFormat, Resolution};

use crate::{Backend, EncodeError};

pub const fn fourcc_for(format: PixelFormat) -> DrmFourcc {
    DrmFourcc::from_code(match (format.chroma, format.depth) {
        (ChromaSubsampling::Yuv420, BitDepth::Eight) => *b"YU12",
        (ChromaSubsampling::Yuv422, BitDepth::Eight) => *b"YU16",
        (ChromaSubsampling::Yuv444, BitDepth::Eight) => *b"YU24",
        (ChromaSubsampling::Yuv420, BitDepth::Ten) => *b"I010",
        (ChromaSubsampling::Yuv422, BitDepth::Ten) => *b"I210",
        (ChromaSubsampling::Yuv444, BitDepth::Ten) => *b"I410",
        (ChromaSubsampling::Yuv420, BitDepth::Twelve) => *b"I012",
        (ChromaSubsampling::Yuv422, BitDepth::Twelve) => *b"I212",
        (ChromaSubsampling::Yuv444, BitDepth::Twelve) => *b"I412",
    })
}

pub fn pixel_format_of(fourcc: DrmFourcc) -> Option<PixelFormat> {
    const CHROMAS: [ChromaSubsampling; 3] = [
        ChromaSubsampling::Yuv420,
        ChromaSubsampling::Yuv422,
        ChromaSubsampling::Yuv444,
    ];
    const DEPTHS: [BitDepth; 3] = [BitDepth::Eight, BitDepth::Ten, BitDepth::Twelve];

    CHROMAS.into_iter().find_map(|chroma| {
        DEPTHS.into_iter().find_map(|depth| {
            let format = PixelFormat { chroma, depth };
            (fourcc_for(format) == fourcc).then_some(format)
        })
    })
}

pub fn fourcc_text(fourcc: DrmFourcc) -> String {
    fourcc
        .0
        .to_le_bytes()
        .iter()
        .map(|&byte| {
            if byte.is_ascii_graphic() {
                char::from(byte)
            } else {
                '?'
            }
        })
        .collect()
}

pub const fn bytes_per_sample(depth: BitDepth) -> usize {
    match depth {
        BitDepth::Eight => 1,
        BitDepth::Ten | BitDepth::Twelve => 2,
    }
}

const fn chroma_shifts(chroma: ChromaSubsampling) -> (u32, u32) {
    match chroma {
        ChromaSubsampling::Yuv420 => (1, 1),
        ChromaSubsampling::Yuv422 => (1, 0),
        ChromaSubsampling::Yuv444 => (0, 0),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct YuvPlane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
    pub width: usize,
    pub rows: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct YuvFrame<'a> {
    pub format: PixelFormat,
    pub resolution: Resolution,
    pub planes: [YuvPlane<'a>; 3],
}

impl<'a> YuvFrame<'a> {
    pub fn from_frame(
        frame: &'a Frame,
        backend: Backend,
        format: PixelFormat,
    ) -> Result<Self, EncodeError> {
        let FrameBuffer::Shm { data, stride } = &frame.buffer else {
            return Err(EncodeError::CpuFrameRequired { backend });
        };

        let expected = fourcc_for(format);
        if frame.format != expected {
            return Err(EncodeError::FrameFormatMismatch {
                backend,
                expected,
                actual: frame.format,
            });
        }

        let (shift_x, shift_y) = chroma_shifts(format.chroma);
        let sample = bytes_per_sample(format.depth);

        let luma_width = frame.resolution.width as usize;
        let luma_rows = frame.resolution.height as usize;
        let luma_stride = *stride as usize;
        let chroma_width = luma_width.div_ceil(1 << shift_x);
        let chroma_rows = luma_rows.div_ceil(1 << shift_y);
        let chroma_stride = luma_stride.div_ceil(1 << shift_x);

        let minimum = luma_width * sample;
        if luma_stride < minimum {
            return Err(EncodeError::FrameStrideTooSmall {
                minimum,
                actual: luma_stride,
            });
        }

        let luma_len = luma_stride * luma_rows;
        let chroma_len = chroma_stride * chroma_rows;
        let needed = luma_len + 2 * chroma_len;
        if data.len() < needed {
            return Err(EncodeError::FrameBufferTooShort {
                needed,
                actual: data.len(),
            });
        }

        let (luma, rest) = data.split_at(luma_len);
        let (cb, rest) = rest.split_at(chroma_len);
        let cr = &rest[..chroma_len];

        Ok(Self {
            format,
            resolution: frame.resolution,
            planes: [
                YuvPlane {
                    data: luma,
                    stride: luma_stride,
                    width: luma_width,
                    rows: luma_rows,
                },
                YuvPlane {
                    data: cb,
                    stride: chroma_stride,
                    width: chroma_width,
                    rows: chroma_rows,
                },
                YuvPlane {
                    data: cr,
                    stride: chroma_stride,
                    width: chroma_width,
                    rows: chroma_rows,
                },
            ],
        })
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use rusplay_capture::{Frame, FrameBuffer};
    use rusplay_core::{BitDepth, ChromaSubsampling, PixelFormat, Resolution, Timestamp};

    use super::{bytes_per_sample, chroma_shifts, fourcc_for};

    pub fn yuv_frame(
        format: PixelFormat,
        resolution: Resolution,
        index: u64,
        timestamp: Timestamp,
    ) -> Frame {
        let sample = bytes_per_sample(format.depth);
        let (shift_x, shift_y) = chroma_shifts(format.chroma);
        let width = resolution.width as usize;
        let height = resolution.height as usize;
        let chroma_width = width.div_ceil(1 << shift_x);
        let chroma_height = height.div_ceil(1 << shift_y);
        let max = (1_u32 << format.depth.bits()) - 1;

        let mut data = Vec::new();
        for (plane, (plane_width, rows)) in [
            (width, height),
            (chroma_width, chroma_height),
            (chroma_width, chroma_height),
        ]
        .into_iter()
        .enumerate()
        {
            for y in 0..rows {
                for x in 0..plane_width {
                    let noise = (x as u64 * 7 + y as u64 * 13 + index * 31 + plane as u64 * 101)
                        .wrapping_mul(2_654_435_761)
                        >> 7;
                    let gradient = (x * max as usize / plane_width.max(1)) as u64;
                    let value = (gradient + noise % 16 + index * 3) % u64::from(max + 1);
                    data.extend_from_slice(&value.to_le_bytes()[..sample]);
                }
            }
        }

        Frame {
            resolution,
            format: fourcc_for(format),
            timestamp,
            buffer: FrameBuffer::Shm {
                data,
                stride: u32::try_from(width * sample).unwrap(),
            },
        }
    }

    pub fn all_formats() -> Vec<PixelFormat> {
        [
            ChromaSubsampling::Yuv420,
            ChromaSubsampling::Yuv422,
            ChromaSubsampling::Yuv444,
        ]
        .into_iter()
        .flat_map(|chroma| {
            [BitDepth::Eight, BitDepth::Ten, BitDepth::Twelve]
                .into_iter()
                .map(move |depth| PixelFormat { chroma, depth })
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use rusplay_capture::FrameBuffer;
    use rusplay_core::Timestamp;

    use super::{fixtures::*, *};

    #[test]
    fn fourcc_round_trips_for_every_format() {
        for format in all_formats() {
            assert_eq!(pixel_format_of(fourcc_for(format)), Some(format));
        }
    }

    #[test]
    fn fourcc_text_spells_the_code() {
        let format = PixelFormat {
            chroma: ChromaSubsampling::Yuv420,
            depth: BitDepth::Eight,
        };

        assert_eq!(fourcc_text(fourcc_for(format)), "YU12");
    }

    #[test]
    fn plane_geometry_follows_chroma_subsampling() {
        let resolution = Resolution::new(64, 32);

        for format in all_formats() {
            let frame = yuv_frame(format, resolution, 0, Timestamp::ZERO);
            let view = YuvFrame::from_frame(&frame, Backend::X264, format).unwrap();

            let expected_width = match format.chroma {
                ChromaSubsampling::Yuv444 => 64,
                _ => 32,
            };
            let expected_rows = match format.chroma {
                ChromaSubsampling::Yuv420 => 16,
                _ => 32,
            };

            assert_eq!(view.planes[1].width, expected_width);
            assert_eq!(view.planes[2].rows, expected_rows);
            assert_eq!(view.planes[0].stride, 64 * bytes_per_sample(format.depth));
        }
    }

    #[test]
    fn mismatched_format_is_reported() {
        let have = PixelFormat {
            chroma: ChromaSubsampling::Yuv420,
            depth: BitDepth::Eight,
        };
        let want = PixelFormat {
            chroma: ChromaSubsampling::Yuv444,
            depth: BitDepth::Eight,
        };
        let frame = yuv_frame(have, Resolution::new(16, 16), 0, Timestamp::ZERO);

        let error = YuvFrame::from_frame(&frame, Backend::X264, want).unwrap_err();

        assert!(matches!(error, EncodeError::FrameFormatMismatch { .. }));
    }

    #[test]
    fn short_buffer_is_reported() {
        let format = PixelFormat::default();
        let mut frame = yuv_frame(format, Resolution::new(16, 16), 0, Timestamp::ZERO);
        if let FrameBuffer::Shm { data, .. } = &mut frame.buffer {
            data.truncate(10);
        }

        let error = YuvFrame::from_frame(&frame, Backend::X264, format).unwrap_err();

        assert!(matches!(error, EncodeError::FrameBufferTooShort { .. }));
    }

    #[test]
    fn narrow_stride_is_reported() {
        let format = PixelFormat::default();
        let mut frame = yuv_frame(format, Resolution::new(16, 16), 0, Timestamp::ZERO);
        if let FrameBuffer::Shm { stride, .. } = &mut frame.buffer {
            *stride = 8;
        }

        let error = YuvFrame::from_frame(&frame, Backend::X264, format).unwrap_err();

        assert!(matches!(error, EncodeError::FrameStrideTooSmall { .. }));
    }
}
