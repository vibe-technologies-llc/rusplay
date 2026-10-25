use crate::{Geometry, Kernel};

#[derive(Debug, Clone, PartialEq)]
pub struct Taps {
    pub first: i64,
    pub anchor: i64,
    pub weights: Vec<f32>,
}

impl Taps {
    pub fn separable(kernel: Kernel, source_len: u32, target_len: u32, target_index: u32) -> Self {
        Self::separable_with_phase(kernel, source_len, target_len, target_index, 0.0)
    }

    pub fn separable_with_phase(
        kernel: Kernel,
        source_len: u32,
        target_len: u32,
        target_index: u32,
        phase: f64,
    ) -> Self {
        debug_assert_eq!(kernel.geometry(), Geometry::Separable);

        let ratio = f64::from(source_len) / f64::from(target_len);
        let stretch = ratio.max(1.0);
        let support = kernel.radius() * stretch;
        let center = (f64::from(target_index) + 0.5) * ratio - 0.5 + phase;

        #[allow(clippy::cast_possible_truncation)]
        let first = (center - support).floor() as i64 + 1;
        #[allow(clippy::cast_possible_truncation)]
        let last = (center + support).ceil() as i64 - 1;

        #[allow(clippy::cast_possible_truncation)]
        let anchor = center.floor() as i64;

        #[allow(clippy::cast_precision_loss)]
        let raw: Vec<f64> = (first..=last)
            .map(|tap| kernel.weight((tap as f64 - center) / stretch))
            .collect();
        let total: f64 = raw.iter().sum();

        #[allow(clippy::cast_possible_truncation)]
        let weights = raw.iter().map(|weight| (weight / total) as f32).collect();

        Self {
            first,
            anchor,
            weights,
        }
    }
}

#[cfg(test)]
mod tests {
    use rusplay_core::ScaleFilter;

    use super::*;

    #[test]
    fn downscale_taps_widen_with_the_ratio_and_normalize() {
        let kernel = Kernel::new(ScaleFilter::Hermite);

        let taps = Taps::separable(kernel, 2560, 1920, 100);
        let total: f32 = taps.weights.iter().sum();

        assert!(taps.weights.len() >= 2);
        assert!((total - 1.0).abs() < 1e-5);
    }

    #[test]
    fn upscale_taps_span_the_kernel_radius() {
        let kernel = Kernel::new(ScaleFilter::Lanczos3);

        let taps = Taps::separable(kernel, 1920, 2560, 1000);

        assert_eq!(taps.weights.len(), 6);
        assert_eq!(taps.anchor - taps.first, 2);
    }

    #[test]
    fn left_phase_centres_the_tap_on_the_even_source_pixel() {
        let kernel = Kernel::new(ScaleFilter::Bilinear);

        let taps = Taps::separable_with_phase(kernel, 64, 32, 5, -0.5);

        assert_eq!(taps.anchor, 10);
        assert_eq!(taps.first, 9);
        assert_eq!(taps.weights, vec![0.25, 0.5, 0.25]);
    }
}
