use std::rc::Rc;

use cros_codecs::codec::av1::parser::{
    BitDepth as Av1BitDepth, FrameHeaderObu, FrameType, NUM_REF_FRAMES, ObuAction, ObuType,
    ParsedObu, Parser, Profile as Av1Profile, SEG_LVL_MAX, SegmentationParams, SequenceHeaderObu,
    TileGroupObu,
};
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, EncodedFrame, PixelFormat, Resolution, SignalledColor,
    Timestamp,
};

use crate::{
    Backend, BitstreamUnit, DecodeError, DecodedFrame,
    vaapi::{
        Stream, StreamFormat, annexb, ffi,
        signal::HdrTracker,
        va::{Buffer, Display, INVALID_SURFACE, PoolSurface},
    },
};

const CODEC: Codec = Codec::Av1;
const GRAIN_SURFACES: usize = 1;
const STRENGTH_SECONDARY_MAX: u32 = 4;

fn malformed(unit: BitstreamUnit) -> impl FnOnce(String) -> DecodeError {
    annexb::malformed(CODEC, unit)
}

#[derive(Debug, Clone)]
struct Slot {
    surface: PoolSurface,
    display: PoolSurface,
    render: Resolution,
}

struct Pending {
    header: FrameHeaderObu,
    buffers: Vec<Buffer>,
    surface: Option<PoolSurface>,
    display: Option<PoolSurface>,
}

pub(super) struct Av1Decoder {
    parser: Parser,
    stream: Option<Stream>,
    sequence: Option<Rc<SequenceHeaderObu>>,
    slots: [Option<Slot>; NUM_REF_FRAMES],
    hdr: HdrTracker,
}

impl Av1Decoder {
    pub(super) fn new() -> Self {
        Self {
            parser: Parser::default(),
            stream: None,
            sequence: None,
            slots: Default::default(),
            hdr: HdrTracker::default(),
        }
    }

    pub(super) fn decode(
        &mut self,
        display: &Rc<Display>,
        frame: &EncodedFrame,
    ) -> Result<Option<DecodedFrame>, DecodeError> {
        let mut data = frame.data.as_slice();
        let mut pending: Option<Pending> = None;
        let mut output = None;

        while !data.is_empty() {
            let obu = match self
                .parser
                .read_obu(data)
                .map_err(malformed(BitstreamUnit::Obu))?
            {
                ObuAction::Process(obu) => obu,
                ObuAction::Drop(length) => {
                    data = data
                        .get(usize::try_from(length).unwrap_or(usize::MAX)..)
                        .unwrap_or_default();
                    continue;
                }
            };
            let length = obu.bytes_used;
            if obu.header.obu_type == ObuType::Metadata {
                self.hdr.read_av1_metadata(obu.as_ref());
            }

            match self
                .parser
                .parse_obu(obu)
                .map_err(malformed(BitstreamUnit::Obu))?
            {
                ParsedObu::SequenceHeader(sequence) => {
                    self.sequence = Some(sequence);
                }
                ParsedObu::FrameHeader(header) => {
                    if let Some(previous) = pending.take() {
                        output = self.finish(previous, frame.timestamp)?.or(output);
                    }
                    let current = self.begin(display, header)?;
                    if current.surface.is_none() {
                        output = self.finish(current, frame.timestamp)?.or(output);
                    } else {
                        pending = Some(current);
                    }
                }
                ParsedObu::TileGroup(tile_group) => {
                    let mut current = pending.take().ok_or_else(|| DecodeError::Malformed {
                        codec: CODEC,
                        unit: BitstreamUnit::TileGroup,
                        reason: String::from("tile group without a frame header"),
                    })?;
                    if self.add_tile_group(&mut current, &tile_group)? {
                        output = self.finish(current, frame.timestamp)?.or(output);
                    } else {
                        pending = Some(current);
                    }
                }
                ParsedObu::Frame(frame_obu) => {
                    if let Some(previous) = pending.take() {
                        output = self.finish(previous, frame.timestamp)?.or(output);
                    }
                    let mut current = self.begin(display, frame_obu.header)?;
                    if self.add_tile_group(&mut current, &frame_obu.tile_group)? {
                        output = self.finish(current, frame.timestamp)?.or(output);
                    } else {
                        pending = Some(current);
                    }
                }
                _ => {}
            }

            data = data.get(length..).unwrap_or_default();
        }

        if let Some(previous) = pending.take() {
            output = self.finish(previous, frame.timestamp)?.or(output);
        }

        Ok(output)
    }

