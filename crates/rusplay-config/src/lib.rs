mod audio;
mod capture;
mod decoder;
mod error;
mod input;
mod network;
mod scaling;
mod video;

use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

pub use crate::{
    audio::{AudioConfig, FlacConfig, OpusConfig, OpusFrameDuration},
    capture::{CaptureConfig, CursorMode, SourceKind},
    decoder::{NvdecDecoderConfig, SoftwareDecoderConfig, VaapiDecoderConfig, VideoDecoderConfig},
    error::ConfigError,
    input::{InputConfig, PointerMode},
    network::NetworkConfig,
    scaling::{DownscaleConfig, ScalingConfig, UpscaleConfig},
    video::{
        EncoderParams, NvencConfig, NvencPreset, NvencTuning, RateControl, SoftwarePreset,
        SvtAv1Config, VaapiConfig, VideoConfig, VideoEncoderConfig, X264Config, X264Tune,
        X265Config, X265Tune,
    },
};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub video: VideoConfig,
    pub audio: AudioConfig,
    pub network: NetworkConfig,
    pub input: InputConfig,
    pub capture: CaptureConfig,
    pub scaling: ScalingConfig,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;

        let config: Self = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source: Box::new(source),
        })?;

        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        self.video.validate()?;
        self.audio.validate()?;
        self.network.validate()?;
        self.capture.validate()?;
        self.scaling.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        assert!(Config::default().validate().is_ok());
    }

    #[test]
    fn empty_document_yields_defaults() {
        let parsed: Config = toml::from_str("").unwrap();

        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn restore_token_file_is_configurable() {
        let parsed: Config = toml::from_str(
            r#"
            [capture]
            restore_token_file = "/tmp/rusplay-token"
            "#,
        )
        .unwrap();

        assert_eq!(
            parsed.capture.restore_token_file,
            Some(std::path::PathBuf::from("/tmp/rusplay-token"))
        );
        assert!(parsed.validate().is_ok());
    }

    #[test]
    fn empty_restore_token_file_is_rejected() {
        let parsed: Config = toml::from_str(
            r#"
            [capture]
            restore_token_file = ""
            "#,
        )
        .unwrap();

        assert!(matches!(
            parsed.validate(),
            Err(ConfigError::EmptyRestoreTokenFile)
        ));
    }

    #[test]
    fn software_encoder_params_pass_through() {
        let parsed: Config = toml::from_str(
            r#"
            [video.encoder]
            backend = "x264"
            preset = "veryfast"

            [video.encoder.params]
            aq-mode = "3"
            "#,
        )
        .unwrap();

        let VideoEncoderConfig::X264(x264) = parsed.video.encoder else {
            panic!("expected the x264 backend, got {:?}", parsed.video.encoder);
        };

        assert_eq!(x264.preset, SoftwarePreset::Veryfast);
        assert_eq!(x264.params.get("aq-mode").map(String::as_str), Some("3"));
    }
}
