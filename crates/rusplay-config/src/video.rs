use std::{collections::BTreeMap, num::NonZeroU32, path::PathBuf};

use rusplay_core::{Codec, ColorSpace, FrameRate, PixelFormat, Resolution};
use serde::{Deserialize, Serialize};

use crate::{ConfigError, VideoDecoderConfig};

pub type EncoderParams = BTreeMap<String, String>;

const MAX_QP: u8 = 51;
const MAX_SVT_AV1_PRESET: u8 = 13;
const PRESENT_FRAME_LATENCY_RANGE: std::ops::RangeInclusive<u8> = 1..=3;
const MAX_PRESENT_MARGIN_US: u32 = 50_000;
const MAX_PRESENT_JITTER_US: u32 = 100_000;
const MAX_DISPLAY_DEBOUNCE_MS: u32 = 5_000;
const MAX_QUALITY_INTERVAL_MS: u32 = 60_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VideoConfig {
    pub resolution: Option<Resolution>,
    pub frame_rate: FrameRate,
    pub codec: Codec,
    pub pixel_format: PixelFormat,
    pub color: ColorSpace,
    pub bitrate_kbps: u32,
    pub encoder: VideoEncoderConfig,
    pub encoder_gpu_handoff: bool,
    pub decoder: VideoDecoderConfig,
    pub vbv_frames: u8,
    pub decode_queue_frames: u8,
    pub present_mode: PresentMode,
    pub present_frame_latency: u8,
    pub present_pacing: PresentPacing,
    pub present_margin_us: u32,
    pub present_jitter_us: u32,
    pub stats_overlay: bool,
    pub display_debounce_ms: u32,
    pub quality_interval_ms: u32,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            resolution: None,
            frame_rate: FrameRate::default(),
            codec: Codec::default(),
            pixel_format: PixelFormat::default(),
            color: ColorSpace::default(),
            bitrate_kbps: 80_000,
            encoder: VideoEncoderConfig::default(),
            encoder_gpu_handoff: true,
            decoder: VideoDecoderConfig::default(),
            vbv_frames: 2,
            decode_queue_frames: 3,
            present_mode: PresentMode::default(),
            present_frame_latency: 1,
            present_pacing: PresentPacing::default(),
            present_margin_us: 3_000,
            present_jitter_us: 8_000,
            stats_overlay: false,
            display_debounce_ms: 200,
            quality_interval_ms: 1_000,
        }
    }
}

impl VideoConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if !self.frame_rate.is_valid() {
            return Err(ConfigError::InvalidFrameRate {
                numerator: self.frame_rate.numerator,
                denominator: self.frame_rate.denominator,
            });
        }

        if let Some(resolution) = self
            .resolution
            .filter(|resolution| resolution.pixel_count() == 0)
        {
            return Err(ConfigError::EmptyStreamResolution {
                width: resolution.width,
                height: resolution.height,
            });
        }

        if self.bitrate_kbps == 0 {
            return Err(ConfigError::ZeroVideoBitrate);
        }

        let hdr = self.color.hdr;
        if !hdr.reference_white_in_range() {
            return Err(ConfigError::ReferenceWhiteOutOfRange(
                hdr.reference_white_nits,
            ));
        }
        if !hdr.peak_in_range() {
            return Err(ConfigError::PeakLuminanceOutOfRange(hdr.peak_nits));
        }
        if let Some(mastering) = hdr.mastering.filter(|mastering| !mastering.is_ordered()) {
            return Err(ConfigError::MasteringLuminanceInverted {
                min_nits: mastering.min_nits.nits(),
                max_nits: mastering.max_nits.nits(),
            });
        }

        if self.vbv_frames == 0 {
            return Err(ConfigError::ZeroVbvFrames);
        }

        if self.decode_queue_frames == 0 {
            return Err(ConfigError::ZeroDecodeQueue);
        }
        if !PRESENT_FRAME_LATENCY_RANGE.contains(&self.present_frame_latency) {
            return Err(ConfigError::PresentFrameLatencyOutOfRange(
                self.present_frame_latency,
            ));
        }
        if self.present_margin_us > MAX_PRESENT_MARGIN_US {
            return Err(ConfigError::PresentMarginOutOfRange(self.present_margin_us));
        }
        if self.present_jitter_us > MAX_PRESENT_JITTER_US {
            return Err(ConfigError::PresentJitterOutOfRange(self.present_jitter_us));
        }

        if self.display_debounce_ms > MAX_DISPLAY_DEBOUNCE_MS {
            return Err(ConfigError::DisplayDebounceOutOfRange(
                self.display_debounce_ms,
            ));
        }

        if self.quality_interval_ms > MAX_QUALITY_INTERVAL_MS {
            return Err(ConfigError::QualityIntervalOutOfRange(
                self.quality_interval_ms,
            ));
        }

        self.encoder.validate()?;
        self.decoder.validate()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PresentMode {
    #[default]
    Mailbox,
    Immediate,
    Fifo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PresentPacing {
    #[default]
    Off,
    Latency,
    Smooth,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "kebab-case")]
