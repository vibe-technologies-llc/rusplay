use rusplay_core::{ChannelLayout, SampleRate};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("{0:?} is not a multistream layout")]
    OpusNotMultistream(ChannelLayout),
    #[error("opus stream {stream} produced a packet that cannot be self-delimited")]
    OpusSubPacketMalformed { stream: usize },
    #[error("opus packet needs {needed} bytes but the buffer holds {capacity}")]
    OpusPacketBufferTooSmall { needed: usize, capacity: usize },
    #[error("opus cannot run at {} Hz", .0.hz())]
    OpusSampleRateUnsupported(SampleRate),
    #[error("opus encoder setup failed: {0}")]
    OpusEncoderInit(&'static str),
    #[error("opus decoder setup failed: {0}")]
    OpusDecoderInit(&'static str),
    #[error("opus encode failed: {0}")]
    OpusEncode(&'static str),
    #[error("opus decode failed: {0}")]
    OpusDecode(&'static str),
    #[error("expected {expected} interleaved samples, got {found}")]
    FrameLengthMismatch { expected: usize, found: usize },
    #[error("resampler setup rejected")]
    ResamplerInit(#[source] rubato::ResamplerConstructionError),
    #[error("resampling failed")]
    Resample(#[source] rubato::ResampleError),
    #[error("resampler buffers have inconsistent sizes")]
    ResampleBuffer(#[source] rubato::audioadapter_buffers::SizeError),
    #[error("{samples} samples do not divide into {channels} channels")]
    UnalignedSamples { channels: usize, samples: usize },
    #[cfg(feature = "flac")]
    #[error("flac encoder settings rejected")]
    FlacSettings(#[source] flacenc::error::VerifyError),
    #[cfg(feature = "flac")]
    #[error("flac encoder could not read the samples")]
    FlacSource(#[source] flacenc::error::SourceError),
    #[cfg(feature = "flac")]
    #[error("flac encode failed")]
    FlacEncode(#[source] flacenc::error::EncodeError),
    #[cfg(feature = "flac")]
    #[error("flac frame could not be serialised")]
    FlacWrite(#[source] flacenc::error::OutputError<flacenc::bitsink::ByteSink>),
    #[cfg(feature = "flac")]
    #[error("flac frame needs {needed} bytes but the packet buffer holds {capacity}")]
    FlacPacketBufferTooSmall { needed: usize, capacity: usize },
    #[cfg(feature = "flac")]
    #[error("flac decode failed")]
    FlacDecode(#[source] claxon::Error),
    #[cfg(feature = "flac")]
    #[error("flac packet holds no frame")]
    FlacEmptyPacket,
    #[cfg(feature = "flac")]
    #[error("flac frame has {found} channels, expected {expected}")]
    FlacChannelMismatch { expected: usize, found: usize },
}
