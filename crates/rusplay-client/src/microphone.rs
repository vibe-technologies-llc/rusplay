use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    time::Duration,
};

use rusplay_audio::{AudioEncoder, AudioInput, OpusAudioEncoder};
use rusplay_config::AudioConfig;
use rusplay_net::{FecScheme, Packetizer, Transport};
use rusplay_protocol::StreamKind;

use crate::{ClientError, stats::Counters};

const READ_TIMEOUT: Duration = Duration::from_millis(20);
const FEC_BLOCK_DATA_SHARDS: u8 = 32;
const BYTES_PER_SAMPLE: usize = size_of::<f32>();
const PACKET_HEADROOM_BYTES: usize = 1024;

#[cfg(feature = "pipewire")]
pub fn default_microphone(
    config: &AudioConfig,
) -> Result<Option<Box<dyn AudioInput>>, ClientError> {
    if !config.microphone.enabled {
        return Ok(None);
    }

    let capture = rusplay_audio::AudioCapture::start_from(
        &config.microphone_stream(),
        rusplay_audio::CaptureTarget::Source,
    )?;

    Ok(Some(Box::new(capture)))
}

#[cfg(not(feature = "pipewire"))]
pub fn default_microphone(
    config: &AudioConfig,
) -> Result<Option<Box<dyn AudioInput>>, ClientError> {
    if config.microphone.enabled {
        return Err(ClientError::MicrophoneNotCompiled);
    }

    Ok(None)
}

#[derive(Debug)]
pub(crate) struct MicrophoneLinks {
    pub transport: Arc<Transport>,
    pub host: SocketAddr,
    pub ready: Sender<Result<(), ClientError>>,
    pub stop: Arc<AtomicBool>,
    pub counters: Arc<Counters>,
}

pub(crate) struct MicrophoneEncoder {
    encoder: OpusAudioEncoder,
    packetizer: Packetizer,
    pcm: Vec<f32>,
    packet: Vec<u8>,
    number: u32,
}

impl MicrophoneEncoder {
    pub(crate) fn new(stream: &AudioConfig, max_datagram: usize) -> Result<Self, ClientError> {
        let encoder = OpusAudioEncoder::new(stream)?;
        let packetizer = Packetizer::new(
            StreamKind::Microphone,
            max_datagram,
            FecScheme::for_overhead(FEC_BLOCK_DATA_SHARDS, stream.fec_overhead_percent),
        )?;
        let frame_len = encoder.frame_samples() * usize::from(stream.channels.channel_count());

        Ok(Self {
            encoder,
            packetizer,
            pcm: vec![0.0; frame_len],
            packet: vec![0; frame_len * BYTES_PER_SAMPLE + PACKET_HEADROOM_BYTES],
            number: 0,
        })
    }

    pub(crate) fn next_datagrams(
        &mut self,
        source: &mut dyn AudioInput,
        timeout: Duration,
    ) -> Result<Option<Vec<Vec<u8>>>, ClientError> {
        if !source.read_frame(&mut self.pcm, timeout)? {
            return Ok(None);
        }

        let written = self.encoder.encode(&self.pcm, &mut self.packet)?;
        if written == 0 {
            return Ok(None);
        }

        let datagrams = self
            .packetizer
            .packetize(self.number, false, &self.packet[..written])?;
        self.number = self.number.wrapping_add(1);

        Ok(Some(datagrams))
    }
}

pub(crate) fn run(
    config: &AudioConfig,
    mut source: Box<dyn AudioInput>,
    links: &MicrophoneLinks,
) -> Result<(), ClientError> {
    let stream = config.microphone_stream();
    let max_datagram = links.transport.max_datagram_len(&links.host);
    let mut encoder = match MicrophoneEncoder::new(&stream, max_datagram) {
        Ok(encoder) => {
            links.ready.send(Ok(())).ok();
            encoder
        }
        Err(error) => {
            links.ready.send(Err(error)).ok();
            return Ok(());
        }
    };

    let result = serve(&mut encoder, source.as_mut(), links);
    links.stop.store(true, Ordering::Relaxed);

    result
}

