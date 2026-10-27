use rusplay_config::AudioConfig;

use crate::{AudioDecoder, AudioError};

const MILLIS_PER_SECOND: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AudioOutputStats {
    pub underruns: u64,
    pub trims: u64,
    pub overflows: u64,
}

pub trait AudioOutput: Send + 'static {
    fn submit(&mut self, pcm: &[f32]) -> Result<usize, AudioError>;

    fn stats(&self) -> AudioOutputStats;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlayerStats {
    pub late: u64,
    pub lost: u64,
    pub recovered: u64,
    pub concealed: u64,
    pub decode_errors: u64,
}

pub struct ConcealingPlayer {
    decoder: Box<dyn AudioDecoder>,
    output: Box<dyn AudioOutput>,
    pcm: Vec<f32>,
    channels: usize,
    max_conceal_frames: u32,
    expected: Option<u32>,
    stats: PlayerStats,
}

impl ConcealingPlayer {
    pub fn new(
        decoder: Box<dyn AudioDecoder>,
        output: Box<dyn AudioOutput>,
        config: &AudioConfig,
    ) -> Self {
        let channels = usize::from(config.channels.channel_count());
        let frame_samples = config.frame_samples().max(1);
        let conceal_samples = config.sample_rate.hz() as usize * usize::from(config.max_conceal_ms)
            / MILLIS_PER_SECOND;
        let max_conceal_frames = u32::try_from(conceal_samples / frame_samples).unwrap_or(u32::MAX);

        Self {
            decoder,
            output,
            pcm: vec![0.0; frame_samples * channels],
            channels,
            max_conceal_frames,
            expected: None,
            stats: PlayerStats::default(),
        }
    }

    pub fn stats(&self) -> PlayerStats {
        self.stats
    }

    pub fn output_stats(&self) -> AudioOutputStats {
        self.output.stats()
    }

    pub fn process(&mut self, number: u32, packet: &[u8]) -> Result<(), AudioError> {
        let gap = self
            .expected
            .map_or(0, |expected| number.wrapping_sub(expected).cast_signed());
        if gap < 0 {
            self.stats.late += 1;
            return Ok(());
        }
        self.expected = Some(number.wrapping_add(1));

        let lost = gap.cast_unsigned();
        self.stats.lost += u64::from(lost);
        self.bridge_gap(lost, packet)?;
        self.decode(packet)
    }

    fn bridge_gap(&mut self, lost: u32, next_packet: &[u8]) -> Result<(), AudioError> {
        for index in 0..lost.min(self.max_conceal_frames) {
            let produced = if index + 1 == lost {
                self.stats.recovered += 1;
                self.decoder.recover(next_packet, &mut self.pcm)
            } else {
                self.stats.concealed += 1;
                self.decoder.conceal(&mut self.pcm)
            };

            match produced {
                Ok(samples) => self.emit(samples)?,
                Err(_) => self.stats.decode_errors += 1,
            }
        }

        Ok(())
    }

    fn decode(&mut self, packet: &[u8]) -> Result<(), AudioError> {
        if let Ok(samples) = self.decoder.decode(packet, &mut self.pcm) {
            return self.emit(samples);
        }

        self.stats.decode_errors += 1;
        if let Ok(samples) = self.decoder.conceal(&mut self.pcm) {
            self.stats.concealed += 1;
            self.emit(samples)?;
        }

        Ok(())
    }

