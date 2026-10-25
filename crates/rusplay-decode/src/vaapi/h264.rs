mod dpb;

use std::{io::Cursor, rc::Rc};

use cros_codecs::codec::h264::parser::{NaluHeader, NaluType, Parser, Pps, Slice, SliceType, Sps};
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, EncodedFrame, PixelFormat, Resolution, Timestamp,
};

use crate::{
    BitstreamUnit, DecodeError, DecodedFrame,
    vaapi::{
        Stream, StreamFormat, annexb, ffi,
        h264::dpb::{
            Decoded, Dpb, MAX_REFERENCE_FRAMES, OrderCount, Reference, ReferenceList, Structure,
            invalid_picture,
        },
        reorder::{Depth, Reorder},
        va::{Display, PoolSurface, Profile, SurfaceId},
    },
};

const CODEC: Codec = Codec::H264;
const MAX_LIST_ENTRIES: usize = 32;
const LEVEL_3_1: u8 = 31;
const BASELINE_PROFILE_IDC: u8 = 66;
const ZIGZAG_4X4: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];
const ZIGZAG_8X8: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

fn malformed(unit: BitstreamUnit) -> impl FnOnce(String) -> DecodeError {
    annexb::malformed(CODEC, unit)
}

struct OpenFrame {
    surface: PoolSurface,
    first: Structure,
    frame_num: u32,
    timestamp: Timestamp,
    pocs: [Option<i32>; 2],
    restart: bool,
}

impl OpenFrame {
    fn new(surface: PoolSurface, first: Structure, frame_num: u32, timestamp: Timestamp) -> Self {
        Self {
            surface,
            first,
            frame_num,
            timestamp,
            pocs: [None, None],
            restart: false,
        }
    }

    fn completes_with(&self, structure: Structure, frame_num: u32, idr: bool) -> bool {
        structure == self.first.opposite() && frame_num == self.frame_num && !idr
    }

    fn record(&mut self, structure: Structure, top: Option<i32>, bottom: Option<i32>, shift: i32) {
        if structure != Structure::Bottom {
            self.pocs[0] = top.map(|count| count - shift);
        }
        if structure != Structure::Top {
            self.pocs[1] = bottom.map(|count| count - shift);
        }
    }

    fn key(&self) -> i32 {
        self.pocs.iter().flatten().copied().min().unwrap_or(0)
    }
}

pub(super) struct H264Decoder {
    parser: Parser,
    stream: Option<Stream>,
    dpb: Dpb,
    reorder: Reorder<DecodedFrame>,
    open_frame: Option<OpenFrame>,
}

impl H264Decoder {
    pub(super) fn new() -> Self {
        Self {
            parser: Parser::default(),
            stream: None,
            dpb: Dpb::default(),
            reorder: Reorder::new(),
            open_frame: None,
        }
    }

    pub(super) fn drain(&mut self) -> Result<Vec<DecodedFrame>, DecodeError> {
        if let Some(open) = self.open_frame.take() {
            self.complete(&open)?;
        }

        Ok(self.reorder.drain())
    }

    pub(super) fn decode(
        &mut self,
        display: &Rc<Display>,
        frame: &EncodedFrame,
    ) -> Result<Option<DecodedFrame>, DecodeError> {
        let mut cursor = Cursor::new(frame.data.as_slice());
        let mut slices: Vec<Slice<'_>> = Vec::new();

        while let Some(nalu) = annexb::next_nalu::<NaluHeader>(&mut cursor, CODEC)? {
            match nalu.header.type_ {
                NaluType::Sps => {
                    self.parser
                        .parse_sps(&nalu)
                        .map_err(malformed(BitstreamUnit::SequenceParameterSet))?;
                }
                NaluType::Pps => {
                    self.parser
                        .parse_pps(&nalu)
                        .map_err(malformed(BitstreamUnit::PictureParameterSet))?;
                }
                NaluType::Slice | NaluType::SliceIdr => {
                    let slice = self
                        .parser
                        .parse_slice_header(nalu)
                        .map_err(malformed(BitstreamUnit::SliceHeader))?;

                    if starts_new_picture(&slice, slices.last()) {
                        self.decode_picture(display, &slices, frame.timestamp)?;
                        slices.clear();
                    }

                    slices.push(slice);
                }
                _ => {}
            }
        }

        if !slices.is_empty() {
            self.decode_picture(display, &slices, frame.timestamp)?;
        }

        Ok(self.reorder.pop())
    }