fn serve(
    encoder: &mut MicrophoneEncoder,
    source: &mut dyn AudioInput,
    links: &MicrophoneLinks,
) -> Result<(), ClientError> {
    while !links.stop.load(Ordering::Relaxed) {
        let datagrams = encoder.next_datagrams(source, READ_TIMEOUT)?;
        links.counters.microphone_overruns.set(source.overruns());

        let Some(datagrams) = datagrams else {
            continue;
        };
        for datagram in &datagrams {
            links.transport.send_to(datagram, links.host)?;
        }
        links.counters.microphone_packets_sent.increment();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{thread, time::Instant};

    use rusplay_audio::{AudioDecoder, AudioError, CODEC_THREAD_STACK_BYTES, OpusAudioDecoder};
    use rusplay_net::{Reassembler, ReassemblyEvent, ReassemblyLimits};

    use super::*;

    const MAX_DATAGRAM: usize = 1200;
    const AMPLITUDE: f32 = 0.5;
    const TONE_STEP: f32 = 0.05;
    const FRAMES: u32 = 8;

    struct ToneSource {
        position: usize,
        silent_reads: usize,
    }

    impl AudioInput for ToneSource {
        fn read_frame(
            &mut self,
            frame: &mut [f32],
            _timeout: Duration,
        ) -> Result<bool, AudioError> {
            if self.silent_reads > 0 {
                self.silent_reads -= 1;
                return Ok(false);
            }
            for sample in frame.iter_mut() {
                *sample = (f32::from(u16::try_from(self.position).unwrap()) * TONE_STEP).sin()
                    * AMPLITUDE;
                self.position += 1;
            }
            Ok(true)
        }

        fn overruns(&self) -> u64 {
            9
        }
    }

    struct BrokenSource;

    impl AudioInput for BrokenSource {
        fn read_frame(
            &mut self,
            frame: &mut [f32],
            _timeout: Duration,
        ) -> Result<bool, AudioError> {
            Err(AudioError::FrameLengthMismatch {
                expected: 0,
                found: frame.len(),
            })
        }
    }

    fn on_codec_thread(test: impl FnOnce() + Send + 'static) {
        thread::Builder::new()
            .stack_size(CODEC_THREAD_STACK_BYTES)
            .spawn(test)
            .unwrap()
            .join()
            .unwrap();
    }

    fn stream() -> AudioConfig {
        AudioConfig::default().microphone_stream()
    }

    #[test]
    fn frames_become_numbered_microphone_datagrams_that_decode_back() {
        on_codec_thread(|| {
            let stream = stream();
            let mut encoder = MicrophoneEncoder::new(&stream, MAX_DATAGRAM).unwrap();
            let mut source = ToneSource {
                position: 0,
                silent_reads: 0,
            };
            let mut reassembler =
                Reassembler::new(StreamKind::Microphone, ReassemblyLimits::default());
            let mut events = Vec::new();

            for _ in 0..FRAMES {
                let datagrams = encoder
                    .next_datagrams(&mut source, Duration::ZERO)
                    .unwrap()
                    .unwrap();
                for datagram in &datagrams {
                    reassembler
                        .push(datagram, Instant::now(), &mut events)
                        .unwrap();
                }
            }

            let packets: Vec<_> = events
                .into_iter()
                .filter_map(|event| match event {
                    ReassemblyEvent::Frame(frame) => Some(frame),
                    _ => None,
                })
                .collect();
            let mut decoder = OpusAudioDecoder::new(&stream).unwrap();
            let mut pcm = vec![0.0; stream.frame_samples()];
            let mut peak = 0.0f32;
            for packet in &packets {
                decoder.decode(&packet.data, &mut pcm).unwrap();
                peak = pcm.iter().fold(peak, |peak, sample| peak.max(sample.abs()));
            }

            assert_eq!(
                packets
                    .iter()
                    .map(|packet| packet.frame)
                    .collect::<Vec<_>>(),
                (0..FRAMES).collect::<Vec<_>>()
            );
            assert!(peak > AMPLITUDE / 2.0);
        });
    }

    #[test]
    fn a_starved_source_yields_no_datagram_and_does_not_consume_a_number() {
        on_codec_thread(|| {
            let mut encoder = MicrophoneEncoder::new(&stream(), MAX_DATAGRAM).unwrap();
            let mut source = ToneSource {
                position: 0,
                silent_reads: 1,
            };

            let starved = encoder.next_datagrams(&mut source, Duration::ZERO).unwrap();
            let fed = encoder
                .next_datagrams(&mut source, Duration::ZERO)
                .unwrap()
                .unwrap();

            assert!(starved.is_none());
            let (header, _) = rusplay_protocol::PacketHeader::parse(&fed[0]).unwrap();
            assert_eq!(header.stream, StreamKind::Microphone);
            assert_eq!(header.frame, 0);
        });
    }

    #[test]
    fn a_failing_source_is_an_error() {
        on_codec_thread(|| {
            let mut encoder = MicrophoneEncoder::new(&stream(), MAX_DATAGRAM).unwrap();

            let result = encoder.next_datagrams(&mut BrokenSource, Duration::ZERO);

            assert!(matches!(result, Err(ClientError::Audio(_))));
        });
    }
}
