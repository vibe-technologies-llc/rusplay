use serde::{Deserialize, Serialize};

use crate::{Config, ConfigError, PointerMode, PresentMode, VideoDecoderConfig};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HostProfile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decoder: Option<VideoDecoderConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub present_mode: Option<PresentMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub present_frame_latency: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyboard: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pointer: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gamepads: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pointer_mode: Option<PointerMode>,
}

impl HostProfile {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn overlay(&self, config: &mut Config) {
        let Self {
            decoder,
            present_mode,
            present_frame_latency,
            keyboard,
            pointer,
            gamepads,
            pointer_mode,
        } = self;

        if let Some(decoder) = decoder {
            config.video.decoder = decoder.clone();
        }
        if let Some(present_mode) = *present_mode {
            config.video.present_mode = present_mode;
        }
        if let Some(latency) = *present_frame_latency {
            config.video.present_frame_latency = latency;
        }
        if let Some(keyboard) = *keyboard {
            config.input.keyboard = keyboard;
        }
        if let Some(pointer) = *pointer {
            config.input.pointer = pointer;
        }
        if let Some(gamepads) = *gamepads {
            config.input.gamepads = gamepads;
        }
        if let Some(pointer_mode) = *pointer_mode {
            config.input.pointer_mode = pointer_mode;
        }
    }

    pub fn apply(&self, base: &Config) -> Result<Config, ConfigError> {
        let mut merged = base.clone();
        self.overlay(&mut merged);
        merged.validate()?;

        Ok(merged)
    }

    pub fn from_toml_str(text: &str) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|source| ConfigError::ProfileParse {
            source: Box::new(source),
        })
    }

    pub fn to_toml_string(&self) -> Result<String, ConfigError> {
        toml::to_string(self).map_err(|source| ConfigError::Serialize { source })
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;
    use crate::SoftwareDecoderConfig;

    fn full_profile() -> HostProfile {
        HostProfile {
            decoder: Some(VideoDecoderConfig::Software(SoftwareDecoderConfig {
                threads: NonZeroU32::new(4),
            })),
            present_mode: Some(PresentMode::Fifo),
            present_frame_latency: Some(2),
            keyboard: Some(false),
            pointer: Some(false),
            gamepads: Some(false),
            pointer_mode: Some(PointerMode::Relative),
        }
    }

    #[test]
    fn an_empty_profile_leaves_the_config_alone() {
        let base = Config::default();

        assert!(HostProfile::default().is_empty());
        assert_eq!(HostProfile::default().apply(&base).unwrap(), base);
    }

    #[test]
    fn every_set_field_replaces_the_global_value() {
        let base = Config::default();

        let merged = full_profile().apply(&base).unwrap();

        assert_eq!(
            merged.video.decoder,
            VideoDecoderConfig::Software(SoftwareDecoderConfig {
                threads: NonZeroU32::new(4),
            })
        );
        assert_eq!(merged.video.present_mode, PresentMode::Fifo);
        assert_eq!(merged.video.present_frame_latency, 2);
        assert!(!merged.input.keyboard);
        assert!(!merged.input.pointer);
        assert!(!merged.input.gamepads);
        assert_eq!(merged.input.pointer_mode, PointerMode::Relative);
    }

    #[test]
    fn each_field_alone_changes_the_merged_config() {
        let base = Config::default();
        let text = full_profile().to_toml_string().unwrap();
        let table: toml::Table = toml::from_str(&text).unwrap();

        assert_eq!(table.len(), 7);
        for (key, value) in table {
            let single = toml::Table::from_iter([(key.clone(), value)]);
            let profile = HostProfile::from_toml_str(&toml::to_string(&single).unwrap()).unwrap();

            assert_ne!(profile.apply(&base).unwrap(), base, "{key} is not applied");
        }
    }

    #[test]
    fn unset_fields_follow_the_global_config() {
        let mut base = Config::default();
        base.video.present_mode = PresentMode::Immediate;
        base.input.keyboard = false;
        base.audio.opus.bitrate_kbps += 1;
        let profile = HostProfile {
            pointer: Some(false),
            ..HostProfile::default()
        };

        let merged = profile.apply(&base).unwrap();

        assert_eq!(merged.video.present_mode, PresentMode::Immediate);
        assert!(!merged.input.keyboard);
        assert!(!merged.input.pointer);
        assert_eq!(merged.audio, base.audio);
    }

    #[test]
    fn the_merged_config_is_validated() {
        let profile = HostProfile {
            present_frame_latency: Some(0),
            ..HostProfile::default()
        };

        let result = profile.apply(&Config::default());

        assert!(matches!(
            result,
            Err(ConfigError::PresentFrameLatencyOutOfRange(0))
        ));
    }

    #[test]
    fn an_invalid_decoder_override_is_rejected() {
        let profile = HostProfile {
            decoder: Some(VideoDecoderConfig::Software(SoftwareDecoderConfig {
                threads: NonZeroU32::new(1000),
            })),
            ..HostProfile::default()
        };

        assert!(matches!(
            profile.apply(&Config::default()),
            Err(ConfigError::DecoderThreadsOutOfRange(1000))
        ));
    }

    #[test]
    fn a_profile_round_trips_through_toml() {
        let profile = full_profile();

        let text = profile.to_toml_string().unwrap();

        assert_eq!(HostProfile::from_toml_str(&text).unwrap(), profile);
    }

    #[test]
    fn an_empty_profile_serializes_to_nothing() {
        assert_eq!(HostProfile::default().to_toml_string().unwrap(), "");
        assert!(HostProfile::from_toml_str("").unwrap().is_empty());
    }

    #[test]
    fn unknown_keys_are_refused() {
        assert!(matches!(
            HostProfile::from_toml_str("bitrate = 3"),
            Err(ConfigError::ProfileParse { .. })
        ));
    }
}
