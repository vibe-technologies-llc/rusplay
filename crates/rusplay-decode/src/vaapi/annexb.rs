use std::{fmt::Debug, io::Cursor};

use cros_codecs::codec::h264::nalu::{Header, Nalu};
use rusplay_core::Codec;

use crate::{BitstreamUnit, DecodeError};

pub(super) fn malformed(codec: Codec, unit: BitstreamUnit) -> impl FnOnce(String) -> DecodeError {
    move |reason| DecodeError::Malformed {
        codec,
        unit,
        reason,
    }
}

fn has_start_code(data: &[u8]) -> bool {
    data.windows(3).any(|window| window == [0, 0, 1])
}

pub(super) fn next_nalu<'a, H: Header + Debug>(
    cursor: &mut Cursor<&'a [u8]>,
    codec: Codec,
) -> Result<Option<Nalu<'a, H>>, DecodeError> {
    let position = usize::try_from(cursor.position()).unwrap_or(usize::MAX);
    let remaining = cursor.get_ref().get(position..).unwrap_or_default();
    if !has_start_code(remaining) {
        return Ok(None);
    }

    Nalu::next(cursor)
        .map(Some)
        .map_err(malformed(codec, BitstreamUnit::NalUnit))
}
