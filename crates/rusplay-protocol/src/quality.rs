use rusplay_core::{Decibels, PlaneScores, QualityScore};

use crate::ProtocolError;

const STEPS_PER_DECIBEL: f32 = 100.0;
const REASON_ENCODER_UNSUPPORTED: u8 = 0;
const REASON_HOST_DISABLED: u8 = 1;
const REASON_MEASUREMENT_FAILED: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QualityDecibels(pub u16);

impl QualityDecibels {
    pub const LOSSLESS: Self = Self(u16::MAX);
    const LARGEST_FINITE: u16 = u16::MAX - 1;

    pub fn from_decibels(decibels: Decibels) -> Self {
        if decibels.is_identical() {
            return Self::LOSSLESS;
        }

        let steps = (decibels.value() * STEPS_PER_DECIBEL)
            .round()
            .clamp(0.0, f32::from(Self::LARGEST_FINITE));
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let steps = steps as u16;

        Self(steps)
    }

    pub fn decibels(self) -> Decibels {
        if self == Self::LOSSLESS {
            Decibels::IDENTICAL
        } else {
            Decibels::new(f32::from(self.0) / STEPS_PER_DECIBEL)
        }
    }

    pub const fn is_lossless(self) -> bool {
        self.0 == Self::LOSSLESS.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QualityPlanes {
    pub luma: QualityDecibels,
    pub cb: QualityDecibels,
    pub cr: QualityDecibels,
}

impl QualityPlanes {
    fn from_scores(scores: PlaneScores) -> Self {
        Self {
            luma: QualityDecibels::from_decibels(scores.luma),
            cb: QualityDecibels::from_decibels(scores.cb),
            cr: QualityDecibels::from_decibels(scores.cr),
        }
    }

    const fn as_array(self) -> [QualityDecibels; 3] {
        [self.luma, self.cb, self.cr]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QualityReport {
    pub frame: u32,
    pub psnr: QualityPlanes,
    pub psnr_overall: QualityDecibels,
    pub xpsnr: QualityPlanes,
}

impl QualityReport {
    pub const ENCODED_LEN: usize = 4 + 7 * 2;

    pub fn new(frame: u32, score: &QualityScore) -> Self {
        Self {
            frame,
            psnr: QualityPlanes::from_scores(score.psnr),
            psnr_overall: QualityDecibels::from_decibels(score.psnr_overall),
            xpsnr: QualityPlanes::from_scores(score.xpsnr),
        }
    }

    pub(crate) fn write(&self, out: &mut [u8]) {
        out[..4].copy_from_slice(&self.frame.to_be_bytes());
        let values = self
            .psnr
            .as_array()
            .into_iter()
            .chain([self.psnr_overall])
            .chain(self.xpsnr.as_array());
        for (slot, value) in out[4..].as_chunks_mut::<2>().0.iter_mut().zip(values) {
            *slot = value.0.to_be_bytes();
        }
    }

    pub(crate) fn read(body: &[u8]) -> Self {
        let value = |index: usize| {
            let at = 4 + 2 * index;
            QualityDecibels(u16::from_be_bytes([body[at], body[at + 1]]))
        };
        let planes = |first: usize| QualityPlanes {
            luma: value(first),
            cb: value(first + 1),
            cr: value(first + 2),
        };

        Self {
            frame: u32::from_be_bytes([body[0], body[1], body[2], body[3]]),
            psnr: planes(0),
            psnr_overall: value(3),
            xpsnr: planes(4),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QualityUnavailableReason {
    EncoderUnsupported,
    HostDisabled,
    MeasurementFailed,
}

impl QualityUnavailableReason {
    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::EncoderUnsupported => REASON_ENCODER_UNSUPPORTED,
            Self::HostDisabled => REASON_HOST_DISABLED,
            Self::MeasurementFailed => REASON_MEASUREMENT_FAILED,
        }
    }

    pub(crate) const fn from_code(code: u8) -> Result<Self, ProtocolError> {
        match code {
            REASON_ENCODER_UNSUPPORTED => Ok(Self::EncoderUnsupported),
            REASON_HOST_DISABLED => Ok(Self::HostDisabled),
            REASON_MEASUREMENT_FAILED => Ok(Self::MeasurementFailed),
            other => Err(ProtocolError::UnknownQualityUnavailableReason(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decibels_travel_in_hundredths_with_lossless_at_the_top() {
        let measured = QualityDecibels::from_decibels(Decibels::new(41.237));
        let lossless = QualityDecibels::from_decibels(Decibels::IDENTICAL);
        let negative = QualityDecibels::from_decibels(Decibels::new(-3.0));
        let huge = QualityDecibels::from_decibels(Decibels::new(1_000.0));

        assert_eq!(measured, QualityDecibels(4_124));
        assert!((measured.decibels().value() - 41.24).abs() < 1e-4);
        assert_eq!(lossless, QualityDecibels::LOSSLESS);
        assert!(lossless.decibels().is_identical());
        assert_eq!(negative, QualityDecibels(0));
        assert_eq!(huge, QualityDecibels(u16::MAX - 1));
        assert!(!huge.is_lossless());
    }

    #[test]
    fn a_report_carries_every_plane_of_a_score() {
        let score = QualityScore {
            psnr: PlaneScores {
                luma: Decibels::new(40.0),
                cb: Decibels::new(45.5),
                cr: Decibels::IDENTICAL,
            },
            psnr_overall: Decibels::new(42.0),
            xpsnr: PlaneScores {
                luma: Decibels::new(38.25),
                cb: Decibels::new(44.0),
                cr: Decibels::new(43.0),
            },
        };

        let report = QualityReport::new(7, &score);

        assert_eq!(report.frame, 7);
        assert_eq!(report.psnr.luma, QualityDecibels(4_000));
        assert_eq!(report.psnr.cb, QualityDecibels(4_550));
        assert_eq!(report.psnr.cr, QualityDecibels::LOSSLESS);
        assert_eq!(report.psnr_overall, QualityDecibels(4_200));
        assert_eq!(report.xpsnr.luma, QualityDecibels(3_825));
    }

    #[test]
    fn reasons_have_stable_codes() {
        for reason in [
            QualityUnavailableReason::EncoderUnsupported,
            QualityUnavailableReason::HostDisabled,
            QualityUnavailableReason::MeasurementFailed,
        ] {
            assert_eq!(
                QualityUnavailableReason::from_code(reason.code()),
                Ok(reason)
            );
        }
        assert_eq!(
            QualityUnavailableReason::from_code(3),
            Err(ProtocolError::UnknownQualityUnavailableReason(3))
        );
    }
}
