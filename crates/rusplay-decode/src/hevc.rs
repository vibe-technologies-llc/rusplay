mod bits;
mod picture;
mod rps;
mod scaling;
mod sequence;
mod slice;
mod stop;

use std::{collections::BTreeMap, rc::Rc};

use cros_codecs::codec::h265::parser::NaluType;
use rusplay_core::Codec;

use self::{
    bits::Bits,
    picture::{PictureParameters, scan_picture_parameter_set},
    sequence::{SequenceParameters, scan_sequence_parameter_set, sub_layer_count},
    slice::{picture_parameter_set_id, scan_slice_header},
    stop::Stop,
};
use crate::{BitstreamUnit, DecodeError};

const CODEC: Codec = Codec::Hevc;
const NAL_HEADER_LEN: usize = 2;
const VPS_LAYER_BITS: usize = 6;

pub(crate) struct CheckedSequenceParameterSet(Option<(u8, SequenceParameters)>);

pub(crate) struct CheckedPictureParameterSet(Option<(u8, PictureParameters)>);

#[derive(Default)]
pub(crate) struct ParameterSets {
    sequences: BTreeMap<u8, Rc<SequenceParameters>>,
    pictures: BTreeMap<u8, PictureParameters>,
}

fn after_header(nalu: &[u8]) -> Bits<'_> {
    Bits::new(nalu.get(NAL_HEADER_LEN..).unwrap_or_default())
}

fn conclude<T>(scan: Result<T, Stop>, unit: BitstreamUnit) -> Result<Option<T>, DecodeError> {
    match scan {
        Ok(value) => Ok(Some(value)),
        Err(stop) => match stop.reason() {
            Some(reason) => Err(DecodeError::Malformed {
                codec: CODEC,
                unit,
                reason: String::from(reason),
            }),
            None => Ok(None),
        },
    }
}

pub(crate) fn check_video_parameter_set(nalu: &[u8]) -> Result<(), DecodeError> {
    let scan = (|| {
        let mut bits = after_header(nalu);
        bits.skip(4 + 1 + 1 + VPS_LAYER_BITS)?;
        sub_layer_count(&mut bits).map(drop)
    })();

    conclude(scan, BitstreamUnit::VideoParameterSet).map(drop)
}

pub(crate) fn check_sequence_parameter_set(
    nalu: &[u8],
) -> Result<CheckedSequenceParameterSet, DecodeError> {
    let scan = scan_sequence_parameter_set(&mut after_header(nalu));

    conclude(scan, BitstreamUnit::SequenceParameterSet).map(CheckedSequenceParameterSet)
}

impl ParameterSets {
    pub(crate) fn keep_sequence_parameter_set(&mut self, checked: CheckedSequenceParameterSet) {
        if let Some((id, parameters)) = checked.0 {
            self.sequences.insert(id, Rc::new(parameters));
        }
    }

    pub(crate) fn check_picture_parameter_set(
        &self,
        nalu: &[u8],
    ) -> Result<CheckedPictureParameterSet, DecodeError> {
        let scan = scan_picture_parameter_set(&mut after_header(nalu), |id| {
            self.sequences.get(&id).cloned()
        });

        conclude(scan, BitstreamUnit::PictureParameterSet).map(CheckedPictureParameterSet)
    }

    pub(crate) fn keep_picture_parameter_set(&mut self, checked: CheckedPictureParameterSet) {
        if let Some((id, parameters)) = checked.0 {
            self.pictures.insert(id, parameters);
        }
    }

    pub(crate) fn check_slice_header(
        &self,
        nalu: &[u8],
        nalu_type: NaluType,
    ) -> Result<(), DecodeError> {
        let mut bits = after_header(nalu);
        let scan = picture_parameter_set_id(&mut bits, nalu_type.is_irap()).and_then(
            |(first_slice_segment_in_picture, id)| {
                let pictures = self
                    .pictures
                    .get(&id)
                    .ok_or(Stop::UnknownPictureParameterSet)?;
                scan_slice_header(
                    &mut bits,
                    pictures,
                    nalu_type,
                    first_slice_segment_in_picture,
                )
            },
        );

        conclude(scan, BitstreamUnit::SliceHeader).map(drop)
    }
}

#[cfg(test)]
mod tests;
