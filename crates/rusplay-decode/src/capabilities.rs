use rusplay_config::{NvdecDecoderConfig, VaapiDecoderConfig, VideoDecoderConfig};
use rusplay_core::{Codec, CodecFormat, FormatSet};

use crate::{Backend, DecodeError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    backend: Backend,
    formats: FormatSet,
}

impl Capabilities {
    pub const fn new(backend: Backend, formats: FormatSet) -> Self {
        Self { backend, formats }
    }

    pub const fn backend(&self) -> Backend {
        self.backend
    }

    pub const fn formats(&self) -> FormatSet {
        self.formats
    }

    pub const fn supports(&self, format: CodecFormat) -> bool {
        self.formats.contains(format)
    }
}

pub fn capabilities(config: &VideoDecoderConfig) -> Result<Capabilities, DecodeError> {
    let formats = match config {
        VideoDecoderConfig::Auto(_) => {
            return match crate::resolve(config, None) {
                VideoDecoderConfig::Auto(_) => Err(DecodeError::NoDecoderAvailable),
                resolved => capabilities(&resolved),
            };
        }
        VideoDecoderConfig::Vaapi(vaapi) => vaapi_formats(vaapi)?,
        VideoDecoderConfig::Nvdec(nvdec) => nvdec_formats(nvdec)?,
        VideoDecoderConfig::Software(_) if cfg!(any(feature = "software", feature = "ffmpeg")) => {
            software_formats()
        }
        VideoDecoderConfig::Software(_) => {
            return Err(DecodeError::BackendNotCompiled {
                backend: Backend::Software,
            });
        }
    };

    let backend = Backend::of(config).ok_or(DecodeError::NoDecoderAvailable)?;

    Ok(Capabilities::new(backend, formats))
}

#[cfg(feature = "vaapi")]
fn vaapi_formats(config: &VaapiDecoderConfig) -> Result<FormatSet, DecodeError> {
    crate::vaapi::capabilities(config)
}

#[cfg(not(feature = "vaapi"))]
fn vaapi_formats(_config: &VaapiDecoderConfig) -> Result<FormatSet, DecodeError> {
    Err(DecodeError::BackendNotCompiled {
        backend: Backend::Vaapi,
    })
}

#[cfg(feature = "nvdec")]
fn nvdec_formats(config: &NvdecDecoderConfig) -> Result<FormatSet, DecodeError> {
    crate::nvdec::capabilities(config)
}

#[cfg(not(feature = "nvdec"))]
fn nvdec_formats(_config: &NvdecDecoderConfig) -> Result<FormatSet, DecodeError> {
    Err(DecodeError::BackendNotCompiled {
        backend: Backend::Nvdec,
    })
}

fn software_formats() -> FormatSet {
    CodecFormat::all()
        .filter(|format| software_codec_ready(format.codec))
        .collect()
}

fn software_codec_ready(codec: Codec) -> bool {
    match codec {
        Codec::Av1 => cfg!(feature = "software"),
        Codec::H264 | Codec::Hevc => ffmpeg_codec_ready(codec),
    }
}

#[cfg(feature = "ffmpeg")]
fn ffmpeg_codec_ready(codec: Codec) -> bool {
    crate::ffmpeg::decoder_available(codec)
}

#[cfg(not(feature = "ffmpeg"))]
fn ffmpeg_codec_ready(_codec: Codec) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use rusplay_config::SoftwareDecoderConfig;
    use rusplay_core::{BitDepth, ChromaSubsampling, PixelFormat};

    use super::*;

    #[test]
    fn software_formats_follow_the_compiled_codecs() {
        let config = VideoDecoderConfig::Software(SoftwareDecoderConfig::default());

        let result = capabilities(&config);

        if !cfg!(any(feature = "software", feature = "ffmpeg")) {
            assert!(matches!(
                result,
                Err(DecodeError::BackendNotCompiled {
                    backend: Backend::Software
                })
            ));
            return;
        }

        let formats = result.unwrap().formats();
        let count = |codec| {
            formats
                .iter()
                .filter(|format| format.codec == codec)
                .count()
        };

        assert_eq!(
            count(Codec::Av1),
            if cfg!(feature = "software") { 9 } else { 0 }
        );
        if cfg!(feature = "software") {
            assert!(formats.iter().any(|format| {
                format
                    == CodecFormat::new(
                        Codec::Av1,
                        PixelFormat {
                            chroma: ChromaSubsampling::Yuv444,
                            depth: BitDepth::Twelve,
                        },
                    )
            }));
        }

        for codec in [Codec::H264, Codec::Hevc] {
            let decoded = count(codec);
            if cfg!(feature = "ffmpeg") {
                assert!(decoded == 0 || decoded == 9, "{codec} advertised {decoded}");
            } else {
                assert_eq!(decoded, 0);
            }
        }
    }

    #[test]
    fn software_capabilities_match_what_opens() {
        if !cfg!(any(feature = "software", feature = "ffmpeg")) {
            return;
        }
        let config = VideoDecoderConfig::Software(SoftwareDecoderConfig::default());
        let formats = capabilities(&config).unwrap().formats();

        for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
            let decodable = formats.iter().any(|format| format.codec == codec);

            assert_eq!(crate::open(codec, &config).is_ok(), decodable);
        }
    }

    #[test]
    fn vaapi_capabilities_match_their_feature() {
        let config = VideoDecoderConfig::Vaapi(VaapiDecoderConfig {
            device: "/dev/dri/rusplay-missing-node".into(),
            ..VaapiDecoderConfig::default()
        });

        let result = capabilities(&config);

        if cfg!(feature = "vaapi") {
            assert!(matches!(&result, Err(error) if error.is_device_open()));
        } else {
            assert!(matches!(
                result,
                Err(DecodeError::BackendNotCompiled {
                    backend: Backend::Vaapi
                })
            ));
        }
    }

    #[test]
    fn nvdec_capabilities_without_the_feature_are_not_compiled() {
        if cfg!(feature = "nvdec") {
            return;
        }

        assert!(matches!(
            capabilities(&VideoDecoderConfig::Nvdec(NvdecDecoderConfig::default())),
            Err(DecodeError::BackendNotCompiled {
                backend: Backend::Nvdec
            })
        ));
    }
}
