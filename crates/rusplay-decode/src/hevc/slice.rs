use cros_codecs::codec::h265::parser::NaluType;

use super::{
    bits::{Bits, ceil_log2, count},
    picture::PictureParameters,
    rps::{Sets, ShortTermSet, scan_short_term_set},
    stop::Stop,
};

const MAX_PICTURE_PARAMETER_SET_ID: u32 = 63;
const MAX_LONG_TERM_PICTURES: u32 = 16;
const MAX_REFERENCES_MINUS1: u32 = 14;
const MAX_ENTRY_POINTS: u32 = 32;
const MAX_LUMA_LOG2_WEIGHT_DENOMINATOR: u32 = 7;
const SCREEN_CONTENT_INTEGER_MOTION_VECTOR_SELECT: u32 = 2;
const COLOUR_PLANE_ID_BITS: usize = 2;
const SLICE_TYPE_B: u32 = 0;
const SLICE_TYPE_P: u32 = 1;
const SLICE_TYPE_I: u32 = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SliceType {
    B,
    P,
    I,
}

impl SliceType {
    fn from_code(code: u32) -> Result<Self, Stop> {
        match code {
            SLICE_TYPE_B => Ok(Self::B),
            SLICE_TYPE_P => Ok(Self::P),
            SLICE_TYPE_I => Ok(Self::I),
            _ => Err(Stop::ParserRejects),
        }
    }
}

#[allow(clippy::struct_excessive_bools)]
struct Header {
    slice_type: SliceType,
    sample_adaptive_offset_luma: bool,
    sample_adaptive_offset_chroma: bool,
    deblocking_filter_disabled: bool,
    temporal_mvp_enabled: bool,
}

struct References {
    l0_minus1: u32,
    l1_minus1: u32,
}

pub(super) fn picture_parameter_set_id(bits: &mut Bits, irap: bool) -> Result<(bool, u8), Stop> {
    let first_slice_segment_in_picture = bits.bit()?;
    if irap {
        bits.skip(1)?;
    }
    let id = bits.unsigned_max(MAX_PICTURE_PARAMETER_SET_ID)?;

    Ok((
        first_slice_segment_in_picture,
        u8::try_from(id).unwrap_or(0),
    ))
}

pub(super) fn scan_slice_header(
    bits: &mut Bits,
    pictures: &PictureParameters,
    nalu_type: NaluType,
    first_slice_segment_in_picture: bool,
) -> Result<(), Stop> {
    let sequence = &*pictures.sequence;

    let mut dependent = false;
    if !first_slice_segment_in_picture {
        if pictures.dependent_slice_segments_enabled {
            dependent = bits.bit()?;
        }
        bits.skip(ceil_log2(sequence.picture_size_in_ctbs()))?;
    }

    if !dependent {
        scan_independent_fields(bits, pictures, nalu_type)?;
    }

    scan_entry_points(bits, pictures)
}

