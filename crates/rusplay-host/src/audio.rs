use std::{net::SocketAddr, sync::Arc, time::Duration};

use rusplay_audio::{AudioEncoder, AudioError, OpusAudioEncoder};
use rusplay_config::AudioConfig;
use rusplay_core::AudioCodec;
use rusplay_net::{FecScheme, Packetizer, Transport};
use rusplay_protocol::StreamKind;

use crate::{HostError, datagrams, shared::Shared};

const READ_TIMEOUT: Duration = Duration::from_millis(20);
const FEC_BLOCK_DATA_SHARDS: u8 = 32;
const BYTES_PER_SAMPLE: usize = size_of::<f32>();
const PACKET_HEADROOM_BYTES: usize = 1024;
const MIN_PACKET_BUFFER_BYTES: usize = 8192;

pub trait AudioSource {
    fn read_frame(&mut self, frame: &mut [f32], timeout: Duration) -> Result<bool, AudioError>;

    fn overruns(&self) -> u64 {
        0
    }
}

pub type AudioSourceFactory =
    Arc<dyn Fn(&AudioConfig) -> Result<Box<dyn AudioSource>, HostError> + Send + Sync>;

#[cfg(feature = "pipewire")]
impl AudioSource for rusplay_audio::AudioCapture {
    fn read_frame(&mut self, frame: &mut [f32], timeout: Duration) -> Result<bool, AudioError> {
        rusplay_audio::AudioCapture::read_frame(self, frame, timeout)
    }

    fn overruns(&self) -> u64 {
        rusplay_audio::AudioCapture::overruns(self)
    }
}

#[cfg(feature = "pipewire")]
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn default_audio_source() -> Option<AudioSourceFactory> {
    Some(Arc::new(|config| {
        Ok(Box::new(rusplay_audio::AudioCapture::start(config)?) as Box<dyn AudioSource>)
    }))
}

#[cfg(not(feature = "pipewire"))]
pub(crate) fn default_audio_source() -> Option<AudioSourceFactory> {
    None
}

pub(crate) struct AudioLinks {
    pub shared: Arc<Shared>,
    pub transport: Arc<Transport>,
    pub client: SocketAddr,
}

pub(crate) fn run(
    config: AudioConfig,
    factory: &AudioSourceFactory,
    links: &AudioLinks,
) -> Result<(), HostError> {
    let result = serve(&config, factory, links);
    links.shared.stop();

    result
}

fn open_encoder(config: &AudioConfig) -> Result<Box<dyn AudioEncoder>, HostError> {
    match config.codec {
        AudioCodec::Opus => Ok(Box::new(OpusAudioEncoder::new(config)?)),
        #[cfg(feature = "flac")]
        AudioCodec::Flac => Ok(Box::new(rusplay_audio::FlacAudioEncoder::new(config)?)),
        #[cfg(not(feature = "flac"))]
        AudioCodec::Flac => Err(HostError::AudioCodecNotCompiled {
            codec: config.codec,
        }),
    }
}

fn serve(
    config: &AudioConfig,
    factory: &AudioSourceFactory,
    links: &AudioLinks,
) -> Result<(), HostError> {
    let mut encoder = open_encoder(config)?;
    let mut source = factory(config)?;
    let mut packetizer = Packetizer::new(
        StreamKind::Audio,
        links.transport.max_datagram_len(&links.client),
        FecScheme::for_overhead(FEC_BLOCK_DATA_SHARDS, config.fec_overhead_percent),
    )?;

    let frame_len = encoder.frame_samples() * usize::from(config.channels.channel_count());
    let mut pcm = vec![0.0; frame_len];
    let mut packet = vec![0; packet_buffer_len(frame_len)];
    let mut number = 0u32;
    let counters = &links.shared.counters;

    while !links.shared.stopped() {
        if !source.read_frame(&mut pcm, READ_TIMEOUT)? {
            continue;
        }
        counters.audio_overruns.set(source.overruns());
        if !links.shared.streaming() {
            continue;
        }

        let written = encoder.encode(&pcm, &mut packet)?;
        if written == 0 {
            continue;
        }

        let datagrams = packetizer.packetize(number, false, &packet[..written])?;
        datagrams::send_all(&links.transport, links.client, &datagrams)?;
        number = number.wrapping_add(1);
        counters.audio_packets_sent.increment();
    }

    Ok(())
}

fn packet_buffer_len(frame_len: usize) -> usize {
    (frame_len * BYTES_PER_SAMPLE + PACKET_HEADROOM_BYTES).max(MIN_PACKET_BUFFER_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_packet_buffer_holds_a_frame_of_raw_samples() {
        assert_eq!(packet_buffer_len(480), MIN_PACKET_BUFFER_BYTES);
        assert_eq!(packet_buffer_len(32767 * 8), 32767 * 8 * 4 + 1024);
    }
}
