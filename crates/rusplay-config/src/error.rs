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
    #[error("handshake timeout must be greater than zero")]
    ZeroHandshakeTimeout,
    #[error("peer timeout must be greater than zero")]
    ZeroPeerTimeout,
    #[error("keyframe request interval must be greater than zero")]
    ZeroKeyframeRequestInterval,
    #[error("decode queue must hold at least one frame")]
    ZeroDecodeQueue,
    #[error("peers.host_file is set to an empty path")]
    EmptyHostPeersFile,
    #[error("peers.client_file is set to an empty path")]
    EmptyClientPeersFile,
    #[error("software decoder thread count {0} is above 256")]
    DecoderThreadsOutOfRange(u32),
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
            Self::ZeroReassemblyTimeout => Some("network.reassembly_timeout_ms"),
            Self::ZeroControlRetransmit => Some("network.control_retransmit_ms"),
            Self::ZeroDiscoveryPort => Some("network.discovery_port"),
            Self::DiscoveryGroupNotMulticast(_) => Some("network.discovery_group"),
            Self::PinDigitsOutOfRange(_) => Some("network.pairing_pin_digits"),
            Self::ZeroPairingAttempts => Some("network.pairing_max_attempts"),
            Self::ZeroPairingTimeout => Some("network.pairing_timeout_ms"),
            Self::ZeroReconnectTimeout => Some("network.reconnect_timeout_ms"),
            Self::ZeroHandshakeTimeout => Some("network.handshake_timeout_ms"),
            Self::ZeroPeerTimeout => Some("network.peer_timeout_ms"),
            Self::ZeroKeyframeRequestInterval => Some("network.keyframe_request_interval_ms"),
            Self::ZeroDecodeQueue => Some("video.decode_queue_frames"),
            Self::SigmoidCenterOutOfRange(_) => Some("scaling.upscale.sigmoid_center"),
            Self::SigmoidSlopeOutOfRange(_) => Some("scaling.upscale.sigmoid_slope"),
            Self::GamepadSlotsOutOfRange(_) => Some("input.max_gamepads"),
            Self::EmptyHostPeersFile => Some("peers.host_file"),
            Self::EmptyClientPeersFile => Some("peers.client_file"),
            Self::DecoderThreadsOutOfRange(_) => Some("video.decoder.threads"),
            Self::Read { .. }
            | Self::Parse { .. }
            | Self::NoConfigHome
            | Self::Serialize { .. }
            | Self::CreateDirectory { .. }
            | Self::Write { .. } => None,
        }
    }
}
