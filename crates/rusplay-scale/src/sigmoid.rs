use std::ops::RangeInclusive;

use crate::ScaleError;

const CENTER_RANGE: RangeInclusive<f32> = 0.0..=1.0;
const SLOPE_RANGE: RangeInclusive<f32> = 1.0..=20.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sigmoid {
    pub center: f32,
    pub slope: f32,
}

impl Default for Sigmoid {
    fn default() -> Self {
        Self {
            center: 0.75,
            slope: 6.5,
        }
    }
}

impl Sigmoid {
    pub(crate) fn validate(self) -> Result<(), ScaleError> {
        if !CENTER_RANGE.contains(&self.center) {
            return Err(ScaleError::SigmoidCenterOutOfRange(self.center));
        }
        if !SLOPE_RANGE.contains(&self.slope) {
            return Err(ScaleError::SigmoidSlopeOutOfRange(self.slope));
        }

        Ok(())
    }

    pub(crate) fn offset(self) -> f32 {
        1.0 / (1.0 + (self.slope * self.center).exp())
    }

    pub(crate) fn scale(self) -> f32 {
        1.0 / (1.0 + (self.slope * (self.center - 1.0)).exp()) - self.offset()
    }

    pub fn sigmoidize(self, linear: f32) -> f32 {
        let clamped = linear.clamp(0.0, 1.0);

        self.center - (1.0 / (clamped * self.scale() + self.offset()) - 1.0).ln() / self.slope
    }

    pub fn unsigmoidize(self, shaped: f32) -> f32 {
        let logistic = 1.0 / (1.0 + (self.slope * (self.center - shaped)).exp());

        (logistic - self.offset()) / self.scale()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigmoid_maps_the_unit_range_onto_itself() {
        let sigmoid = Sigmoid::default();

        assert!(sigmoid.sigmoidize(0.0).abs() < 1e-5);
        assert!((sigmoid.sigmoidize(1.0) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn unsigmoidize_inverts_sigmoidize() {
        let sigmoid = Sigmoid {
            center: 0.6,
            slope: 9.0,
        };

        for step in 0_u8..=20 {
            let value = f32::from(step) / 20.0;

            assert!((sigmoid.unsigmoidize(sigmoid.sigmoidize(value)) - value).abs() < 1e-5);
        }
    }

    #[test]
    fn parameters_are_validated() {
        let off_center = Sigmoid {
            center: -0.1,
            ..Sigmoid::default()
        };
        let flat = Sigmoid {
            slope: 0.0,
            ..Sigmoid::default()
        };

        assert_eq!(Sigmoid::default().validate(), Ok(()));
        assert_eq!(
            off_center.validate(),
            Err(ScaleError::SigmoidCenterOutOfRange(-0.1))
        );
        assert_eq!(
            flat.validate(),
            Err(ScaleError::SigmoidSlopeOutOfRange(0.0))
        );
    }
}