    fn begin(
        &mut self,
        display: &Rc<Display>,
        header: FrameHeaderObu,
    ) -> Result<Pending, DecodeError> {
        if header.show_existing_frame {
            return Ok(Pending {
                header,
                buffers: Vec::new(),
                surface: None,
                display: None,
            });
        }

        let sequence = Rc::clone(self.sequence.as_ref().ok_or(
            DecodeError::MissingParameterSet {
                codec: CODEC,
                unit: BitstreamUnit::SequenceHeader,
                id: 0,
            },
        )?);

        let format = stream_format(&sequence)?;
        let (stream, reopened) = Stream::ensure(&mut self.stream, display, CODEC, format)?;
        if reopened {
            self.slots = Default::default();
        }
        stream.color = self.hdr.resolve(
            signalled_color(&sequence),
            header.frame_type == FrameType::KeyFrame,
        );

        let surface = stream.pool.acquire()?;
        let grain = if header.film_grain_params.apply_grain {
            Some(stream.pool.acquire()?)
        } else {
            None
        };

        let parameters =
            picture_parameters(&sequence, &header, &surface, grain.as_ref(), &self.slots);
        let buffers = vec![
            stream
                .session
                .parameter(ffi::VABufferType_VAPictureParameterBufferType, &parameters)?,
        ];

        Ok(Pending {
            header,
            buffers,
            surface: Some(surface),
            display: grain,
        })
    }

    fn add_tile_group(
        &mut self,
        pending: &mut Pending,
        tile_group: &TileGroupObu<'_>,
    ) -> Result<bool, DecodeError> {
        let Some(stream) = self.stream.as_ref() else {
            return Err(DecodeError::MissingParameterSet {
                codec: CODEC,
                unit: BitstreamUnit::SequenceHeader,
                id: 0,
            });
        };
        if pending.surface.is_none() {
            return Err(DecodeError::Malformed {
                codec: CODEC,
                unit: BitstreamUnit::TileGroup,
                reason: String::from("tile group for a frame shown from the reference buffer"),
            });
        }

        let tiles: Vec<ffi::VASliceParameterBufferAV1> = tile_group
            .tiles
            .iter()
            .map(|tile| ffi::VASliceParameterBufferAV1 {
                slice_data_size: tile.tile_size,
                slice_data_offset: tile.tile_offset,
                slice_data_flag: ffi::VA_SLICE_DATA_FLAG_ALL,
                tile_row: u16::try_from(tile.tile_row).unwrap_or(0),
                tile_column: u16::try_from(tile.tile_col).unwrap_or(0),
                tg_start: u16::try_from(tile_group.tg_start).unwrap_or(0),
                tg_end: u16::try_from(tile_group.tg_end).unwrap_or(0),
                anchor_frame_idx: 0,
                tile_idx_in_tile_list: 0,
                ..Default::default()
            })
            .collect();

        pending.buffers.push(
            stream
                .session
                .parameters(ffi::VABufferType_VASliceParameterBufferType, &tiles)?,
        );
        pending
            .buffers
            .push(stream.session.slice_data(tile_group.obu.as_ref())?);

        let tiles = pending.header.tile_info.tile_cols * pending.header.tile_info.tile_rows;
        Ok(tile_group.tg_end + 1 >= tiles)
    }

    fn finish(
        &mut self,
        pending: Pending,
        timestamp: Timestamp,
    ) -> Result<Option<DecodedFrame>, DecodeError> {
        let Pending {
            header,
            buffers,
            surface,
            display,
        } = pending;

        let stream = self
            .stream
            .as_ref()
            .ok_or(DecodeError::MissingParameterSet {
                codec: CODEC,
                unit: BitstreamUnit::SequenceHeader,
                id: 0,
            })?;

        let (slot, shown) = if let Some(surface) = surface {
            stream.session.decode(surface.id, &buffers)?;
            drop(buffers);

            let slot = Slot {
                display: display.unwrap_or_else(|| surface.clone()),
                surface,
                render: Resolution::new(header.render_width, header.render_height),
            };
            (Some(slot.clone()), header.show_frame.then_some(slot))
        } else {
            let shown = self
                .slots
                .get(usize::from(header.frame_to_show_map_idx))
                .cloned()
                .flatten()
                .ok_or_else(|| DecodeError::Malformed {
                    codec: CODEC,
                    unit: BitstreamUnit::FrameHeader,
                    reason: String::from("show_existing_frame names an empty reference slot"),
                })?;
            let refresh = (header.frame_type == FrameType::KeyFrame).then(|| shown.clone());
            (refresh, Some(shown))
        };

        if let Some(slot) = slot {
            for (index, target) in self.slots.iter_mut().enumerate() {
                if (header.refresh_frame_flags >> index) & 1 == 1 {
                    *target = Some(slot.clone());
                }
            }
        }

        self.parser
            .ref_frame_update(&header)
            .map_err(malformed(BitstreamUnit::FrameHeader))?;

        shown
            .map(|slot| stream.output_sized(&slot.display, timestamp, slot.render))
            .transpose()
    }
}

