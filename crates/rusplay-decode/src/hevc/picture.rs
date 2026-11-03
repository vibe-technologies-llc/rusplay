use std::rc::Rc;

use super::{
    bits::{Bits, count},
    scaling::scan_scaling_list_data,
    sequence::SequenceParameters,
    stop::Stop,
};

const MAX_PICTURE_PARAMETER_SET_ID: u32 = 63;
const MAX_SEQUENCE_PARAMETER_SET_ID: u32 = 15;
const MAX_DEFAULT_REFERENCES_MINUS1: u32 = 14;
const MAX_TILE_COLUMNS_MINUS1: u32 = 18;
const MAX_TILE_ROWS_MINUS1: u32 = 20;
const MAX_STORED_TILE_COUNT_MINUS1: u32 = 255;
const MAX_CHROMA_QP_OFFSETS_MINUS1: u32 = 5;
const EXTENSION_FOUR_BITS: usize = 4;

#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)]
pub(super) struct PictureParameters {
    pub(super) sequence: Rc<SequenceParameters>,
    pub(super) dependent_slice_segments_enabled: bool,
    pub(super) output_flag_present: bool,
    pub(super) extra_slice_header_bits: usize,
    pub(super) cabac_init_present: bool,
    pub(super) default_l0_references_minus1: u32,
    pub(super) default_l1_references_minus1: u32,
    pub(super) slice_chroma_qp_offsets_present: bool,
    pub(super) weighted_prediction: bool,
    pub(super) weighted_bi_prediction: bool,
    pub(super) tiles_enabled: bool,
    pub(super) entropy_coding_sync_enabled: bool,
    pub(super) tile_columns_minus1: u32,
    pub(super) tile_rows_minus1: u32,
    pub(super) loop_filter_across_slices_enabled: bool,
    pub(super) deblocking_filter_override_enabled: bool,
    pub(super) deblocking_filter_disabled: bool,
    pub(super) lists_modification_present: bool,
    pub(super) chroma_qp_offset_list_enabled: bool,
    pub(super) current_picture_reference_enabled: bool,
    pub(super) slice_act_qp_offsets_present: bool,
}

struct Tiles {
    columns_minus1: u32,
    rows_minus1: u32,
}

struct ScreenContentExtension {
    current_picture_reference_enabled: bool,
    slice_act_qp_offsets_present: bool,
}

fn scan_tile_sizes(
    bits: &mut Bits,
    divisions_minus1: u32,
    total: u32,
    overflow: Stop,
) -> Result<(), Stop> {
    let mut remaining = total - 1;
    for _ in 0..divisions_minus1 {
        if remaining == 0 {
            return Err(overflow);
        }
        let size_minus1 = bits.unsigned_max(remaining - 1)?;
        remaining -= size_minus1 + 1;
    }

    Ok(())
}

fn scan_tiles(bits: &mut Bits, sequence: &SequenceParameters) -> Result<Tiles, Stop> {
    let columns_minus1 = bits.unsigned_max(sequence.width_in_ctbs - 1)?;
    let rows_minus1 = bits.unsigned_max(sequence.height_in_ctbs - 1)?;
    if columns_minus1 > MAX_STORED_TILE_COUNT_MINUS1 || rows_minus1 > MAX_STORED_TILE_COUNT_MINUS1 {
        return Err(Stop::ParserRejects);
    }
    if columns_minus1 > MAX_TILE_COLUMNS_MINUS1 {
        return Err(Stop::TileColumnCount);
    }
    if rows_minus1 > MAX_TILE_ROWS_MINUS1 {
        return Err(Stop::TileRowCount);
    }

    let uniform_spacing = bits.bit()?;
    if !uniform_spacing {
        scan_tile_sizes(
            bits,
            columns_minus1,
            sequence.width_in_ctbs,
            Stop::TileColumnWidth,
        )?;
        scan_tile_sizes(
            bits,
            rows_minus1,
            sequence.height_in_ctbs,
            Stop::TileRowHeight,
        )?;
    }
    bits.skip(1)?;

    Ok(Tiles {
        columns_minus1,
        rows_minus1,
    })
}

fn scan_range_extension(bits: &mut Bits, transform_skip_enabled: bool) -> Result<bool, Stop> {
    if transform_skip_enabled {
        bits.unsigned()?;
    }
    bits.skip(1)?;

    let chroma_qp_offset_list_enabled = bits.bit()?;
    if chroma_qp_offset_list_enabled {
        bits.unsigned()?;
        let entries = bits.unsigned_max(MAX_CHROMA_QP_OFFSETS_MINUS1)? + 1;
        for _ in 0..entries {
            bits.signed()?;
            bits.signed()?;
        }
    }

    bits.unsigned()?;
    bits.unsigned()?;

    Ok(chroma_qp_offset_list_enabled)
}

