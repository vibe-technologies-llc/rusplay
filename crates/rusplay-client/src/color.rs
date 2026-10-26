use rusplay_core::{ColorSpace, HdrLevels, MatrixCoefficients, PixelFormat};
use rusplay_scale::{ColorVolume, OutputLight};

use crate::present::whole_nits;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameColor {
    pub space: ColorSpace,
    pub matrix: MatrixCoefficients,
    pub coded: PixelFormat,
    pub light: OutputLight,
}

impl FrameColor {
    pub fn of_stream(space: ColorSpace, coded: PixelFormat) -> Self {
        Self {
            space,
            matrix: space.matrix_coefficients(),
            coded,
            light: OutputLight::linearizable(space.transfer),
        }
    }

    #[must_use]
    pub fn mapped_to(self, target: &ColorVolume) -> Self {
        Self {
            space: ColorSpace {
                primaries: target.primaries,
                transfer: target.transfer,
                hdr: HdrLevels {
                    reference_white_nits: whole_nits(target.reference_white_nits),
                    peak_nits: whole_nits(target.peak_nits),
                    mastering: None,
                    content_light: None,
                },
                ..self.space
            },
            light: OutputLight::Linear,
            ..self
        }
    }
}
