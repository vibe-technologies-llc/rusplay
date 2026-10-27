use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError, Sender},
    },
    time::Duration,
};

use rusplay_audio::{AudioDecoder, AudioOutput, ConcealingPlayer, OpusAudioDecoder};
use rusplay_config::AudioConfig;
use rusplay_core::AudioCodec;

use crate::{ClientError, stats::Counters};

const IDLE_WAKE: Duration = Duration::from_millis(50);

#[cfg(feature = "pipewire")]
pub fn default_audio_output(
    config: &AudioConfig,
) -> Result<Option<Box<dyn AudioOutput>>, ClientError> {
    Ok(Some(Box::new(rusplay_audio::AudioPlayback::start(config)?)))
}

#[cfg(not(feature = "pipewire"))]
pub fn default_audio_output(
    _config: &AudioConfig,
) -> Result<Option<Box<dyn AudioOutput>>, ClientError> {
    Ok(None)
}

#[derive(Debug)]
pub(crate) struct EncodedAudio {
    pub number: u32,
    pub data: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct AudioLinks {
    pub queue: Receiver<EncodedAudio>,
    pub ready: Sender<Result<(), ClientError>>,
    pub stop: Arc<AtomicBool>,
    pub counters: Arc<Counters>,
}

pub(crate) fn open_decoder(config: &AudioConfig) -> Result<Box<dyn AudioDecoder>, ClientError> {
    match config.codec {
        AudioCodec::Opus => Ok(Box::new(OpusAudioDecoder::new(config)?)),
        #[cfg(feature = "flac")]
        AudioCodec::Flac => Ok(Box::new(rusplay_audio::FlacAudioDecoder::new(config)?)),
        #[cfg(not(feature = "flac"))]
        AudioCodec::Flac => Err(ClientError::AudioCodecNotCompiled {
            codec: config.codec,
        }),
    }
}

pub(crate) struct AudioPipeline {
    player: ConcealingPlayer,
    counters: Arc<Counters>,
}

impl AudioPipeline {
    pub(crate) fn new(
        decoder: Box<dyn AudioDecoder>,
        output: Box<dyn AudioOutput>,
        config: &AudioConfig,
        counters: Arc<Counters>,
    ) -> Self {
        Self {
            player: ConcealingPlayer::new(decoder, output, config),
            counters,
        }
    }

    pub(crate) fn process(&mut self, packet: &EncodedAudio) -> Result<(), ClientError> {
        let result = self.player.process(packet.number, &packet.data);
        self.publish_stats();

        Ok(result?)
    }

