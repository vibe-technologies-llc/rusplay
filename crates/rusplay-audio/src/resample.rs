use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction, audioadapter_buffers::direct::InterleavedSlice,
};

use crate::AudioError;

const CHUNK_INPUT_FRAMES: usize = 120;
const SINC_LENGTH: usize = 64;
const OVERSAMPLING_FACTOR: usize = 128;
const FIXED_RATIO_RELATIVE: f64 = 1.0;

struct Converter {
    resampler: Async<f32>,
    pending: Vec<f32>,
    scratch: Vec<f32>,
}

pub struct PcmResampler {
    converter: Option<Converter>,
    channels: usize,
}

impl PcmResampler {
    pub fn new(input_hz: u32, output_hz: u32, channels: u8) -> Result<Self, AudioError> {
        let channels = usize::from(channels);

        if input_hz == output_hz {
            return Ok(Self {
                converter: None,
                channels,
            });
        }

        let parameters = SincInterpolationParameters {
            sinc_len: SINC_LENGTH,
            f_cutoff: None,
            oversampling_factor: OVERSAMPLING_FACTOR,
            interpolation: SincInterpolationType::Linear,
            window: WindowFunction::BlackmanHarris2,
        };
        let ratio = f64::from(output_hz) / f64::from(input_hz);

        let resampler = Async::<f32>::new_sinc(
            ratio,
            FIXED_RATIO_RELATIVE,
            &parameters,
            CHUNK_INPUT_FRAMES,
            channels,
            FixedAsync::Input,
        )
        .map_err(AudioError::ResamplerInit)?;
        let scratch = vec![0.0; resampler.output_frames_max() * channels];

        Ok(Self {
            converter: Some(Converter {
                resampler,
                pending: Vec::new(),
                scratch,
            }),
            channels,
        })
    }

    pub fn is_passthrough(&self) -> bool {
        self.converter.is_none()
    }

    pub fn delay_frames(&self) -> usize {
        self.converter
            .as_ref()
            .map_or(0, |converter| converter.resampler.output_delay())
    }

    pub fn reset(&mut self) {
        if let Some(converter) = &mut self.converter {
            converter.resampler.reset();
            converter.pending.clear();
        }
    }

