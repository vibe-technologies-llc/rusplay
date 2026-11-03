use rusplay_core::{
    BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, DrmFourcc, MatrixCoefficients,
    PixelFormat, Resolution, TransferFunction,
};

use crate::{
    input::{YuvFrame, YuvPlane},
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

trait Sample {
    const SIZE: usize;

    fn load(bytes: &[u8]) -> u16;

    fn store(value: u16, shift: u32, out: &mut [u8]);
}

struct Narrow;

struct Wide;

impl Sample for Narrow {
    const SIZE: usize = 1;

    fn load(bytes: &[u8]) -> u16 {
        u16::from(bytes[0])
    }

    fn store(value: u16, _shift: u32, out: &mut [u8]) {
        out[0] = u8::try_from(value).unwrap_or(u8::MAX);
    }
}

impl Sample for Wide {
    const SIZE: usize = 2;

    fn load(bytes: &[u8]) -> u16 {
        u16::from_le_bytes([bytes[0], bytes[1]])
    }

    fn store(value: u16, shift: u32, out: &mut [u8]) {
        out[..2].copy_from_slice(&(value << shift).to_le_bytes());
    }
}

#[derive(Clone, Copy)]
struct Row<'a> {
    bytes: &'a [u8],
}

impl<'a> Row<'a> {
    fn of<S: Sample>(plane: &YuvPlane<'a>, y: usize) -> Self {
        let y = y.min(plane.rows.saturating_sub(1));
        let start = y * plane.stride;

        Self {
            bytes: &plane.data[start..start + plane.width * S::SIZE],
        }
    }

    fn len<S: Sample>(self) -> usize {
        self.bytes.len() / S::SIZE
    }

    fn at<S: Sample>(self, x: usize) -> u16 {
        let x = x.min(self.len::<S>().saturating_sub(1));
        S::load(&self.bytes[x * S::SIZE..])
    }
}

fn store_all<S: Sample, const N: usize>(out: &mut [u8], values: [u16; N], shift: u32) {
    for (slot, value) in out.chunks_exact_mut(S::SIZE).zip(values) {
        S::store(value, shift, slot);
    }
}

fn store_y410(out: &mut [u8], [luma, u, v]: [u16; 3]) {
    let word = (3_u32 << 30) | (u32::from(v) << 20) | (u32::from(luma) << 10) | u32::from(u);
    out[..4].copy_from_slice(&word.to_le_bytes());
}

fn pack_luma<S: Sample>(out: &mut [u8], luma: Row<'_>, shift: u32) {
    let main = (out.len() / S::SIZE).min(luma.len::<S>());
    let (head, tail) = out.split_at_mut(main * S::SIZE);

    for (slot, source) in head
        .chunks_exact_mut(S::SIZE)
        .zip(luma.bytes.chunks_exact(S::SIZE))
    {
        S::store(S::load(source), shift, slot);
    }
    for (x, slot) in tail.chunks_exact_mut(S::SIZE).enumerate() {
        S::store(luma.at::<S>(main + x), shift, slot);
    }
}

fn pack_chroma_pairs<S: Sample>(out: &mut [u8], cb: Row<'_>, cr: Row<'_>, shift: u32) {
    let unit = 2 * S::SIZE;
    let main = (out.len() / unit).min(cb.len::<S>()).min(cr.len::<S>());
    let (head, tail) = out.split_at_mut(main * unit);

    for ((slot, u), v) in head
        .chunks_exact_mut(unit)
        .zip(cb.bytes.chunks_exact(S::SIZE))
        .zip(cr.bytes.chunks_exact(S::SIZE))
    {
        store_all::<S, 2>(slot, [S::load(u), S::load(v)], shift);
    }
    for (x, slot) in tail.chunks_exact_mut(unit).enumerate() {
        let x = main + x;
        store_all::<S, 2>(slot, [cb.at::<S>(x), cr.at::<S>(x)], shift);
    }
}

fn pack_pairs<S: Sample>(out: &mut [u8], luma: Row<'_>, cb: Row<'_>, cr: Row<'_>, shift: u32) {
    let unit = 4 * S::SIZE;
    let main = (out.len() / unit)
        .min(luma.len::<S>() / 2)
        .min(cb.len::<S>())
        .min(cr.len::<S>());
    let (head, tail) = out.split_at_mut(main * unit);

    for (((slot, y), u), v) in head
        .chunks_exact_mut(unit)
        .zip(luma.bytes.chunks_exact(2 * S::SIZE))
        .zip(cb.bytes.chunks_exact(S::SIZE))
        .zip(cr.bytes.chunks_exact(S::SIZE))
    {
        let (left, right) = y.split_at(S::SIZE);
        let quad = [S::load(left), S::load(u), S::load(right), S::load(v)];
        store_all::<S, 4>(slot, quad, shift);
    }
    for (pair, slot) in tail.chunks_exact_mut(unit).enumerate() {
        let pair = main + pair;
        let quad = [
            luma.at::<S>(2 * pair),
            cb.at::<S>(pair),
            luma.at::<S>(2 * pair + 1),
            cr.at::<S>(pair),
        ];
        store_all::<S, 4>(slot, quad, shift);
    }
}

