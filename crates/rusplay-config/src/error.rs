use std::{io, net::Ipv4Addr, path::PathBuf};

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
    #[error("reassembly timeout must be greater than zero")]
    ZeroReassemblyTimeout,
    #[error("control retransmit timeout must be greater than zero")]
    ZeroControlRetransmit,
    #[error("discovery port must be greater than zero")]
    ZeroDiscoveryPort,
    #[error("discovery group {0} is not a multicast address")]
    DiscoveryGroupNotMulticast(Ipv4Addr),
    #[error("pairing pin length {0} is outside 4..=12 digits")]
    PinDigitsOutOfRange(u8),
    #[error("pairing must allow at least one attempt")]
    ZeroPairingAttempts,
    #[error("pairing timeout must be greater than zero")]
    ZeroPairingTimeout,
    #[error("sigmoid center {0} is outside 0.0..=1.0")]
    SigmoidCenterOutOfRange(f32),
    #[error("sigmoid slope {0} is outside 1.0..=20.0")]
    SigmoidSlopeOutOfRange(f32),
    #[error("gamepad slot count {0} is outside 1..=16")]
    GamepadSlotsOutOfRange(u8),
    #[error("reconnect timeout must be greater than zero")]
    ZeroReconnectTimeout,
    #[error("peers.host_file is set to an empty path")]
    EmptyHostPeersFile,
    #[error("peers.client_file is set to an empty path")]
    EmptyClientPeersFile,
    #[error("fec overhead {0}% is above 100%")]
    FecOverheadOutOfRange(u8),
    #[error("capture.restore_token_file is set to an empty path")]
    EmptyRestoreTokenFile,
}
