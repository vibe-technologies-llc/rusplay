use std::{fmt, io, net::SocketAddr, time::Duration};

use rusplay_audio::AudioError;
use rusplay_capture::{CaptureError, DrmFourcc};
use rusplay_core::AudioCodec;
use rusplay_encode::{EncodeError, input::fourcc_text};
use rusplay_input::InputError;
use rusplay_net::NetError;
use rusplay_protocol::ProtocolError;
use rusplay_scale::ScaleError;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Capture,
    Encode,
    Send,
    Control,
    Audio,
    Cursor,
}

impl Stage {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Encode => "encode",
            Self::Send => "send",
            Self::Control => "control",
            Self::Audio => "audio",
            Self::Cursor => "cursor",
        }
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Error)]
pub enum HostError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Encode(#[from] EncodeError),
    #[error(transparent)]
    Audio(#[from] AudioError),
    #[error(transparent)]
    Input(#[from] InputError),
    #[error(transparent)]
    Net(#[from] NetError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Scale(#[from] ScaleError),
    #[error("no gpu adapter is available")]
    NoGpuAdapter,
    #[error("failed to open the gpu device")]
    GpuDevice(#[source] wgpu::RequestDeviceError),
    #[error("waiting for the gpu to finish the frame failed")]
    GpuPoll(#[source] wgpu::PollError),
    #[error("reading the converted frame back from the gpu failed")]
    GpuReadback(#[source] wgpu::BufferAsyncError),
    #[error("the mapped readback buffer could not be viewed")]
    GpuMappedRange(#[source] wgpu::MapRangeError),
    #[error("failed to spawn the {stage} thread")]
    SpawnStage {
        stage: Stage,
        #[source]
        source: io::Error,
    },
    #[error("the {stage} thread panicked")]
    StagePanicked { stage: Stage },
    #[error("no client paired or reconnected within {0:?}")]
    AcceptTimeout(Duration),
    #[error("failed to name the paired client at {address}")]
    PeerName {
        address: SocketAddr,
        #[source]
        source: NetError,
    },
    #[error("client speaks protocol version {theirs}, this host speaks {ours}")]
    VersionMismatch { ours: u16, theirs: u16 },
    #[error("client sent no hello within {0:?}")]
    HandshakeTimeout(Duration),
    #[error("client never acknowledged the hello")]
    HelloUndeliverable,
    #[error("nothing arrived from the client for {0:?}")]
    ClientSilent(Duration),
    #[error("captured frames in {} have no gpu upload format", fourcc_text(*fourcc))]
    CaptureFormatUnsupported { fourcc: DrmFourcc },
    #[error("captured frame holds {found} bytes, short of the {needed} its rows need")]
    CaptureBufferTooShort { found: usize, needed: usize },
    #[error("captured frame stride {stride} is narrower than its {width} pixel rows")]
    CaptureStrideTooSmall { stride: u32, width: u32 },
    #[error("screen capture needs the pipewire feature")]
    CaptureNotCompiled,
    #[error("audio codec {codec} is not compiled into this host")]
    AudioCodecNotCompiled { codec: AudioCodec },
    #[error("the {width}x{height} cursor image does not fit the wire format")]
    CursorImageTooLarge { width: u32, height: u32 },
}

impl HostError {
    pub fn is_session_failure(&self) -> bool {
        matches!(
            self,
            Self::Net(_)
                | Self::AcceptTimeout(_)
                | Self::PeerName { .. }
                | Self::VersionMismatch { .. }
                | Self::HandshakeTimeout(_)
                | Self::HelloUndeliverable
                | Self::ClientSilent(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_failures_end_only_their_session() {
        assert!(HostError::ClientSilent(Duration::from_secs(1)).is_session_failure());
        assert!(HostError::Net(NetError::PairingLocked).is_session_failure());
        assert!(!HostError::NoGpuAdapter.is_session_failure());
        assert!(!HostError::Capture(CaptureError::StreamEnded).is_session_failure());
    }
}
