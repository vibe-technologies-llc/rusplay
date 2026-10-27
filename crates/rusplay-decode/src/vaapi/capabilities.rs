use rusplay_config::VaapiDecoderConfig;
use rusplay_core::{BitDepth, ChromaSubsampling, Codec, CodecFormat, FormatSet};

use crate::{
    DecodeError,
    vaapi::{
        ffi, hevc, rt_format,
        va::{Display, Profile},
    },
};

pub(crate) fn probe(config: &VaapiDecoderConfig) -> Result<FormatSet, DecodeError> {
    let display = Display::open(&config.device)?;
    let mut formats = FormatSet::EMPTY;

    for format in CodecFormat::all() {
        let Some(profile) = profile_for(format) else {
            continue;
        };
        if display.has_decoder(profile)?
            && display.rt_formats(profile)? & rt_format(format.format) != 0
        {
            formats.insert(format);
        }
    }

    Ok(formats)
}

fn profile_for(format: CodecFormat) -> Option<Profile> {
    let pixel = format.format;

    match format.codec {
        Codec::H264 => match (pixel.chroma, pixel.depth) {
            (ChromaSubsampling::Yuv420, BitDepth::Eight) => Some(ffi::VAProfile_VAProfileH264High),
            (ChromaSubsampling::Yuv420, BitDepth::Ten) => Some(ffi::VAProfile_VAProfileH264High10),
            (ChromaSubsampling::Yuv422, BitDepth::Eight | BitDepth::Ten) => {
                Some(ffi::VAProfile_VAProfileH264High422)
            }
            _ => None,
        },
        Codec::Hevc => Some(hevc::profile_for(pixel)),
        Codec::Av1 => Some(match (pixel.chroma, pixel.depth) {
            (ChromaSubsampling::Yuv420, BitDepth::Eight | BitDepth::Ten) => {
                ffi::VAProfile_VAProfileAV1Profile0
            }
            (ChromaSubsampling::Yuv444, BitDepth::Eight | BitDepth::Ten) => {
                ffi::VAProfile_VAProfileAV1Profile1
            }
            _ => ffi::VAProfile_VAProfileAV1Profile2,
        }),
    }
}

#[cfg(test)]
mod tests {
    use rusplay_core::PixelFormat;

    use super::*;

    fn format(codec: Codec, chroma: ChromaSubsampling, depth: BitDepth) -> CodecFormat {
        CodecFormat::new(codec, PixelFormat { chroma, depth })
    }

    #[test]
    fn h264_maps_onto_the_high_profiles_and_has_no_444_or_12_bit() {
        assert_eq!(
            profile_for(format(
                Codec::H264,
                ChromaSubsampling::Yuv420,
                BitDepth::Eight
            )),
            Some(ffi::VAProfile_VAProfileH264High)
        );
        assert_eq!(
            profile_for(format(
                Codec::H264,
                ChromaSubsampling::Yuv422,
                BitDepth::Ten
            )),
            Some(ffi::VAProfile_VAProfileH264High422)
        );
        assert_eq!(
            profile_for(format(
                Codec::H264,
                ChromaSubsampling::Yuv444,
                BitDepth::Eight
            )),
            None
        );
        assert_eq!(
            profile_for(format(
                Codec::H264,
                ChromaSubsampling::Yuv420,
                BitDepth::Twelve
            )),
            None
        );
    }

    #[test]
    fn hevc_uses_the_same_profiles_as_the_decoder() {
        assert_eq!(
            profile_for(format(
                Codec::Hevc,
                ChromaSubsampling::Yuv422,
                BitDepth::Eight
            )),
            Some(ffi::VAProfile_VAProfileHEVCMain422_10)
        );
        assert_eq!(
            profile_for(format(
                Codec::Hevc,
                ChromaSubsampling::Yuv420,
                BitDepth::Ten
            )),
            Some(ffi::VAProfile_VAProfileHEVCMain10)
        );
    }

    #[test]
    fn av1_profiles_follow_chroma_and_depth() {
        assert_eq!(
            profile_for(format(Codec::Av1, ChromaSubsampling::Yuv420, BitDepth::Ten)),
            Some(ffi::VAProfile_VAProfileAV1Profile0)
        );
        assert_eq!(
            profile_for(format(
                Codec::Av1,
                ChromaSubsampling::Yuv444,
                BitDepth::Eight
            )),
            Some(ffi::VAProfile_VAProfileAV1Profile1)
        );
        assert_eq!(
            profile_for(format(
                Codec::Av1,
                ChromaSubsampling::Yuv422,
                BitDepth::Eight
            )),
            Some(ffi::VAProfile_VAProfileAV1Profile2)
        );
        assert_eq!(
            profile_for(format(
                Codec::Av1,
                ChromaSubsampling::Yuv420,
                BitDepth::Twelve
            )),
            Some(ffi::VAProfile_VAProfileAV1Profile2)
        );
    }

    #[test]
    fn a_missing_device_is_a_device_error() {
        let config = VaapiDecoderConfig {
            device: "/dev/dri/rusplay-missing-node".into(),
        };

        assert!(matches!(
            probe(&config),
            Err(DecodeError::DeviceOpen { .. })
        ));
    }
}
