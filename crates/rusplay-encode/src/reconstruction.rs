use std::slice;

use rusplay_core::{
    PixelFormat, PlaneBytes, QualityError, Resolution, SamplePicture, SamplePlane, chroma_size,
};

use crate::EncodeError;

#[derive(Clone, Copy)]
pub(crate) enum ReconPlanes<'a> {
    Planar([PlaneBytes<'a>; 3]),
    SemiPlanar {
        luma: PlaneBytes<'a>,
        chroma: PlaneBytes<'a>,
    },
}

pub(crate) fn picture(
    format: PixelFormat,
    resolution: Resolution,
    planes: ReconPlanes<'_>,
) -> Result<SamplePicture, QualityError> {
    let luma_size = (resolution.width as usize, resolution.height as usize);
    let (chroma_width, chroma_height) = chroma_size(luma_size, format.chroma);

    let planes = match planes {
        ReconPlanes::Planar([luma, cb, cr]) => [
            SamplePlane::read(luma, luma_size.0, luma_size.1)?,
            SamplePlane::read(cb, chroma_width, chroma_height)?,
            SamplePlane::read(cr, chroma_width, chroma_height)?,
        ],
        ReconPlanes::SemiPlanar { luma, chroma } => {
            let (cb, cr) = SamplePlane::read_interleaved(chroma, chroma_width, chroma_height)?;
            [SamplePlane::read(luma, luma_size.0, luma_size.1)?, cb, cr]
        }
    };

    SamplePicture::new(format, planes)
}

pub(crate) struct RawPlane {
    pub data: *const u8,
    pub stride: usize,
    pub row_bytes: usize,
    pub rows: usize,
}

impl RawPlane {
    pub(crate) unsafe fn bytes<'a>(&self, bytes_per_sample: usize, shift: u32) -> PlaneBytes<'a> {
        let len = if self.rows == 0 || self.data.is_null() {
            0
        } else {
            self.stride * (self.rows - 1) + self.row_bytes
        };
        let data = if len == 0 {
            &[][..]
        } else {
            unsafe { slice::from_raw_parts(self.data, len) }
        };

        PlaneBytes {
            data,
            stride: self.stride,
            bytes_per_sample,
            shift,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) enum ReconstructionSlot {
    #[default]
    Idle,
    Armed,
    Captured(Result<SamplePicture, EncodeError>),
}

impl ReconstructionSlot {
    pub(crate) fn arm(&mut self) {
        *self = Self::Armed;
    }

    pub(crate) const fn is_armed(&self) -> bool {
        matches!(self, Self::Armed)
    }

    pub(crate) fn fill(&mut self, capture: impl FnOnce() -> Result<SamplePicture, EncodeError>) {
        if self.is_armed() {
            *self = Self::Captured(capture());
        }
    }

    pub(crate) fn take(&mut self) -> Result<Option<SamplePicture>, EncodeError> {
        match std::mem::take(self) {
            Self::Captured(result) => result.map(Some),
            Self::Idle | Self::Armed => Ok(None),
        }
    }
}