fn scan_screen_content_extension(
    bits: &mut Bits,
    sequence: &SequenceParameters,
) -> Result<ScreenContentExtension, Stop> {
    let current_picture_reference_enabled = bits.bit()?;
    let mut slice_act_qp_offsets_present = false;
    if bits.bit()? {
        slice_act_qp_offsets_present = bits.bit()?;
        for _ in 0..3 {
            bits.signed()?;
        }
    }

    if bits.bit()? {
        let initializers = bits.unsigned_max(sequence.palette_predictor_size)?;
        if initializers > 0 {
            let monochrome = bits.bit()?;
            let luma = bits.unsigned()?;
            if luma != sequence.bit_depth_luma_minus8 {
                return Err(Stop::ParserRejects);
            }
            let mut depths = vec![luma];
            if !monochrome {
                let chroma = bits.unsigned()?;
                if chroma != sequence.bit_depth_chroma_minus8 {
                    return Err(Stop::ParserRejects);
                }
                depths.extend([chroma, chroma]);
            }
            for depth in depths {
                for _ in 0..initializers {
                    bits.skip(count(depth) + 8)?;
                }
            }
        }
    }

    Ok(ScreenContentExtension {
        current_picture_reference_enabled,
        slice_act_qp_offsets_present,
    })
}

pub(super) fn scan_picture_parameter_set(
    bits: &mut Bits,
    sequences: impl Fn(u8) -> Option<Rc<SequenceParameters>>,
) -> Result<(u8, PictureParameters), Stop> {
    let id = bits.unsigned_max(MAX_PICTURE_PARAMETER_SET_ID)?;
    let sequence_id = bits.unsigned_max(MAX_SEQUENCE_PARAMETER_SET_ID)?;
    let sequence = u8::try_from(sequence_id)
        .ok()
        .and_then(sequences)
        .ok_or(Stop::ParserRejects)?;

    let dependent_slice_segments_enabled = bits.bit()?;
    let output_flag_present = bits.bit()?;
    let extra_slice_header_bits = count(bits.bits(3)?);
    bits.skip(1)?;
    let cabac_init_present = bits.bit()?;

    let default_l0_references_minus1 = bits.unsigned_max(MAX_DEFAULT_REFERENCES_MINUS1)?;
    let default_l1_references_minus1 = bits.unsigned_max(MAX_DEFAULT_REFERENCES_MINUS1)?;
    bits.signed()?;
    bits.skip(1)?;
    let transform_skip_enabled = bits.bit()?;
    if bits.bit()? {
        bits.unsigned()?;
    }
    bits.signed()?;
    bits.signed()?;

    let slice_chroma_qp_offsets_present = bits.bit()?;
    let weighted_prediction = bits.bit()?;
    let weighted_bi_prediction = bits.bit()?;
    bits.skip(1)?;
    let tiles_enabled = bits.bit()?;
    let entropy_coding_sync_enabled = bits.bit()?;

    let mut tiles = Tiles {
        columns_minus1: 0,
        rows_minus1: 0,
    };
    if tiles_enabled {
        tiles = scan_tiles(bits, &sequence)?;
    }

    let loop_filter_across_slices_enabled = bits.bit()?;
    let mut deblocking_filter_override_enabled = false;
    let mut deblocking_filter_disabled = false;
    if bits.bit()? {
        deblocking_filter_override_enabled = bits.bit()?;
        deblocking_filter_disabled = bits.bit()?;
        if !deblocking_filter_disabled {
            bits.signed()?;
            bits.signed()?;
        }
    }

    if bits.bit()? {
        scan_scaling_list_data(bits)?;
    }

    let lists_modification_present = bits.bit()?;
    bits.unsigned()?;
    bits.skip(1)?;

    let mut chroma_qp_offset_list_enabled = false;
    let mut screen_content = ScreenContentExtension {
        current_picture_reference_enabled: false,
        slice_act_qp_offsets_present: false,
    };
    if bits.bit()? {
        if bits.bit()? {
            chroma_qp_offset_list_enabled = scan_range_extension(bits, transform_skip_enabled)?;
        }
        if bits.bit()? || bits.bit()? {
            return Err(Stop::ParserRejects);
        }
        if bits.bit()? {
            screen_content = scan_screen_content_extension(bits, &sequence)?;
        }
        bits.skip(EXTENSION_FOUR_BITS)?;
    }

    Ok((
        u8::try_from(id).unwrap_or(0),
        PictureParameters {
            sequence,
            dependent_slice_segments_enabled,
            output_flag_present,
            extra_slice_header_bits,
            cabac_init_present,
            default_l0_references_minus1,
            default_l1_references_minus1,
            slice_chroma_qp_offsets_present,
            weighted_prediction,
            weighted_bi_prediction,
            tiles_enabled,
            entropy_coding_sync_enabled,
            tile_columns_minus1: tiles.columns_minus1,
            tile_rows_minus1: tiles.rows_minus1,
            loop_filter_across_slices_enabled,
            deblocking_filter_override_enabled,
            deblocking_filter_disabled,
            lists_modification_present,
            chroma_qp_offset_list_enabled,
            current_picture_reference_enabled: screen_content.current_picture_reference_enabled,
            slice_act_qp_offsets_present: screen_content.slice_act_qp_offsets_present,
        },
    ))
}
