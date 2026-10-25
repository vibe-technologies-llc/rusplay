use rusplay_core::{ChannelLayout, SampleRate};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("opus cannot carry {0:?} without multistream support")]
    OpusLayoutUnsupported(ChannelLayout),
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
}
