use std::{f64::consts::FRAC_PI_2, ops::RangeInclusive};

use rusplay_core::{BitDepth, ChromaSubsampling, ColorRange};

use crate::{Quantization, ScaleError};

pub const DEBAND_ITERATIONS: RangeInclusive<u32> = 1..=4;
pub const DEBAND_RADIUS: RangeInclusive<u32> = 1..=64;
pub const DEBAND_THRESHOLD: RangeInclusive<f32> = 0.0..=16.0;

const DIRECTION_COUNT: u32 = 8;
const DIRECTION_BITS: u32 = 12;
#[cfg(test)]
const DIRECTION_SEED_SHIFT: u32 = 16;
#[cfg(test)]
const ITERATION_SEEDS: u32 = 16;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DebandSettings {
    pub iterations: u32,
    pub radius: u32,
    pub threshold: f32,
    pub chroma_threshold: f32,
}

impl DebandSettings {
    pub(crate) fn validate(&self) -> Result<(), ScaleError> {
        if !DEBAND_ITERATIONS.contains(&self.iterations) {
            return Err(ScaleError::DebandIterationsOutOfRange(self.iterations));
        }
        if !DEBAND_RADIUS.contains(&self.radius) {
            return Err(ScaleError::DebandRadiusOutOfRange(self.radius));
        }
        if !DEBAND_THRESHOLD.contains(&self.threshold) {
            return Err(ScaleError::DebandThresholdOutOfRange(self.threshold));
        }
        if !DEBAND_THRESHOLD.contains(&self.chroma_threshold) {
            return Err(ScaleError::DebandChromaThresholdOutOfRange(
                self.chroma_threshold,
            ));
        }

        Ok(())
    }

    pub(crate) fn plane_thresholds(&self, range: ColorRange) -> (f64, f64) {
        let eight_bit = Quantization::new(BitDepth::Eight, range);

        (
            f64::from(self.threshold) / eight_bit.luma.0,
            f64::from(self.chroma_threshold) / eight_bit.chroma.0,
        )
    }