    fn complete(&mut self, open: &OpenFrame) -> Result<(), DecodeError> {
        let stream = self
            .stream
            .as_ref()
            .ok_or(DecodeError::MissingParameterSet {
                codec: CODEC,
                unit: BitstreamUnit::SequenceParameterSet,
                id: 0,
            })?;
        let output = stream.output(&open.surface, open.timestamp)?;

        if open.restart {
            self.reorder.flush();
        }
        self.reorder.push(open.key(), output);

        Ok(())
    }

    fn take_partner(
        &mut self,
        structure: Structure,
        frame_num: u32,
        idr: bool,
    ) -> Result<Option<OpenFrame>, DecodeError> {
        let Some(open) = self.open_frame.take() else {
            return Ok(None);
        };

        if open.completes_with(structure, frame_num, idr) {
            Ok(Some(open))
        } else {
            self.complete(&open)?;
            Ok(None)
        }
    }

    fn settle(&mut self, mut open: OpenFrame, picture: &Decoded<'_>) -> Result<(), DecodeError> {
        self.dpb.finish(picture);

        let order = picture.order;
        let had_mmco5 = self.dpb.previous.had_mmco5;
        let shift = if had_mmco5 { order.poc() } else { 0 };
        open.record(picture.structure, order.top, order.bottom, shift);
        open.restart |= picture.idr || had_mmco5;

        if picture.structure.is_field() && !picture.second_field {
            self.open_frame = Some(open);
            Ok(())
        } else {
            self.complete(&open)
        }
    }

