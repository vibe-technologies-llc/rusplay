use super::{
    bits::{Bits, count},
    rps::{Sets, ShortTermSet, scan_short_term_set},
    scaling::scan_scaling_list_data,
    stop::Stop,
};

const MAX_SUB_LAYERS_MINUS1: u32 = 6;
const MAX_SEQUENCE_PARAMETER_SET_ID: u32 = 15;
const SUB_LAYER_SLOTS: usize = 8;
const PROFILE_BITS: usize = 88;
const LEVEL_BITS: usize = 8;
const SEPARATE_COLOUR_PLANE_CHROMA_FORMAT: u32 = 3;
const MAX_CHROMA_FORMAT: u32 = 3;
const MAX_PICTURE_DIMENSION: u32 = 16888;
const MAX_BIT_DEPTH_MINUS8: u32 = 6;
const MAX_POC_LSB_BITS_MINUS4: u32 = 12;
const MAX_DEC_PIC_BUFFERING_MINUS1: u32 = 16;
const MAX_MIN_CODING_BLOCK_MINUS3: u32 = 3;
const MAX_CODING_BLOCK_DIFF: u32 = 6;
const MAX_MIN_TRANSFORM_BLOCK_MINUS2: u32 = 3;
const MAX_TRANSFORM_BLOCK_DIFF: u32 = 3;
const MAX_TRANSFORM_BLOCK_LOG2: u32 = 5;
const MAX_TRANSFORM_HIERARCHY_DEPTH: u32 = 4;
const MAX_PCM_CODING_BLOCK_DIFF: u32 = 2;
const PCM_BIT_DEPTH_BITS: usize = 4;
const MAX_SHORT_TERM_SETS: u32 = 64;
const MAX_LONG_TERM_SPS_PICTURES: u32 = 32;
const MAX_CHROMA_SAMPLE_LOCATION: u32 = 5;
const EXTENDED_ASPECT_RATIO: u32 = 255;
const MAX_MIN_SPATIAL_SEGMENTATION: u32 = 4095;
const MAX_MV_LENGTH_HORIZONTAL: u32 = 16;
const MAX_MV_LENGTH_VERTICAL: u32 = 15;
const MAX_ELEMENTAL_DURATION: u32 = 2047;
const MAX_CPB_COUNT_MINUS1: u32 = 31;
const MAX_PALETTE_SIZE: u32 = 64;
const MAX_PALETTE_PREDICTOR_SIZE: u32 = 128;
const PALETTE_INITIALIZER_SLOTS: u32 = 128;
const RANGE_EXTENSION_FLAGS: usize = 9;
const MOTION_VECTOR_RESOLUTION_BITS: usize = 2;
const TIMING_WORD_BITS: usize = 32;
const VPS_ID_BITS: usize = 4;

#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)]
pub(super) struct SequenceParameters {
    pub(super) chroma_format_idc: u32,
    pub(super) chroma_array_type: u32,
    pub(super) bit_depth_luma_minus8: u32,
    pub(super) bit_depth_chroma_minus8: u32,
    pub(super) width_in_ctbs: u32,
    pub(super) height_in_ctbs: u32,
    pub(super) pic_order_count_lsb_bits: usize,
    pub(super) max_dec_pic_buffering_minus1: u8,
    pub(super) short_term_sets: Vec<ShortTermSet>,
    pub(super) long_term_present: bool,
    pub(super) long_term_used_by_current: Vec<bool>,
    pub(super) temporal_mvp_enabled: bool,
    pub(super) sample_adaptive_offset_enabled: bool,
    pub(super) separate_colour_plane: bool,
    pub(super) motion_vector_resolution_control_idc: u32,
    pub(super) palette_predictor_size: u32,
}

impl SequenceParameters {
    pub(super) fn picture_size_in_ctbs(&self) -> u32 {
        self.width_in_ctbs.wrapping_mul(self.height_in_ctbs)
    }
}

pub(super) fn sub_layer_count(bits: &mut Bits) -> Result<usize, Stop> {
    let minus1 = bits.bits(3)?;
    if minus1 > MAX_SUB_LAYERS_MINUS1 {
        return Err(Stop::SubLayerCount);
    }

    Ok(count(minus1))
}

fn scan_profile_tier_level(bits: &mut Bits, sub_layers: usize) -> Result<(), Stop> {
    bits.skip(PROFILE_BITS + LEVEL_BITS)?;

    let mut level_present = Vec::with_capacity(sub_layers);
    for _ in 0..sub_layers {
        bits.bit()?;
        level_present.push(bits.bit()?);
    }
    if sub_layers > 0 {
        bits.skip((SUB_LAYER_SLOTS - sub_layers) * 2)?;
    }
    for present in level_present {
        if present {
            bits.skip(PROFILE_BITS + LEVEL_BITS)?;
        }
    }

    Ok(())
}

