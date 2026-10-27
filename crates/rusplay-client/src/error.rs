use std::{fmt, io, path::PathBuf, time::Duration};

use rusplay_audio::AudioError;
use rusplay_config::{ConfigError, OutputColorSpace, OutputFormat, PresentMode};
use rusplay_core::{AudioCodec, FormatSet};
use rusplay_decode::DecodeError;
use rusplay_input::InputError;
use rusplay_net::NetError;
use rusplay_protocol::ProtocolError;
use rusplay_scale::ScaleError;
use thiserror::Error;
use wayland_client::{
    ConnectError, DispatchError,
    backend::WaylandError,
    globals::{BindError, GlobalError},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Receive,
    Decode,
    Present,
    Audio,
    Microphone,
    Input,
    Gamepad,
    Window,
}

impl Stage {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Receive => "receive",
            Self::Decode => "decode",
            Self::Present => "present",
            Self::Audio => "audio",
            Self::Microphone => "microphone",
            Self::Input => "input",
            Self::Gamepad => "gamepad",
            Self::Window => "window",
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
    Protocol(#[from] ProtocolError),
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
    #[error(
        "the host encodes {encodable} and this client decodes {decodable}, with no format in common"
    )]
    NoCommonFormat {
        decodable: FormatSet,
        encodable: FormatSet,
    },
    #[error("host ended the session before answering the hello")]
    HandshakeAborted,
    #[error("host never acknowledged the hello")]
    HelloUndeliverable,
    #[error("nothing arrived from the host for {0:?}")]
    HostSilent(Duration),
    #[error("audio codec {codec} is not compiled into this client")]
    AudioCodecNotCompiled { codec: AudioCodec },
    #[error("the microphone needs the pipewire feature, or audio.microphone.enabled turned off")]
    MicrophoneNotCompiled,
    #[error("failed to read the host book {path}")]
    HostBookRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to write the host book {path}")]
    HostBookWrite {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("host book {path} is corrupt at line {line}")]
    HostBookCorrupt { path: PathBuf, line: usize },
    #[error("failed to render a host profile for the host book")]
    HostProfileRender(#[source] ConfigError),
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
    #[error("decoded dma-buf plane {plane} names object {object}, which the frame does not have")]
    DmaBufObjectMissing { plane: usize, object: usize },
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
    #[error("the window handle does not name a live wayland surface")]
    WindowHandleInvalid,
    #[error("the compositor does not offer {interface}, which the stream surface needs")]
    WindowGlobalMissing {
        interface: &'static str,
        #[source]
        source: BindError,
    },
    #[error("the wayland connection failed while managing the stream surface")]
    WindowDispatch(#[source] DispatchError),
    #[error("the wayland connection failed while following the stream surface's output")]
    OutputFollowRead(#[source] WaylandError),
    #[error("waiting on the wayland connection failed while following the stream surface's output")]
    OutputFollowPoll(#[source] io::Error),
    #[error("failed to create a gpu surface for the stream")]
    WindowSurface(#[source] wgpu::CreateSurfaceError),
    #[error("no gpu adapter can present to the stream surface")]
    WindowAdapter(#[source] wgpu::RequestAdapterError),
    #[error("failed to open the gpu device for the stream")]
    WindowDevice(#[source] wgpu::RequestDeviceError),
    #[error(
        "the stream surface offers no {format:?} swapchain in the {color_space:?} colour space"
    )]
    SurfaceOutputUnsupported {
        color_space: OutputColorSpace,
        format: OutputFormat,
    },
    #[error("the stream surface does not offer the {mode:?} present mode")]
    PresentModeUnsupported { mode: PresentMode },
    #[error("the stream surface was lost")]
    SurfaceLost,
    #[error("the gpu rejected the stream surface texture")]
    SurfaceValidation,
}
