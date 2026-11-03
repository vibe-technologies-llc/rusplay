use std::time::Instant;

use rusplay_core::{BitDepth, ChromaSubsampling, PixelFormat, Resolution};

use super::pack;
use crate::{
    input::{YuvFrame, YuvPlane},
    vaapi::va::ImageLayout,
};

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

fn reference_pack(view: &YuvFrame<'_>, data: &mut [u8], layout: ImageLayout, padded: Resolution) {
    let format = view.format;
    let wide = format.depth != BitDepth::Eight;
    let shift = 16 - u32::from(format.depth.bits());
    let [luma, cb, cr] = view.planes;
    let width = padded.width as usize;
    let rows = padded.height as usize;
    let chroma_width = width.div_ceil(2);
    let chroma_rows = rows.div_ceil(2);
    let get = |plane: &YuvPlane<'_>, x: usize, y: usize| {
        let x = x.min(plane.width.saturating_sub(1));
        let y = y.min(plane.rows.saturating_sub(1));
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
            for y in 0..chroma_rows {
                let row = layout.offsets[1] + y * layout.pitches[1];
                for x in 0..chroma_width {
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
                for pair in 0..chroma_width {
                    let left = get(&luma, 2 * pair, y);
                    let right = get(&luma, 2 * pair + 1, y);
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

struct Source {
    format: PixelFormat,
    resolution: Resolution,
    buffers: [Vec<u8>; 3],
    strides: [usize; 3],
    widths: [usize; 3],
    rows: [usize; 3],
}

impl Source {
    fn new(format: PixelFormat, resolution: Resolution, slack: usize, seed: u64) -> Self {
        let sample = if format.depth == BitDepth::Eight {
            1
        } else {
            2
        };
        let (shift_x, shift_y) = match format.chroma {
            ChromaSubsampling::Yuv420 => (1, 1),
            ChromaSubsampling::Yuv422 => (1, 0),
            ChromaSubsampling::Yuv444 => (0, 0),
        };
        let luma_width = resolution.width as usize;
        let luma_rows = resolution.height as usize;
        let chroma_width = luma_width.div_ceil(1 << shift_x);
        let chroma_rows = luma_rows.div_ceil(1 << shift_y);
        let widths = [luma_width, chroma_width, chroma_width];
        let rows = [luma_rows, chroma_rows, chroma_rows];
        let strides = widths.map(|width| width * sample + slack);
        let mut state = seed | 1;
        let buffers = [0, 1, 2].map(|plane| {
            (0..strides[plane] * rows[plane])
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    state.to_le_bytes()[3]
                })
                .collect()
        });

        Self {
            format,
            resolution,
            buffers,
            strides,
            widths,
            rows,
        }
    }

    fn view(&self) -> YuvFrame<'_> {
        let plane = |index: usize| YuvPlane {
            data: &self.buffers[index],
            stride: self.strides[index],
            width: self.widths[index],
            rows: self.rows[index],
        };

        YuvFrame {
            format: self.format,
            resolution: self.resolution,
            planes: [plane(0), plane(1), plane(2)],
        }
    }
}

fn image_layout(format: PixelFormat, padded: Resolution, slack: usize) -> (ImageLayout, usize) {
    let sample = if format.depth == BitDepth::Eight {
        1
    } else {
        2
    };
    let width = padded.width as usize;
    let rows = padded.height as usize;
    let chroma_width = width.div_ceil(2);
    let chroma_rows = rows.div_ceil(2);
    let base = 7;

    match format.chroma {
        ChromaSubsampling::Yuv420 => {
            let luma_pitch = width * sample + slack;
            let chroma_pitch = chroma_width * 2 * sample + slack;
            let second = base + luma_pitch * rows + 3;
            let layout = ImageLayout {
                offsets: [base, second, 0],
                pitches: [luma_pitch, chroma_pitch, 0],
            };
            (layout, second + chroma_pitch * chroma_rows + 5)
        }
        ChromaSubsampling::Yuv422 => {
            let pitch = chroma_width * 4 * sample + slack;
            let layout = ImageLayout {
                offsets: [base, 0, 0],
                pitches: [pitch, 0, 0],
            };
            (layout, base + pitch * rows + 5)
        }
        ChromaSubsampling::Yuv444 => {
            let unit = if format.depth == BitDepth::Twelve {
                8
            } else {
                4
            };
            let pitch = width * unit + slack;
            let layout = ImageLayout {
                offsets: [base, 0, 0],
                pitches: [pitch, 0, 0],
            };
            (layout, base + pitch * rows + 5)
        }
    }
}

fn all_formats() -> Vec<PixelFormat> {
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

#[test]
fn packing_matches_the_reference_for_every_format_extent_and_stride() {
    let cases = [
        (Resolution::new(1, 1), Resolution::new(2, 2)),
        (Resolution::new(7, 5), Resolution::new(8, 6)),
        (Resolution::new(7, 5), Resolution::new(7, 5)),
        (Resolution::new(16, 4), Resolution::new(16, 4)),
        (Resolution::new(33, 9), Resolution::new(48, 16)),
        (Resolution::new(34, 8), Resolution::new(35, 11)),
        (Resolution::new(130, 3), Resolution::new(144, 4)),
    ];

    for format in all_formats() {
        for (resolution, padded) in cases {
            for slack in [0, 1, 6, 64] {
                let source = Source::new(format, resolution, slack, 0x9e37_79b9_7f4a_7c15);
                let (layout, size) = image_layout(format, padded, slack);
                let mut expected = vec![0xa5; size];
                let mut actual = vec![0xa5; size];

                reference_pack(&source.view(), &mut expected, layout, padded);
                pack(&source.view(), &mut actual, layout, padded);

                assert_eq!(
                    actual, expected,
                    "{format:?} {resolution} padded {padded} slack {slack}"
                );
            }
        }
    }
}

fn constant_source(format: PixelFormat, resolution: Resolution, samples: [u16; 3]) -> Source {
    let mut source = Source::new(format, resolution, 0, 1);
    for (buffer, value) in source.buffers.iter_mut().zip(samples) {
        if format.depth == BitDepth::Eight {
            buffer.fill(u8::try_from(value).unwrap());
        } else {
            for pair in buffer.as_chunks_mut::<2>().0 {
                pair.copy_from_slice(&value.to_le_bytes());
            }
        }
    }
    source
}

#[test]
fn eight_bit_422_packs_yuyv() {
    let format = PixelFormat {
        chroma: ChromaSubsampling::Yuv422,
        depth: BitDepth::Eight,
    };
    let resolution = Resolution::new(2, 1);
    let mut source = constant_source(format, resolution, [0, 0, 0]);
    source.buffers[0] = vec![0x10, 0x20];
    source.buffers[1] = vec![0x30];
    source.buffers[2] = vec![0x40];
    let (layout, size) = image_layout(format, resolution, 0);
    let mut data = vec![0; size];

    pack(&source.view(), &mut data, layout, resolution);

    assert_eq!(&data[layout.offsets[0]..][..4], &[0x10, 0x30, 0x20, 0x40]);
}

#[test]
fn eight_bit_444_packs_ayuv() {
    let format = PixelFormat {
        chroma: ChromaSubsampling::Yuv444,
        depth: BitDepth::Eight,
    };
    let resolution = Resolution::new(1, 1);
    let source = constant_source(format, resolution, [0x11, 0x22, 0x33]);
    let (layout, size) = image_layout(format, resolution, 0);
    let mut data = vec![0; size];

    pack(&source.view(), &mut data, layout, resolution);

    assert_eq!(&data[layout.offsets[0]..][..4], &[0x33, 0x22, 0x11, 0xff]);
}

#[test]
fn ten_bit_444_packs_y410_and_twelve_bit_shifts_into_the_high_bits() {
    let ten = PixelFormat {
        chroma: ChromaSubsampling::Yuv444,
        depth: BitDepth::Ten,
    };
    let twelve = PixelFormat {
        chroma: ChromaSubsampling::Yuv444,
        depth: BitDepth::Twelve,
    };
    let resolution = Resolution::new(1, 1);
    let (ten_layout, ten_size) = image_layout(ten, resolution, 0);
    let (twelve_layout, twelve_size) = image_layout(twelve, resolution, 0);
    let mut ten_data = vec![0; ten_size];
    let mut twelve_data = vec![0; twelve_size];

    pack(
        &constant_source(ten, resolution, [0x155, 0x2aa, 0x3ff]).view(),
        &mut ten_data,
        ten_layout,
        resolution,
    );
    pack(
        &constant_source(twelve, resolution, [0x123, 0x456, 0x789]).view(),
        &mut twelve_data,
        twelve_layout,
        resolution,
    );

    let word = (3_u32 << 30) | (0x3ff << 20) | (0x155 << 10) | 0x2aa;
    assert_eq!(&ten_data[ten_layout.offsets[0]..][..4], &word.to_le_bytes());
    let expected: Vec<u8> = [0x456_u16, 0x123, 0x789, 0x0fff]
        .into_iter()
        .flat_map(|value| (value << 4).to_le_bytes())
        .collect();
    assert_eq!(&twelve_data[twelve_layout.offsets[0]..][..8], &expected);
}

fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

#[test]
#[ignore = "measures the packer against the reference at 4K"]
fn pack_times() {
    let resolution = Resolution::new(3840, 2160);

    for chroma in [ChromaSubsampling::Yuv422, ChromaSubsampling::Yuv444] {
        for depth in [BitDepth::Eight, BitDepth::Ten, BitDepth::Twelve] {
            let format = PixelFormat { chroma, depth };
            let source = Source::new(format, resolution, 0, 7);
            let (layout, size) = image_layout(format, resolution, 0);
            let mut data = vec![0; size];
            let mut reference = Vec::new();
            let mut current = Vec::new();

            for _ in 0..15 {
                let started = Instant::now();
                reference_pack(&source.view(), &mut data, layout, resolution);
                reference.push(started.elapsed().as_secs_f64() * 1e3);

                let started = Instant::now();
                pack(&source.view(), &mut data, layout, resolution);
                current.push(started.elapsed().as_secs_f64() * 1e3);
            }

            eprintln!(
                "{format:?} 4K reference {:.2} ms, packer {:.2} ms",
                median(reference),
                median(current)
            );
        }
    }
}