pub enum VideoEncoderConfig {
    Vaapi(VaapiConfig),
    Nvenc(NvencConfig),
    X264(X264Config),
    X265(X265Config),
    SvtAv1(SvtAv1Config),
}

impl Default for VideoEncoderConfig {
    fn default() -> Self {
        Self::Vaapi(VaapiConfig::default())
    }
}

impl VideoEncoderConfig {
    pub const fn is_constant_quality(&self) -> bool {
        matches!(
            self,
            Self::Vaapi(VaapiConfig {
                rate_control: RateControl::ConstantQuality { .. },
                ..
            }) | Self::Nvenc(NvencConfig {
                rate_control: RateControl::ConstantQuality { .. },
                ..
            })
        )
    }

    fn validate(&self) -> Result<(), ConfigError> {
        match self {
            Self::Vaapi(vaapi) => vaapi.rate_control.validate(),
            Self::Nvenc(nvenc) => nvenc.rate_control.validate(),
            Self::SvtAv1(svt) if svt.preset > MAX_SVT_AV1_PRESET => {
                Err(ConfigError::SvtAv1PresetOutOfRange(svt.preset))
            }
            Self::X264(_) | Self::X265(_) | Self::SvtAv1(_) => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum RateControl {
    #[default]
    Cbr,
    Vbr {
        max_bitrate_kbps: u32,
    },
    ConstantQuality {
        qp: u8,
    },
}

impl RateControl {
    pub const DEFAULT_VBR: Self = Self::Vbr {
        max_bitrate_kbps: 120_000,
    };
    pub const DEFAULT_CONSTANT_QUALITY: Self = Self::ConstantQuality { qp: 23 };

    fn validate(self) -> Result<(), ConfigError> {
        match self {
            Self::ConstantQuality { qp } if qp > MAX_QP => Err(ConfigError::QpOutOfRange(qp)),
            Self::Cbr | Self::Vbr { .. } | Self::ConstantQuality { .. } => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VaapiConfig {
    pub device: PathBuf,
    pub rate_control: RateControl,
    pub low_power: bool,
    pub params: EncoderParams,
}

impl Default for VaapiConfig {
    fn default() -> Self {
        Self {
            device: PathBuf::from("/dev/dri/renderD128"),
            rate_control: RateControl::default(),
            low_power: true,
            params: EncoderParams::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NvencPreset {
    #[default]
    P1,
    P2,
    P3,
    P4,
    P5,
    P6,
    P7,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NvencTuning {
    #[default]
    UltraLowLatency,
    LowLatency,
    HighQuality,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NvencConfig {
    pub gpu_index: u32,
    pub preset: NvencPreset,
    pub tuning: NvencTuning,
    pub rate_control: RateControl,
    pub params: EncoderParams,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SoftwarePreset {
    Ultrafast,
    #[default]
    Superfast,
    Veryfast,
    Faster,
    Fast,
    Medium,
    Slow,
    Slower,
    Veryslow,
    Placebo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum X264Tune {
    #[default]
    Zerolatency,
    Film,
    Animation,
    Grain,
    Stillimage,
    Psnr,
    Ssim,
    Fastdecode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct X264Config {
    pub preset: SoftwarePreset,
    pub tune: Vec<X264Tune>,
    pub threads: Option<NonZeroU32>,
    pub params: EncoderParams,
}

impl Default for X264Config {
    fn default() -> Self {
        Self {
            preset: SoftwarePreset::default(),
            tune: vec![X264Tune::Zerolatency],
            threads: None,
            params: EncoderParams::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum X265Tune {
    #[default]
    Zerolatency,
    Grain,
    Animation,
    Psnr,
    Ssim,
    Fastdecode,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct X265Config {
    pub preset: SoftwarePreset,
    pub tune: Option<X265Tune>,
    pub threads: Option<NonZeroU32>,
    pub params: EncoderParams,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SvtAv1LogLevel {
    Off,
    Fatal,
    Error,
    #[default]
    Warn,
    Info,
    Debug,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SvtAv1Config {
    pub preset: u8,
    pub low_latency: bool,
    pub log_level: SvtAv1LogLevel,
    pub threads: Option<NonZeroU32>,
    pub params: EncoderParams,
}

impl Default for SvtAv1Config {
    fn default() -> Self {
        Self {
            preset: 10,
            low_latency: true,
            log_level: SvtAv1LogLevel::default(),
            threads: None,
            params: EncoderParams::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn present_mode_defaults_to_mailbox_and_uses_kebab_case_names() {
        let config: VideoConfig = toml::from_str("present_mode = \"immediate\"").unwrap();

        assert_eq!(config.present_mode, PresentMode::Immediate);
        assert_eq!(VideoConfig::default().present_mode, PresentMode::Mailbox);
    }

    #[test]
    fn the_stats_overlay_is_off_by_default() {
        let parsed: VideoConfig = toml::from_str("stats_overlay = true").unwrap();

        assert!(!VideoConfig::default().stats_overlay);
        assert!(parsed.stats_overlay);
    }

    #[test]
    fn present_frame_latency_is_range_checked() {
        let mut config = VideoConfig::default();
        assert_eq!(config.present_frame_latency, 1);

        config.present_frame_latency = 0;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::PresentFrameLatencyOutOfRange(0))
        ));

        config.present_frame_latency = 4;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::PresentFrameLatencyOutOfRange(4))
        ));

        config.present_frame_latency = 3;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn present_pacing_is_off_by_default_and_parses_kebab_case_names() {
        let parsed: VideoConfig = toml::from_str("present_pacing = \"smooth\"").unwrap();

        assert_eq!(VideoConfig::default().present_pacing, PresentPacing::Off);
        assert_eq!(parsed.present_pacing, PresentPacing::Smooth);
    }

    #[test]
    fn the_display_debounce_is_range_checked_and_may_be_zero() {
        let mut config = VideoConfig::default();

        assert_eq!(config.display_debounce_ms, 200);

        config.display_debounce_ms = MAX_DISPLAY_DEBOUNCE_MS + 1;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::DisplayDebounceOutOfRange(5_001))
        ));

        config.display_debounce_ms = 0;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn the_quality_interval_is_range_checked_and_may_be_zero() {
        let mut config = VideoConfig::default();

        assert_eq!(config.quality_interval_ms, 1_000);

        config.quality_interval_ms = MAX_QUALITY_INTERVAL_MS + 1;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::QualityIntervalOutOfRange(60_001))
        ));

        config.quality_interval_ms = 0;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn present_margin_and_jitter_are_range_checked() {
        let mut config = VideoConfig {
            present_margin_us: MAX_PRESENT_MARGIN_US + 1,
            ..VideoConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(ConfigError::PresentMarginOutOfRange(50_001))
        ));

        config.present_margin_us = MAX_PRESENT_MARGIN_US;
        config.present_jitter_us = MAX_PRESENT_JITTER_US + 1;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::PresentJitterOutOfRange(100_001))
        ));

        config.present_jitter_us = 0;
        config.present_margin_us = 0;
        assert!(config.validate().is_ok());
    }

    fn with_resolution(width: u32, height: u32) -> VideoConfig {
        VideoConfig {
            resolution: Some(Resolution::new(width, height)),
            ..VideoConfig::default()
        }
    }

    #[test]
    fn stream_resolution_with_a_zero_side_is_rejected() {
        assert!(with_resolution(1920, 1080).validate().is_ok());
        assert!(matches!(
            with_resolution(0, 1080).validate(),
            Err(ConfigError::EmptyStreamResolution {
                width: 0,
                height: 1080
            })
        ));
        assert!(matches!(
            with_resolution(1920, 0).validate(),
            Err(ConfigError::EmptyStreamResolution {
                width: 1920,
                height: 0
            })
        ));
    }
}
