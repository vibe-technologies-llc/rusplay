use rusplay_capture::{DrmFourcc, Plane};
use rusplay_core::{BitDepth, ChromaSubsampling, PixelFormat, Resolution};

use crate::{
    Backend, EncodeError,
    input::{YuvFrame, YuvPlane, bytes_per_sample},
    nvenc::ffi,
};

const BACKEND: Backend = Backend::Nvenc;
const PITCH_ALIGNMENT: u32 = 4;
pub(super) const LINEAR_MODIFIER: u64 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Layout {
    SemiPlanar420,
    SemiPlanar422,
    Planar,
}

impl Layout {
    pub(super) const fn of(format: PixelFormat) -> Self {
        match format.chroma {
            ChromaSubsampling::Yuv420 => Self::SemiPlanar420,
            ChromaSubsampling::Yuv422 => Self::SemiPlanar422,
            ChromaSubsampling::Yuv444 => Self::Planar,
        }
    }

    pub(super) const fn planes(self) -> usize {
        match self {
            Self::SemiPlanar420 | Self::SemiPlanar422 => 2,
            Self::Planar => 3,
        }
    }

    pub(super) const fn rows(self, height: u32) -> usize {
        let height = height as usize;
        match self {
            Self::SemiPlanar420 => height + height.div_ceil(2),
            Self::SemiPlanar422 => 2 * height,
            Self::Planar => 3 * height,
        }
    }
}

pub(crate) const fn buffer_format(format: PixelFormat) -> Option<u32> {
    match (format.chroma, format.depth) {
        (ChromaSubsampling::Yuv420, BitDepth::Eight) => Some(ffi::BUFFER_FORMAT_NV12),
        (ChromaSubsampling::Yuv420, BitDepth::Ten) => Some(ffi::BUFFER_FORMAT_YUV420_10BIT),
        (ChromaSubsampling::Yuv422, BitDepth::Eight) => Some(ffi::BUFFER_FORMAT_NV16),
        (ChromaSubsampling::Yuv422, BitDepth::Ten) => Some(ffi::BUFFER_FORMAT_P210),
        (ChromaSubsampling::Yuv444, BitDepth::Eight) => Some(ffi::BUFFER_FORMAT_YUV444),
        (ChromaSubsampling::Yuv444, BitDepth::Ten) => Some(ffi::BUFFER_FORMAT_YUV444_10BIT),
        _ => None,
    }
}