fn signalled_color(sequence: &SequenceHeaderObu) -> SignalledColor {
    let color = &sequence.color_config;
    let signalled = SignalledColor::default().with_full_range(color.color_range);
    if !color.color_description_present_flag {
        return signalled;
    }

    let code = |value: u32| u8::try_from(value).unwrap_or(u8::MAX);
    signalled.with_h273_codes(
        code(color.color_primaries as u32),
        code(color.transfer_characteristics as u32),
        code(color.matrix_coefficients as u32),
    )
}

fn stream_format(sequence: &SequenceHeaderObu) -> Result<StreamFormat, DecodeError> {
    let color = &sequence.color_config;
    if color.mono_chrome {
        return Err(DecodeError::Monochrome {
            backend: Backend::Vaapi,
            codec: CODEC,
        });
    }

    let chroma = match (color.subsampling_x, color.subsampling_y) {
        (true, true) => ChromaSubsampling::Yuv420,
        (true, false) => ChromaSubsampling::Yuv422,
        (false, _) => ChromaSubsampling::Yuv444,
    };
    let depth = match sequence.bit_depth {
        Av1BitDepth::Depth8 => BitDepth::Eight,
        Av1BitDepth::Depth10 => BitDepth::Ten,
        Av1BitDepth::Depth12 => BitDepth::Twelve,
    };
    let profile = match sequence.seq_profile {
        Av1Profile::Profile0 => ffi::VAProfile_VAProfileAV1Profile0,
        Av1Profile::Profile1 => ffi::VAProfile_VAProfileAV1Profile1,
        Av1Profile::Profile2 => ffi::VAProfile_VAProfileAV1Profile2,
    };
    let coded = Resolution::new(
        u32::from(sequence.max_frame_width_minus_1) + 1,
        u32::from(sequence.max_frame_height_minus_1) + 1,
    );

    Ok(StreamFormat {
        profile,
        profile_idc: sequence.seq_profile as u8,
        pixel_format: PixelFormat { chroma, depth },
        coded,
        visible: coded,
        reference_frames: NUM_REF_FRAMES + GRAIN_SURFACES,
        reorder_frames: 0,
    })
}

fn cdef_strength(primary: u32, secondary: u32) -> u8 {
    let secondary = if secondary == STRENGTH_SECONDARY_MAX {
        secondary - 1
    } else {
        secondary
    };
    u8::try_from(((primary & 0xf) << 2) | (secondary & 0x3)).unwrap_or(0)
}

