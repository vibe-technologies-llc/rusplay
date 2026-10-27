use std::time::Duration;

#[cfg(feature = "pipewire")]
mod capture;
mod error;
#[cfg(feature = "flac")]
mod flac;
mod multistream;
mod opus;
#[cfg(feature = "pipewire")]
mod playback;
mod player;
#[cfg(feature = "pipewire")]
mod pw_common;
mod resample;
#[cfg(feature = "pipewire")]
mod ring;

#[cfg(feature = "flac")]
pub use crate::flac::{FlacAudioDecoder, FlacAudioEncoder};
#[cfg(feature = "pipewire")]
pub use crate::{
    capture::{AudioCapture, CaptureTarget},
    playback::{AudioPlayback, PlaybackTarget},
};
pub use crate::{
    error::AudioError,
    opus::{OpusAudioDecoder, OpusAudioEncoder},
    player::{AudioOutput, AudioOutputStats, ConcealingPlayer, PlayerStats},
    resample::PcmResampler,
};

pub const CODEC_THREAD_STACK_BYTES: usize = 8 * 1024 * 1024;

pub trait AudioEncoder {
    fn frame_samples(&self) -> usize;

    fn encode(&mut self, pcm: &[f32], out: &mut [u8]) -> Result<usize, AudioError>;
}

pub trait AudioDecoder {
    fn decode(&mut self, packet: &[u8], pcm: &mut [f32]) -> Result<usize, AudioError>;

    fn conceal(&mut self, pcm: &mut [f32]) -> Result<usize, AudioError>;

    fn recover(&mut self, next_packet: &[u8], pcm: &mut [f32]) -> Result<usize, AudioError> {
        let _ = next_packet;
        self.conceal(pcm)
    }
}

pub trait AudioInput: Send + 'static {
    fn read_frame(&mut self, frame: &mut [f32], timeout: Duration) -> Result<bool, AudioError>;

    fn overruns(&self) -> u64 {
        0
    }
}
