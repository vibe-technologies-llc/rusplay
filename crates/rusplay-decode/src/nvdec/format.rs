use std::ffi::{c_int, c_short};

use rusplay_core::{BitDepth, ChromaSubsampling, Codec, PixelFormat, Resolution};

use crate::{
    Backend, DecodeError,
    nvdec::ffi::{self, DecodeCaps, ShortRect, VideoFormat},
};

const BACKEND: Backend = Backend::Nvdec;
const MACROBLOCK_PIXELS: u64 = 256;

pub(super) const fn cuda_codec(codec: Codec) -> c_int {
    match codec {
        Codec::H264 => ffi::CUDA_VIDEO_CODEC_H264,
        Codec::Hevc => ffi::CUDA_VIDEO_CODEC_HEVC,
        Codec::Av1 => ffi::CUDA_VIDEO_CODEC_AV1,
    }
}

pub(super) const fn cuda_chroma(chroma: ChromaSubsampling) -> c_int {
    match chroma {
        ChromaSubsampling::Yuv420 => ffi::CHROMA_FORMAT_420,
        ChromaSubsampling::Yuv422 => ffi::CHROMA_FORMAT_422,
        ChromaSubsampling::Yuv444 => ffi::CHROMA_FORMAT_444,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SurfaceFormat {
    Nv12,
    P016,
    Nv16,
    P216,
    Yuv444,
    Yuv444Wide,
}

impl SurfaceFormat {
    pub(super) const fn of(format: PixelFormat) -> Self {
        match (format.chroma, format.depth) {
            (ChromaSubsampling::Yuv420, BitDepth::Eight) => Self::Nv12,
            (ChromaSubsampling::Yuv420, BitDepth::Ten | BitDepth::Twelve) => Self::P016,
            (ChromaSubsampling::Yuv422, BitDepth::Eight) => Self::Nv16,
            (ChromaSubsampling::Yuv422, BitDepth::Ten | BitDepth::Twelve) => Self::P216,
            (ChromaSubsampling::Yuv444, BitDepth::Eight) => Self::Yuv444,
            (ChromaSubsampling::Yuv444, BitDepth::Ten | BitDepth::Twelve) => Self::Yuv444Wide,
        }
    }

    pub(super) const fn code(self) -> c_int {
        match self {
            Self::Nv12 => ffi::SURFACE_FORMAT_NV12,
            Self::P016 => ffi::SURFACE_FORMAT_P016,
            Self::Nv16 => ffi::SURFACE_FORMAT_NV16,
            Self::P216 => ffi::SURFACE_FORMAT_P216,
            Self::Yuv444 => ffi::SURFACE_FORMAT_YUV444,
            Self::Yuv444Wide => ffi::SURFACE_FORMAT_YUV444_16BIT,
        }
    }

    pub(super) const fn semi_planar(self) -> bool {
        matches!(self, Self::Nv12 | Self::P016 | Self::Nv16 | Self::P216)
    }

    pub(super) const fn bytes_per_sample(self) -> usize {
        match self {
            Self::Nv12 | Self::Nv16 | Self::Yuv444 => 1,
            Self::P016 | Self::P216 | Self::Yuv444Wide => 2,
        }
    }

    const fn mask_bit(self) -> u16 {
        1 << self.code()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StreamFormat {
    pub pixel_format: PixelFormat,
    pub surface: SurfaceFormat,
    pub coded: Resolution,
    pub display: Resolution,
    pub crop: ShortRect,
    pub target: Resolution,
    pub decode_surfaces: u32,
}

impl StreamFormat {
    pub(super) fn of(codec: Codec, format: &VideoFormat) -> Result<Self, DecodeError> {
        let chroma = match format.chroma_format {
            ffi::CHROMA_FORMAT_420 => ChromaSubsampling::Yuv420,
            ffi::CHROMA_FORMAT_422 => ChromaSubsampling::Yuv422,
            ffi::CHROMA_FORMAT_444 => ChromaSubsampling::Yuv444,
            _ => {
                return Err(DecodeError::Monochrome {
                    backend: BACKEND,
                    codec,
                });
            }
        };
        let bits = format
            .bit_depth_luma_minus8
            .max(format.bit_depth_chroma_minus8)
            .saturating_add(8);
        let depth = match bits {
            8 => BitDepth::Eight,
            10 => BitDepth::Ten,
            12 => BitDepth::Twelve,
            other => {
                return Err(DecodeError::BitDepthUnsupported {
                    backend: BACKEND,
                    codec,
                    bits: other,
                });
            }
        };
        let pixel_format = PixelFormat { chroma, depth };

        let picture = Resolution::new(format.coded_width, format.coded_height);
        let area = format.display_area;
        let left = clamp_edge(area.left, picture.width);
        let top = clamp_edge(area.top, picture.height);
        let right = clamp_edge(area.right, picture.width).max(left);
        let bottom = clamp_edge(area.bottom, picture.height).max(top);
        let display = Resolution::new(right - left, bottom - top);
        if display.width == 0 || display.height == 0 {
            return Err(DecodeError::ResolutionUnsupported {
                backend: BACKEND,
                codec,
                width: display.width,
                height: display.height,
            });
        }

        let target = Resolution::new(
            even(display.width).min(picture.width - left),
            even(display.height).min(picture.height - top),
        );
        let short = |value: u32| c_short::try_from(value).unwrap_or(c_short::MAX);
        let crop = ShortRect {
            left: short(left),
            top: short(top),
            right: short(left + target.width),
            bottom: short(top + target.height),
        };

        Ok(Self {
            pixel_format,
            surface: SurfaceFormat::of(pixel_format),
            coded: picture,
            display,
            crop,
            target,
            decode_surfaces: u32::from(format.min_num_decode_surfaces.max(1)),
        })
    }

    pub(super) fn check(&self, codec: Codec, caps: &DecodeCaps) -> Result<(), DecodeError> {
        if caps.is_supported == 0 || caps.output_format_mask & self.surface.mask_bit() == 0 {
            return Err(unsupported(codec, self.pixel_format));
        }

        let Resolution { width, height } = self.coded;
        let macroblocks = u64::from(width) * u64::from(height) / MACROBLOCK_PIXELS;
        if width > caps.max_width
            || height > caps.max_height
            || width < u32::from(caps.min_width)
            || height < u32::from(caps.min_height)
            || macroblocks > u64::from(caps.max_mb_count)
        {
            return Err(DecodeError::ResolutionUnsupported {
                backend: BACKEND,
                codec,
                width,
                height,
            });
        }

        Ok(())
    }
}

pub(super) fn unsupported(codec: Codec, format: PixelFormat) -> DecodeError {
    if format.chroma != ChromaSubsampling::Yuv420 {
        DecodeError::ChromaUnsupported {
            backend: BACKEND,
            codec,
            chroma: format.chroma,
        }
    } else if format.depth != BitDepth::Eight {
        DecodeError::BitDepthUnsupported {
            backend: BACKEND,
            codec,
            bits: format.depth.bits(),
        }
    } else {
        DecodeError::CodecUnsupported {
            backend: BACKEND,
            codec,
        }
    }
}

fn clamp_edge(value: c_int, limit: u32) -> u32 {
    u32::try_from(value).unwrap_or(0).min(limit)
}

const fn even(value: u32) -> u32 {
    value + (value & 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvdec::ffi::IntRect;

    fn video_format(chroma_format: c_int, bits_minus8: u8) -> VideoFormat {
        VideoFormat {
            codec: ffi::CUDA_VIDEO_CODEC_HEVC,
            bit_depth_luma_minus8: bits_minus8,
            bit_depth_chroma_minus8: bits_minus8,
            min_num_decode_surfaces: 5,
            coded_width: 1920,
            coded_height: 1088,
            display_area: IntRect {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
            chroma_format,
            ..VideoFormat::default()
        }
    }

    fn caps(mask: u16) -> DecodeCaps {
        DecodeCaps {
            is_supported: 1,
            output_format_mask: mask,
            max_width: 4096,
            max_height: 4096,
            max_mb_count: 65_536,
            min_width: 144,
            min_height: 144,
            ..DecodeCaps::default()
        }
    }

    #[test]
    fn codecs_map_onto_cuda_video_codecs() {
        assert_eq!(cuda_codec(Codec::H264), 4);
        assert_eq!(cuda_codec(Codec::Hevc), 8);
        assert_eq!(cuda_codec(Codec::Av1), 11);
    }

    #[test]
    fn chroma_maps_onto_cuda_chroma_formats() {
        assert_eq!(cuda_chroma(ChromaSubsampling::Yuv420), 1);
        assert_eq!(cuda_chroma(ChromaSubsampling::Yuv422), 2);
        assert_eq!(cuda_chroma(ChromaSubsampling::Yuv444), 3);
    }

    #[test]
    fn every_pixel_format_has_a_native_surface_format() {
        let cases = [
            (
                ChromaSubsampling::Yuv420,
                BitDepth::Eight,
                SurfaceFormat::Nv12,
                0,
            ),
            (
                ChromaSubsampling::Yuv420,
                BitDepth::Ten,
                SurfaceFormat::P016,
                1,
            ),
            (
                ChromaSubsampling::Yuv420,
                BitDepth::Twelve,
                SurfaceFormat::P016,
                1,
            ),
            (
                ChromaSubsampling::Yuv422,
                BitDepth::Eight,
                SurfaceFormat::Nv16,
                4,
            ),
            (
                ChromaSubsampling::Yuv422,
                BitDepth::Ten,
                SurfaceFormat::P216,
                5,
            ),
            (
                ChromaSubsampling::Yuv444,
                BitDepth::Eight,
                SurfaceFormat::Yuv444,
                2,
            ),
            (
                ChromaSubsampling::Yuv444,
                BitDepth::Twelve,
                SurfaceFormat::Yuv444Wide,
                3,
            ),
        ];

        for (chroma, depth, surface, code) in cases {
            let found = SurfaceFormat::of(PixelFormat { chroma, depth });

            assert_eq!(found, surface);
            assert_eq!(found.code(), code);
            assert_eq!(found.semi_planar(), chroma != ChromaSubsampling::Yuv444);
            assert_eq!(
                found.bytes_per_sample(),
                if depth == BitDepth::Eight { 1 } else { 2 }
            );
        }
    }

    #[test]
    fn a_10_bit_422_sequence_crops_to_its_display_area() {
        let format =
            StreamFormat::of(Codec::Hevc, &video_format(ffi::CHROMA_FORMAT_422, 2)).unwrap();

        assert_eq!(
            format.pixel_format,
            PixelFormat {
                chroma: ChromaSubsampling::Yuv422,
                depth: BitDepth::Ten,
            }
        );
        assert_eq!(format.surface, SurfaceFormat::P216);
        assert_eq!(format.coded, Resolution::new(1920, 1088));
        assert_eq!(format.display, Resolution::new(1920, 1080));
        assert_eq!(format.target, Resolution::new(1920, 1080));
        assert_eq!(
            format.crop,
            ShortRect {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            }
        );
        assert_eq!(format.decode_surfaces, 5);
    }

    #[test]
    fn an_odd_display_size_maps_an_even_target_and_keeps_the_display_size() {
        let mut odd = video_format(ffi::CHROMA_FORMAT_444, 0);
        odd.display_area.right = 1917;
        odd.display_area.bottom = 1079;

        let format = StreamFormat::of(Codec::Hevc, &odd).unwrap();

        assert_eq!(format.display, Resolution::new(1917, 1079));
        assert_eq!(format.target, Resolution::new(1918, 1080));
        assert_eq!(format.crop.right, 1918);
        assert_eq!(format.crop.bottom, 1080);
    }

    #[test]
    fn mixed_bit_depths_take_the_larger() {
        let mut mixed = video_format(ffi::CHROMA_FORMAT_420, 0);
        mixed.bit_depth_chroma_minus8 = 4;

        let format = StreamFormat::of(Codec::Av1, &mixed).unwrap();

        assert_eq!(format.pixel_format.depth, BitDepth::Twelve);
        assert_eq!(format.surface, SurfaceFormat::P016);
    }

    #[test]
    fn monochrome_and_unknown_depths_are_rejected() {
        let monochrome =
            StreamFormat::of(Codec::Hevc, &video_format(ffi::CHROMA_FORMAT_MONOCHROME, 0));
        let fourteen = StreamFormat::of(Codec::Hevc, &video_format(ffi::CHROMA_FORMAT_420, 6));

        assert!(matches!(
            monochrome,
            Err(DecodeError::Monochrome {
                backend: Backend::Nvdec,
                codec: Codec::Hevc,
            })
        ));
        assert!(matches!(
            fourteen,
            Err(DecodeError::BitDepthUnsupported {
                backend: Backend::Nvdec,
                bits: 14,
                ..
            })
        ));
    }

    #[test]
    fn capabilities_reject_formats_and_sizes_the_engine_lacks() {
        let p216 = StreamFormat::of(Codec::Hevc, &video_format(ffi::CHROMA_FORMAT_422, 2)).unwrap();
        let nv12 = StreamFormat::of(Codec::H264, &video_format(ffi::CHROMA_FORMAT_420, 0)).unwrap();
        let p016 = StreamFormat::of(Codec::Av1, &video_format(ffi::CHROMA_FORMAT_420, 2)).unwrap();
        let mut small = caps(1 << ffi::SURFACE_FORMAT_NV12);
        small.max_width = 1280;

        assert!(
            p216.check(Codec::Hevc, &caps(1 << ffi::SURFACE_FORMAT_P216))
                .is_ok()
        );
        assert!(matches!(
            p216.check(Codec::Hevc, &caps(1 << ffi::SURFACE_FORMAT_P016)),
            Err(DecodeError::ChromaUnsupported {
                chroma: ChromaSubsampling::Yuv422,
                ..
            })
        ));
        assert!(matches!(
            p016.check(Codec::Av1, &DecodeCaps::default()),
            Err(DecodeError::BitDepthUnsupported { bits: 10, .. })
        ));
        assert!(matches!(
            nv12.check(Codec::H264, &DecodeCaps::default()),
            Err(DecodeError::CodecUnsupported {
                backend: Backend::Nvdec,
                codec: Codec::H264,
            })
        ));
        assert!(matches!(
            nv12.check(Codec::H264, &small),
            Err(DecodeError::ResolutionUnsupported {
                width: 1920,
                height: 1088,
                ..
            })
        ));
    }
}
