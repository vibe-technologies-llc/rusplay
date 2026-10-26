use rusplay_capture::DrmFourcc;
use rusplay_core::{
    BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, MatrixCoefficients, PixelFormat,
    TransferFunction,
};

use crate::{
    input::YuvFrame,
    vaapi::{
        ffi,
        va::{ImageLayout, SurfaceFormat},
    },
};

const fn code(bytes: [u8; 4]) -> u32 {
    u32::from_le_bytes(bytes)
}

pub(super) const fn rt_format(format: PixelFormat) -> u32 {
    match (format.chroma, format.depth) {
        (ChromaSubsampling::Yuv420, BitDepth::Eight) => ffi::VA_RT_FORMAT_YUV420,
        (ChromaSubsampling::Yuv420, BitDepth::Ten) => ffi::VA_RT_FORMAT_YUV420_10,
        (ChromaSubsampling::Yuv420, BitDepth::Twelve) => ffi::VA_RT_FORMAT_YUV420_12,
        (ChromaSubsampling::Yuv422, BitDepth::Eight) => ffi::VA_RT_FORMAT_YUV422,
        (ChromaSubsampling::Yuv422, BitDepth::Ten) => ffi::VA_RT_FORMAT_YUV422_10,
        (ChromaSubsampling::Yuv422, BitDepth::Twelve) => ffi::VA_RT_FORMAT_YUV422_12,
        (ChromaSubsampling::Yuv444, BitDepth::Eight) => ffi::VA_RT_FORMAT_YUV444,
        (ChromaSubsampling::Yuv444, BitDepth::Ten) => ffi::VA_RT_FORMAT_YUV444_10,
        (ChromaSubsampling::Yuv444, BitDepth::Twelve) => ffi::VA_RT_FORMAT_YUV444_12,
    }
}

pub(super) const fn surface_format(format: PixelFormat) -> SurfaceFormat {
    let (va, drm) = match (format.chroma, format.depth) {
        (ChromaSubsampling::Yuv420, BitDepth::Eight) => (*b"NV12", *b"NV12"),
        (ChromaSubsampling::Yuv420, BitDepth::Ten) => (*b"P010", *b"P010"),
        (ChromaSubsampling::Yuv420, BitDepth::Twelve) => (*b"P012", *b"P012"),
        (ChromaSubsampling::Yuv422, BitDepth::Eight) => (*b"YUY2", *b"YUYV"),
        (ChromaSubsampling::Yuv422, BitDepth::Ten) => (*b"Y210", *b"Y210"),
        (ChromaSubsampling::Yuv422, BitDepth::Twelve) => (*b"Y212", *b"Y212"),
        (ChromaSubsampling::Yuv444, BitDepth::Eight) => (*b"AYUV", *b"AYUV"),
        (ChromaSubsampling::Yuv444, BitDepth::Ten) => (*b"Y410", *b"Y410"),
        (ChromaSubsampling::Yuv444, BitDepth::Twelve) => (*b"Y412", *b"Y412"),
    };

    SurfaceFormat {
        rt_format: rt_format(format),
        va_fourcc: code(va),
        drm_fourcc: code(drm),
    }
}

pub(crate) const fn dma_buf_fourcc(format: PixelFormat) -> DrmFourcc {
    DrmFourcc(surface_format(format).drm_fourcc)
}

pub(crate) const fn dma_buf_planes(format: PixelFormat) -> usize {
    match format.chroma {
        ChromaSubsampling::Yuv420 => 2,
        ChromaSubsampling::Yuv422 | ChromaSubsampling::Yuv444 => 1,
    }
}

pub(super) const fn coded_bytes_per_pixel_x2(format: PixelFormat) -> u64 {
    let samples_x2 = match format.chroma {
        ChromaSubsampling::Yuv420 => 3,
        ChromaSubsampling::Yuv422 => 4,
        ChromaSubsampling::Yuv444 => 6,
    };
    let bytes = match format.depth {
        BitDepth::Eight => 1,
        BitDepth::Ten | BitDepth::Twelve => 2,
    };

    samples_x2 * bytes
}

const UNSPECIFIED_PRIMARIES: u8 = 2;