    #[cfg(test)]
    pub(crate) fn pair_offsets(
        &self,
        pixel: (u32, u32),
        plane: DebandPlane,
        iteration: u32,
        shift: (u32, u32),
    ) -> [(i64, i64); 2] {
        let seed = permute(
            pixel.0.wrapping_add(permute(
                pixel
                    .1
                    .wrapping_add(permute((plane as u32) * ITERATION_SEEDS + iteration)),
            )),
        );
        let low = self.radius * (iteration - 1) / self.iterations + 1;
        let high = (self.radius * iteration / self.iterations).max(low);
        let reach = low + seed % (high - low + 1);
        let direction = (seed >> DIRECTION_SEED_SHIFT) % DIRECTION_COUNT;
        let (cosine, sine) = directions()[direction as usize];
        let along = reach * cosine;
        let across = reach * sine;
        let round = |value: u32, shift: u32| {
            i64::from((value + ((1 << (DIRECTION_BITS - 1)) << shift)) >> (DIRECTION_BITS + shift))
        };

        [
            (round(along, shift.0), round(across, shift.1)),
            (-round(across, shift.0), round(along, shift.1)),
        ]
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DebandPlane {
    Luma = 0,
    Chroma = 1,
}

pub(crate) const fn chroma_shift(chroma: ChromaSubsampling) -> (u32, u32) {
    match chroma {
        ChromaSubsampling::Yuv420 => (1, 1),
        ChromaSubsampling::Yuv422 => (1, 0),
        ChromaSubsampling::Yuv444 => (0, 0),
    }
}

#[cfg(test)]
pub(crate) fn pair_weight(deviation: f64, threshold: f64) -> f64 {
    (2.0 - 2.0 * deviation / threshold).clamp(0.0, 1.0)
}

fn directions() -> [(u32, u32); DIRECTION_COUNT as usize] {
    let scale = f64::from(1_u32 << DIRECTION_BITS);
    std::array::from_fn(|index| {
        #[allow(clippy::cast_precision_loss)]
        let angle = FRAC_PI_2 * index as f64 / f64::from(DIRECTION_COUNT);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let fixed = |value: f64| (value * scale).round() as u32;
        (fixed(angle.cos()), fixed(angle.sin()))
    })
}

#[cfg(test)]
fn permute(value: u32) -> u32 {
    let state = value.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let word = ((state >> ((state >> 28) + 4)) ^ state).wrapping_mul(277_803_737);
    (word >> 22) ^ word
}

pub(crate) fn params_bytes(
    settings: &DebandSettings,
    range: ColorRange,
    chroma: ChromaSubsampling,
) -> Vec<u8> {
    let (luma_threshold, chroma_threshold) = settings.plane_thresholds(range);
    let (shift_x, shift_y) = chroma_shift(chroma);
    let narrow = |value: f64| {
        #[allow(clippy::cast_possible_truncation)]
        let narrowed = value as f32;
        narrowed.to_bits()
    };

    let table = directions()
        .into_iter()
        .flat_map(|(cosine, sine)| [cosine, sine]);
    let tail = [
        narrow(luma_threshold),
        narrow(chroma_threshold),
        settings.iterations,
        settings.radius,
        shift_x,
        shift_y,
        0,
        0,
    ];

    table.chain(tail).flat_map(u32::to_ne_bytes).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> DebandSettings {
        DebandSettings {
            iterations: 2,
            radius: 16,
            threshold: 3.0,
            chroma_threshold: 3.0,
        }
    }

    #[test]
    fn directions_cover_a_quarter_turn_in_fixed_point() {
        let table = directions();

        assert_eq!(table[0], (4096, 0));
        assert_eq!(table[4], (2896, 2896));
        assert!(table.iter().all(|&(cosine, sine)| {
            let length = f64::from(cosine).hypot(f64::from(sine));
            (length - 4096.0).abs() < 1.0
        }));
    }

    #[test]
    fn pairs_are_perpendicular_and_inside_their_ring() {
        let settings = settings();

        for iteration in 1..=settings.iterations {
            for y in 0..32 {
                for x in 0..32 {
                    let [first, second] =
                        settings.pair_offsets((x, y), DebandPlane::Luma, iteration, (0, 0));
                    #[allow(clippy::cast_precision_loss)]
                    let length = (first.0 as f64).hypot(first.1 as f64);

                    assert_eq!(second, (-first.1, first.0));
                    assert!(first.0 >= 0 && first.1 >= 0);
                    assert!(
                        length >= f64::from(8 * (iteration - 1))
                            && length <= f64::from(8 * iteration) + 0.75,
                        "iteration {iteration} at ({x}, {y}): {first:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn subsampled_offsets_halve_the_luma_offsets() {
        let settings = settings();

        let [luma, _] = settings.pair_offsets((5, 9), DebandPlane::Chroma, 2, (0, 0));
        let [chroma, _] = settings.pair_offsets((5, 9), DebandPlane::Chroma, 2, (1, 1));

        assert!((chroma.0 - luma.0 / 2).abs() <= 1);
        assert!((chroma.1 - luma.1 / 2).abs() <= 1);
    }

    #[test]
    fn thresholds_are_eight_bit_codes_of_the_range() {
        let settings = settings();

        let (limited_luma, limited_chroma) = settings.plane_thresholds(ColorRange::Limited);
        let (full_luma, full_chroma) = settings.plane_thresholds(ColorRange::Full);

        assert!((limited_luma - 3.0 / 219.0).abs() < 1e-12);
        assert!((limited_chroma - 3.0 / 224.0).abs() < 1e-12);
        assert!((full_luma - 3.0 / 255.0).abs() < 1e-12);
        assert!((full_chroma - 3.0 / 255.0).abs() < 1e-12);
    }

    #[test]
    fn out_of_range_settings_are_refused() {
        let base = settings();

        assert!(base.validate().is_ok());
        assert!(matches!(
            DebandSettings {
                iterations: 0,
                ..base
            }
            .validate(),
            Err(ScaleError::DebandIterationsOutOfRange(0))
        ));
        assert!(matches!(
            DebandSettings { radius: 65, ..base }.validate(),
            Err(ScaleError::DebandRadiusOutOfRange(65))
        ));
        assert!(matches!(
            DebandSettings {
                threshold: -1.0,
                ..base
            }
            .validate(),
            Err(ScaleError::DebandThresholdOutOfRange(_))
        ));
        assert!(matches!(
            DebandSettings {
                chroma_threshold: f32::NAN,
                ..base
            }
            .validate(),
            Err(ScaleError::DebandChromaThresholdOutOfRange(_))
        ));
    }
}
