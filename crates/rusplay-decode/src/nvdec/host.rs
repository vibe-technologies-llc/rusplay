use std::{
    mem,
    sync::mpsc::{self, Receiver, Sender},
};

use rusplay_core::{BitDepth, ChromaSubsampling, Resolution};

use crate::{CpuFrame, CpuPlane, frame::PlaneStorage, nvdec::format::StreamFormat};

const PLANES: usize = 3;
const SAMPLE_BITS: u32 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PlaneShape {
    pub offset: usize,
    pub stride: usize,
    pub width: u32,
    pub height: u32,
}

impl PlaneShape {
    pub(super) fn row_bytes(&self, bytes_per_sample: usize) -> usize {
        self.width as usize * bytes_per_sample
    }

    fn len(&self) -> usize {
        self.stride * self.height as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HostLayout {
    pub planes: [PlaneShape; PLANES],
    pub bytes_per_sample: usize,
    pub shift: u32,
    pub size: usize,
}

impl HostLayout {
    pub(super) fn of(format: &StreamFormat) -> Self {
        let Resolution { width, height } = format.display;
        let (chroma_width, chroma_height) = match format.pixel_format.chroma {
            ChromaSubsampling::Yuv420 => (width.div_ceil(2), height.div_ceil(2)),
            ChromaSubsampling::Yuv422 => (width.div_ceil(2), height),
            ChromaSubsampling::Yuv444 => (width, height),
        };
        let bytes_per_sample = format.surface.bytes_per_sample();
        let shift = match format.pixel_format.depth {
            BitDepth::Eight => 0,
            depth => SAMPLE_BITS - u32::from(depth.bits()),
        };

        let mut offset = 0;
        let planes = [
            (width, height),
            (chroma_width, chroma_height),
            (chroma_width, chroma_height),
        ]
        .map(|(width, height)| {
            let stride = width as usize * bytes_per_sample;
            let plane = PlaneShape {
                offset,
                stride,
                width,
                height,
            };
            offset += plane.len();
            plane
        });

        Self {
            planes,
            bytes_per_sample,
            shift,
            size: offset,
        }
    }

    pub(super) fn interleaved_chroma_len(&self) -> usize {
        self.planes[1].len() * 2
    }
}

pub(super) fn deinterleave(
    interleaved: &[u8],
    bytes_per_sample: usize,
    u: &mut [u8],
    v: &mut [u8],
) {
    let pairs = interleaved.chunks_exact(bytes_per_sample * 2);
    let us = u.chunks_exact_mut(bytes_per_sample);
    let vs = v.chunks_exact_mut(bytes_per_sample);

    for ((pair, u), v) in pairs.zip(us).zip(vs) {
        let (first, second) = pair.split_at(bytes_per_sample);
        u.copy_from_slice(first);
        v.copy_from_slice(second);
    }
}

pub(super) fn shift_to_low_bits(samples: &mut [u8], shift: u32) {
    if shift == 0 {
        return;
    }

    for sample in samples.as_chunks_mut::<2>().0 {
        *sample = (u16::from_le_bytes(*sample) >> shift).to_le_bytes();
    }
}

#[derive(Debug)]
pub(super) struct BufferPool {
    recycle: Sender<Vec<u8>>,
    returned: Receiver<Vec<u8>>,
}

impl BufferPool {
    pub(super) fn new() -> Self {
        let (recycle, returned) = mpsc::channel();

        Self { recycle, returned }
    }

    pub(super) fn take(&self, len: usize) -> PooledBuffer {
        let reused = self.returned.try_iter().find(|buffer| buffer.len() == len);

        PooledBuffer {
            data: reused.unwrap_or_else(|| vec![0; len]),
            recycle: self.recycle.clone(),
        }
    }
}

#[derive(Debug)]
pub(super) struct PooledBuffer {
    data: Vec<u8>,
    recycle: Sender<Vec<u8>>,
}

impl PooledBuffer {
    pub(super) fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

impl Drop for PooledBuffer {
    fn drop(&mut self) {
        let _ = self.recycle.send(mem::take(&mut self.data));
    }
}

#[derive(Debug)]
pub(super) struct HostFrame {
    buffer: PooledBuffer,
    layout: HostLayout,
}

impl HostFrame {
    pub(super) fn new(buffer: PooledBuffer, layout: HostLayout) -> Self {
        Self { buffer, layout }
    }

    pub(super) fn into_cpu(self) -> CpuFrame {
        CpuFrame::new(self)
    }
}

impl PlaneStorage for HostFrame {
    fn plane_count(&self) -> usize {
        PLANES
    }

    fn plane(&self, index: usize) -> Option<CpuPlane<'_>> {
        let shape = self.layout.planes.get(index)?;
        let data = self
            .buffer
            .data
            .get(shape.offset..shape.offset + shape.len())?;

        Some(CpuPlane {
            data,
            stride: shape.stride,
            width: shape.width,
            height: shape.height,
            bytes_per_sample: u8::try_from(self.layout.bytes_per_sample).ok()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use rusplay_core::{Codec, PixelFormat};

    use super::*;
    use crate::nvdec::{
        ffi::{self, IntRect, VideoFormat},
        format::StreamFormat,
    };

    fn stream(chroma_format: i32, bits_minus8: u8, width: u32, height: u32) -> StreamFormat {
        let format = VideoFormat {
            bit_depth_luma_minus8: bits_minus8,
            bit_depth_chroma_minus8: bits_minus8,
            coded_width: width.next_multiple_of(16),
            coded_height: height.next_multiple_of(16),
            display_area: IntRect {
                left: 0,
                top: 0,
                right: i32::try_from(width).unwrap(),
                bottom: i32::try_from(height).unwrap(),
            },
            chroma_format,
            ..VideoFormat::default()
        };

        StreamFormat::of(Codec::Hevc, &format).unwrap()
    }

    #[test]
    fn a_420_frame_is_three_tightly_packed_planes() {
        let layout = HostLayout::of(&stream(ffi::CHROMA_FORMAT_420, 0, 255, 145));

        assert_eq!(layout.bytes_per_sample, 1);
        assert_eq!(layout.shift, 0);
        assert_eq!(
            layout
                .planes
                .map(|plane| (plane.offset, plane.stride, plane.width, plane.height)),
            [
                (0, 255, 255, 145),
                (255 * 145, 128, 128, 73),
                (255 * 145 + 128 * 73, 128, 128, 73),
            ]
        );
        assert_eq!(layout.size, 255 * 145 + 2 * 128 * 73);
        assert_eq!(layout.interleaved_chroma_len(), 2 * 128 * 73);
    }

    #[test]
    fn high_bit_depth_layouts_use_two_bytes_and_shift_to_the_low_bits() {
        let ten = HostLayout::of(&stream(ffi::CHROMA_FORMAT_422, 2, 64, 32));
        let twelve = HostLayout::of(&stream(ffi::CHROMA_FORMAT_444, 4, 64, 32));

        assert_eq!(ten.bytes_per_sample, 2);
        assert_eq!(ten.shift, 6);
        assert_eq!(ten.planes[1].width, 32);
        assert_eq!(ten.planes[1].height, 32);
        assert_eq!(ten.planes[1].stride, 64);
        assert_eq!(twelve.shift, 4);
        assert_eq!(twelve.planes[2].width, 64);
        assert_eq!(twelve.planes[2].stride, 128);
    }

    #[test]
    fn interleaved_chroma_splits_into_u_and_v() {
        let eight = [1, 2, 3, 4, 5, 6];
        let mut first_eight = [0; 3];
        let mut second_eight = [0; 3];
        let sixteen = [0x40, 0x00, 0x80, 0x00, 0xc0, 0xff, 0xff, 0xff];
        let mut first_wide = [0; 4];
        let mut second_wide = [0; 4];

        deinterleave(&eight, 1, &mut first_eight, &mut second_eight);
        deinterleave(&sixteen, 2, &mut first_wide, &mut second_wide);

        assert_eq!(first_eight, [1, 3, 5]);
        assert_eq!(second_eight, [2, 4, 6]);
        assert_eq!(first_wide, [0x40, 0x00, 0xc0, 0xff]);
        assert_eq!(second_wide, [0x80, 0x00, 0xff, 0xff]);
    }

    #[test]
    fn msb_aligned_samples_move_to_the_low_bits() {
        let mut ten = Vec::new();
        for value in [0_u16, 64 << 6, 1023 << 6] {
            ten.extend_from_slice(&value.to_le_bytes());
        }
        let mut twelve = (4095_u16 << 4).to_le_bytes().to_vec();

        shift_to_low_bits(&mut ten, 6);
        shift_to_low_bits(&mut twelve, 4);

        assert_eq!(ten, [0, 0, 64, 0, 0xff, 0x03]);
        assert_eq!(twelve, 4095_u16.to_le_bytes());
    }

    #[test]
    fn a_host_frame_exposes_its_planes_and_recycles_its_buffer() {
        let format = stream(ffi::CHROMA_FORMAT_420, 2, 32, 16);
        let layout = HostLayout::of(&format);
        let pool = BufferPool::new();

        let frame = HostFrame::new(pool.take(layout.size), layout).into_cpu();
        let planes: Vec<_> = frame
            .planes()
            .map(|plane| {
                (
                    plane.width,
                    plane.height,
                    plane.stride,
                    plane.bytes_per_sample,
                )
            })
            .collect();
        drop(frame);
        let reused = pool.take(layout.size);

        assert_eq!(
            format.pixel_format,
            PixelFormat {
                chroma: ChromaSubsampling::Yuv420,
                depth: BitDepth::Ten,
            }
        );
        assert_eq!(planes, [(32, 16, 64, 2), (16, 8, 32, 2), (16, 8, 32, 2)]);
        assert_eq!(reused.data.len(), layout.size);
        assert!(pool.returned.try_recv().is_err());
    }
}