pub(super) const fn colour_primaries(primaries: ColorPrimaries) -> u8 {
    match primaries.h273_code() {
        Some(code) => code,
        None => UNSPECIFIED_PRIMARIES,
    }
}

pub(super) const fn matrix_coefficients(matrix: MatrixCoefficients) -> u8 {
    matrix.h273_code()
}

pub(super) const fn transfer_characteristics(transfer: TransferFunction) -> u8 {
    transfer.h273_code()
}

pub(super) const fn full_range(range: ColorRange) -> bool {
    matches!(range, ColorRange::Full)
}

fn sample(plane: &[u8], stride: usize, wide: bool, x: usize, y: usize) -> u16 {
    if wide {
        let at = y * stride + 2 * x;
        u16::from_le_bytes([plane[at], plane[at + 1]])
    } else {
        u16::from(plane[y * stride + x])
    }
}

fn put_u16(data: &mut [u8], at: usize, value: u16) {
    data[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

pub(super) fn pack(view: &YuvFrame<'_>, data: &mut [u8], layout: ImageLayout) {
    let format = view.format;
    let wide = format.depth != BitDepth::Eight;
    let shift = 16 - u32::from(format.depth.bits());
    let [luma, cb, cr] = view.planes;
    let width = luma.width;
    let rows = luma.rows;
    let get = |plane: &crate::input::YuvPlane<'_>, x: usize, y: usize| {
        sample(plane.data, plane.stride, wide, x, y)
    };

    match format.chroma {
        ChromaSubsampling::Yuv420 => {
            for y in 0..rows {
                let row = layout.offsets[0] + y * layout.pitches[0];
                for x in 0..width {
                    let value = get(&luma, x, y);
                    if wide {
                        put_u16(data, row + 2 * x, value << shift);
                    } else {
                        data[row + x] = u8::try_from(value).unwrap_or(u8::MAX);
                    }
                }
            }
            for y in 0..cb.rows {
                let row = layout.offsets[1] + y * layout.pitches[1];
                for x in 0..cb.width {
                    let (u, v) = (get(&cb, x, y), get(&cr, x, y));
                    if wide {
                        put_u16(data, row + 4 * x, u << shift);
                        put_u16(data, row + 4 * x + 2, v << shift);
                    } else {
                        data[row + 2 * x] = u8::try_from(u).unwrap_or(u8::MAX);
                        data[row + 2 * x + 1] = u8::try_from(v).unwrap_or(u8::MAX);
                    }
                }
            }
        }
        ChromaSubsampling::Yuv422 => {
            for y in 0..rows {
                let row = layout.offsets[0] + y * layout.pitches[0];
                for pair in 0..cb.width {
                    let left = get(&luma, (2 * pair).min(width - 1), y);
                    let right = get(&luma, (2 * pair + 1).min(width - 1), y);
                    let quad = [left, get(&cb, pair, y), right, get(&cr, pair, y)];
                    for (index, value) in quad.into_iter().enumerate() {
                        if wide {
                            put_u16(data, row + 8 * pair + 2 * index, value << shift);
                        } else {
                            data[row + 4 * pair + index] = u8::try_from(value).unwrap_or(u8::MAX);
                        }
                    }
                }
            }
        }
        ChromaSubsampling::Yuv444 => {
            for y in 0..rows {
                let row = layout.offsets[0] + y * layout.pitches[0];
                for x in 0..width {
                    let (luma, u, v) = (get(&luma, x, y), get(&cb, x, y), get(&cr, x, y));
                    match format.depth {
                        BitDepth::Eight => {
                            let bytes = [v, u, luma, 0xff]
                                .map(|value| u8::try_from(value).unwrap_or(u8::MAX));
                            data[row + 4 * x..row + 4 * x + 4].copy_from_slice(&bytes);
                        }
                        BitDepth::Ten => {
                            let word = (3_u32 << 30)
                                | (u32::from(v) << 20)
                                | (u32::from(luma) << 10)
                                | u32::from(u);
                            data[row + 4 * x..row + 4 * x + 4].copy_from_slice(&word.to_le_bytes());
                        }
                        BitDepth::Twelve => {
                            for (index, value) in [u, luma, v, 0x0fff].into_iter().enumerate() {
                                put_u16(data, row + 8 * x + 2 * index, value << shift);
                            }
                        }
                    }
                }
            }
        }
    }
}
