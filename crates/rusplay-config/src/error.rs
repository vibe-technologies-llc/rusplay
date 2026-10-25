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
    #[error("hdr reference white {0} nits is outside 1..=10000")]
    ReferenceWhiteOutOfRange(u32),
    #[error("hdr peak luminance {0} nits is outside 1..=10000")]
    PeakLuminanceOutOfRange(u32),
    #[error("mastering display minimum {min_nits} cd/m² is not below its maximum {max_nits} cd/m²")]
    MasteringLuminanceInverted { min_nits: f64, max_nits: f64 },
    #[error("display target reference white {0} nits is outside 1..=10000")]
    TargetReferenceWhiteOutOfRange(u32),
    #[error("display target peak {0} nits is outside 1..=10000")]
    TargetPeakOutOfRange(u32),
    #[error("display target black {black_nits} cd/m² is not below its peak {peak_nits} nits")]
    TargetBlackNotBelowPeak { black_nits: f64, peak_nits: u32 },
    #[error("peak detection percentile {0} is outside 0..=100")]
    PeakPercentileOutOfRange(f32),
    #[error("peak detection must smooth over at least one frame")]
    ZeroSmoothingFrames,
    #[error("scene cut threshold {0} dB is not a positive number")]
    SceneThresholdOutOfRange(f32),
    #[error("gamut mapping knee {0} is outside 0.0..1.0")]
    GamutKneeOutOfRange(f32),
    #[error("svt-av1 preset {0} is outside 0..=13")]
    SvtAv1PresetOutOfRange(u8),
    #[error("constant quality qp {0} is outside 0..=51")]
    QpOutOfRange(u8),
    #[error("audio bitrate must be greater than zero")]
    ZeroAudioBitrate,
    #[error("flac compression level {0} is outside 0..=8")]
    FlacCompressionOutOfRange(u8),
    #[error("audio decode queue must hold at least one packet")]
    ZeroAudioDecodeQueue,
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
    #[error("pointer sensitivity {0} is outside 0.05..=20.0")]
    PointerSensitivityOutOfRange(f32),
    #[error("gamepad deadzone {0} is outside 0.0..=0.9")]
    GamepadDeadzoneOutOfRange(f32),
    #[error("reconnect timeout must be greater than zero")]
    ZeroReconnectTimeout,
    #[error("handshake timeout must be greater than zero")]
    ZeroHandshakeTimeout,
    #[error("peer timeout must be greater than zero")]
    ZeroPeerTimeout,
    #[error("keyframe request interval must be greater than zero")]
    ZeroKeyframeRequestInterval,
    #[error("pacing window {0}% is outside 1..=100%")]
    PacingWindowOutOfRange(u8),
    #[error("keepalive interval must be greater than zero")]
    ZeroKeepaliveInterval,
    #[error(
        "keepalive interval {keepalive_ms} ms is not below the peer timeout {peer_timeout_ms} ms"
    )]
    KeepaliveNotBelowPeerTimeout {
        keepalive_ms: u32,
        peer_timeout_ms: u32,
    },
    #[error("decode queue must hold at least one frame")]
    ZeroDecodeQueue,
    #[error("peers.host_file is set to an empty path")]
    EmptyHostPeersFile,
    #[error("peers.client_file is set to an empty path")]
    EmptyClientPeersFile,
    #[error("peers.host_name is set to an empty name")]
    EmptyHostName,
    #[error("peers.host_name is {bytes} bytes, above 64")]
    HostNameTooLong { bytes: usize },
    #[error("audio fec overhead {0}% is above 100%")]
    AudioFecOverheadOutOfRange(u8),
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
            Self::ReferenceWhiteOutOfRange(_) => Some("video.color.hdr.reference_white_nits"),
            Self::PeakLuminanceOutOfRange(_) => Some("video.color.hdr.peak_nits"),
            Self::MasteringLuminanceInverted { .. } => Some("video.color.hdr.mastering.min_nits"),
            Self::TargetReferenceWhiteOutOfRange(_) => Some("color.target.reference_white_nits"),
            Self::TargetPeakOutOfRange(_) => Some("color.target.peak_nits"),
            Self::TargetBlackNotBelowPeak { .. } => Some("color.target.black_nits"),
            Self::GamutKneeOutOfRange(_) => Some("color.gamut_mapping.knee"),
            Self::PeakPercentileOutOfRange(_) => Some("color.tone_mapping.peak_percentile"),
            Self::ZeroSmoothingFrames => Some("color.tone_mapping.smoothing_frames"),
            Self::SceneThresholdOutOfRange(_) => Some("color.tone_mapping.scene_threshold_db"),
            Self::SvtAv1PresetOutOfRange(_) => Some("video.encoder.preset"),
            Self::QpOutOfRange(_) => Some("video.encoder.rate_control.qp"),
            Self::ZeroAudioBitrate => Some("audio.opus.bitrate_kbps"),
            Self::FlacCompressionOutOfRange(_) => Some("audio.flac.compression_level"),
            Self::ZeroAudioDecodeQueue => Some("audio.decode_queue_packets"),
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
            Self::PacingWindowOutOfRange(_) => Some("network.pacing_window_percent"),
            Self::ZeroKeepaliveInterval | Self::KeepaliveNotBelowPeerTimeout { .. } => {
                Some("network.keepalive_interval_ms")
            }
            Self::ZeroDecodeQueue => Some("video.decode_queue_frames"),
            Self::SigmoidCenterOutOfRange(_) => Some("scaling.upscale.sigmoid_center"),
            Self::SigmoidSlopeOutOfRange(_) => Some("scaling.upscale.sigmoid_slope"),
            Self::GamepadSlotsOutOfRange(_) => Some("input.max_gamepads"),
            Self::PointerSensitivityOutOfRange(_) => Some("input.pointer_sensitivity"),
            Self::GamepadDeadzoneOutOfRange(_) => Some("input.gamepad_deadzone"),
            Self::EmptyHostPeersFile => Some("peers.host_file"),
            Self::EmptyClientPeersFile => Some("peers.client_file"),
            Self::EmptyHostName | Self::HostNameTooLong { .. } => Some("peers.host_name"),
            Self::AudioFecOverheadOutOfRange(_) => Some("audio.fec_overhead_percent"),
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