fn scan_independent_fields(
    bits: &mut Bits,
    pictures: &PictureParameters,
    nalu_type: NaluType,
) -> Result<(), Stop> {
    let sequence = &*pictures.sequence;

    bits.skip(pictures.extra_slice_header_bits)?;
    let slice_type = SliceType::from_code(bits.unsigned()?)?;
    if pictures.output_flag_present {
        bits.skip(1)?;
    }
    if sequence.separate_colour_plane {
        bits.skip(COLOUR_PLANE_ID_BITS)?;
    }

    let mut header = Header {
        slice_type,
        sample_adaptive_offset_luma: false,
        sample_adaptive_offset_chroma: false,
        deblocking_filter_disabled: pictures.deblocking_filter_disabled,
        temporal_mvp_enabled: false,
    };
    let mut references = ShortTermSet::default();
    let mut long_term_used = Vec::new();
    let mut sps_set = None;

    if !nalu_type.is_idr() {
        bits.skip(sequence.pic_order_count_lsb_bits)?;
        let from_sequence = bits.bit()?;

        if from_sequence {
            let declared = sequence.short_term_sets.len();
            let mut index = 0;
            if declared > 1 {
                index = count(bits.bits(ceil_log2(u32::try_from(declared).unwrap_or(u32::MAX)))?);
                if index >= declared {
                    return Err(Stop::ParserRejects);
                }
            }
            sps_set = Some(index);
        } else {
            let sets = Sets {
                existing: &sequence.short_term_sets,
                declared: sequence.short_term_sets.len(),
                max_dec_pic_buffering_minus1: sequence.max_dec_pic_buffering_minus1,
            };
            references = scan_short_term_set(bits, &sets, sequence.short_term_sets.len())?;
        }

        if sequence.long_term_present {
            long_term_used = scan_long_term_pictures(bits, pictures)?;
        }

        if sequence.temporal_mvp_enabled {
            header.temporal_mvp_enabled = bits.bit()?;
        }
    }

    if sequence.sample_adaptive_offset_enabled {
        header.sample_adaptive_offset_luma = bits.bit()?;
        if sequence.chroma_array_type != 0 {
            header.sample_adaptive_offset_chroma = bits.bit()?;
        }
    }

    if header.slice_type != SliceType::I {
        let set = match sps_set {
            Some(index) => sequence
                .short_term_sets
                .get(index)
                .ok_or(Stop::ParserRejects)?,
            None => &references,
        };
        scan_inter_fields(bits, pictures, &header, set, &long_term_used)?;
    }

    bits.signed()?;
    if pictures.slice_chroma_qp_offsets_present {
        bits.signed()?;
        bits.signed()?;
    }
    if pictures.slice_act_qp_offsets_present {
        for _ in 0..3 {
            bits.signed()?;
        }
    }
    if pictures.chroma_qp_offset_list_enabled {
        bits.skip(1)?;
    }

    let override_flag = pictures.deblocking_filter_override_enabled && bits.bit()?;
    if override_flag {
        header.deblocking_filter_disabled = bits.bit()?;
        if !header.deblocking_filter_disabled {
            bits.signed()?;
            bits.signed()?;
        }
    }

    if pictures.loop_filter_across_slices_enabled
        && (header.sample_adaptive_offset_luma
            || header.sample_adaptive_offset_chroma
            || !header.deblocking_filter_disabled)
    {
        bits.skip(1)?;
    }

    Ok(())
}

fn scan_long_term_pictures(
    bits: &mut Bits,
    pictures: &PictureParameters,
) -> Result<Vec<bool>, Stop> {
    let sequence = &*pictures.sequence;
    let sps_entries = sequence.long_term_used_by_current.len();

    let from_sequence = if sps_entries > 0 {
        bits.unsigned_max(u32::try_from(sps_entries).unwrap_or(u32::MAX))?
    } else {
        0
    };
    let signalled = bits.unsigned()?;
    if count(from_sequence) + count(signalled) > count(MAX_LONG_TERM_PICTURES) {
        return Err(Stop::LongTermPictureCount);
    }

    let mut used = Vec::new();
    for index in 0..from_sequence + signalled {
        if index < from_sequence {
            let entry = if sps_entries > 1 {
                bits.bits(ceil_log2(u32::try_from(sps_entries).unwrap_or(u32::MAX)))?
            } else {
                0
            };
            let flag = sequence
                .long_term_used_by_current
                .get(count(entry))
                .ok_or(Stop::ParserRejects)?;
            used.push(*flag);
        } else {
            bits.skip(sequence.pic_order_count_lsb_bits)?;
            used.push(bits.bit()?);
        }

        if bits.bit()? {
            bits.unsigned()?;
        }
    }

    Ok(used)
}

