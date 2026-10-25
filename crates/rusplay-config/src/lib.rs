mod audio;
mod capture;
mod error;
mod input;
mod network;
mod paths;
mod scaling;
mod video;

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

pub use crate::{
    audio::{AudioConfig, FlacConfig, OpusConfig, OpusFrameDuration},
    capture::{CaptureConfig, CursorMode, SourceKind},
    error::ConfigError,
    input::{InputConfig, PointerMode},
    network::NetworkConfig,
    paths::{ConfigEnvironment, ConfigRole},
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

    pub fn load_or_default(path: &Path) -> Result<Self, ConfigError> {
        match Self::load(path) {
            Err(ConfigError::Read { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                Ok(Self::default())
            }
            other => other,
        }
    }

    pub fn to_toml_string(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(self).map_err(|source| ConfigError::Serialize { source })
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        self.validate()?;
        let text = self.to_toml_string()?;

        if let Some(directory) = path
            .parent()
            .filter(|directory| !directory.as_os_str().is_empty())
        {
            fs::create_dir_all(directory).map_err(|source| ConfigError::CreateDirectory {
                path: directory.to_owned(),
                source,
            })?;
        }

        let staging = staging_path(path);
        let write_error = |source| ConfigError::Write {
            path: path.to_owned(),
            source,
        };

        fs::write(&staging, text).map_err(write_error)?;
        fs::rename(&staging, path).map_err(|source| {
            let _ = fs::remove_file(&staging);
            write_error(source)
        })
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        self.video.validate()?;
        self.audio.validate()?;
        self.network.validate()?;
        self.capture.validate()?;
        self.scaling.validate()
    }
}

fn staging_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".tmp");

    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("rusplay-config-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);

        directory
    }

    #[test]
    fn saved_config_loads_back_identical() {
        let directory = scratch("round-trip");
        let path = directory.join("nested").join("client.toml");

        let mut config = Config::default();
        config.video.bitrate_kbps = 42_000;
        config.scaling.upscale.antiringing = 0.35;
        config.video.encoder = VideoEncoderConfig::X264(X264Config {
            tune: vec![X264Tune::Film, X264Tune::Grain],
            threads: std::num::NonZeroU32::new(4),
            params: EncoderParams::from([("aq-mode".to_owned(), "3".to_owned())]),
            ..X264Config::default()
        });

        config.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        fs::remove_dir_all(&directory).unwrap();

        assert_eq!(loaded, config);
    }

    #[test]
    fn invalid_config_is_not_written() {
        let directory = scratch("invalid");
        let path = directory.join("client.toml");

        let mut config = Config::default();
        config.network.mtu = 100;

        let result = config.save(&path);

        assert!(matches!(result, Err(ConfigError::MtuOutOfRange(100))));
        assert!(!path.exists());
    }

    #[test]
    fn missing_file_loads_defaults() {
        let path = scratch("missing").join("client.toml");

        assert_eq!(Config::load_or_default(&path).unwrap(), Config::default());
    }

    #[test]
    fn malformed_file_is_still_an_error() {
        let directory = scratch("malformed");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("client.toml");
        fs::write(&path, "video = [").unwrap();

        let result = Config::load_or_default(&path);
        fs::remove_dir_all(&directory).unwrap();

        assert!(matches!(result, Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn every_validation_error_names_a_field() {
        let errors = [
            ConfigError::ZeroVideoBitrate,
            ConfigError::MtuOutOfRange(1),
            ConfigError::EmptyRestoreTokenFile,
        ];

        for error in errors {
            assert!(error.field().is_some());
        }
    }

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