    fn publish_stats(&self) {
        let player = self.player.stats();
        let output = self.player.output_stats();

        self.counters.audio_packets_late.set(player.late);
        self.counters.audio_packets_lost.set(player.lost);
        self.counters.audio_frames_recovered.set(player.recovered);
        self.counters.audio_frames_concealed.set(player.concealed);
        self.counters.audio_decode_errors.set(player.decode_errors);
        self.counters.audio_underruns.set(output.underruns);
        self.counters.audio_trims.set(output.trims);
        self.counters.audio_overflows.set(output.overflows);
    }
}

pub(crate) fn run(
    config: &AudioConfig,
    output: Box<dyn AudioOutput>,
    links: &AudioLinks,
) -> Result<(), ClientError> {
    let decoder = match open_decoder(config) {
        Ok(decoder) => {
            links.ready.send(Ok(())).ok();
            decoder
        }
        Err(error) => {
            links.ready.send(Err(error)).ok();
            return Ok(());
        }
    };

    let mut pipeline = AudioPipeline::new(decoder, output, config, Arc::clone(&links.counters));
    let result = decode_loop(&mut pipeline, links);
    links.stop.store(true, Ordering::Relaxed);

    result
}

fn decode_loop(pipeline: &mut AudioPipeline, links: &AudioLinks) -> Result<(), ClientError> {
    loop {
        if links.stop.load(Ordering::Relaxed) {
            return Ok(());
        }

        match links.queue.recv_timeout(IDLE_WAKE) {
            Ok(packet) => pipeline.process(&packet)?,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use parking_lot::Mutex;
    use rusplay_audio::{
        AudioEncoder, AudioError, AudioOutputStats, CODEC_THREAD_STACK_BYTES, OpusAudioEncoder,
    };

    use super::*;

    const CHANNELS: usize = 2;
    const FRAME_SAMPLES: usize = 240;
    const PACKET_BUFFER: usize = 4000;
    const DECODED: u32 = 1000;
    const CONCEALED: u32 = 2000;
    const RECOVERED: u32 = 3000;

    fn marker(bits: u32) -> f32 {
        f32::from_bits(bits)
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Produced {
        Decoded(u32),
        Concealed,
        Recovered(u32),
    }

    struct ScriptedDecoder {
        fail_on: Option<u8>,
    }

    impl AudioDecoder for ScriptedDecoder {
        fn decode(&mut self, packet: &[u8], pcm: &mut [f32]) -> Result<usize, AudioError> {
            if self.fail_on == Some(packet[0]) {
                return Err(AudioError::OpusDecode("scripted failure"));
            }
            pcm[..FRAME_SAMPLES * CHANNELS].fill(marker(DECODED + u32::from(packet[0])));
            Ok(FRAME_SAMPLES)
        }

        fn conceal(&mut self, pcm: &mut [f32]) -> Result<usize, AudioError> {
            pcm[..FRAME_SAMPLES * CHANNELS].fill(marker(CONCEALED));
            Ok(FRAME_SAMPLES)
        }

        fn recover(&mut self, next_packet: &[u8], pcm: &mut [f32]) -> Result<usize, AudioError> {
            pcm[..FRAME_SAMPLES * CHANNELS].fill(marker(RECOVERED + u32::from(next_packet[0])));
            Ok(FRAME_SAMPLES)
        }
    }

    #[derive(Clone, Default)]
    struct Capture {
        submitted: Arc<Mutex<Vec<Vec<f32>>>>,
        broken: Arc<AtomicBool>,
    }

    impl AudioOutput for Capture {
        fn submit(&mut self, pcm: &[f32]) -> Result<usize, AudioError> {
            if self.broken.load(Ordering::Relaxed) {
                return Err(AudioError::UnalignedSamples {
                    channels: CHANNELS,
                    samples: pcm.len(),
                });
            }
            self.submitted.lock().push(pcm.to_vec());
            Ok(0)
        }

        fn stats(&self) -> AudioOutputStats {
            AudioOutputStats {
                underruns: 3,
                trims: 2,
                overflows: 1,
            }
        }
    }

    impl Capture {
        fn produced(&self) -> Vec<Produced> {
            self.submitted
                .lock()
                .iter()
                .map(|frame| {
                    assert_eq!(frame.len(), FRAME_SAMPLES * CHANNELS);
                    match frame[0].to_bits() {
                        CONCEALED => Produced::Concealed,
                        bits if bits >= RECOVERED => Produced::Recovered(bits - RECOVERED),
                        bits => Produced::Decoded(bits - DECODED),
                    }
                })
                .collect()
        }
    }

    fn packet(number: u32) -> EncodedAudio {
        EncodedAudio {
            number,
            data: vec![u8::try_from(number).unwrap()],
        }
    }

    fn pipeline(
        config: &AudioConfig,
        decoder: ScriptedDecoder,
    ) -> (AudioPipeline, Capture, Arc<Counters>) {
        let output = Capture::default();
        let counters = Arc::new(Counters::default());
        let pipeline = AudioPipeline::new(
            Box::new(decoder),
            Box::new(output.clone()),
            config,
            Arc::clone(&counters),
        );

        (pipeline, output, counters)
    }

    fn conceal_limit(ms: u16) -> AudioConfig {
        AudioConfig {
            max_conceal_ms: ms,
            ..AudioConfig::default()
        }
    }

    #[test]
    fn consecutive_packets_are_decoded_in_order() {
        let (mut pipeline, output, counters) =
            pipeline(&AudioConfig::default(), ScriptedDecoder { fail_on: None });

        for number in 10..13 {
            pipeline.process(&packet(number)).unwrap();
        }

        let stats = counters.snapshot();
        assert_eq!(
            output.produced(),
            [
                Produced::Decoded(10),
                Produced::Decoded(11),
                Produced::Decoded(12)
            ]
        );
        assert_eq!(stats.audio_packets_lost, 0);
        assert_eq!(stats.audio_frames_concealed, 0);
    }

    #[test]
    fn a_single_lost_packet_is_rebuilt_from_the_next_one() {
        let (mut pipeline, output, counters) =
            pipeline(&AudioConfig::default(), ScriptedDecoder { fail_on: None });

        pipeline.process(&packet(1)).unwrap();
        pipeline.process(&packet(3)).unwrap();

        let stats = counters.snapshot();
        assert_eq!(
            output.produced(),
            [
                Produced::Decoded(1),
                Produced::Recovered(3),
                Produced::Decoded(3)
            ]
        );
        assert_eq!(stats.audio_packets_lost, 1);
        assert_eq!(stats.audio_frames_recovered, 1);
        assert_eq!(stats.audio_frames_concealed, 0);
    }

    #[test]
    fn a_longer_gap_is_concealed_then_recovered_at_its_end() {
        let (mut pipeline, output, counters) =
            pipeline(&AudioConfig::default(), ScriptedDecoder { fail_on: None });

        pipeline.process(&packet(1)).unwrap();
        pipeline.process(&packet(5)).unwrap();

        let stats = counters.snapshot();
        assert_eq!(
            output.produced(),
            [
                Produced::Decoded(1),
                Produced::Concealed,
                Produced::Concealed,
                Produced::Recovered(5),
                Produced::Decoded(5),
            ]
        );
        assert_eq!(stats.audio_packets_lost, 3);
        assert_eq!(stats.audio_frames_concealed, 2);
    }

    #[test]
    fn concealment_stops_at_the_configured_limit() {
        let (mut pipeline, output, counters) =
            pipeline(&conceal_limit(10), ScriptedDecoder { fail_on: None });

        pipeline.process(&packet(1)).unwrap();
        pipeline.process(&packet(50)).unwrap();

        let stats = counters.snapshot();
        assert_eq!(
            output.produced(),
            [
                Produced::Decoded(1),
                Produced::Concealed,
                Produced::Concealed,
                Produced::Decoded(50),
            ]
        );
        assert_eq!(stats.audio_packets_lost, 48);
        assert_eq!(stats.audio_frames_recovered, 0);
    }

    #[test]
    fn a_zero_limit_disables_concealment() {
        let (mut pipeline, output, _counters) =
            pipeline(&conceal_limit(0), ScriptedDecoder { fail_on: None });

        pipeline.process(&packet(1)).unwrap();
        pipeline.process(&packet(3)).unwrap();

        assert_eq!(
            output.produced(),
            [Produced::Decoded(1), Produced::Decoded(3)]
        );
    }

    #[test]
    fn late_and_duplicate_packets_are_discarded() {
        let (mut pipeline, output, counters) =
            pipeline(&AudioConfig::default(), ScriptedDecoder { fail_on: None });

        pipeline.process(&packet(5)).unwrap();
        pipeline.process(&packet(5)).unwrap();
        pipeline.process(&packet(4)).unwrap();
        pipeline.process(&packet(6)).unwrap();

        assert_eq!(
            output.produced(),
            [Produced::Decoded(5), Produced::Decoded(6)]
        );
        assert_eq!(counters.snapshot().audio_packets_late, 2);
    }

    #[test]
    fn frame_numbers_may_wrap() {
        let (mut pipeline, output, counters) =
            pipeline(&AudioConfig::default(), ScriptedDecoder { fail_on: None });
        let wrapping = |number: u32| EncodedAudio {
            number,
            data: vec![1],
        };

        pipeline.process(&wrapping(u32::MAX)).unwrap();
        pipeline.process(&wrapping(0)).unwrap();

        assert_eq!(output.produced().len(), 2);
        assert_eq!(counters.snapshot().audio_packets_lost, 0);
    }

    #[test]
    fn a_corrupt_packet_is_concealed_and_counted() {
        let (mut pipeline, output, counters) = pipeline(
            &AudioConfig::default(),
            ScriptedDecoder { fail_on: Some(2) },
        );

        pipeline.process(&packet(1)).unwrap();
        pipeline.process(&packet(2)).unwrap();
        pipeline.process(&packet(3)).unwrap();

        let stats = counters.snapshot();
        assert_eq!(
            output.produced(),
            [
                Produced::Decoded(1),
                Produced::Concealed,
                Produced::Decoded(3)
            ]
        );
        assert_eq!(stats.audio_decode_errors, 1);
        assert_eq!(stats.audio_packets_lost, 0);
    }

    #[test]
    fn output_counters_are_published() {
        let (mut pipeline, _output, counters) =
            pipeline(&AudioConfig::default(), ScriptedDecoder { fail_on: None });

        pipeline.process(&packet(1)).unwrap();

        let stats = counters.snapshot();
        assert_eq!(
            (
                stats.audio_underruns,
                stats.audio_trims,
                stats.audio_overflows
            ),
            (3, 2, 1)
        );
    }

    #[test]
    fn a_failing_output_ends_the_stage_with_an_error() {
        let (mut pipeline, output, _counters) =
            pipeline(&AudioConfig::default(), ScriptedDecoder { fail_on: None });
        output.broken.store(true, Ordering::Relaxed);

        let result = pipeline.process(&packet(1));

        assert!(matches!(result, Err(ClientError::Audio(_))));
    }

    #[test]
    fn opus_packets_survive_a_lost_packet() {
        thread::Builder::new()
            .stack_size(CODEC_THREAD_STACK_BYTES)
            .spawn(opus_round_trip_with_loss)
            .unwrap()
            .join()
            .unwrap();
    }

    fn opus_round_trip_with_loss() {
        let config = AudioConfig::default();
        let mut encoder = OpusAudioEncoder::new(&config).unwrap();
        let output = Capture::default();
        let counters = Arc::new(Counters::default());
        let mut pipeline = AudioPipeline::new(
            open_decoder(&config).unwrap(),
            Box::new(output.clone()),
            &config,
            Arc::clone(&counters),
        );

        let mut packets = Vec::new();
        for number in 0..6_u32 {
            let tone: Vec<f32> = (0..FRAME_SAMPLES * CHANNELS)
                .map(|index| {
                    (f32::from(u16::try_from(index + number as usize * 7).unwrap()) * 0.05).sin()
                        * 0.5
                })
                .collect();
            let mut data = vec![0; PACKET_BUFFER];
            let written = encoder.encode(&tone, &mut data).unwrap();
            data.truncate(written);
            packets.push(EncodedAudio { number, data });
        }

        for packet in packets.iter().filter(|packet| packet.number != 3) {
            pipeline.process(packet).unwrap();
        }

        let stats = counters.snapshot();
        let submitted = output.submitted.lock();
        assert_eq!(submitted.len(), 6);
        assert!(
            submitted
                .iter()
                .all(|frame| frame.len() == FRAME_SAMPLES * CHANNELS)
        );
        assert!(submitted[3].iter().any(|sample| *sample != 0.0));
        assert_eq!(stats.audio_packets_lost, 1);
        assert_eq!(stats.audio_frames_recovered, 1);
        assert_eq!(stats.audio_decode_errors, 0);
    }
}