    pub fn push(&mut self, interleaved: &[f32], out: &mut Vec<f32>) -> Result<(), AudioError> {
        if !interleaved.len().is_multiple_of(self.channels) {
            return Err(AudioError::UnalignedSamples {
                channels: self.channels,
                samples: interleaved.len(),
            });
        }

        let Some(converter) = &mut self.converter else {
            out.extend_from_slice(interleaved);
            return Ok(());
        };

        converter.pending.extend_from_slice(interleaved);

        loop {
            let frames_in = converter.resampler.input_frames_next();
            let samples_in = frames_in * self.channels;
            if converter.pending.len() < samples_in {
                return Ok(());
            }

            let input =
                InterleavedSlice::new(&converter.pending[..samples_in], self.channels, frames_in)
                    .map_err(AudioError::ResampleBuffer)?;
            let frames_out_capacity = converter.scratch.len() / self.channels;
            let mut output = InterleavedSlice::new_mut(
                &mut converter.scratch,
                self.channels,
                frames_out_capacity,
            )
            .map_err(AudioError::ResampleBuffer)?;

            let (consumed, produced) = converter
                .resampler
                .process_into_buffer(&input, &mut output, None)
                .map_err(AudioError::Resample)?;

            out.extend_from_slice(&converter.scratch[..produced * self.channels]);
            converter.pending.drain(..consumed * self.channels);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;

    use super::*;

    const TONE_HZ: f64 = 1_000.0;
    const OUTPUT_HZ: u32 = 48_000;
    const SECONDS: usize = 2;

    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn tone(hz: u32, channels: usize, seconds: usize) -> Vec<f32> {
        let frames = hz as usize * seconds;
        let mut samples = Vec::with_capacity(frames * channels);
        for frame in 0..frames {
            let cycles = TONE_HZ * frame as f64 / f64::from(hz);
            let value = (TAU * cycles).sin() as f32 * 0.5;
            samples.extend(std::iter::repeat_n(value, channels));
        }
        samples
    }

    fn resample_in_odd_chunks(
        input_hz: u32,
        channels: usize,
        input: &[f32],
    ) -> (PcmResampler, Vec<f32>) {
        let mut resampler =
            PcmResampler::new(input_hz, OUTPUT_HZ, u8::try_from(channels).unwrap()).unwrap();
        let mut output = Vec::new();

        let chunk_samples = 333 * channels;
        for chunk in input.chunks(chunk_samples) {
            resampler.push(chunk, &mut output).unwrap();
        }

        (resampler, output)
    }

    fn rising_zero_crossings(samples: &[f32], channels: usize) -> usize {
        let first_channel: Vec<f32> = samples.iter().step_by(channels).copied().collect();
        first_channel
            .windows(2)
            .filter(|pair| pair[0] < 0.0 && pair[1] >= 0.0)
            .count()
    }

    fn peak(samples: &[f32]) -> f32 {
        samples
            .iter()
            .fold(0.0, |peak, sample| peak.max(sample.abs()))
    }

    #[allow(clippy::cast_precision_loss)]
    fn assert_keeps_frequency(input_hz: u32, channels: usize) {
        let input = tone(input_hz, channels, SECONDS);
        let (resampler, output) = resample_in_odd_chunks(input_hz, channels, &input);

        let settled = &output[resampler.delay_frames() * channels * 2..];
        let seconds = (settled.len() / channels) as f64 / f64::from(OUTPUT_HZ);
        let measured_hz = rising_zero_crossings(settled, channels) as f64 / seconds;

        assert!(
            (measured_hz - TONE_HZ).abs() < TONE_HZ * 0.01,
            "{input_hz} Hz tone came out at {measured_hz} Hz"
        );
        assert!((peak(settled) - 0.5).abs() < 0.02, "peak {}", peak(settled));
    }

    #[test]
    fn tone_keeps_frequency_from_44100() {
        assert_keeps_frequency(44_100, 1);
    }

    #[test]
    fn tone_keeps_frequency_from_96000() {
        assert_keeps_frequency(96_000, 2);
    }

    #[test]
    fn tone_keeps_frequency_from_32000() {
        assert_keeps_frequency(32_000, 2);
    }

    #[test]
    fn output_length_tracks_the_ratio() {
        let input = tone(44_100, 2, SECONDS);
        let (_, output) = resample_in_odd_chunks(44_100, 2, &input);

        let expected = SECONDS * OUTPUT_HZ as usize * 2;
        let slack = (CHUNK_INPUT_FRAMES * 2 * 2) * 2;
        assert!(output.len() <= expected && output.len() + slack >= expected);
    }

    #[test]
    fn equal_rates_pass_through_untouched() {
        let mut resampler = PcmResampler::new(OUTPUT_HZ, OUTPUT_HZ, 2).unwrap();
        let input = tone(OUTPUT_HZ, 2, 1);
        let mut output = Vec::new();

        resampler.push(&input, &mut output).unwrap();

        assert!(resampler.is_passthrough());
        assert_eq!(resampler.delay_frames(), 0);
        assert_eq!(output, input);
    }

    #[test]
    fn partial_frames_are_rejected() {
        let mut resampler = PcmResampler::new(44_100, OUTPUT_HZ, 2).unwrap();
        let mut output = Vec::new();

        assert!(matches!(
            resampler.push(&[0.0; 3], &mut output),
            Err(AudioError::UnalignedSamples {
                channels: 2,
                samples: 3
            })
        ));
    }

    #[test]
    fn zero_rate_is_rejected() {
        assert!(matches!(
            PcmResampler::new(0, OUTPUT_HZ, 2),
            Err(AudioError::ResamplerInit(_))
        ));
    }

    #[test]
    fn reset_drops_buffered_input() {
        let mut resampler = PcmResampler::new(44_100, OUTPUT_HZ, 1).unwrap();
        let mut output = Vec::new();

        resampler.push(&[0.25; 100], &mut output).unwrap();
        resampler.reset();
        resampler.push(&[0.25; 100], &mut output).unwrap();

        assert_eq!(output.len(), 0);
    }
}
