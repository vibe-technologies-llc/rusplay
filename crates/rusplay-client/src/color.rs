use rusplay_core::{ColorSpace, PixelFormat};
use rusplay_scale::{OutputLight, YuvMatrix};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct HdrMetadata {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameColor {
    pub space: ColorSpace,
    pub matrix: YuvMatrix,
    pub coded: PixelFormat,
    pub light: OutputLight,
    pub hdr: Option<HdrMetadata>,
}

impl FrameColor {
    pub fn of_stream(space: ColorSpace, coded: PixelFormat) -> Self {
        Self {
            space,
            matrix: YuvMatrix::for_primaries(space.primaries),
            coded,
            light: OutputLight::linearizable(space.transfer),
            hdr: None,
        }
    }
}