fn picture_parameters(
    sequence: &SequenceHeaderObu,
    header: &FrameHeaderObu,
    surface: &PoolSurface,
    grain: Option<&PoolSurface>,
    slots: &[Option<Slot>; NUM_REF_FRAMES],
) -> ffi::VADecPictureParameterBufferAV1 {
    let color = &sequence.color_config;
    let quantization = &header.quantization_params;
    let loop_filter = &header.loop_filter_params;
    let cdef = &header.cdef_params;
    let tiles = &header.tile_info;

    let mut parameters = ffi::VADecPictureParameterBufferAV1 {
        profile: sequence.seq_profile as u8,
        order_hint_bits_minus_1: u8::try_from(sequence.order_hint_bits_minus_1).unwrap_or(0),
        bit_depth_idx: sequence.bit_depth as u8,
        matrix_coefficients: color.matrix_coefficients as u8,
        current_frame: surface.id,
        current_display_picture: grain.map_or(INVALID_SURFACE, |grain| grain.id),
        anchor_frames_num: 0,
        anchor_frames_list: std::ptr::null_mut(),
        frame_width_minus1: u16::try_from(header.upscaled_width.saturating_sub(1)).unwrap_or(0),
        frame_height_minus1: u16::try_from(header.frame_height.saturating_sub(1)).unwrap_or(0),
        primary_ref_frame: u8::try_from(header.primary_ref_frame).unwrap_or(0),
        order_hint: u8::try_from(header.order_hint).unwrap_or(0),
        ref_frame_idx: header.ref_frame_idx,
        tile_cols: u8::try_from(tiles.tile_cols).unwrap_or(0),
        tile_rows: u8::try_from(tiles.tile_rows).unwrap_or(0),
        tile_count_minus_1: 0,
        context_update_tile_id: u16::try_from(tiles.context_update_tile_id).unwrap_or(0),
        superres_scale_denominator: u8::try_from(header.superres_denom).unwrap_or(8),
        interp_filter: header.interpolation_filter as u8,
        filter_level: [
            loop_filter.loop_filter_level[0],
            loop_filter.loop_filter_level[1],
        ],
        filter_level_u: loop_filter.loop_filter_level[2],
        filter_level_v: loop_filter.loop_filter_level[3],
        ref_deltas: loop_filter.loop_filter_ref_deltas,
        mode_deltas: loop_filter.loop_filter_mode_deltas,
        base_qindex: u8::try_from(quantization.base_q_idx).unwrap_or(0),
        y_dc_delta_q: i8::try_from(quantization.delta_q_y_dc).unwrap_or(0),
        u_dc_delta_q: i8::try_from(quantization.delta_q_u_dc).unwrap_or(0),
        u_ac_delta_q: i8::try_from(quantization.delta_q_u_ac).unwrap_or(0),
        v_dc_delta_q: i8::try_from(quantization.delta_q_v_dc).unwrap_or(0),
        v_ac_delta_q: i8::try_from(quantization.delta_q_v_ac).unwrap_or(0),
        cdef_damping_minus_3: u8::try_from(cdef.cdef_damping.saturating_sub(3)).unwrap_or(0),
        cdef_bits: u8::try_from(cdef.cdef_bits).unwrap_or(0),
        ..Default::default()
    };

    for (target, slot) in parameters.ref_frame_map.iter_mut().zip(slots) {
        *target = slot
            .as_ref()
            .map_or(INVALID_SURFACE, |slot| slot.surface.id);
    }
    for (target, &width) in parameters
        .width_in_sbs_minus_1
        .iter_mut()
        .zip(&tiles.width_in_sbs_minus_1)
    {
        *target = u16::try_from(width).unwrap_or(0);
    }
    for (target, &height) in parameters
        .height_in_sbs_minus_1
        .iter_mut()
        .zip(&tiles.height_in_sbs_minus_1)
    {
        *target = u16::try_from(height).unwrap_or(0);
    }

    let strengths = 1_usize << cdef.cdef_bits.min(3);
    for index in 0..strengths {
        parameters.cdef_y_strengths[index] = cdef_strength(
            cdef.cdef_y_pri_strength[index],
            cdef.cdef_y_sec_strength[index],
        );
        parameters.cdef_uv_strengths[index] = cdef_strength(
            cdef.cdef_uv_pri_strength[index],
            cdef.cdef_uv_sec_strength[index],
        );
    }

    let motion = &header.global_motion_params;
    for (index, warp) in parameters.wm.iter_mut().enumerate() {
        let reference = index + 1;
        warp.wmtype = motion.gm_type[reference] as u32;
        warp.wmmat[..6].copy_from_slice(&motion.gm_params[reference]);
        warp.invalid = u8::from(!motion.warp_valid[reference]);
    }

    fill_segmentation(&mut parameters.seg_info, &header.segmentation_params);
    fill_film_grain(&mut parameters.film_grain_info, header);
    set_sequence_fields(&mut parameters, sequence);
    set_frame_fields(&mut parameters, header);

    parameters
}

fn fill_segmentation(target: &mut ffi::VASegmentationStructAV1, segmentation: &SegmentationParams) {
    target.feature_data = segmentation.feature_data;
    for (mask, enabled) in target
        .feature_mask
        .iter_mut()
        .zip(&segmentation.feature_enabled)
    {
        *mask = (0..SEG_LVL_MAX)
            .filter(|&feature| enabled[feature])
            .fold(0, |mask, feature| mask | (1 << feature));
    }
    let segment_fields = unsafe { &mut target.segment_info_fields.bits };
    segment_fields.set_enabled(u32::from(segmentation.segmentation_enabled));
    segment_fields.set_update_map(u32::from(segmentation.segmentation_update_map));
    segment_fields.set_temporal_update(u32::from(segmentation.segmentation_temporal_update));
    segment_fields.set_update_data(u32::from(segmentation.segmentation_update_data));
}