    fn emit(&mut self, samples: usize) -> Result<(), AudioError> {
        let len = (samples * self.channels).min(self.pcm.len());
        self.output.submit(&self.pcm[..len])?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::Mutex;

    use super::*;

    const FRAME_SAMPLES: usize = 480;
    const DECODED: u32 = 1000;
    const CONCEALED: u32 = 2000;
    const RECOVERED: u32 = 3000;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Produced {
        Decoded(u32),
        Concealed,
        Recovered(u32),
    }

    struct ScriptedDecoder;

    impl AudioDecoder for ScriptedDecoder {
        fn decode(&mut self, packet: &[u8], pcm: &mut [f32]) -> Result<usize, AudioError> {
            pcm.fill(f32::from_bits(DECODED + u32::from(packet[0])));
            Ok(FRAME_SAMPLES)
        }

        fn conceal(&mut self, pcm: &mut [f32]) -> Result<usize, AudioError> {
            pcm.fill(f32::from_bits(CONCEALED));
            Ok(FRAME_SAMPLES)
        }

        fn recover(&mut self, next_packet: &[u8], pcm: &mut [f32]) -> Result<usize, AudioError> {
            pcm.fill(f32::from_bits(RECOVERED + u32::from(next_packet[0])));
            Ok(FRAME_SAMPLES)
        }
    }

    #[derive(Clone, Default)]
    struct Capture {
        frames: Arc<Mutex<Vec<Vec<f32>>>>,
    }

    impl AudioOutput for Capture {
        fn submit(&mut self, pcm: &[f32]) -> Result<usize, AudioError> {
            self.frames.lock().push(pcm.to_vec());
            Ok(0)
        }

        fn stats(&self) -> AudioOutputStats {
            AudioOutputStats::default()
        }
    }

    impl Capture {
        fn produced(&self) -> Vec<Produced> {
            self.frames
                .lock()
                .iter()
                .map(|frame| match frame[0].to_bits() {
                    CONCEALED => Produced::Concealed,
                    bits if bits >= RECOVERED => Produced::Recovered(bits - RECOVERED),
                    bits => Produced::Decoded(bits - DECODED),
                })
                .collect()
        }
    }

    fn player(config: &AudioConfig) -> (ConcealingPlayer, Capture) {
        let output = Capture::default();
        let player =
            ConcealingPlayer::new(Box::new(ScriptedDecoder), Box::new(output.clone()), config);

        (player, output)
    }

    fn mono() -> AudioConfig {
        AudioConfig::default().microphone_stream()
    }

    fn feed(player: &mut ConcealingPlayer, number: u32) {
        player
            .process(number, &[u8::try_from(number).unwrap()])
            .unwrap();
    }

    #[test]
    fn frames_are_sized_by_the_stream_config() {
        let (mut player, output) = player(&mono());

        feed(&mut player, 1);

        assert_eq!(output.frames.lock()[0].len(), 480);
    }

    #[test]
    fn a_lost_packet_is_rebuilt_from_the_next_one() {
        let (mut player, output) = player(&mono());

        feed(&mut player, 1);
        feed(&mut player, 3);

        assert_eq!(
            output.produced(),
            [
                Produced::Decoded(1),
                Produced::Recovered(3),
                Produced::Decoded(3)
            ]
        );
        assert_eq!(player.stats().lost, 1);
        assert_eq!(player.stats().recovered, 1);
    }

    #[test]
    fn a_longer_gap_is_concealed_up_to_the_configured_limit() {
        let config = AudioConfig {
            max_conceal_ms: 20,
            ..mono()
        };
        let (mut player, output) = player(&config);

        feed(&mut player, 1);
        feed(&mut player, 50);

        assert_eq!(
            output.produced(),
            [
                Produced::Decoded(1),
                Produced::Concealed,
                Produced::Concealed,
                Produced::Decoded(50),
            ]
        );
        assert_eq!(player.stats().lost, 48);
    }

    #[test]
    fn late_and_duplicate_packets_are_discarded() {
        let (mut player, output) = player(&mono());

        feed(&mut player, 5);
        feed(&mut player, 5);
        feed(&mut player, 4);
        feed(&mut player, 6);

        assert_eq!(
            output.produced(),
            [Produced::Decoded(5), Produced::Decoded(6)]
        );
        assert_eq!(player.stats().late, 2);
    }

    #[test]
    fn numbers_may_wrap() {
        let (mut player, output) = player(&mono());

        player.process(u32::MAX, &[1]).unwrap();
        player.process(0, &[1]).unwrap();

        assert_eq!(output.produced().len(), 2);
        assert_eq!(player.stats().lost, 0);
    }
}
