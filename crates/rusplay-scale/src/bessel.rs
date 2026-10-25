use std::f64::consts::{FRAC_PI_4, PI};

const SERIES_LIMIT: f64 = 8.0;
const ASYMPTOTIC_PHASE: f64 = 3.0 * FRAC_PI_4;

pub(crate) fn j1(x: f64) -> f64 {
    let magnitude = x.abs();

    if magnitude < SERIES_LIMIT {
        let square = x * x;
        let numerator = x
            * (72_362_614_232.0
                + square
                    * (-7_895_059_235.0
                        + square
                            * (242_396_853.1
                                + square
                                    * (-2_972_611.439
                                        + square * (15_704.482_60 + square * -30.160_366_06)))));
        let denominator = 144_725_228_442.0
            + square
                * (2_300_535_178.0
                    + square
                        * (18_583_304.74
                            + square * (99_447.433_94 + square * (376.999_139_7 + square))));
        return numerator / denominator;
    }

    let inverse = SERIES_LIMIT / magnitude;
    let square = inverse * inverse;
    let phase = magnitude - ASYMPTOTIC_PHASE;
    let cosine_term = 1.0
        + square
            * (0.183_105e-2
                + square
                    * (-0.351_639_649_6e-4
                        + square * (0.245_752_017_4e-5 + square * -0.240_337_019e-6)));
    let sine_term = 0.046_874_999_95
        + square
            * (-0.200_269_087_3e-3
                + square
                    * (0.844_919_909_6e-5
                        + square * (-0.882_289_87e-6 + square * 0.105_787_412e-6)));
    let amplitude = (2.0 / (PI * magnitude)).sqrt()
        * (phase.cos() * cosine_term - inverse * phase.sin() * sine_term);

    amplitude.copysign(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_reference_values() {
        assert!((j1(1.0) - 0.440_050_585_744_933_5).abs() < 1e-8);
        assert!((j1(10.0) - 0.043_472_746_168_861_44).abs() < 1e-8);
        assert!((j1(-1.0) + 0.440_050_585_744_933_5).abs() < 1e-8);
    }
}
