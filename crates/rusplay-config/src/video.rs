use std::{collections::BTreeMap, num::NonZeroU32, path::PathBuf};

use rusplay_core::{Codec, ColorSpace, FrameRate, PixelFormat, Resolution};
use serde::{Deserialize, Serialize};

use crate::{ConfigError, VideoDecoderConfig};

pub type EncoderParams = BTreeMap<String, String>;

const MAX_QP: u8 = 51;
const MAX_SVT_AV1_PRESET: u8 = 13;
const PRESENT_FRAME_LATENCY_RANGE: std::ops::RangeInclusive<u8> = 1..=3;

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
    pub decode_queue_frames: u8,
    pub present_mode: PresentMode,
    pub present_frame_latency: u8,
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
            decode_queue_frames: 3,
            present_mode: PresentMode::default(),
            present_frame_latency: 1,
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

        if self.decode_queue_frames == 0 {
            return Err(ConfigError::ZeroDecodeQueue);
        }
        if !PRESENT_FRAME_LATENCY_RANGE.contains(&self.present_frame_latency) {
            return Err(ConfigError::PresentFrameLatencyOutOfRange(
                self.present_frame_latency,
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
}
