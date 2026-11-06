use std::{num::NonZeroU32, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::ConfigError;

const MAX_SOFTWARE_DECODER_THREADS: u32 = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "kebab-case")]
pub enum VideoDecoderConfig {
    Vaapi(VaapiDecoderConfig),
    Nvdec(NvdecDecoderConfig),
    Software(SoftwareDecoderConfig),
}

impl Default for VideoDecoderConfig {
    fn default() -> Self {
        Self::Vaapi(VaapiDecoderConfig::default())
    }
}

impl VideoDecoderConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        match self {
            Self::Software(SoftwareDecoderConfig {
                threads: Some(threads),
            }) if threads.get() > MAX_SOFTWARE_DECODER_THREADS => {
                Err(ConfigError::DecoderThreadsOutOfRange(threads.get()))
            }
            Self::Vaapi(_) | Self::Nvdec(_) | Self::Software(_) => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VaapiDecoderConfig {
    pub device: PathBuf,
    pub copy_output: bool,
}

impl Default for VaapiDecoderConfig {
    fn default() -> Self {
        Self {
            device: PathBuf::from("/dev/dri/renderD128"),
            copy_output: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NvdecDecoderConfig {
    pub gpu_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SoftwareDecoderConfig {
    pub threads: Option<NonZeroU32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    #[test]
    fn vaapi_copy_output_is_opt_in_and_round_trips() {
        let parsed: Config =
            toml::from_str("[video.decoder]\nbackend = \"vaapi\"\ncopy_output = true\n").unwrap();
        let VideoDecoderConfig::Vaapi(vaapi) = &parsed.video.decoder else {
            panic!("expected VAAPI")
        };
        let encoded = toml::to_string(&parsed).unwrap();
        let restored: Config = toml::from_str(&encoded).unwrap();

        assert!(!VaapiDecoderConfig::default().copy_output);
        assert!(vaapi.copy_output);
        assert_eq!(restored.video.decoder, parsed.video.decoder);
    }

    #[test]
    fn software_decoder_section_parses() {
        let parsed: Config = toml::from_str(
            r#"
            [video.decoder]
            backend = "software"
            threads = 4
            "#,
        )
        .unwrap();

        assert_eq!(
            parsed.video.decoder,
            VideoDecoderConfig::Software(SoftwareDecoderConfig {
                threads: NonZeroU32::new(4),
            })
        );
        assert!(parsed.validate().is_ok());
    }

    #[test]
    fn too_many_software_decoder_threads_is_rejected() {
        let decoder = VideoDecoderConfig::Software(SoftwareDecoderConfig {
            threads: NonZeroU32::new(MAX_SOFTWARE_DECODER_THREADS + 1),
        });

        assert!(matches!(
            decoder.validate(),
            Err(ConfigError::DecoderThreadsOutOfRange(257))
        ));
    }
}
