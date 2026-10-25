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
}