fn scan_sub_layer_hrd(bits: &mut Bits, cpb_count: u32, sub_picture: bool) -> Result<(), Stop> {
    for _ in 0..cpb_count {
        bits.signed()?;
        bits.signed()?;
        if sub_picture {
            bits.signed()?;
            bits.signed()?;
        }
        bits.bit()?;
    }

    Ok(())
}

fn scan_hrd_parameters(
    bits: &mut Bits,
    common_information: bool,
    sub_layers: usize,
) -> Result<(), Stop> {
    let mut nal = false;
    let mut vcl = false;
    let mut sub_picture = false;

    if common_information {
        nal = bits.bit()?;
        vcl = bits.bit()?;
        if nal || vcl {
            sub_picture = bits.bit()?;
            if sub_picture {
                bits.skip(8 + 5 + 1 + 5)?;
            }
            bits.skip(4 + 4)?;
            if sub_picture {
                bits.skip(4)?;
            }
            bits.skip(5 + 5 + 5)?;
        }
    }

    for _ in 0..=sub_layers {
        let fixed_rate = bits.bit()?;
        let fixed_within_sequence = !fixed_rate && bits.bit()?;
        let mut low_delay = false;
        if fixed_within_sequence {
            bits.unsigned_max(MAX_ELEMENTAL_DURATION)?;
        } else {
            low_delay = bits.bit()?;
        }

        let mut cpb_count_minus1 = 0;
        if !low_delay {
            cpb_count_minus1 = bits.unsigned_max(MAX_CPB_COUNT_MINUS1)?;
        }

        if nal {
            scan_sub_layer_hrd(bits, cpb_count_minus1 + 1, sub_picture)?;
        }
        if vcl {
            scan_sub_layer_hrd(bits, cpb_count_minus1 + 1, sub_picture)?;
        }
    }

    Ok(())
}

fn scan_video_usability(bits: &mut Bits, sub_layers: usize) -> Result<(), Stop> {
    if bits.bit()? && bits.bits(8)? == EXTENDED_ASPECT_RATIO {
        bits.skip(16 + 16)?;
    }
    if bits.bit()? {
        bits.skip(1)?;
    }
    if bits.bit()? {
        bits.skip(3 + 1)?;
        if bits.bit()? {
            bits.skip(8 + 8 + 8)?;
        }
    }
    if bits.bit()? {
        bits.unsigned_max(MAX_CHROMA_SAMPLE_LOCATION)?;
        bits.unsigned_max(MAX_CHROMA_SAMPLE_LOCATION)?;
    }

    bits.skip(3)?;
    if bits.bit()? {
        for _ in 0..4 {
            bits.unsigned()?;
        }
    }

    if bits.bit()? {
        bits.skip(2 * TIMING_WORD_BITS)?;
        if bits.bit()? {
            bits.unsigned()?;
        }
        if bits.bit()? {
            scan_hrd_parameters(bits, true, sub_layers)?;
        }
    }

    if bits.bit()? {
        bits.skip(3)?;
        bits.unsigned_max(MAX_MIN_SPATIAL_SEGMENTATION)?;
        bits.unsigned()?;
        bits.unsigned()?;
        bits.unsigned_max(MAX_MV_LENGTH_HORIZONTAL)?;
        bits.unsigned_max(MAX_MV_LENGTH_VERTICAL)?;
    }

    Ok(())
}

struct SccExtension {
    motion_vector_resolution_control_idc: u32,
    palette_predictor_size: u32,
}

