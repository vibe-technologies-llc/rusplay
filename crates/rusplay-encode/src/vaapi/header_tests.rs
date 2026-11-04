mod av1;
mod h264;
mod hevc;
mod sei;

use rusplay_core::{
    BitDepth, ChromaSiting, ChromaSubsampling, ColorPrimaries, ColorRange, ColorSpace, FrameRate,
    HdrLevels, MatrixCoefficients, PixelFormat, Resolution, TransferFunction,
};

use crate::{
    hdr::fixtures::hdr10_color,
    vaapi::{Picture, StreamInfo},
};

const PEAK_KBPS: u32 = 20_000;
const BITS_PER_SECOND: u32 = 20_000_000;
const FIRST_SURFACE: u32 = 100;
const CODED_BUFFER: u32 = 7;
const START_CODE: [u8; 4] = [0, 0, 0, 1];

pub(super) struct ColorCase {
    pub(super) color: ColorSpace,
    pub(super) primaries: u8,
    pub(super) transfer: u8,
    pub(super) matrix: u8,
    pub(super) full_range: bool,
}

pub(super) fn pixel(chroma: ChromaSubsampling, depth: BitDepth) -> PixelFormat {
    PixelFormat { chroma, depth }
}

pub(super) fn every_format() -> Vec<PixelFormat> {
    let mut formats = Vec::new();
    for chroma in [
        ChromaSubsampling::Yuv420,
        ChromaSubsampling::Yuv422,
        ChromaSubsampling::Yuv444,
    ] {
        for depth in [BitDepth::Eight, BitDepth::Ten, BitDepth::Twelve] {
            formats.push(pixel(chroma, depth));
        }
    }
    formats
}

pub(super) fn frame_rates() -> [FrameRate; 3] {
    [
        FrameRate::new(60, 1),
        FrameRate::new(60_000, 1_001),
        FrameRate::new(24, 1),
    ]
}

fn color(
    primaries: ColorPrimaries,
    transfer: TransferFunction,
    matrix: Option<MatrixCoefficients>,
    range: ColorRange,
) -> ColorSpace {
    ColorSpace {
        primaries,
        transfer,
        matrix,
        range,
        hdr: HdrLevels::default(),
    }
}

pub(super) fn color_cases() -> Vec<ColorCase> {
    let case = |color, primaries, transfer, matrix, full_range| ColorCase {
        color,
        primaries,
        transfer,
        matrix,
        full_range,
    };

    vec![
        case(ColorSpace::default(), 1, 1, 1, true),
        case(
            color(
                ColorPrimaries::Bt709,
                TransferFunction::Bt709,
                None,
                ColorRange::Limited,
            ),
            1,
            1,
            1,
            false,
        ),
        case(
            color(
                ColorPrimaries::Bt2020,
                TransferFunction::Pq,
                Some(MatrixCoefficients::Bt2020Ncl),
                ColorRange::Limited,
            ),
            9,
            16,
            9,
            false,
        ),
        case(
            color(
                ColorPrimaries::DisplayP3,
                TransferFunction::Srgb,
                Some(MatrixCoefficients::Bt601),
                ColorRange::Full,
            ),
            12,
            13,
            6,
            true,
        ),
        case(
            color(
                ColorPrimaries::Bt601_625,
                TransferFunction::Gamma22,
                None,
                ColorRange::Limited,
            ),
            5,
            4,
            6,
            false,
        ),
        case(
            color(
                ColorPrimaries::Bt601_525,
                TransferFunction::Linear,
                None,
                ColorRange::Full,
            ),
            6,
            8,
            6,
            true,
        ),
        case(
            color(
                ColorPrimaries::Bt2020,
                TransferFunction::Hlg,
                None,
                ColorRange::Limited,
            ),
            9,
            18,
            9,
            false,
        ),
        case(
            color(
                ColorPrimaries::DciP3,
                TransferFunction::Bt709,
                Some(MatrixCoefficients::Bt709),
                ColorRange::Limited,
            ),
            11,
            1,
            1,
            false,
        ),
        case(hdr10_color(), 9, 16, 9, false),
    ]
}

pub(super) fn stream_info(format: PixelFormat, resolution: Resolution) -> StreamInfo {
    StreamInfo {
        resolution,
        format,
        color: ColorSpace::default(),
        frame_rate: FrameRate::new(60, 1),
        peak_kbps: PEAK_KBPS,
        qp: None,
        siting: ChromaSiting::default(),
        long_term_references: true,
    }
}

pub(super) fn picture(index: u64, keyframe: bool) -> Picture {
    Picture {
        keyframe,
        index,
        recon: FIRST_SURFACE + u32::try_from(index).unwrap(),
        coded: CODED_BUFFER,
        bits_per_second: BITS_PER_SECOND,
        packed_headers: 0,
    }
}

pub(super) fn assert_no_start_code_emulation(bytes: &[u8]) {
    assert_eq!(bytes[..START_CODE.len()], START_CODE);

    for window in bytes[START_CODE.len()..].windows(3) {
        assert!(
            window[0] != 0 || window[1] != 0 || window[2] > 2,
            "unescaped {window:?} in {bytes:?}"
        );
    }
}

pub(super) fn bits_at(bytes: &[u8], offset: u32, count: u32) -> u32 {
    let mut value = 0;
    for position in offset..offset + count {
        let byte = bytes[position as usize / 8];
        let bit = (byte >> (7 - position % 8)) & 1;
        value = (value << 1) | u32::from(bit);
    }
    value
}