fn scan_inter_fields(
    bits: &mut Bits,
    pictures: &PictureParameters,
    header: &Header,
    short_term: &ShortTermSet,
    long_term_used: &[bool],
) -> Result<(), Stop> {
    let mut references = References {
        l0_minus1: pictures.default_l0_references_minus1,
        l1_minus1: pictures.default_l1_references_minus1,
    };
    if bits.bit()? {
        references.l0_minus1 = bits.unsigned_max(MAX_REFERENCES_MINUS1)?;
        if header.slice_type == SliceType::B {
            references.l1_minus1 = bits.unsigned_max(MAX_REFERENCES_MINUS1)?;
        }
    }
    let is_b = header.slice_type == SliceType::B;

    let current_pictures = short_term.used_count()
        + long_term_used.iter().filter(|&&used| used).count()
        + usize::from(pictures.current_picture_reference_enabled);

    if pictures.lists_modification_present && current_pictures > 1 {
        let entry_bits = ceil_log2(u32::try_from(current_pictures).unwrap_or(u32::MAX));
        if bits.bit()? {
            bits.skip((count(references.l0_minus1) + 1) * entry_bits)?;
        }
        if is_b && bits.bit()? {
            bits.skip((count(references.l1_minus1) + 1) * entry_bits)?;
        }
    }

    if is_b {
        bits.skip(1)?;
    }
    if pictures.cabac_init_present {
        bits.skip(1)?;
    }

    if header.temporal_mvp_enabled {
        let from_l0 = if is_b { bits.bit()? } else { true };
        let list_minus1 = if from_l0 {
            references.l0_minus1
        } else {
            references.l1_minus1
        };
        if list_minus1 > 0 {
            bits.unsigned_max(list_minus1)?;
        }
    }

    let weighted = (pictures.weighted_prediction && header.slice_type == SliceType::P)
        || (pictures.weighted_bi_prediction && is_b);
    if weighted {
        scan_prediction_weights(bits, pictures, &references, is_b)?;
    }

    bits.unsigned()?;
    if pictures.sequence.motion_vector_resolution_control_idc
        == SCREEN_CONTENT_INTEGER_MOTION_VECTOR_SELECT
    {
        bits.skip(1)?;
    }

    Ok(())
}

fn scan_prediction_weights(
    bits: &mut Bits,
    pictures: &PictureParameters,
    references: &References,
    is_b: bool,
) -> Result<(), Stop> {
    let sequence = &*pictures.sequence;

    bits.unsigned_max(MAX_LUMA_LOG2_WEIGHT_DENOMINATOR)?;
    if sequence.chroma_array_type != 0 {
        bits.signed()?;
    }

    let l0_chroma_flags = sequence.chroma_array_type != 0;
    let l1_chroma_flags = sequence.chroma_format_idc != 0;
    scan_weight_list(bits, references.l0_minus1, l0_chroma_flags)?;
    if is_b {
        scan_weight_list(bits, references.l1_minus1, l1_chroma_flags)?;
    }

    Ok(())
}

fn scan_weight_list(
    bits: &mut Bits,
    references_minus1: u32,
    chroma_flags: bool,
) -> Result<(), Stop> {
    let entries = count(references_minus1) + 1;

    let mut luma = Vec::with_capacity(entries);
    for _ in 0..entries {
        luma.push(bits.bit()?);
    }
    let mut chroma = vec![false; entries];
    if chroma_flags {
        for flag in &mut chroma {
            *flag = bits.bit()?;
        }
    }

    for (luma_weighted, chroma_weighted) in luma.into_iter().zip(chroma) {
        if luma_weighted {
            bits.signed()?;
            bits.signed()?;
        }
        if chroma_weighted {
            for _ in 0..4 {
                bits.signed()?;
            }
        }
    }

    Ok(())
}

fn scan_entry_points(bits: &mut Bits, pictures: &PictureParameters) -> Result<(), Stop> {
    if !pictures.tiles_enabled && !pictures.entropy_coding_sync_enabled {
        return Ok(());
    }

    let sequence = &*pictures.sequence;
    let maximum = match (pictures.tiles_enabled, pictures.entropy_coding_sync_enabled) {
        (false, _) => sequence.height_in_ctbs - 1,
        (true, false) => (pictures.tile_columns_minus1 + 1) * (pictures.tile_rows_minus1 + 1) - 1,
        (true, true) => (pictures.tile_columns_minus1 + 1) * sequence.height_in_ctbs - 1,
    };

    let entry_points = bits.unsigned_max(maximum)?;
    if entry_points > MAX_ENTRY_POINTS {
        return Err(Stop::EntryPointCount);
    }

    Ok(())
}
