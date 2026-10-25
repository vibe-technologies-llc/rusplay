use rusplay_config::VideoEncoderConfig;
use rusplay_core::{BitDepth, ChromaSubsampling, Codec, PixelFormat};

use crate::{Backend, EncodeError};

const CODECS: [Codec; 3] = [Codec::H264, Codec::Hevc, Codec::Av1];
const CHROMAS: [ChromaSubsampling; 3] = [
    ChromaSubsampling::Yuv420,
    ChromaSubsampling::Yuv422,
    ChromaSubsampling::Yuv444,
];
const DEPTHS: [BitDepth; 3] = [BitDepth::Eight, BitDepth::Ten, BitDepth::Twelve];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    backend: Backend,
    formats: Vec<(Codec, PixelFormat)>,
}

impl Capabilities {
    pub fn probe(
        backend: Backend,
        mut supports: impl FnMut(Codec, PixelFormat) -> Result<bool, EncodeError>,
    ) -> Result<Self, EncodeError> {
        let mut formats = Vec::new();
        for codec in CODECS {
            for chroma in CHROMAS {
                for depth in DEPTHS {
                    let format = PixelFormat { chroma, depth };
                    if supports(codec, format)? {
                        formats.push((codec, format));
                    }
                }
            }
        }

        Ok(Self { backend, formats })
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    pub fn supports(&self, codec: Codec, format: PixelFormat) -> bool {
        self.formats.contains(&(codec, format))
    }

    pub fn formats(&self, codec: Codec) -> impl Iterator<Item = PixelFormat> + '_ {
        self.formats
            .iter()
            .filter(move |(candidate, _)| *candidate == codec)
            .map(|&(_, format)| format)
    }

    pub fn codecs(&self) -> Vec<Codec> {
        CODECS
            .into_iter()
            .filter(|&codec| self.formats(codec).next().is_some())
            .collect()
    }
}

pub fn capabilities(config: &VideoEncoderConfig) -> Result<Capabilities, EncodeError> {
    let backend = Backend::from(config);

    match config {
        #[cfg(feature = "vaapi")]
        VideoEncoderConfig::Vaapi(vaapi) => crate::vaapi::capabilities(vaapi),
        #[cfg(feature = "x264")]
        VideoEncoderConfig::X264(_) => Capabilities::probe(backend, |codec, format| {
            Ok(codec == Codec::H264 && crate::x264::supports(format))
        }),
        #[cfg(feature = "x265")]
        VideoEncoderConfig::X265(_) => Capabilities::probe(backend, |codec, format| {
            Ok(codec == Codec::Hevc && crate::x265::supports(format))
        }),
        #[cfg(feature = "svt-av1")]
        VideoEncoderConfig::SvtAv1(_) => Capabilities::probe(backend, |codec, format| {
            Ok(codec == Codec::Av1 && crate::svt_av1::supports(format))
        }),
        _ => Err(EncodeError::BackendNotCompiled { backend }),
    }
}

#[cfg(test)]
mod tests {
    use rusplay_config::NvencConfig;

    use super::*;

    #[test]
    fn probe_keeps_only_accepted_formats() {
        let only_hevc_10 = Capabilities::probe(Backend::X265, |codec, format| {
            Ok(codec == Codec::Hevc && format.depth == BitDepth::Ten)
        })
        .unwrap();
        let ten = |chroma| PixelFormat {
            chroma,
            depth: BitDepth::Ten,
        };

        assert_eq!(only_hevc_10.backend(), Backend::X265);
        assert_eq!(only_hevc_10.codecs(), vec![Codec::Hevc]);
        assert!(only_hevc_10.supports(Codec::Hevc, ten(ChromaSubsampling::Yuv422)));
        assert!(!only_hevc_10.supports(Codec::Av1, ten(ChromaSubsampling::Yuv420)));
        assert_eq!(
            only_hevc_10.formats(Codec::Hevc).collect::<Vec<_>>(),
            CHROMAS.map(ten).to_vec()
        );
    }

    #[test]
    fn probe_errors_propagate() {
        let error = Capabilities::probe(Backend::Vaapi, |_, _| {
            Err(EncodeError::BackendNotCompiled {
                backend: Backend::Vaapi,
            })
        })
        .unwrap_err();

        assert!(matches!(error, EncodeError::BackendNotCompiled { .. }));
    }

    #[cfg(any(feature = "x264", feature = "x265", feature = "svt-av1"))]
    #[test]
    fn software_capabilities_match_what_opens() {
        use rusplay_config::VideoConfig;

        use crate::{open, testing::TEST_RESOLUTION};

        let mut encoders = Vec::new();
        #[cfg(feature = "x264")]
        encoders.push(VideoEncoderConfig::X264(
            rusplay_config::X264Config::default(),
        ));
        #[cfg(feature = "x265")]
        encoders.push(VideoEncoderConfig::X265(
            rusplay_config::X265Config::default(),
        ));
        #[cfg(feature = "svt-av1")]
        encoders.push(VideoEncoderConfig::SvtAv1(
            rusplay_config::SvtAv1Config::default(),
        ));

        for encoder in encoders {
            let found = capabilities(&encoder).unwrap();
            for codec in CODECS {
                for chroma in CHROMAS {
                    for depth in DEPTHS {
                        let format = PixelFormat { chroma, depth };
                        let video = VideoConfig {
                            resolution: Some(TEST_RESOLUTION),
                            codec,
                            pixel_format: format,
                            encoder: encoder.clone(),
                            ..VideoConfig::default()
                        };

                        assert_eq!(
                            open(&video).is_ok(),
                            found.supports(codec, format),
                            "{} {codec} {chroma} {}-bit",
                            found.backend(),
                            depth.bits()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn backends_without_a_build_report_it() {
        let error = capabilities(&VideoEncoderConfig::Nvenc(NvencConfig::default())).unwrap_err();

        assert!(matches!(
            error,
            EncodeError::BackendNotCompiled {
                backend: Backend::Nvenc
            }
        ));
    }
}
