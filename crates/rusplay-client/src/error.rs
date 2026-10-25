use std::{fmt, io, time::Duration};

use rusplay_audio::AudioError;
use rusplay_core::AudioCodec;
use rusplay_decode::{DecodeError, DrmFourcc};
use rusplay_input::InputError;
use rusplay_net::NetError;
use rusplay_scale::ScaleError;
use thiserror::Error;
use wayland_client::{ConnectError, DispatchError, globals::GlobalError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Receive,
    Decode,
    Present,
    Audio,
}

impl Stage {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Receive => "receive",
            Self::Decode => "decode",
            Self::Present => "present",
            Self::Audio => "audio",
        }
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Audio(#[from] AudioError),
    #[error(transparent)]
    Input(#[from] InputError),
    #[error(transparent)]
    Net(#[from] NetError),
    #[error(transparent)]
    Scale(#[from] ScaleError),
    #[error("failed to spawn the {stage} thread")]
    SpawnStage {
        stage: Stage,
        #[source]
        source: io::Error,
    },
    #[error("the {stage} thread panicked")]
    StagePanicked { stage: Stage },
    #[error("the {stage} thread ended before it was ready")]
    StageNotReady { stage: Stage },
    #[error("host speaks protocol version {theirs}, this client speaks {ours}")]
    VersionMismatch { ours: u16, theirs: u16 },
    #[error("host did not answer the hello within {0:?}")]
    HandshakeTimeout(Duration),
    #[error("host ended the session before answering the hello")]
    HandshakeAborted,
    #[error("host never acknowledged the hello")]
    HelloUndeliverable,
    #[error("nothing arrived from the host for {0:?}")]
    HostSilent(Duration),
    #[error("audio codec {codec} is not compiled into this client")]
    AudioCodecNotCompiled { codec: AudioCodec },
    #[error("failed to connect to the wayland compositor")]
    WaylandConnect(#[source] ConnectError),
    #[error("failed to list the wayland globals")]
    WaylandRegistry(#[source] GlobalError),
    #[error("the wayland connection failed while querying the display")]
    WaylandDispatch(#[source] DispatchError),
    #[error("the compositor does not implement wp_color_manager_v1")]
    ColorManagementUnsupported,
    #[error("the compositor advertises no outputs")]
    NoOutputs,
    #[error("no output is named {name}")]
    OutputNotFound { name: String },
    #[error("the compositor could not describe the output (cause {cause}): {message}")]
    ImageDescriptionFailed { cause: u32, message: String },
    #[error("the compositor ended the output description without all of its information")]
    ImageInformationIncomplete,
    #[error("the target black level {black_nits} cd/m² is not below the peak {peak_nits} cd/m²")]
    TargetBlackNotBelowPeak { black_nits: f64, peak_nits: f64 },
    #[error("decoded {fourcc} dma-buf frames cannot be presented without dma-buf import into wgpu")]
    DmaBufPresentUnsupported { fourcc: DrmFourcc },
    #[error("decoded frame has {found} planes, presentation needs {expected}")]
    PlaneCountMismatch { found: usize, expected: usize },
    #[error(
        "decoded plane {plane} is {found_width}x{found_height}, the frame layout expects {expected_width}x{expected_height}"
    )]
    PlaneShapeMismatch {
        plane: usize,
        found_width: u32,
        found_height: u32,
        expected_width: u32,
        expected_height: u32,
    },
    #[error("decoded plane {plane} holds {found} bytes, short of the {needed} its rows need")]
    PlaneTooShort {
        plane: usize,
        found: usize,
        needed: usize,
    },
}
