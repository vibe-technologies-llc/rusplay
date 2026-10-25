use rusplay_core::{ColorSpace, MatrixCoefficients, PixelFormat};
use rusplay_scale::OutputLight;

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
}
