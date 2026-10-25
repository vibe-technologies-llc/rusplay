mod error;
mod opus;

pub use crate::{
    error::AudioError,
    opus::{OpusAudioDecoder, OpusAudioEncoder},
};

pub const CODEC_THREAD_STACK_BYTES: usize = 8 * 1024 * 1024;

pub trait AudioEncoder {
    fn frame_samples(&self) -> usize;

    fn encode(&mut self, pcm: &[f32], out: &mut [u8]) -> Result<usize, AudioError>;
}

pub trait AudioDecoder {
    fn decode(&mut self, packet: &[u8], pcm: &mut [f32]) -> Result<usize, AudioError>;
}
