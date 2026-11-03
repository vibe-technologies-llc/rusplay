use super::bits::BitReader;
use crate::{Av1SyntaxError, BitstreamUnit};

const HIGHEST_PROFILE: u32 = 2;
const PROFILE_HIGH: u32 = 1;
const PROFILE_PROFESSIONAL: u32 = 2;
const SELECT: u8 = 2;
const TIER_LEVEL_THRESHOLD: u32 = 7;
const PRIMARIES_BT709: u32 = 1;
const TRANSFER_SRGB: u32 = 13;
const MATRIX_IDENTITY: u32 = 0;
const UNSPECIFIED: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DecoderModel {
    pub(super) buffer_removal_time_length: u32,
    pub(super) frame_presentation_time_length: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OperatingPoint {
    pub(super) idc: u32,
    pub(super) decoder_model_present: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FrameIdLengths {
    pub(super) frame_id: u32,
    pub(super) delta: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub(super) struct Sequence {
    pub(super) reduced_still_picture_header: bool,
    pub(super) decoder_model: Option<DecoderModel>,
    pub(super) equal_picture_interval: bool,
    pub(super) operating_points: Vec<OperatingPoint>,
    pub(super) frame_width_bits: u32,
    pub(super) frame_height_bits: u32,
    pub(super) max_frame_width: u32,
    pub(super) max_frame_height: u32,
    pub(super) frame_id_lengths: Option<FrameIdLengths>,
    pub(super) use_128x128_superblock: bool,
    pub(super) enable_warped_motion: bool,
    pub(super) enable_order_hint: bool,
    pub(super) enable_ref_frame_mvs: bool,
    pub(super) force_screen_content_tools: u8,
    pub(super) force_integer_mv: u8,
    pub(super) order_hint_bits: u32,
    pub(super) enable_superres: bool,
    pub(super) enable_cdef: bool,
    pub(super) enable_restoration: bool,
    pub(super) mono_chrome: bool,
    pub(super) subsampling_x: bool,
    pub(super) subsampling_y: bool,
    pub(super) separate_uv_delta_q: bool,
    pub(super) film_grain_params_present: bool,
}

impl Sequence {
    pub(super) fn planes(&self) -> u32 {
        if self.mono_chrome { 1 } else { 3 }
    }

    pub(super) fn superblock_shift(&self) -> u32 {
        if self.use_128x128_superblock { 5 } else { 4 }
    }
}

pub(super) fn parse(payload: &[u8]) -> Result<Sequence, Av1SyntaxError> {
    let mut r = BitReader::new(payload, BitstreamUnit::SequenceHeader);

    let profile = r.bits(3)?;
    if profile > HIGHEST_PROFILE {
        return Err(Av1SyntaxError::ReservedProfile(profile));
    }
    r.bit()?;
    let reduced_still_picture_header = r.bit()?;

    let Operation {
        decoder_model,
        equal_picture_interval,
        operating_points,
    } = operation(&mut r, reduced_still_picture_header)?;

    let frame_width_bits = r.bits(4)? + 1;
    let frame_height_bits = r.bits(4)? + 1;
    let max_frame_width = r.bits(frame_width_bits)? + 1;
    let max_frame_height = r.bits(frame_height_bits)? + 1;

    let frame_id_lengths = if !reduced_still_picture_header && r.bit()? {
        let delta = r.bits(4)? + 2;
        let additional = r.bits(3)? + 1;
        Some(FrameIdLengths {
            frame_id: delta + additional,
            delta,
        })
    } else {
        None
    };

    let use_128x128_superblock = r.bit()?;
    r.bit()?;
    r.bit()?;

    let mut enable_warped_motion = false;
    let mut enable_order_hint = false;
    let mut enable_ref_frame_mvs = false;
    let mut force_screen_content_tools = SELECT;
    let mut force_integer_mv = SELECT;
    let mut order_hint_bits = 0;

    if !reduced_still_picture_header {
        r.bit()?;
        r.bit()?;
        enable_warped_motion = r.bit()?;
        r.bit()?;
        enable_order_hint = r.bit()?;
        if enable_order_hint {
            r.bit()?;
            enable_ref_frame_mvs = r.bit()?;
        }
        if !r.bit()? {
            force_screen_content_tools = u8::from(r.bit()?);
        }
        force_integer_mv = if force_screen_content_tools > 0 && !r.bit()? {
            u8::from(r.bit()?)
        } else {
            SELECT
        };
        if enable_order_hint {
            order_hint_bits = r.bits(3)? + 1;
        }
    }

    let enable_superres = r.bit()?;
    let enable_cdef = r.bit()?;
    let enable_restoration = r.bit()?;
    let color = color_config(&mut r, profile)?;
    let film_grain_params_present = r.bit()?;

    Ok(Sequence {
        reduced_still_picture_header,
        decoder_model,
        equal_picture_interval,
        operating_points,
        frame_width_bits,
        frame_height_bits,
        max_frame_width,
        max_frame_height,
        frame_id_lengths,
        use_128x128_superblock,
        enable_warped_motion,
        enable_order_hint,
        enable_ref_frame_mvs,
        force_screen_content_tools,
        force_integer_mv,
        order_hint_bits,
        enable_superres,
        enable_cdef,
        enable_restoration,
        mono_chrome: color.mono_chrome,
        subsampling_x: color.subsampling_x,
        subsampling_y: color.subsampling_y,
        separate_uv_delta_q: color.separate_uv_delta_q,
        film_grain_params_present,
    })
}

struct Operation {
    decoder_model: Option<DecoderModel>,
    equal_picture_interval: bool,
    operating_points: Vec<OperatingPoint>,
}

fn operation(r: &mut BitReader<'_>, reduced: bool) -> Result<Operation, Av1SyntaxError> {
    let mut decoder_model = None;
    let mut equal_picture_interval = false;
    let mut operating_points = Vec::new();

    if reduced {
        r.bits(5)?;
        operating_points.push(OperatingPoint {
            idc: 0,
            decoder_model_present: false,
        });
    } else {
        let mut buffer_delay_length = 0;
        if r.bit()? {
            r.bits(32)?;
            r.bits(32)?;
            equal_picture_interval = r.bit()?;
            if equal_picture_interval {
                r.uvlc()?;
            }
            if r.bit()? {
                buffer_delay_length = r.bits(5)? + 1;
                r.bits(32)?;
                decoder_model = Some(DecoderModel {
                    buffer_removal_time_length: r.bits(5)? + 1,
                    frame_presentation_time_length: r.bits(5)? + 1,
                });
            }
        }

        let initial_display_delay_present = r.bit()?;
        let count = r.bits(5)? + 1;
        for _ in 0..count {
            let idc = r.bits(12)?;
            if r.bits(5)? > TIER_LEVEL_THRESHOLD {
                r.bit()?;
            }
            let decoder_model_present = decoder_model.is_some() && r.bit()?;
            if decoder_model_present {
                r.bits(buffer_delay_length)?;
                r.bits(buffer_delay_length)?;
                r.bit()?;
            }
            if initial_display_delay_present && r.bit()? {
                r.bits(4)?;
            }
            operating_points.push(OperatingPoint {
                idc,
                decoder_model_present,
            });
        }
    }

    Ok(Operation {
        decoder_model,
        equal_picture_interval,
        operating_points,
    })
}

#[allow(clippy::struct_excessive_bools)]
struct ColorConfig {
    mono_chrome: bool,
    subsampling_x: bool,
    subsampling_y: bool,
    separate_uv_delta_q: bool,
}

fn color_config(r: &mut BitReader<'_>, profile: u32) -> Result<ColorConfig, Av1SyntaxError> {
    let high_bitdepth = r.bit()?;
    let twelve_bit = profile == PROFILE_PROFESSIONAL && high_bitdepth && r.bit()?;
    let mono_chrome = profile != PROFILE_HIGH && r.bit()?;

    let (primaries, transfer, matrix) = if r.bit()? {
        (r.bits(8)?, r.bits(8)?, r.bits(8)?)
    } else {
        (UNSPECIFIED, UNSPECIFIED, UNSPECIFIED)
    };

    if mono_chrome {
        r.bit()?;
        return Ok(ColorConfig {
            mono_chrome,
            subsampling_x: true,
            subsampling_y: true,
            separate_uv_delta_q: false,
        });
    }

    let (subsampling_x, subsampling_y) =
        if primaries == PRIMARIES_BT709 && transfer == TRANSFER_SRGB && matrix == MATRIX_IDENTITY {
            (false, false)
        } else {
            r.bit()?;
            let subsampling = match profile {
                0 => (true, true),
                PROFILE_HIGH => (false, false),
                _ if twelve_bit => {
                    let x = r.bit()?;
                    (x, x && r.bit()?)
                }
                _ => (true, false),
            };
            if subsampling == (true, true) {
                r.bits(2)?;
            }
            subsampling
        };

    Ok(ColorConfig {
        mono_chrome,
        subsampling_x,
        subsampling_y,
        separate_uv_delta_q: r.bit()?,
    })
}
