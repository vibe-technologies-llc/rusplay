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
        VideoDecoderConfig::Vaapi(vaapi) => vaapi_formats(vaapi)?,
        VideoDecoderConfig::Nvdec(nvdec) => nvdec_formats(nvdec)?,
        VideoDecoderConfig::Software(_) if cfg!(feature = "software") => software_formats(),
        VideoDecoderConfig::Software(_) => {
            return Err(DecodeError::BackendNotCompiled {
                backend: Backend::Software,
            });
        }
    };

    Ok(Capabilities::new(Backend::of(config), formats))
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
        .filter(|format| format.codec == Codec::Av1)
        .collect()
}

#[cfg(test)]
mod tests {
    use rusplay_config::SoftwareDecoderConfig;
    use rusplay_core::{BitDepth, ChromaSubsampling, PixelFormat};

    use super::*;

    #[test]
    fn software_decodes_every_av1_format_and_nothing_else() {
        let config = VideoDecoderConfig::Software(SoftwareDecoderConfig::default());

        let result = capabilities(&config);

        if cfg!(feature = "software") {
            let capabilities = result.unwrap();
            assert_eq!(capabilities.backend(), Backend::Software);
            assert_eq!(capabilities.formats().iter().count(), 9);
            assert!(
                capabilities
                    .formats()
                    .iter()
                    .all(|format| format.codec == Codec::Av1)
            );
            assert!(capabilities.supports(CodecFormat::new(
                Codec::Av1,
                PixelFormat {
                    chroma: ChromaSubsampling::Yuv444,
                    depth: BitDepth::Twelve,
                },
            )));
        } else {
            assert!(matches!(
                result,
                Err(DecodeError::BackendNotCompiled {
                    backend: Backend::Software
                })
            ));
        }
    }

    #[test]
    fn software_capabilities_match_what_opens() {
        if !cfg!(feature = "software") {
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
        });

        let result = capabilities(&config);

        if cfg!(feature = "vaapi") {
            assert!(matches!(result, Err(DecodeError::DeviceOpen { .. })));
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
