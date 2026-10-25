use crate::{Geometry, Kernel};

#[derive(Debug, Clone, PartialEq)]
pub struct Taps {
    pub first: i64,
    pub weights: Vec<f32>,
}

impl Taps {
    pub fn separable(kernel: Kernel, source_len: u32, target_len: u32, target_index: u32) -> Self {
        debug_assert_eq!(kernel.geometry(), Geometry::Separable);

        let ratio = f64::from(source_len) / f64::from(target_len);
        let stretch = ratio.max(1.0);
        let support = kernel.radius() * stretch;
        let center = (f64::from(target_index) + 0.5) * ratio - 0.5;

        #[allow(clippy::cast_possible_truncation)]
        let first = (center - support).floor() as i64 + 1;
        #[allow(clippy::cast_possible_truncation)]
        let last = (center + support).ceil() as i64 - 1;

        #[allow(clippy::cast_precision_loss)]
        let raw: Vec<f64> = (first..=last)
            .map(|tap| kernel.weight((tap as f64 - center) / stretch))
            .collect();
        let total: f64 = raw.iter().sum();

        #[allow(clippy::cast_possible_truncation)]
        let weights = raw.iter().map(|weight| (weight / total) as f32).collect();

        Self { first, weights }
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
    }
}
