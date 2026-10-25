mod audio;
mod capture;
mod color;
mod decoder;
mod error;
mod input;
mod network;
mod paths;
mod peers;
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
    color::{ColorConfig, DisplayTargetConfig, GamutMappingConfig, ToneMappingConfig},
    decoder::{NvdecDecoderConfig, SoftwareDecoderConfig, VaapiDecoderConfig, VideoDecoderConfig},
    error::ConfigError,
    input::{InputConfig, PointerMode},
    network::NetworkConfig,
    paths::{ConfigEnvironment, ConfigRole},
    peers::PeersConfig,
    scaling::{DownscaleConfig, ScalingConfig, UpscaleConfig},
    video::{
        EncoderParams, NvencConfig, NvencPreset, NvencTuning, RateControl, SoftwarePreset,
        SvtAv1Config, SvtAv1LogLevel, VaapiConfig, VideoConfig, VideoEncoderConfig, X264Config,
        X264Tune, X265Config, X265Tune,
    },
};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub video: VideoConfig,
    pub audio: AudioConfig,
    pub network: NetworkConfig,
    pub peers: PeersConfig,
    pub input: InputConfig,
    pub capture: CaptureConfig,
    pub scaling: ScalingConfig,
    pub color: ColorConfig,
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
        self.peers.validate()?;
        self.capture.validate()?;
        self.input.validate()?;
        self.scaling.validate()?;
        self.color.validate()
    }
}

fn staging_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".tmp");

    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use rusplay_core::{ColorPrimaries, MatrixCoefficients, TransferFunction};

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
    fn empty_decode_queue_is_rejected() {
        let mut config = Config::default();
        config.video.decode_queue_frames = 0;

        assert!(matches!(
            config.validate(),
            Err(ConfigError::ZeroDecodeQueue)
        ));
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
    fn hdr_levels_are_configurable_and_range_checked() {
        let parsed: Config = toml::from_str(
            r"
            [video.color.hdr]
            reference_white_nits = 100
            peak_nits = 600
            ",
        )
        .unwrap();
        let too_bright: Config = toml::from_str(
            r"
            [video.color.hdr]
            peak_nits = 10001
            ",
        )
        .unwrap();
        let no_white: Config = toml::from_str(
            r"
            [video.color.hdr]
            reference_white_nits = 0
            ",
        )
        .unwrap();

        assert_eq!(parsed.video.color.hdr.reference_white_nits, 100);
        assert_eq!(parsed.video.color.hdr.peak_nits, 600);
        assert!(parsed.validate().is_ok());
        assert!(matches!(
            too_bright.validate(),
            Err(ConfigError::PeakLuminanceOutOfRange(10001))
        ));
        assert!(matches!(
            no_white.validate(),
            Err(ConfigError::ReferenceWhiteOutOfRange(0))
        ));
    }

    #[test]
    fn colour_description_parses_named_and_custom_primaries() {
        let named: Config = toml::from_str(
            r#"
            [video.color]
            primaries = "display-p3"
            transfer = "gamma22"
            matrix = "bt601"
            "#,
        )
        .unwrap();
        let custom: Config = toml::from_str(
            r"
            [video.color.primaries.custom]
            red = [0.68, 0.32]
            green = [0.265, 0.69]
            blue = [0.15, 0.06]
            white = [0.3127, 0.329]
            ",
        )
        .unwrap();

        assert_eq!(named.video.color.primaries, ColorPrimaries::DisplayP3);
        assert_eq!(named.video.color.transfer, TransferFunction::Gamma22);
        assert_eq!(
            named.video.color.matrix_coefficients(),
            MatrixCoefficients::Bt601
        );
        assert_eq!(
            custom.video.color.primaries.chromaticities(),
            ColorPrimaries::DisplayP3.chromaticities()
        );
        assert_eq!(
            toml::from_str::<Config>(&custom.to_toml_string().unwrap()).unwrap(),
            custom
        );
    }

    #[test]
    fn degenerate_custom_primaries_fail_to_parse() {
        let parsed = toml::from_str::<Config>(
            r"
            [video.color.primaries.custom]
            red = [0.1, 0.1]
            green = [0.2, 0.2]
            blue = [0.3, 0.3]
            white = [0.3127, 0.329]
            ",
        );

        assert!(parsed.is_err());
    }

    #[test]
    fn hdr_metadata_is_optional_and_validated() {
        let parsed: Config = toml::from_str(
            r#"
            [video.color.hdr.mastering]
            primaries = "bt2020"
            max_nits = 4000.0
            min_nits = 0.005

            [video.color.hdr.content_light]
            max_cll_nits = 1200
            max_fall_nits = 300
            "#,
        )
        .unwrap();
        let inverted: Config = toml::from_str(
            r"
            [video.color.hdr.mastering]
            max_nits = 0.001
            min_nits = 0.01
            ",
        )
        .unwrap();

        let mastering = parsed.video.color.hdr.mastering.unwrap();
        assert_eq!(mastering.primaries, ColorPrimaries::Bt2020);
        assert_eq!(mastering.min_nits.units(), 50);
        assert!((parsed.video.color.hdr.content_peak_nits() - 1200.0).abs() < f64::EPSILON);
        assert!(parsed.validate().is_ok());
        assert!(matches!(
            inverted.validate(),
            Err(ConfigError::MasteringLuminanceInverted { .. })
        ));
        assert_eq!(Config::default().video.color.hdr.mastering, None);
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
    fn peer_files_are_configurable() {
        let parsed: Config = toml::from_str(
            r#"
            [peers]
            host_file = "/tmp/clients"
            client_file = "/tmp/hosts"
            "#,
        )
        .unwrap();

        assert_eq!(
            parsed.peers.host_file,
            Some(std::path::PathBuf::from("/tmp/clients"))
        );
        assert_eq!(
            parsed.peers.client_file,
            Some(std::path::PathBuf::from("/tmp/hosts"))
        );
        assert!(parsed.validate().is_ok());
    }

    #[test]
    fn empty_peer_files_are_rejected() {
        let host: Config = toml::from_str("[peers]\nhost_file = \"\"").unwrap();
        let client: Config = toml::from_str("[peers]\nclient_file = \"\"").unwrap();

        assert!(matches!(
            host.validate(),
            Err(ConfigError::EmptyHostPeersFile)
        ));
        assert!(matches!(
            client.validate(),
            Err(ConfigError::EmptyClientPeersFile)
        ));
    }

    #[test]
    fn svt_av1_log_level_defaults_to_warn_and_parses() {
        let defaults: Config = toml::from_str("[video.encoder]\nbackend = \"svt-av1\"").unwrap();
        let quiet: Config =
            toml::from_str("[video.encoder]\nbackend = \"svt-av1\"\nlog_level = \"off\"").unwrap();

        let VideoEncoderConfig::SvtAv1(defaults) = defaults.video.encoder else {
            panic!("expected the svt-av1 backend");
        };
        let VideoEncoderConfig::SvtAv1(quiet) = quiet.video.encoder else {
            panic!("expected the svt-av1 backend");
        };

        assert_eq!(defaults.log_level, SvtAv1LogLevel::Warn);
        assert_eq!(quiet.log_level, SvtAv1LogLevel::Off);
        assert!(SvtAv1LogLevel::Off < SvtAv1LogLevel::Debug);
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
