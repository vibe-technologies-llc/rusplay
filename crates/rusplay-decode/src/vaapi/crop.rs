use rusplay_core::{Codec, Resolution};

use crate::{BitstreamUnit, DecodeError};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct CropOffsets {
    pub(super) left: u64,
    pub(super) right: u64,
    pub(super) top: u64,
    pub(super) bottom: u64,
}

impl CropOffsets {
    pub(super) fn scaled(left: u32, right: u32, top: u32, bottom: u32, unit: (u32, u32)) -> Self {
        let (unit_x, unit_y) = (u64::from(unit.0), u64::from(unit.1));

        Self {
            left: u64::from(left) * unit_x,
            right: u64::from(right) * unit_x,
            top: u64::from(top) * unit_y,
            bottom: u64::from(bottom) * unit_y,
        }
    }
}

pub(super) fn visible_resolution(
    codec: Codec,
    unit: BitstreamUnit,
    picture: Resolution,
    crop: CropOffsets,
) -> Result<Resolution, DecodeError> {
    if crop.left != 0 || crop.top != 0 {
        return Err(DecodeError::CropOriginUnsupported {
            codec,
            left: crop.left,
            top: crop.top,
        });
    }

    let width = u64::from(picture.width).checked_sub(crop.right);
    let height = u64::from(picture.height).checked_sub(crop.bottom);

    match (width, height) {
        (Some(width @ 1..), Some(height @ 1..)) => Ok(Resolution::new(
            u32::try_from(width).unwrap_or(picture.width),
            u32::try_from(height).unwrap_or(picture.height),
        )),
        _ => Err(DecodeError::Malformed {
            codec,
            unit,
            reason: format!(
                "cropping {} columns and {} rows leaves nothing of a {}x{} picture",
                crop.right, crop.bottom, picture.width, picture.height
            ),
        }),
    }
}