    fn decode_picture(
        &mut self,
        display: &Rc<Display>,
        slices: &[Slice<'_>],
        timestamp: Timestamp,
    ) -> Result<(), DecodeError> {
        let first = &slices[0];
        let header = &first.header;

        let structure = Structure::of(header.field_pic_flag, header.bottom_field_flag);
        let idr = first.nalu.header.idr_pic_flag;
        let nal_ref_idc = first.nalu.header.ref_idc;
        let frame_num = u32::from(header.frame_num);

        let mut partner = self.take_partner(structure, frame_num, idr)?;

        let pps = Rc::clone(self.parser.get_pps(header.pic_parameter_set_id).ok_or(
            DecodeError::MissingParameterSet {
                codec: CODEC,
                unit: BitstreamUnit::PictureParameterSet,
                id: header.pic_parameter_set_id,
            },
        )?);
        let sps = Rc::clone(&pps.sps);

        let format = stream_format(&sps)?;
        let (stream, reopened) = Stream::ensure(&mut self.stream, display, CODEC, format)?;
        if reopened {
            self.dpb.reset();
            partner = None;
        }
        self.reorder.configure(reorder_depth(&sps));

        let max_frame_num = sps.max_frame_num();

        if idr {
            self.dpb.reset();
        }
        self.dpb.update_frame_num_wrap(frame_num, max_frame_num);

        let order = self
            .dpb
            .order_count(&sps, first, idr, nal_ref_idc, structure);
        let surface = match &partner {
            Some(open) => open.surface.clone(),
            None => stream.pool.acquire()?,
        };
        let partner_poc = partner
            .as_ref()
            .and_then(|open| open.pocs[1 - structure.parity().unwrap_or(0)]);

        let picture = current_picture(surface.id, frame_num, structure, &order, partner_poc);

        let mut buffers = vec![
            stream.session.parameter(
                ffi::VABufferType_VAPictureParameterBufferType,
                &picture_parameters(
                    &sps,
                    &pps,
                    picture,
                    &self.dpb.references,
                    frame_num,
                    nal_ref_idc,
                    structure,
                ),
            )?,
            stream
                .session
                .parameter(ffi::VABufferType_VAIQMatrixBufferType, &iq_matrix(&pps))?,
        ];

        for slice in slices {
            let lists =
                self.dpb
                    .reference_lists(slice, structure, order.poc(), frame_num, max_frame_num);
            let parameters = slice_parameters(slice, &sps, &pps, &lists, &self.dpb.references);
            buffers.push(
                stream
                    .session
                    .parameter(ffi::VABufferType_VASliceParameterBufferType, &parameters)?,
            );
            buffers.push(stream.session.slice_data(slice.nalu.as_ref())?);
        }

        stream.session.decode(surface.id, &buffers)?;
        drop(buffers);

        let second_field = partner.is_some();
        let open = partner
            .take()
            .unwrap_or_else(|| OpenFrame::new(surface.clone(), structure, frame_num, timestamp));

        self.settle(
            open,
            &Decoded {
                marking: &header.dec_ref_pic_marking,
                idr,
                nal_ref_idc,
                max_num_ref_frames: sps.max_num_ref_frames,
                surface,
                structure,
                second_field,
                partner_poc,
                frame_num,
                pic_order_cnt_lsb: i32::from(header.pic_order_cnt_lsb),
                order,
            },
        )
    }
}

fn current_picture(
    surface: SurfaceId,
    frame_num: u32,
    structure: Structure,
    order: &OrderCount,
    partner_poc: Option<i32>,
) -> ffi::VAPictureH264 {
    let (top, bottom) = match structure {
        Structure::Frame => (order.top, order.bottom),
        Structure::Top => (order.top, partner_poc),
        Structure::Bottom => (partner_poc, order.bottom),
    };

    ffi::VAPictureH264 {
        picture_id: surface,
        frame_idx: frame_num,
        flags: structure.va_flags(),
        TopFieldOrderCnt: top.unwrap_or(0),
        BottomFieldOrderCnt: bottom.unwrap_or(0),
        ..Default::default()
    }
}

fn starts_new_picture(slice: &Slice<'_>, previous: Option<&Slice<'_>>) -> bool {
    let Some(previous) = previous else {
        return false;
    };
    let (header, other) = (&slice.header, &previous.header);

    header.first_mb_in_slice == 0
        || header.field_pic_flag != other.field_pic_flag
        || header.bottom_field_flag != other.bottom_field_flag
}

pub(super) fn reorder_depth(sps: &Sps) -> Depth {
    let vui = &sps.vui_parameters;

    if sps.vui_parameters_present_flag && vui.bitstream_restriction_flag {
        let buffering = usize::try_from(vui.max_dec_frame_buffering).unwrap_or(usize::MAX);
        let reorder = usize::try_from(vui.max_num_reorder_frames).unwrap_or(usize::MAX);
        Depth::Fixed(reorder.min(buffering).min(MAX_REFERENCE_FRAMES))
    } else if sps.pic_order_cnt_type == 2 || sps.profile_idc == BASELINE_PROFILE_IDC {
        Depth::Fixed(0)
    } else {
        Depth::Learned {
            ceiling: sps.max_dpb_frames().min(MAX_REFERENCE_FRAMES),
        }
    }
}

fn stream_format(sps: &Sps) -> Result<StreamFormat, DecodeError> {
    let chroma = match sps.chroma_format_idc {
        1 => ChromaSubsampling::Yuv420,
        2 => ChromaSubsampling::Yuv422,
        3 => ChromaSubsampling::Yuv444,
        _ => {
            return Err(DecodeError::Monochrome {
                backend: crate::Backend::Vaapi,
                codec: CODEC,
            });
        }
    };
    let depth = match sps.bit_depth_luma_minus8.max(sps.bit_depth_chroma_minus8) {
        0 => BitDepth::Eight,
        2 => BitDepth::Ten,
        4 => BitDepth::Twelve,
        other => {
            return Err(DecodeError::BitDepthUnsupported {
                backend: crate::Backend::Vaapi,
                codec: CODEC,
                bits: other + 8,
            });
        }
    };

    let profile: Profile = match sps.profile_idc {
        66 if sps.constraint_set1_flag => ffi::VAProfile_VAProfileH264ConstrainedBaseline,
        66 | 77 => ffi::VAProfile_VAProfileH264Main,
        100 => ffi::VAProfile_VAProfileH264High,
        110 => ffi::VAProfile_VAProfileH264High10,
        122 => ffi::VAProfile_VAProfileH264High422,
        _ => ffi::VAProfile_VAProfileNone,
    };

    let visible = sps.visible_rectangle();

    Ok(StreamFormat {
        profile,
        profile_idc: sps.profile_idc,
        pixel_format: PixelFormat { chroma, depth },
        coded: Resolution::new(sps.width(), sps.height()),
        visible: Resolution::new(visible.max.x, visible.max.y),
        reference_frames: sps
            .max_dpb_frames()
            .max(usize::from(sps.max_num_ref_frames)),
        reorder_frames: reorder_depth(sps).capacity(),
    })
}

fn picture_parameters(
    sps: &Sps,
    pps: &Pps,
    current: ffi::VAPictureH264,
    references: &[Reference],
    frame_num: u32,
    nal_ref_idc: u8,
    structure: Structure,
) -> ffi::VAPictureParameterBufferH264 {
    let mut parameters = ffi::VAPictureParameterBufferH264 {
        CurrPic: current,
        ReferenceFrames: [invalid_picture(); MAX_REFERENCE_FRAMES],
        picture_width_in_mbs_minus1: sps.pic_width_in_mbs_minus1,
        picture_height_in_mbs_minus1: ((sps.pic_height_in_map_units_minus1 + 1)
            << u16::from(!sps.frame_mbs_only_flag))
            - 1,
        bit_depth_luma_minus8: sps.bit_depth_luma_minus8,
        bit_depth_chroma_minus8: sps.bit_depth_chroma_minus8,
        num_ref_frames: sps.max_num_ref_frames,
        pic_init_qp_minus26: pps.pic_init_qp_minus26,
        pic_init_qs_minus26: pps.pic_init_qs_minus26,
        chroma_qp_index_offset: pps.chroma_qp_index_offset,
        second_chroma_qp_index_offset: pps.second_chroma_qp_index_offset,
        frame_num: u16::try_from(frame_num).unwrap_or(u16::MAX),
        ..Default::default()
    };

    let ordered = references
        .iter()
        .filter(|reference| !reference.has_long_term())
        .chain(
            references
                .iter()
                .filter(|reference| reference.has_long_term()),
        );
    for (slot, reference) in parameters.ReferenceFrames.iter_mut().zip(ordered) {
        *slot = reference.va_frame();
    }

    let sequence = unsafe { &mut parameters.seq_fields.bits };
    sequence.set_chroma_format_idc(u32::from(sps.chroma_format_idc));
    sequence.set_residual_colour_transform_flag(u32::from(sps.separate_colour_plane_flag));
    sequence.set_gaps_in_frame_num_value_allowed_flag(u32::from(
        sps.gaps_in_frame_num_value_allowed_flag,
    ));
    sequence.set_frame_mbs_only_flag(u32::from(sps.frame_mbs_only_flag));
    sequence.set_mb_adaptive_frame_field_flag(u32::from(sps.mb_adaptive_frame_field_flag));
    sequence.set_direct_8x8_inference_flag(u32::from(sps.direct_8x8_inference_flag));
    sequence.set_MinLumaBiPredSize8x8(u32::from(sps.level_idc as u8 >= LEVEL_3_1));
    sequence.set_log2_max_frame_num_minus4(u32::from(sps.log2_max_frame_num_minus4));
    sequence.set_pic_order_cnt_type(u32::from(sps.pic_order_cnt_type));
    sequence
        .set_log2_max_pic_order_cnt_lsb_minus4(u32::from(sps.log2_max_pic_order_cnt_lsb_minus4));
    sequence.set_delta_pic_order_always_zero_flag(u32::from(sps.delta_pic_order_always_zero_flag));

    let picture = unsafe { &mut parameters.pic_fields.bits };
    picture.set_entropy_coding_mode_flag(u32::from(pps.entropy_coding_mode_flag));
    picture.set_weighted_pred_flag(u32::from(pps.weighted_pred_flag));
    picture.set_weighted_bipred_idc(u32::from(pps.weighted_bipred_idc));
    picture.set_transform_8x8_mode_flag(u32::from(pps.transform_8x8_mode_flag));
    picture.set_field_pic_flag(u32::from(structure.is_field()));
    picture.set_constrained_intra_pred_flag(u32::from(pps.constrained_intra_pred_flag));
    picture.set_pic_order_present_flag(u32::from(pps.bottom_field_pic_order_in_frame_present_flag));
    picture.set_deblocking_filter_control_present_flag(u32::from(
        pps.deblocking_filter_control_present_flag,
    ));
    picture.set_redundant_pic_cnt_present_flag(u32::from(pps.redundant_pic_cnt_present_flag));
    picture.set_reference_pic_flag(u32::from(nal_ref_idc != 0));

    parameters
}

fn iq_matrix(pps: &Pps) -> ffi::VAIQMatrixBufferH264 {
    let mut matrix = ffi::VAIQMatrixBufferH264::default();

    for (raster, zigzag) in matrix.ScalingList4x4.iter_mut().zip(&pps.scaling_lists_4x4) {
        for (index, &position) in ZIGZAG_4X4.iter().enumerate() {
            raster[position] = zigzag[index];
        }
    }
    for (raster, zigzag) in matrix.ScalingList8x8.iter_mut().zip(&pps.scaling_lists_8x8) {
        for (index, &position) in ZIGZAG_8X8.iter().enumerate() {
            raster[position] = zigzag[index];
        }
    }

    matrix
}

fn slice_parameters(
    slice: &Slice<'_>,
    sps: &Sps,
    pps: &Pps,
    lists: &[ReferenceList; 2],
    references: &[Reference],
) -> ffi::VASliceParameterBufferH264 {
    let header = &slice.header;
    let table = &header.pred_weight_table;

    let mut parameters = ffi::VASliceParameterBufferH264 {
        slice_data_size: u32::try_from(slice.nalu.size).unwrap_or(u32::MAX),
        slice_data_offset: 0,
        slice_data_flag: ffi::VA_SLICE_DATA_FLAG_ALL,
        slice_data_bit_offset: u16::try_from(header.header_bit_size).unwrap_or(u16::MAX),
        first_mb_in_slice: u16::try_from(header.first_mb_in_slice).unwrap_or(u16::MAX),
        slice_type: header.slice_type as u8,
        direct_spatial_mv_pred_flag: u8::from(header.direct_spatial_mv_pred_flag),
        num_ref_idx_l0_active_minus1: header.num_ref_idx_l0_active_minus1,
        num_ref_idx_l1_active_minus1: header.num_ref_idx_l1_active_minus1,
        cabac_init_idc: header.cabac_init_idc,
        slice_qp_delta: header.slice_qp_delta,
        disable_deblocking_filter_idc: header.disable_deblocking_filter_idc,
        slice_alpha_c0_offset_div2: header.slice_alpha_c0_offset_div2,
        slice_beta_offset_div2: header.slice_beta_offset_div2,
        RefPicList0: [invalid_picture(); MAX_LIST_ENTRIES],
        RefPicList1: [invalid_picture(); MAX_LIST_ENTRIES],
        luma_log2_weight_denom: table.luma_log2_weight_denom,
        chroma_log2_weight_denom: table.chroma_log2_weight_denom,
        ..Default::default()
    };

    for (target, list) in [&mut parameters.RefPicList0, &mut parameters.RefPicList1]
        .into_iter()
        .zip(lists)
    {
        for (slot, entry) in target.iter_mut().zip(list) {
            if let Some(entry) = entry
                && let Some(reference) = references.get(entry.index)
            {
                *slot = reference.va_picture(entry.structure);
            }
        }
    }

    let explicit_l0 =
        pps.weighted_pred_flag && matches!(header.slice_type, SliceType::P | SliceType::Sp);
    let explicit_b = pps.weighted_bipred_idc == 1 && header.slice_type == SliceType::B;
    let chroma = sps.chroma_array_type() != 0;

    if explicit_l0 || explicit_b {
        let count = usize::from(header.num_ref_idx_l0_active_minus1) + 1;
        parameters.luma_weight_l0_flag = 1;
        parameters.chroma_weight_l0_flag = u8::from(chroma);
        for index in 0..count.min(MAX_LIST_ENTRIES) {
            parameters.luma_weight_l0[index] = table.luma_weight_l0[index];
            parameters.luma_offset_l0[index] = i16::from(table.luma_offset_l0[index]);
            for component in 0..2 {
                parameters.chroma_weight_l0[index][component] =
                    table.chroma_weight_l0[index][component];
                parameters.chroma_offset_l0[index][component] =
                    i16::from(table.chroma_offset_l0[index][component]);
            }
        }
    }

    if explicit_b {
        let count = usize::from(header.num_ref_idx_l1_active_minus1) + 1;
        parameters.luma_weight_l1_flag = 1;
        parameters.chroma_weight_l1_flag = u8::from(chroma);
        for index in 0..count.min(MAX_LIST_ENTRIES) {
            parameters.luma_weight_l1[index] = table.luma_weight_l1[index];
            parameters.luma_offset_l1[index] = table.luma_offset_l1[index];
            for component in 0..2 {
                parameters.chroma_weight_l1[index][component] =
                    table.chroma_weight_l1[index][component];
                parameters.chroma_offset_l1[index][component] =
                    i16::from(table.chroma_offset_l1[index][component]);
            }
        }
    }

    parameters
}
