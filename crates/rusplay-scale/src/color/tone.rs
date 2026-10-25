use rusplay_core::{ToneMapOperator, pq_from_nits, pq_to_nits};

use crate::ScaleError;

pub(crate) const MIN_PQ_RANGE: f64 = 1e-6;
pub(crate) const BT2446A_GAMMA: f64 = 2.4;
const BT2446A_RHO_GAIN: f64 = 32.0;
const BT2446A_LINEAR_END: f64 = 0.7399;
const BT2446A_QUADRATIC_END: f64 = 0.9909;
const SPLINE_PIVOT: f64 = 0.7;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DynamicPeak {
    pub percentile: f64,
    pub smoothing_frames: u32,
    pub scene_threshold_db: f64,
}

impl DynamicPeak {
    pub const DEFAULT_PERCENTILE: f64 = 99.995;
    pub const DEFAULT_SMOOTHING_FRAMES: u32 = 20;
    pub const DEFAULT_SCENE_THRESHOLD_DB: f64 = 3.0;

    pub(crate) fn validate(&self) -> Result<(), ScaleError> {
        if !(self.percentile > 0.0 && self.percentile <= 100.0) {
            return Err(ScaleError::PeakPercentileOutOfRange(self.percentile));
        }
        if self.smoothing_frames == 0 {
            return Err(ScaleError::ZeroSmoothingFrames);
        }
        if !(self.scene_threshold_db.is_finite() && self.scene_threshold_db > 0.0) {
            return Err(ScaleError::SceneThresholdOutOfRange(
                self.scene_threshold_db,
            ));
        }

        Ok(())
    }
}

impl Default for DynamicPeak {
    fn default() -> Self {
        Self {
            percentile: Self::DEFAULT_PERCENTILE,
            smoothing_frames: Self::DEFAULT_SMOOTHING_FRAMES,
            scene_threshold_db: Self::DEFAULT_SCENE_THRESHOLD_DB,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ToneMapping {
    pub operator: ToneMapOperator,
    pub dynamic_peak: Option<DynamicPeak>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToneCurve {
    pub operator: ToneMapOperator,
    pub source_peak_nits: f64,
    pub source_black_nits: f64,
    pub target_peak_nits: f64,
    pub target_black_nits: f64,
}

impl ToneCurve {
    pub fn pq_source_peak(&self) -> f64 {
        pq_from_nits(self.source_peak_nits)
    }

    pub fn pq_source_black(&self) -> f64 {
        pq_from_nits(self.source_black_nits)
    }

    fn pq_range(&self) -> f64 {
        (self.pq_source_peak() - self.pq_source_black()).max(MIN_PQ_RANGE)
    }

    pub fn max_lum(&self) -> f64 {
        (pq_from_nits(self.target_peak_nits) - self.pq_source_black()) / self.pq_range()
    }

    pub fn min_lum(&self) -> f64 {
        ((pq_from_nits(self.target_black_nits) - self.pq_source_black()) / self.pq_range()).max(0.0)
    }

    pub fn knee_start(&self) -> f64 {
        1.5 * self.max_lum() - 0.5
    }

    pub fn map_nits(&self, nits: f64) -> f64 {
        pq_to_nits(self.map_pq(pq_from_nits(nits)))
    }

    pub fn map_pq(&self, code: f64) -> f64 {
        let normalized = ((code - self.pq_source_black()) / self.pq_range()).clamp(0.0, 1.0);
        let in_pq = |mapped: f64| mapped * self.pq_range() + self.pq_source_black();

        match self.operator {
            ToneMapOperator::Bt2390 => in_pq(self.bt2390(normalized)),
            ToneMapOperator::Spline => in_pq(self.spline(normalized)),
            ToneMapOperator::Bt2446a => pq_from_nits(self.bt2446a(pq_to_nits(code))),
            ToneMapOperator::Reinhard => pq_from_nits(self.reinhard(pq_to_nits(code))),
            ToneMapOperator::Clip => pq_from_nits(pq_to_nits(code).min(self.target_peak_nits)),
        }
    }

    pub fn bt2390(&self, normalized: f64) -> f64 {
        let knee = self.knee_start();
        let max_lum = self.max_lum();

        let rolled = if normalized >= knee && knee < 1.0 {
            let t = (normalized - knee) / (1.0 - knee);
            let (t2, t3) = (t * t, t * t * t);
            (2.0 * t3 - 3.0 * t2 + 1.0) * knee
                + (t3 - 2.0 * t2 + t) * (1.0 - knee)
                + (-2.0 * t3 + 3.0 * t2) * max_lum
        } else {
            normalized
        };

        rolled + self.min_lum() * (1.0 - rolled).powi(4)
    }

    fn spline(&self, normalized: f64) -> f64 {
        let max_lum = self.max_lum();
        let pivot = SPLINE_PIVOT * max_lum;
        if max_lum >= 1.0 || normalized <= pivot {
            return normalized;
        }

        let span = 1.0 - pivot;
        let start_slope = (3.0 * (max_lum - pivot) / span).min(1.0);
        let t = (normalized - pivot) / span;
        let (t2, t3) = (t * t, t * t * t);

        (2.0 * t3 - 3.0 * t2 + 1.0) * pivot
            + (t3 - 2.0 * t2 + t) * span * start_slope
            + (-2.0 * t3 + 3.0 * t2) * max_lum
    }

    fn bt2446a(&self, nits: f64) -> f64 {
        let (source, target) = (self.source_peak_nits, self.target_peak_nits);
        if source <= target {
            return nits.min(target);
        }

        let rho = |peak: f64| 1.0 + BT2446A_RHO_GAIN * (peak / 10_000.0).powf(1.0 / BT2446A_GAMMA);
        let (source_rho, display_rho) = (rho(source), rho(target));
        let encoded = (nits / source).clamp(0.0, 1.0).powf(1.0 / BT2446A_GAMMA);
        let perceptual = (1.0 + (source_rho - 1.0) * encoded).ln() / source_rho.ln();
        let toned = if perceptual <= BT2446A_LINEAR_END {
            1.077 * perceptual
        } else if perceptual < BT2446A_QUADRATIC_END {
            (-1.151 * perceptual + 2.7811) * perceptual - 0.6302
        } else {
            0.5 * perceptual + 0.5
        };
        let sdr = (display_rho.powf(toned) - 1.0) / (display_rho - 1.0);

        target * sdr.clamp(0.0, 1.0).powf(BT2446A_GAMMA)
    }

    fn reinhard(&self, nits: f64) -> f64 {
        let target = self.target_peak_nits;
        let white = self.source_peak_nits / target;
        if white <= 1.0 {
            return nits.min(target);
        }

        let x = nits.max(0.0) / target;
        let mapped = x * (1.0 + x / (white * white)) / (1.0 + x);

        mapped.min(1.0) * target
    }

    pub(crate) fn words(&self) -> [u32; 8] {
        [
            self.source_peak_nits,
            self.target_peak_nits,
            self.pq_source_peak(),
            self.pq_source_black(),
            self.min_lum(),
            self.max_lum(),
            self.knee_start(),
            0.0,
        ]
        .map(|value| {
            #[allow(clippy::cast_possible_truncation)]
            let narrowed = value as f32;
            narrowed.to_bits()
        })
    }
}