fn set_sequence_fields(
    parameters: &mut ffi::VADecPictureParameterBufferAV1,
    sequence: &SequenceHeaderObu,
) {
    let color = &sequence.color_config;
    let sequence_fields = unsafe { &mut parameters.seq_info_fields.fields };
    sequence_fields.set_still_picture(u32::from(sequence.still_picture));
    sequence_fields.set_use_128x128_superblock(u32::from(sequence.use_128x128_superblock));
    sequence_fields.set_enable_filter_intra(u32::from(sequence.enable_filter_intra));
    sequence_fields.set_enable_intra_edge_filter(u32::from(sequence.enable_intra_edge_filter));
    sequence_fields.set_enable_interintra_compound(u32::from(sequence.enable_interintra_compound));
    sequence_fields.set_enable_masked_compound(u32::from(sequence.enable_masked_compound));
    sequence_fields.set_enable_dual_filter(u32::from(sequence.enable_dual_filter));
    sequence_fields.set_enable_order_hint(u32::from(sequence.enable_order_hint));
    sequence_fields.set_enable_jnt_comp(u32::from(sequence.enable_jnt_comp));
    sequence_fields.set_enable_cdef(u32::from(sequence.enable_cdef));
    sequence_fields.set_mono_chrome(u32::from(color.mono_chrome));
    sequence_fields.set_color_range(u32::from(color.color_range));
    sequence_fields.set_subsampling_x(u32::from(color.subsampling_x));
    sequence_fields.set_subsampling_y(u32::from(color.subsampling_y));
    sequence_fields.set_chroma_sample_position(color.chroma_sample_position as u32);
    sequence_fields.set_film_grain_params_present(u32::from(sequence.film_grain_params_present));
}

fn set_frame_fields(parameters: &mut ffi::VADecPictureParameterBufferAV1, header: &FrameHeaderObu) {
    let quantization = &header.quantization_params;
    let loop_filter = &header.loop_filter_params;
    let restoration = &header.loop_restoration_params;

    let picture_fields = unsafe { &mut parameters.pic_info_fields.bits };
    picture_fields.set_frame_type(header.frame_type as u32);
    picture_fields.set_show_frame(u32::from(header.show_frame));
    picture_fields.set_showable_frame(u32::from(header.showable_frame));
    picture_fields.set_error_resilient_mode(u32::from(header.error_resilient_mode));
    picture_fields.set_disable_cdf_update(u32::from(header.disable_cdf_update));
    picture_fields.set_allow_screen_content_tools(header.allow_screen_content_tools);
    picture_fields.set_force_integer_mv(header.force_integer_mv);
    picture_fields.set_allow_intrabc(u32::from(header.allow_intrabc));
    picture_fields.set_use_superres(u32::from(header.use_superres));
    picture_fields.set_allow_high_precision_mv(u32::from(header.allow_high_precision_mv));
    picture_fields.set_is_motion_mode_switchable(u32::from(header.is_motion_mode_switchable));
    picture_fields.set_use_ref_frame_mvs(u32::from(header.use_ref_frame_mvs));
    picture_fields.set_disable_frame_end_update_cdf(u32::from(header.disable_frame_end_update_cdf));
    picture_fields
        .set_uniform_tile_spacing_flag(u32::from(header.tile_info.uniform_tile_spacing_flag));
    picture_fields.set_allow_warped_motion(u32::from(header.allow_warped_motion));
    picture_fields.set_large_scale_tile(0);

    let filter_fields = unsafe { &mut parameters.loop_filter_info_fields.bits };
    filter_fields.set_sharpness_level(loop_filter.loop_filter_sharpness);
    filter_fields.set_mode_ref_delta_enabled(u8::from(loop_filter.loop_filter_delta_enabled));
    filter_fields.set_mode_ref_delta_update(u8::from(loop_filter.loop_filter_delta_update));

    let matrix_fields = unsafe { &mut parameters.qmatrix_fields.bits };
    matrix_fields.set_using_qmatrix(u16::from(quantization.using_qmatrix));
    matrix_fields.set_qm_y(u16::try_from(quantization.qm_y).unwrap_or(0));
    matrix_fields.set_qm_u(u16::try_from(quantization.qm_u).unwrap_or(0));
    matrix_fields.set_qm_v(u16::try_from(quantization.qm_v).unwrap_or(0));

    let mode_fields = unsafe { &mut parameters.mode_control_fields.bits };
    mode_fields.set_delta_q_present_flag(u32::from(quantization.delta_q_present));
    mode_fields.set_log2_delta_q_res(quantization.delta_q_res);
    mode_fields.set_delta_lf_present_flag(u32::from(loop_filter.delta_lf_present));
    mode_fields.set_log2_delta_lf_res(u32::from(loop_filter.delta_lf_res));
    mode_fields.set_delta_lf_multi(u32::from(loop_filter.delta_lf_multi));
    mode_fields.set_tx_mode(header.tx_mode as u32);
    mode_fields.set_reference_select(u32::from(header.reference_select));
    mode_fields.set_reduced_tx_set_used(u32::from(header.reduced_tx_set));
    mode_fields.set_skip_mode_present(u32::from(header.skip_mode_present));

    let restoration_fields = unsafe { &mut parameters.loop_restoration_fields.bits };
    restoration_fields.set_yframe_restoration_type(restoration.frame_restoration_type[0] as u16);
    restoration_fields.set_cbframe_restoration_type(restoration.frame_restoration_type[1] as u16);
    restoration_fields.set_crframe_restoration_type(restoration.frame_restoration_type[2] as u16);
    restoration_fields.set_lr_unit_shift(u16::from(restoration.lr_unit_shift));
    restoration_fields.set_lr_uv_shift(u16::from(restoration.lr_uv_shift));
}