fn pack_triples<S: Sample>(
    out: &mut [u8],
    luma: Row<'_>,
    cb: Row<'_>,
    cr: Row<'_>,
    unit: usize,
    store: impl Fn(&mut [u8], [u16; 3]),
) {
    let main = (out.len() / unit)
        .min(luma.len::<S>())
        .min(cb.len::<S>())
        .min(cr.len::<S>());
    let (head, tail) = out.split_at_mut(main * unit);

    for (((slot, y), u), v) in head
        .chunks_exact_mut(unit)
        .zip(luma.bytes.chunks_exact(S::SIZE))
        .zip(cb.bytes.chunks_exact(S::SIZE))
        .zip(cr.bytes.chunks_exact(S::SIZE))
    {
        store(slot, [S::load(y), S::load(u), S::load(v)]);
    }
    for (x, slot) in tail.chunks_exact_mut(unit).enumerate() {
        let x = main + x;
        store(slot, [luma.at::<S>(x), cb.at::<S>(x), cr.at::<S>(x)]);
    }
}

fn pack_rows<S: Sample>(
    view: &YuvFrame<'_>,
    data: &mut [u8],
    layout: ImageLayout,
    padded: Resolution,
) {
    let depth = view.format.depth;
    let shift = 16 - u32::from(depth.bits());
    let [luma, cb, cr] = &view.planes;
    let width = padded.width as usize;
    let rows = padded.height as usize;
    let chroma_width = width.div_ceil(2);
    let chroma_rows = rows.div_ceil(2);

    match view.format.chroma {
        ChromaSubsampling::Yuv420 => {
            for y in 0..rows {
                let start = layout.offsets[0] + y * layout.pitches[0];
                let out = &mut data[start..start + width * S::SIZE];
                pack_luma::<S>(out, Row::of::<S>(luma, y), shift);
            }
            for y in 0..chroma_rows {
                let start = layout.offsets[1] + y * layout.pitches[1];
                let out = &mut data[start..start + chroma_width * 2 * S::SIZE];
                let (u, v) = (Row::of::<S>(cb, y), Row::of::<S>(cr, y));
                pack_chroma_pairs::<S>(out, u, v, shift);
            }
        }
        ChromaSubsampling::Yuv422 => {
            for y in 0..rows {
                let start = layout.offsets[0] + y * layout.pitches[0];
                let out = &mut data[start..start + chroma_width * 4 * S::SIZE];
                let (l, u, v) = (
                    Row::of::<S>(luma, y),
                    Row::of::<S>(cb, y),
                    Row::of::<S>(cr, y),
                );
                pack_pairs::<S>(out, l, u, v, shift);
            }
        }
        ChromaSubsampling::Yuv444 => {
            let unit = match depth {
                BitDepth::Eight | BitDepth::Ten => 4,
                BitDepth::Twelve => 8,
            };
            for y in 0..rows {
                let start = layout.offsets[0] + y * layout.pitches[0];
                let out = &mut data[start..start + width * unit];
                let (l, u, v) = (
                    Row::of::<S>(luma, y),
                    Row::of::<S>(cb, y),
                    Row::of::<S>(cr, y),
                );
                match depth {
                    BitDepth::Eight => pack_triples::<S>(out, l, u, v, unit, |slot, [y, u, v]| {
                        store_all::<S, 4>(slot, [v, u, y, 0xff], shift);
                    }),
                    BitDepth::Ten => pack_triples::<S>(out, l, u, v, unit, |slot, values| {
                        store_y410(slot, values);
                    }),
                    BitDepth::Twelve => pack_triples::<S>(out, l, u, v, unit, |slot, [y, u, v]| {
                        store_all::<S, 4>(slot, [u, y, v, 0x0fff], shift);
                    }),
                }
            }
        }
    }
}

pub(super) fn pack(view: &YuvFrame<'_>, data: &mut [u8], layout: ImageLayout, padded: Resolution) {
    if view.format.depth == BitDepth::Eight {
        pack_rows::<Narrow>(view, data, layout, padded);
    } else {
        pack_rows::<Wide>(view, data, layout, padded);
    }
}

#[cfg(test)]
mod tests;