fn scan_screen_content_extension(
    bits: &mut Bits,
    chroma_format_idc: u32,
    bit_depth_luma_minus8: u32,
    bit_depth_chroma_minus8: u32,
) -> Result<SccExtension, Stop> {
    bits.skip(1)?;
    let mut palette_predictor_size = 0;

    if bits.bit()? {
        let palette_max_size = bits.unsigned_max(MAX_PALETTE_SIZE)?;
        let delta = bits.unsigned_max(MAX_PALETTE_PREDICTOR_SIZE - palette_max_size)?;
        palette_predictor_size = palette_max_size + delta;

        if bits.bit()? {
            let wrapped_maximum = u32::from(
                u8::try_from(palette_predictor_size)
                    .unwrap_or(u8::MAX)
                    .wrapping_sub(1),
            );
            let initializers = bits.unsigned_max(wrapped_maximum)? + 1;
            if initializers > PALETTE_INITIALIZER_SLOTS {
                return Err(Stop::PalettePredictorSize);
            }

            let components = if chroma_format_idc == 0 { 1 } else { 3 };
            for component in 0..components {
                let depth = if component == 0 {
                    bit_depth_luma_minus8
                } else {
                    bit_depth_chroma_minus8
                };
                for _ in 0..initializers {
                    bits.skip(count(depth) + 8)?;
                }
            }
        }
    }

    let motion_vector_resolution_control_idc = bits.bits(MOTION_VECTOR_RESOLUTION_BITS)?;
    bits.skip(1)?;

    Ok(SccExtension {
        motion_vector_resolution_control_idc,
        palette_predictor_size,
    })
}

struct Format {
    chroma_format_idc: u32,
    chroma_array_type: u32,
    separate_colour_plane: bool,
    width: u32,
    height: u32,
    bit_depth_luma_minus8: u32,
    bit_depth_chroma_minus8: u32,
}

fn scan_format(bits: &mut Bits) -> Result<Format, Stop> {
    let chroma_format_idc = bits.unsigned_max(MAX_CHROMA_FORMAT)?;
    let separate_colour_plane =
        chroma_format_idc == SEPARATE_COLOUR_PLANE_CHROMA_FORMAT && bits.bit()?;
    let chroma_array_type = if separate_colour_plane {
        0
    } else {
        chroma_format_idc
    };

    let width = bits.unsigned()?;
    let height = bits.unsigned()?;
    if !(1..=MAX_PICTURE_DIMENSION).contains(&width)
        || !(1..=MAX_PICTURE_DIMENSION).contains(&height)
    {
        return Err(Stop::ParserRejects);
    }

    if bits.bit()? {
        for _ in 0..4 {
            bits.unsigned()?;
        }
    }

    Ok(Format {
        chroma_format_idc,
        chroma_array_type,
        separate_colour_plane,
        width,
        height,
        bit_depth_luma_minus8: bits.unsigned_max(MAX_BIT_DEPTH_MINUS8)?,
        bit_depth_chroma_minus8: bits.unsigned_max(MAX_BIT_DEPTH_MINUS8)?,
    })
}

fn scan_ordering_info(bits: &mut Bits, sub_layers: usize) -> Result<u32, Stop> {
    let ordering_info_for_every_layer = bits.bit()?;
    let first_ordered = if ordering_info_for_every_layer {
        0
    } else {
        sub_layers
    };

    let mut max_dec_pic_buffering_minus1 = 0;
    for _ in first_ordered..=sub_layers {
        max_dec_pic_buffering_minus1 = bits.unsigned_max(MAX_DEC_PIC_BUFFERING_MINUS1)?;
        bits.unsigned_max(max_dec_pic_buffering_minus1)?;
        bits.unsigned()?;
    }

    Ok(max_dec_pic_buffering_minus1)
}

fn scan_ctb_size(bits: &mut Bits) -> Result<u32, Stop> {
    let min_coding_block_log2 = bits.unsigned_max(MAX_MIN_CODING_BLOCK_MINUS3)? + 3;
    let coding_block_diff = bits.unsigned_max(MAX_CODING_BLOCK_DIFF)?;
    let min_transform_block_log2 = bits.unsigned_max(MAX_MIN_TRANSFORM_BLOCK_MINUS2)? + 2;
    let transform_block_diff = bits.unsigned_max(MAX_TRANSFORM_BLOCK_DIFF)?;

    let ctb_log2 = min_coding_block_log2 + coding_block_diff;
    let max_transform_block_log2 = min_transform_block_log2 + transform_block_diff;
    if max_transform_block_log2 > ctb_log2.min(MAX_TRANSFORM_BLOCK_LOG2) {
        return Err(Stop::ParserRejects);
    }

    Ok(1 << ctb_log2)
}

fn scan_short_term_sets(
    bits: &mut Bits,
    max_dec_pic_buffering_minus1: u8,
) -> Result<Vec<ShortTermSet>, Stop> {
    let declared = bits.unsigned_max(MAX_SHORT_TERM_SETS)?;

    let mut short_term_sets = Vec::new();
    for index in 0..declared {
        let sets = Sets {
            existing: &short_term_sets,
            declared: count(declared),
            max_dec_pic_buffering_minus1,
        };
        let set = scan_short_term_set(bits, &sets, count(index))?;
        short_term_sets.push(set);
    }

    Ok(short_term_sets)
}