fn copy_prefix(target: &mut [u8], source: &[u8]) {
    for (slot, &value) in target.iter_mut().zip(source) {
        *slot = value;
    }
}

fn fill_film_grain(target: &mut ffi::VAFilmGrainStructAV1, header: &FrameHeaderObu) {
    let grain = &header.film_grain_params;

    target.grain_seed = grain.grain_seed;
    target.num_y_points = grain.num_y_points;
    target.num_cb_points = grain.num_cb_points;
    target.num_cr_points = grain.num_cr_points;
    target.cb_mult = grain.cb_mult;
    target.cb_luma_mult = grain.cb_luma_mult;
    target.cb_offset = grain.cb_offset;
    target.cr_mult = grain.cr_mult;
    target.cr_luma_mult = grain.cr_luma_mult;
    target.cr_offset = grain.cr_offset;

    copy_prefix(&mut target.point_y_value, &grain.point_y_value);
    copy_prefix(&mut target.point_y_scaling, &grain.point_y_scaling);
    copy_prefix(&mut target.point_cb_value, &grain.point_cb_value);
    copy_prefix(&mut target.point_cb_scaling, &grain.point_cb_scaling);
    copy_prefix(&mut target.point_cr_value, &grain.point_cr_value);
    copy_prefix(&mut target.point_cr_scaling, &grain.point_cr_scaling);

    let centred = |value: u8| i8::try_from(i16::from(value) - 128).unwrap_or(0);
    for (coefficient, &value) in target
        .ar_coeffs_y
        .iter_mut()
        .zip(&grain.ar_coeffs_y_plus_128)
    {
        *coefficient = centred(value);
    }
    for (coefficient, &value) in target
        .ar_coeffs_cb
        .iter_mut()
        .zip(&grain.ar_coeffs_cb_plus_128)
    {
        *coefficient = centred(value);
    }
    for (coefficient, &value) in target
        .ar_coeffs_cr
        .iter_mut()
        .zip(&grain.ar_coeffs_cr_plus_128)
    {
        *coefficient = centred(value);
    }

    let fields = unsafe { &mut target.film_grain_info_fields.bits };
    fields.set_apply_grain(u32::from(grain.apply_grain));
    fields.set_chroma_scaling_from_luma(u32::from(grain.chroma_scaling_from_luma));
    fields.set_grain_scaling_minus_8(u32::from(grain.grain_scaling_minus_8));
    fields.set_ar_coeff_lag(grain.ar_coeff_lag);
    fields.set_ar_coeff_shift_minus_6(u32::from(grain.ar_coeff_shift_minus_6));
    fields.set_grain_scale_shift(u32::from(grain.grain_scale_shift));
    fields.set_overlap_flag(u32::from(grain.overlap_flag));
    fields.set_clip_to_restricted_range(u32::from(grain.clip_to_restricted_range));
}