pub(crate) const fn dma_buf_fourcc(format: PixelFormat) -> DrmFourcc {
    DrmFourcc::from_code(match (format.chroma, format.depth) {
        (ChromaSubsampling::Yuv420, BitDepth::Eight) => *b"NV12",
        (ChromaSubsampling::Yuv420, _) => *b"P010",
        (ChromaSubsampling::Yuv422, BitDepth::Eight) => *b"NV16",
        (ChromaSubsampling::Yuv422, _) => *b"P210",
        (ChromaSubsampling::Yuv444, BitDepth::Eight) => *b"YU24",
        (ChromaSubsampling::Yuv444, _) => *b"Q410",
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DmaBufLayout {
    pub base: u64,
    pub pitch: u32,
    pub len: u64,
}

pub(crate) fn dma_buf_layout(
    format: PixelFormat,
    resolution: Resolution,
    planes: &[Plane],
) -> Result<DmaBufLayout, EncodeError> {
    let layout = Layout::of(format);
    let Some(first) = planes.first().filter(|_| planes.len() == layout.planes()) else {
        return Err(EncodeError::DmaBufPlaneCount {
            fourcc: dma_buf_fourcc(format),
            expected: layout.planes(),
            actual: planes.len(),
        });
    };

    let pitch = first.stride;
    if pitch % PITCH_ALIGNMENT != 0 {
        return Err(EncodeError::DmaBufStrideUnaligned {
            stride: pitch,
            alignment: PITCH_ALIGNMENT,
        });
    }
    let minimum = resolution.width as usize * bytes_per_sample(format.depth);
    if (pitch as usize) < minimum {
        return Err(EncodeError::FrameStrideTooSmall {
            minimum,
            actual: pitch as usize,
        });
    }

    let base = u64::from(first.offset);
    let plane_bytes = u64::from(pitch) * u64::from(resolution.height);
    for (index, plane) in planes.iter().enumerate().skip(1) {
        if plane.stride != pitch {
            return Err(EncodeError::DmaBufPlaneStride {
                backend: BACKEND,
                plane: index,
                expected: pitch,
                actual: plane.stride,
            });
        }
        let expected = base + index as u64 * plane_bytes;
        if u64::from(plane.offset) != expected {
            return Err(EncodeError::DmaBufPlaneOffset {
                backend: BACKEND,
                plane: index,
                expected,
                actual: plane.offset,
            });
        }
    }

    let len = base + layout.rows(resolution.height) as u64 * u64::from(pitch);

    Ok(DmaBufLayout { base, pitch, len })
}

fn sample(plane: &YuvPlane<'_>, wide: bool, x: usize, y: usize) -> u16 {
    if wide {
        let at = y * plane.stride + 2 * x;
        u16::from_le_bytes([plane.data[at], plane.data[at + 1]])
    } else {
        u16::from(plane.data[y * plane.stride + x])
    }
}

fn put(data: &mut [u8], at: usize, wide: bool, value: u16, shift: u32) {
    if wide {
        data[at..at + 2].copy_from_slice(&(value << shift).to_le_bytes());
    } else {
        data[at] = u8::try_from(value).unwrap_or(u8::MAX);
    }
}

fn copy_plane(plane: &YuvPlane<'_>, data: &mut [u8], start: usize, pitch: usize, wide: bool) {
    let shift = if wide { 16 - 10 } else { 0 };
    let sample_bytes = if wide { 2 } else { 1 };

    for y in 0..plane.rows {
        let row = start + y * pitch;
        for x in 0..plane.width {
            put(
                data,
                row + sample_bytes * x,
                wide,
                sample(plane, wide, x, y),
                shift,
            );
        }
    }
}

pub(super) fn pack(view: &YuvFrame<'_>, data: &mut [u8], pitch: usize) {
    let wide = view.format.depth != BitDepth::Eight;
    let [luma, cb, cr] = view.planes;
    let height = luma.rows;

    copy_plane(&luma, data, 0, pitch, wide);

    match Layout::of(view.format) {
        Layout::Planar => {
            copy_plane(&cb, data, pitch * height, pitch, wide);
            copy_plane(&cr, data, 2 * pitch * height, pitch, wide);
        }
        Layout::SemiPlanar420 | Layout::SemiPlanar422 => {
            let shift = if wide { 16 - 10 } else { 0 };
            let sample_bytes = if wide { 2 } else { 1 };
            for y in 0..cb.rows {
                let row = pitch * height + y * pitch;
                for x in 0..cb.width {
                    let at = row + 2 * sample_bytes * x;
                    put(data, at, wide, sample(&cb, wide, x, y), shift);
                    put(
                        data,
                        at + sample_bytes,
                        wide,
                        sample(&cr, wide, x, y),
                        shift,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rusplay_core::Timestamp;

    use super::*;
    use crate::input::fixtures::yuv_frame;

    const SIZE: Resolution = Resolution::new(6, 4);

    fn format(chroma: ChromaSubsampling, depth: BitDepth) -> PixelFormat {
        PixelFormat { chroma, depth }
    }

    fn plane(offset: u32, stride: u32) -> Plane {
        Plane { offset, stride }
    }

    #[test]
    fn formats_map_to_nvenc_buffer_formats() {
        let nv12 = format(ChromaSubsampling::Yuv420, BitDepth::Eight);
        let p010 = format(ChromaSubsampling::Yuv420, BitDepth::Ten);
        let yuv444 = format(ChromaSubsampling::Yuv444, BitDepth::Eight);
        let yuv444_10 = format(ChromaSubsampling::Yuv444, BitDepth::Ten);
        let nv16 = format(ChromaSubsampling::Yuv422, BitDepth::Eight);
        let p210 = format(ChromaSubsampling::Yuv422, BitDepth::Ten);

        assert_eq!(buffer_format(nv12), Some(ffi::BUFFER_FORMAT_NV12));
        assert_eq!(buffer_format(nv16), Some(ffi::BUFFER_FORMAT_NV16));
        assert_eq!(buffer_format(p210), Some(ffi::BUFFER_FORMAT_P210));
        assert_eq!(buffer_format(p010), Some(ffi::BUFFER_FORMAT_YUV420_10BIT));
        assert_eq!(buffer_format(yuv444), Some(ffi::BUFFER_FORMAT_YUV444));
        assert_eq!(
            buffer_format(yuv444_10),
            Some(ffi::BUFFER_FORMAT_YUV444_10BIT)
        );
        for chroma in [
            ChromaSubsampling::Yuv420,
            ChromaSubsampling::Yuv422,
            ChromaSubsampling::Yuv444,
        ] {
            assert_eq!(buffer_format(format(chroma, BitDepth::Twelve)), None);
        }

        assert_eq!(dma_buf_fourcc(nv12), DrmFourcc::from_code(*b"NV12"));
        assert_eq!(dma_buf_fourcc(p010), DrmFourcc::from_code(*b"P010"));
        assert_eq!(dma_buf_fourcc(yuv444), DrmFourcc::from_code(*b"YU24"));
        assert_eq!(dma_buf_fourcc(yuv444_10), DrmFourcc::from_code(*b"Q410"));
        assert_eq!(dma_buf_fourcc(nv16), DrmFourcc::from_code(*b"NV16"));
        assert_eq!(dma_buf_fourcc(p210), DrmFourcc::from_code(*b"P210"));
    }

    #[test]
    fn semi_planar_422_carries_a_full_height_chroma_plane() {
        let nv16 = format(ChromaSubsampling::Yuv422, BitDepth::Eight);
        let frame = yuv_frame(nv16, SIZE, 4, Timestamp::from_micros(0));
        let view = YuvFrame::from_frame(&frame, BACKEND, nv16).unwrap();
        let pitch = 8;
        let rows = Layout::of(nv16).rows(SIZE.height);
        let mut data = vec![0xee; pitch * rows];

        pack(&view, &mut data, pitch);

        let [_, cb, cr] = view.planes;
        let chroma = pitch * 4;
        assert_eq!(rows, 8);
        assert_eq!(cb.rows, 4);
        assert_eq!(data[chroma], cb.data[0]);
        assert_eq!(data[chroma + 1], cr.data[0]);
        assert_eq!(data[chroma + 3 * pitch + 4], cb.data[3 * cb.stride + 2]);
        assert_eq!(data[chroma + 3 * pitch + 5], cr.data[3 * cr.stride + 2]);
        assert_eq!(&data[chroma + 3 * pitch + 6..], &[0xee, 0xee]);
    }

    #[test]
    fn p210_moves_ten_bit_chroma_to_the_most_significant_bits() {
        let p210 = format(ChromaSubsampling::Yuv422, BitDepth::Ten);
        let frame = yuv_frame(p210, SIZE, 5, Timestamp::from_micros(0));
        let view = YuvFrame::from_frame(&frame, BACKEND, p210).unwrap();
        let pitch = 12;
        let mut data = vec![0; pitch * Layout::of(p210).rows(SIZE.height)];

        pack(&view, &mut data, pitch);

        let [_, cb, cr] = view.planes;
        let packed = |at: usize| u16::from_le_bytes([data[at], data[at + 1]]);
        let chroma = pitch * 4;
        assert_eq!(data.len(), pitch * 8);
        assert_eq!(packed(chroma), sample(&cb, true, 0, 0) << 6);
        assert_eq!(packed(chroma + 2), sample(&cr, true, 0, 0) << 6);
        assert_eq!(packed(chroma + 3 * pitch + 8), sample(&cb, true, 2, 3) << 6);
        assert_eq!(
            packed(chroma + 3 * pitch + 10),
            sample(&cr, true, 2, 3) << 6
        );
    }

    #[test]
    fn nv12_interleaves_chroma_after_the_luma_rows() {
        let nv12 = format(ChromaSubsampling::Yuv420, BitDepth::Eight);
        let frame = yuv_frame(nv12, SIZE, 1, Timestamp::from_micros(0));
        let view = YuvFrame::from_frame(&frame, BACKEND, nv12).unwrap();
        let pitch = 8;
        let mut data = vec![0xee; pitch * Layout::of(nv12).rows(SIZE.height)];

        pack(&view, &mut data, pitch);

        let [luma, cb, cr] = view.planes;
        assert_eq!(&data[..6], &luma.data[..6]);
        assert_eq!(&data[6..8], &[0xee, 0xee]);
        assert_eq!(
            &data[pitch..pitch + 6],
            &luma.data[luma.stride..luma.stride + 6]
        );
        let chroma = pitch * 4;
        assert_eq!(data[chroma], cb.data[0]);
        assert_eq!(data[chroma + 1], cr.data[0]);
        assert_eq!(data[chroma + 4], cb.data[2]);
        assert_eq!(data[chroma + 5], cr.data[2]);
        assert_eq!(data[chroma + pitch], cb.data[cb.stride]);
    }

    #[test]
    fn ten_bit_samples_move_to_the_most_significant_bits() {
        let p010 = format(ChromaSubsampling::Yuv420, BitDepth::Ten);
        let frame = yuv_frame(p010, SIZE, 2, Timestamp::from_micros(0));
        let view = YuvFrame::from_frame(&frame, BACKEND, p010).unwrap();
        let pitch = 16;
        let mut data = vec![0; pitch * Layout::of(p010).rows(SIZE.height)];

        pack(&view, &mut data, pitch);

        let [luma, cb, cr] = view.planes;
        let source = |plane: &YuvPlane<'_>, x: usize| sample(plane, true, x, 0);
        let packed = |at: usize| u16::from_le_bytes([data[at], data[at + 1]]);
        assert_eq!(packed(0), source(&luma, 0) << 6);
        assert_eq!(packed(10), source(&luma, 5) << 6);
        assert_eq!(packed(pitch * 4), source(&cb, 0) << 6);
        assert_eq!(packed(pitch * 4 + 2), source(&cr, 0) << 6);
        assert_eq!(packed(pitch * 4 + 4), source(&cb, 1) << 6);
        assert_eq!(packed(0) & 0x3f, 0);
    }

    #[test]
    fn planar_444_stacks_three_full_planes() {
        let yuv444 = format(ChromaSubsampling::Yuv444, BitDepth::Ten);
        let frame = yuv_frame(yuv444, SIZE, 3, Timestamp::from_micros(0));
        let view = YuvFrame::from_frame(&frame, BACKEND, yuv444).unwrap();
        let pitch = 12;
        let mut data = vec![0; pitch * Layout::of(yuv444).rows(SIZE.height)];

        pack(&view, &mut data, pitch);

        let [_, cb, cr] = view.planes;
        let packed = |at: usize| u16::from_le_bytes([data[at], data[at + 1]]);
        assert_eq!(packed(pitch * 4), sample(&cb, true, 0, 0) << 6);
        assert_eq!(packed(pitch * 8 + 2), sample(&cr, true, 1, 0) << 6);
        assert_eq!(packed(pitch * 11 + 10), sample(&cr, true, 5, 3) << 6);
    }

    #[test]
    fn contiguous_dma_bufs_are_accepted() {
        let nv12 = format(ChromaSubsampling::Yuv420, BitDepth::Eight);
        let yuv444 = format(ChromaSubsampling::Yuv444, BitDepth::Eight);

        let semi = dma_buf_layout(nv12, SIZE, &[plane(64, 8), plane(64 + 32, 8)]).unwrap();
        let planar =
            dma_buf_layout(yuv444, SIZE, &[plane(0, 8), plane(32, 8), plane(64, 8)]).unwrap();

        assert_eq!(
            semi,
            DmaBufLayout {
                base: 64,
                pitch: 8,
                len: 64 + 6 * 8
            }
        );
        assert_eq!(planar.len, 96);

        let p210 = format(ChromaSubsampling::Yuv422, BitDepth::Ten);
        let semi_422 = dma_buf_layout(p210, SIZE, &[plane(0, 12), plane(48, 12)]).unwrap();

        assert_eq!(
            semi_422,
            DmaBufLayout {
                base: 0,
                pitch: 12,
                len: 8 * 12
            }
        );
    }

    #[test]
    fn dma_bufs_nvenc_cannot_address_are_rejected() {
        let nv12 = format(ChromaSubsampling::Yuv420, BitDepth::Eight);

        assert!(matches!(
            dma_buf_layout(nv12, SIZE, &[plane(0, 8)]),
            Err(EncodeError::DmaBufPlaneCount {
                expected: 2,
                actual: 1,
                ..
            })
        ));
        assert!(matches!(
            dma_buf_layout(nv12, SIZE, &[plane(0, 8), plane(40, 8)]),
            Err(EncodeError::DmaBufPlaneOffset {
                plane: 1,
                expected: 32,
                actual: 40,
                ..
            })
        ));
        assert!(matches!(
            dma_buf_layout(nv12, SIZE, &[plane(0, 8), plane(32, 16)]),
            Err(EncodeError::DmaBufPlaneStride {
                plane: 1,
                expected: 8,
                actual: 16,
                ..
            })
        ));
        assert!(matches!(
            dma_buf_layout(nv12, SIZE, &[plane(0, 6), plane(24, 6)]),
            Err(EncodeError::DmaBufStrideUnaligned { stride: 6, .. })
        ));
        assert!(matches!(
            dma_buf_layout(
                format(ChromaSubsampling::Yuv420, BitDepth::Ten),
                SIZE,
                &[plane(0, 8), plane(32, 8)]
            ),
            Err(EncodeError::FrameStrideTooSmall {
                minimum: 12,
                actual: 8
            })
        ));
    }
}