fn scan_long_term_sequence_pictures(
    bits: &mut Bits,
    poc_lsb_bits: usize,
) -> Result<Vec<bool>, Stop> {
    let pictures = bits.unsigned_max(MAX_LONG_TERM_SPS_PICTURES)?;

    let mut used_by_current = Vec::new();
    for _ in 0..pictures {
        bits.skip(poc_lsb_bits)?;
        used_by_current.push(bits.bit()?);
    }

    Ok(used_by_current)
}

fn scan_extensions(bits: &mut Bits, format: &Format) -> Result<SccExtension, Stop> {
    let mut screen_content = SccExtension {
        motion_vector_resolution_control_idc: 0,
        palette_predictor_size: 0,
    };

    if bits.bit()? {
        if bits.bit()? {
            bits.skip(RANGE_EXTENSION_FLAGS)?;
        }
        if bits.bit()? || bits.bit()? {
            return Err(Stop::ParserRejects);
        }
        if bits.bit()? {
            screen_content = scan_screen_content_extension(
                bits,
                format.chroma_format_idc,
                format.bit_depth_luma_minus8,
                format.bit_depth_chroma_minus8,
            )?;
        }
    }

    Ok(screen_content)
}

pub(super) fn scan_sequence_parameter_set(
    bits: &mut Bits,
) -> Result<(u8, SequenceParameters), Stop> {
    bits.skip(VPS_ID_BITS)?;
    let sub_layers = sub_layer_count(bits)?;
    bits.skip(1)?;
    scan_profile_tier_level(bits, sub_layers)?;

    let id = bits.unsigned_max(MAX_SEQUENCE_PARAMETER_SET_ID)?;
    let format = scan_format(bits)?;
    let poc_lsb_bits_minus4 = bits.unsigned_max(MAX_POC_LSB_BITS_MINUS4)?;
    let max_dec_pic_buffering_minus1 = scan_ordering_info(bits, sub_layers)?;
    let ctb_size = scan_ctb_size(bits)?;

    bits.unsigned_max(MAX_TRANSFORM_HIERARCHY_DEPTH)?;
    bits.unsigned_max(MAX_TRANSFORM_HIERARCHY_DEPTH)?;

    if bits.bit()? && bits.bit()? {
        scan_scaling_list_data(bits)?;
    }

    bits.skip(1)?;
    let sample_adaptive_offset_enabled = bits.bit()?;

    if bits.bit()? {
        bits.skip(2 * PCM_BIT_DEPTH_BITS)?;
        bits.unsigned_max(MAX_PCM_CODING_BLOCK_DIFF)?;
        bits.unsigned_max(MAX_PCM_CODING_BLOCK_DIFF)?;
        bits.skip(1)?;
    }

    let max_dec_pic_buffering_minus1 = u8::try_from(max_dec_pic_buffering_minus1).unwrap_or(0);
    let short_term_sets = scan_short_term_sets(bits, max_dec_pic_buffering_minus1)?;

    let pic_order_count_lsb_bits = count(poc_lsb_bits_minus4) + 4;
    let long_term_present = bits.bit()?;
    let long_term_used_by_current = if long_term_present {
        scan_long_term_sequence_pictures(bits, pic_order_count_lsb_bits)?
    } else {
        Vec::new()
    };

    let temporal_mvp_enabled = bits.bit()?;
    bits.skip(1)?;

    if bits.bit()? {
        scan_video_usability(bits, sub_layers)?;
    }

    let screen_content = scan_extensions(bits, &format)?;

    let parameters = SequenceParameters {
        chroma_format_idc: format.chroma_format_idc,
        chroma_array_type: format.chroma_array_type,
        bit_depth_luma_minus8: format.bit_depth_luma_minus8,
        bit_depth_chroma_minus8: format.bit_depth_chroma_minus8,
        width_in_ctbs: format.width.div_ceil(ctb_size),
        height_in_ctbs: format.height.div_ceil(ctb_size),
        pic_order_count_lsb_bits,
        max_dec_pic_buffering_minus1,
        short_term_sets,
        long_term_present,
        long_term_used_by_current,
        temporal_mvp_enabled,
        sample_adaptive_offset_enabled,
        separate_colour_plane: format.separate_colour_plane,
        motion_vector_resolution_control_idc: screen_content.motion_vector_resolution_control_idc,
        palette_predictor_size: screen_content.palette_predictor_size,
    };

    Ok((u8::try_from(id).unwrap_or(0), parameters))
}
