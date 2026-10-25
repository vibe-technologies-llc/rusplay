use std::{io, path::PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}")]
    Read { path: PathBuf, source: io::Error },
    #[error("failed to parse config file {path}")]
    Parse {
        path: PathBuf,
        source: Box<toml::de::Error>,
    },
    #[error("frame rate {numerator}/{denominator} is not a valid rate")]
    InvalidFrameRate { numerator: u32, denominator: u32 },
    #[error("video bitrate must be greater than zero")]
    ZeroVideoBitrate,
    #[error("svt-av1 preset {0} is outside 0..=13")]
    SvtAv1PresetOutOfRange(u8),
    #[error("constant quality qp {0} is outside 0..=51")]
    QpOutOfRange(u8),
    #[error("audio bitrate must be greater than zero")]
    ZeroAudioBitrate,
    #[error("flac compression level {0} is outside 0..=8")]
    FlacCompressionOutOfRange(u8),
    #[error("jitter buffer {0} ms is above 500 ms")]
    JitterBufferOutOfRange(u32),
    #[error("flac block size {0} is outside 32..=32767")]
    FlacBlockSizeOutOfRange(u16),
    #[error("mtu {0} is outside 576..=9000")]
    MtuOutOfRange(u16),
    #[error("antiringing strength {0} is outside 0.0..=1.0")]
    AntiringingOutOfRange(f32),
    #[error("fec overhead {0}% is above 100%")]
    FecOverheadOutOfRange(u8),
    #[error("capture.restore_token_file is set to an empty path")]
    EmptyRestoreTokenFile,
    #[error("neither XDG_CONFIG_HOME nor HOME names an absolute directory for the config file")]
    NoConfigHome,
    #[error("failed to serialize the configuration")]
    Serialize { source: toml::ser::Error },
    #[error("failed to create config directory {path}")]
    CreateDirectory { path: PathBuf, source: io::Error },
    #[error("failed to write config file {path}")]
    Write { path: PathBuf, source: io::Error },
}

impl ConfigError {
    pub const fn field(&self) -> Option<&'static str> {
        match self {
            Self::InvalidFrameRate { .. } => Some("video.frame_rate"),
            Self::ZeroVideoBitrate => Some("video.bitrate_kbps"),
            Self::SvtAv1PresetOutOfRange(_) => Some("video.encoder.preset"),
            Self::QpOutOfRange(_) => Some("video.encoder.rate_control.qp"),
            Self::ZeroAudioBitrate => Some("audio.opus.bitrate_kbps"),
            Self::FlacCompressionOutOfRange(_) => Some("audio.flac.compression_level"),
            Self::JitterBufferOutOfRange(_) => Some("audio.jitter_buffer_ms"),
            Self::FlacBlockSizeOutOfRange(_) => Some("audio.flac.block_size"),
            Self::MtuOutOfRange(_) => Some("network.mtu"),
            Self::AntiringingOutOfRange(_) => Some("scaling.upscale.antiringing"),
            Self::FecOverheadOutOfRange(_) => Some("network.fec_overhead_percent"),
            Self::EmptyRestoreTokenFile => Some("capture.restore_token_file"),
            Self::Read { .. }
            | Self::Parse { .. }
            | Self::NoConfigHome
            | Self::Serialize { .. }
            | Self::CreateDirectory { .. }
            | Self::Write { .. } => None,
        }
    }
}
