use std::{io::Cursor, rc::Rc};

use cros_codecs::codec::h265::parser::{
    NaluHeader, NaluType, Parser, Pps, ScalingLists, ShortTermRefPicSet, Slice, SliceHeader,
    SliceType, Sps,
};
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, EncodedFrame, PixelFormat, Resolution, Timestamp,
};

use crate::{
    Backend, BitstreamUnit, DecodeError, DecodedFrame,
    vaapi::{
        Stream, StreamFormat, annexb, ffi,
        reorder::{Depth, Reorder},
        va::{Display, INVALID_SURFACE, PoolSurface, Profile},
    },
};

const CODEC: Codec = Codec::Hevc;
const MAX_REFERENCE_FRAMES: usize = 15;
const NO_REFERENCE: u8 = 0xff;

const fn up_right_diagonal<const N: usize, const SIDE: usize>() -> [usize; N] {
    let mut scan = [0; N];
    let mut index = 0;
    let mut x: isize = 0;
    let mut y: isize = 0;

    while index < N {
        while y >= 0 {
            if x < SIDE.cast_signed() && y < SIDE.cast_signed() {
                scan[index] = x.unsigned_abs() + SIDE * y.unsigned_abs();
                index += 1;
            }
            y -= 1;
            x += 1;
        }
        y = x;
        x = 0;
    }

    scan
}

const DIAGONAL_4X4: [usize; 16] = up_right_diagonal::<16, 4>();
const DIAGONAL_8X8: [usize; 64] = up_right_diagonal::<64, 8>();

fn malformed(unit: BitstreamUnit) -> impl FnOnce(String) -> DecodeError {
    annexb::malformed(CODEC, unit)
}

#[derive(Debug, Clone)]
struct Reference {
    surface: PoolSurface,
    poc: i32,
    long_term: bool,
}

#[derive(Debug, Default)]
struct ReferenceSet {
    before: Vec<Option<usize>>,
    after: Vec<Option<usize>>,
    long_term: Vec<Option<usize>>,
}

impl ReferenceSet {
    fn flags(&self, index: usize) -> u32 {
        if self.before.contains(&Some(index)) {
            ffi::VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE
        } else if self.after.contains(&Some(index)) {
            ffi::VA_PICTURE_HEVC_RPS_ST_CURR_AFTER
        } else if self.long_term.contains(&Some(index)) {
            ffi::VA_PICTURE_HEVC_RPS_LT_CURR
        } else {
            0
        }
    }
}

pub(super) struct HevcDecoder {
    parser: Parser,
    stream: Option<Stream>,
    references: Vec<Reference>,
    previous_tid0_poc: i32,
    awaiting_random_access: bool,
    skip_leading_pictures: bool,
    last_independent_header: Option<SliceHeader>,
    reorder: Reorder<DecodedFrame>,
}

impl HevcDecoder {
    pub(super) fn new() -> Self {
        Self {
            parser: Parser::default(),
            stream: None,
            references: Vec::new(),
            previous_tid0_poc: 0,
            awaiting_random_access: true,
            skip_leading_pictures: false,
            last_independent_header: None,
            reorder: Reorder::new(),
        }
    }

    pub(super) fn drain(&mut self) -> Vec<DecodedFrame> {
        self.reorder.drain()
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
                NaluType::VpsNut => {
                    self.parser
                        .parse_vps(&nalu)
                        .map_err(malformed(BitstreamUnit::VideoParameterSet))?;
                }
                NaluType::SpsNut => {
                    self.parser
                        .parse_sps(&nalu)
                        .map_err(malformed(BitstreamUnit::SequenceParameterSet))?;
                }
                NaluType::PpsNut => {
                    self.parser
                        .parse_pps(&nalu)
                        .map_err(malformed(BitstreamUnit::PictureParameterSet))?;
                }
                NaluType::EosNut | NaluType::EobNut => {
                    self.awaiting_random_access = true;
                }
                NaluType::TrailN
                | NaluType::TrailR
                | NaluType::TsaN
                | NaluType::TsaR
                | NaluType::StsaN
                | NaluType::StsaR
                | NaluType::RadlN
                | NaluType::RadlR
                | NaluType::RaslN
                | NaluType::RaslR
                | NaluType::BlaWLp
                | NaluType::BlaWRadl
                | NaluType::BlaNLp
                | NaluType::IdrWRadl
                | NaluType::IdrNLp
                | NaluType::CraNut => {
                    let mut slice = self
                        .parser
                        .parse_slice_header(nalu)
                        .map_err(malformed(BitstreamUnit::SliceHeader))?;

                    if slice.header.dependent_slice_segment_flag {
                        let independent =
                            self.last_independent_header.clone().ok_or_else(|| {
                                DecodeError::Malformed {
                                    codec: CODEC,
                                    unit: BitstreamUnit::SliceHeader,
                                    reason: String::from(
                                        "dependent slice segment without an independent one",
                                    ),
                                }
                            })?;
                        slice
                            .replace_header(independent)
                            .map_err(malformed(BitstreamUnit::SliceHeader))?;
                    } else {
                        self.last_independent_header = Some(slice.header.clone());
                    }

                    if slice.header.first_slice_segment_in_pic_flag && !slices.is_empty() {
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

    fn decode_picture(
        &mut self,
        display: &Rc<Display>,
        slices: &[Slice<'_>],
        timestamp: Timestamp,
    ) -> Result<(), DecodeError> {
        let first = &slices[0];
        let header = &first.header;
        let nalu_type = first.nalu.header.type_;

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
            self.references.clear();
        }
        self.reorder.configure(reorder_depth(&sps));

        let irap = nalu_type.is_irap();
        let no_rasl_output =
            irap && (nalu_type.is_idr() || nalu_type.is_bla() || self.awaiting_random_access);
        if irap {
            self.skip_leading_pictures = no_rasl_output;
            self.awaiting_random_access = false;
        } else if self.awaiting_random_access {
            return Ok(());
        }

        if nalu_type.is_rasl() && self.skip_leading_pictures {
            return Ok(());
        }

        let max_lsb = 1_i32 << (sps.log2_max_pic_order_cnt_lsb_minus4 + 4);
        let poc = if irap && no_rasl_output {
            i32::from(header.pic_order_cnt_lsb)
        } else {
            following_order_count(self.previous_tid0_poc, header.pic_order_cnt_lsb, max_lsb)
        };

        if first.nalu.header.nuh_temporal_id() == 0
            && !nalu_type.is_rasl()
            && !nalu_type.is_radl()
            && !nalu_type.is_slnr()
        {
            self.previous_tid0_poc = poc;
        }

        let set = if nalu_type.is_idr() {
            self.references.clear();
            ReferenceSet::default()
        } else {
            apply_reference_set(&mut self.references, &sps, header, poc, max_lsb)
        };

        let surface = stream.pool.acquire()?;
        let picture = Picture {
            sps: &sps,
            pps: &pps,
            nalu_type,
            current: ffi::VAPictureHEVC {
                picture_id: surface.id,
                pic_order_cnt: poc,
                flags: 0,
                ..Default::default()
            },
            references: va_references(&self.references, &set),
            range_extension: is_range_extension(format.profile),
        };
        submit(stream, &picture, slices, &set)?;

        let output = if header.pic_output_flag {
            Some(stream.output(&surface, timestamp)?)
        } else {
            None
        };

        if no_rasl_output {
            self.reorder.flush();
        }
        if let Some(output) = output {
            self.reorder.push(poc, output);
        }

        if self.references.len() >= MAX_REFERENCE_FRAMES {
            self.references.remove(0);
        }
        self.references.push(Reference {
            surface,
            poc,
            long_term: false,
        });

        Ok(())
    }
}

fn reorder_depth(sps: &Sps) -> Depth {
    let highest_layer = usize::from(sps.max_sub_layers_minus1);
    let reorder = sps
        .max_num_reorder_pics
        .get(highest_layer)
        .map_or(0, |&pictures| usize::from(pictures));

    Depth::Fixed(reorder.min(MAX_REFERENCE_FRAMES))
}

fn following_order_count(previous: i32, pic_order_cnt_lsb: u16, max_lsb: i32) -> i32 {
    let low = i32::from(pic_order_cnt_lsb);
    let previous_low = previous & (max_lsb - 1);
    let previous_high = previous - previous_low;

    let high = if low < previous_low && previous_low - low >= max_lsb / 2 {
        previous_high + max_lsb
    } else if low > previous_low && low - previous_low > max_lsb / 2 {
        previous_high - max_lsb
    } else {
        previous_high
    };

    high + low
}

fn va_references(
    references: &[Reference],
    set: &ReferenceSet,
) -> [ffi::VAPictureHEVC; MAX_REFERENCE_FRAMES] {
    let mut frames = [invalid_picture(); MAX_REFERENCE_FRAMES];

    for (index, (slot, reference)) in frames.iter_mut().zip(references).enumerate() {
        let mut flags = set.flags(index);
        if reference.long_term {
            flags |= ffi::VA_PICTURE_HEVC_LONG_TERM_REFERENCE;
        }
        *slot = ffi::VAPictureHEVC {
            picture_id: reference.surface.id,
            pic_order_cnt: reference.poc,
            flags,
            ..Default::default()
        };
    }

    frames
}

struct Picture<'a> {
    sps: &'a Sps,
    pps: &'a Pps,
    nalu_type: NaluType,
    current: ffi::VAPictureHEVC,
    references: [ffi::VAPictureHEVC; MAX_REFERENCE_FRAMES],
    range_extension: bool,
}

fn submit(
    stream: &Stream,
    picture: &Picture<'_>,
    slices: &[Slice<'_>],
    set: &ReferenceSet,
) -> Result<(), DecodeError> {
    let session = &stream.session;
    let base = picture_parameters(picture, &slices[0].header);

    let mut buffers = Vec::new();
    if picture.range_extension {
        let extension = ffi::VAPictureParameterBufferHEVCExtension {
            base,
            rext: picture_range_extension(picture.sps, picture.pps),
            ..Default::default()
        };
        buffers
            .push(session.parameter(ffi::VABufferType_VAPictureParameterBufferType, &extension)?);
    } else {
        buffers.push(session.parameter(ffi::VABufferType_VAPictureParameterBufferType, &base)?);
    }

    if let Some(lists) = scaling_lists(picture.sps, picture.pps) {
        buffers.push(session.parameter(ffi::VABufferType_VAIQMatrixBufferType, &iq_matrix(lists))?);
    }

    for (position, slice) in slices.iter().enumerate() {
        let last = position + 1 == slices.len();
        let lists = reference_lists(&slice.header, set);
        let parameters = slice_parameters(slice, picture.sps, &lists, last);

        let buffer = if picture.range_extension {
            let extension = ffi::VASliceParameterBufferHEVCExtension {
                rext: slice_range_extension(&slice.header, &parameters),
                base: parameters,
            };
            session.parameter(ffi::VABufferType_VASliceParameterBufferType, &extension)?
        } else {
            session.parameter(ffi::VABufferType_VASliceParameterBufferType, &parameters)?
        };
        buffers.push(buffer);
        buffers.push(session.slice_data(slice.nalu.as_ref())?);
    }

    session.decode(picture.current.picture_id, &buffers)
}

#[derive(Debug, Default)]
struct OrderCounts {
    before: Vec<i32>,
    after: Vec<i32>,
    following: Vec<i32>,
    long_term_current: Vec<(i32, bool)>,
    long_term_following: Vec<(i32, bool)>,
}

fn reference_order_counts(sps: &Sps, header: &SliceHeader, poc: i32, max_lsb: i32) -> OrderCounts {
    let short_term: &ShortTermRefPicSet = if header.short_term_ref_pic_set_sps_flag {
        sps.short_term_ref_pic_set
            .get(usize::from(header.curr_rps_idx))
            .unwrap_or(&header.short_term_ref_pic_set)
    } else {
        &header.short_term_ref_pic_set
    };

    let mut counts = OrderCounts::default();

    for index in 0..usize::from(short_term.num_negative_pics) {
        let target = poc + short_term.delta_poc_s0[index];
        if short_term.used_by_curr_pic_s0[index] {
            counts.before.push(target);
        } else {
            counts.following.push(target);
        }
    }
    for index in 0..usize::from(short_term.num_positive_pics) {
        let target = poc + short_term.delta_poc_s1[index];
        if short_term.used_by_curr_pic_s1[index] {
            counts.after.push(target);
        } else {
            counts.following.push(target);
        }
    }

    let long_term_count =
        usize::from(header.num_long_term_sps) + usize::from(header.num_long_term_pics);
    for index in 0..long_term_count.min(header.poc_lsb_lt.len()) {
        let mut target = i32::try_from(header.poc_lsb_lt[index]).unwrap_or(0);
        let full = header.delta_poc_msb_present_flag[index];
        if full {
            let cycle = i32::try_from(header.delta_poc_msb_cycle_lt[index]).unwrap_or(0);
            target += poc - cycle * max_lsb - (poc & (max_lsb - 1));
        }
        if header.used_by_curr_pic_lt[index] {
            counts.long_term_current.push((target, full));
        } else {
            counts.long_term_following.push((target, full));
        }
    }

    counts
}

fn apply_reference_set(
    references: &mut Vec<Reference>,
    sps: &Sps,
    header: &SliceHeader,
    poc: i32,
    max_lsb: i32,
) -> ReferenceSet {
    let OrderCounts {
        before,
        after,
        following,
        long_term_current,
        long_term_following,
    } = reference_order_counts(sps, header, poc, max_lsb);

    let mask = max_lsb - 1;
    let find_long_term = |references: &[Reference], (target, full): (i32, bool)| {
        references.iter().position(|reference| {
            if full {
                reference.poc == target
            } else {
                reference.poc & mask == target
            }
        })
    };

    let long_term: Vec<Option<usize>> = long_term_current
        .iter()
        .map(|&entry| find_long_term(references, entry))
        .collect();
    let long_term_kept: Vec<Option<usize>> = long_term_following
        .iter()
        .map(|&entry| find_long_term(references, entry))
        .collect();

    let find_short_term = |references: &[Reference], target: i32| {
        references
            .iter()
            .position(|reference| !reference.long_term && reference.poc == target)
    };

    let before: Vec<Option<usize>> = before
        .iter()
        .map(|&target| find_short_term(references, target))
        .collect();
    let after: Vec<Option<usize>> = after
        .iter()
        .map(|&target| find_short_term(references, target))
        .collect();
    let short_term_kept: Vec<Option<usize>> = following
        .iter()
        .map(|&target| find_short_term(references, target))
        .collect();

    for index in long_term.iter().chain(&long_term_kept).flatten() {
        references[*index].long_term = true;
    }

    let keep: Vec<usize> = before
        .iter()
        .chain(&after)
        .chain(&short_term_kept)
        .chain(&long_term)
        .chain(&long_term_kept)
        .flatten()
        .copied()
        .collect();
    let remap = retain_references(references, &keep);

    let translate = |entries: Vec<Option<usize>>| -> Vec<Option<usize>> {
        entries
            .into_iter()
            .map(|entry| entry.and_then(|index| remap[index]))
            .collect()
    };

    ReferenceSet {
        before: translate(before),
        after: translate(after),
        long_term: translate(long_term),
    }
}

fn retain_references(references: &mut Vec<Reference>, keep: &[usize]) -> Vec<Option<usize>> {
    let mut remap = vec![None; references.len()];
    let mut kept = Vec::new();

    for (index, reference) in references.drain(..).enumerate() {
        if keep.contains(&index) {
            remap[index] = Some(kept.len());
            kept.push(reference);
        }
    }
    *references = kept;

    remap
}

fn invalid_picture() -> ffi::VAPictureHEVC {
    ffi::VAPictureHEVC {
        picture_id: INVALID_SURFACE,
        flags: ffi::VA_PICTURE_HEVC_INVALID,
        ..Default::default()
    }
}

fn is_range_extension(profile: Profile) -> bool {
    matches!(
        profile,
        ffi::VAProfile_VAProfileHEVCMain12
            | ffi::VAProfile_VAProfileHEVCMain422_10
            | ffi::VAProfile_VAProfileHEVCMain422_12
            | ffi::VAProfile_VAProfileHEVCMain444
            | ffi::VAProfile_VAProfileHEVCMain444_10
            | ffi::VAProfile_VAProfileHEVCMain444_12
    )
}

fn stream_format(sps: &Sps) -> Result<StreamFormat, DecodeError> {
    let chroma = match sps.chroma_format_idc {
        1 => ChromaSubsampling::Yuv420,
        2 => ChromaSubsampling::Yuv422,
        3 => ChromaSubsampling::Yuv444,
        _ => {
            return Err(DecodeError::Monochrome {
                backend: Backend::Vaapi,
                codec: CODEC,
            });
        }
    };
    let bits = sps.bit_depth_luma_minus8.max(sps.bit_depth_chroma_minus8) + 8;
    let depth = match bits {
        8 => BitDepth::Eight,
        10 => BitDepth::Ten,
        12 => BitDepth::Twelve,
        other => {
            return Err(DecodeError::BitDepthUnsupported {
                backend: Backend::Vaapi,
                codec: CODEC,
                bits: other,
            });
        }
    };

    let profile = match (chroma, depth) {
        (ChromaSubsampling::Yuv420, BitDepth::Eight) => ffi::VAProfile_VAProfileHEVCMain,
        (ChromaSubsampling::Yuv420, BitDepth::Ten) => ffi::VAProfile_VAProfileHEVCMain10,
        (ChromaSubsampling::Yuv420, BitDepth::Twelve) => ffi::VAProfile_VAProfileHEVCMain12,
        (ChromaSubsampling::Yuv422, BitDepth::Eight | BitDepth::Ten) => {
            ffi::VAProfile_VAProfileHEVCMain422_10
        }
        (ChromaSubsampling::Yuv422, BitDepth::Twelve) => ffi::VAProfile_VAProfileHEVCMain422_12,
        (ChromaSubsampling::Yuv444, BitDepth::Eight) => ffi::VAProfile_VAProfileHEVCMain444,
        (ChromaSubsampling::Yuv444, BitDepth::Ten) => ffi::VAProfile_VAProfileHEVCMain444_10,
        (ChromaSubsampling::Yuv444, BitDepth::Twelve) => ffi::VAProfile_VAProfileHEVCMain444_12,
    };

    let visible = sps.visible_rectangle();
    let highest_layer = usize::from(sps.max_sub_layers_minus1);
    let buffering = sps
        .max_dec_pic_buffering_minus1
        .get(highest_layer)
        .map_or(MAX_REFERENCE_FRAMES, |&minus1| usize::from(minus1) + 1);

    Ok(StreamFormat {
        profile,
        profile_idc: sps.profile_tier_level.general_profile_idc,
        pixel_format: PixelFormat { chroma, depth },
        coded: Resolution::new(
            u32::from(sps.pic_width_in_luma_samples),
            u32::from(sps.pic_height_in_luma_samples),
        ),
        visible: Resolution::new(visible.max.x, visible.max.y),
        reference_frames: buffering
            .max(sps.max_dpb_size())
            .min(MAX_REFERENCE_FRAMES + 1),
        reorder_frames: reorder_depth(sps).capacity(),
    })
}

fn picture_parameters(
    picture: &Picture<'_>,
    header: &SliceHeader,
) -> ffi::VAPictureParameterBufferHEVC {
    let Picture { sps, pps, .. } = *picture;
    let highest_layer = usize::from(sps.max_sub_layers_minus1);

    let mut parameters = ffi::VAPictureParameterBufferHEVC {
        CurrPic: picture.current,
        ReferenceFrames: picture.references,
        pic_width_in_luma_samples: sps.pic_width_in_luma_samples,
        pic_height_in_luma_samples: sps.pic_height_in_luma_samples,
        sps_max_dec_pic_buffering_minus1: sps
            .max_dec_pic_buffering_minus1
            .get(highest_layer)
            .copied()
            .unwrap_or(0),
        bit_depth_luma_minus8: sps.bit_depth_luma_minus8,
        bit_depth_chroma_minus8: sps.bit_depth_chroma_minus8,
        pcm_sample_bit_depth_luma_minus1: sps.pcm_sample_bit_depth_luma_minus1,
        pcm_sample_bit_depth_chroma_minus1: sps.pcm_sample_bit_depth_chroma_minus1,
        log2_min_luma_coding_block_size_minus3: sps.log2_min_luma_coding_block_size_minus3,
        log2_diff_max_min_luma_coding_block_size: sps.log2_diff_max_min_luma_coding_block_size,
        log2_min_transform_block_size_minus2: sps.log2_min_luma_transform_block_size_minus2,
        log2_diff_max_min_transform_block_size: sps.log2_diff_max_min_luma_transform_block_size,
        log2_min_pcm_luma_coding_block_size_minus3: sps.log2_min_pcm_luma_coding_block_size_minus3,
        log2_diff_max_min_pcm_luma_coding_block_size: sps
            .log2_diff_max_min_pcm_luma_coding_block_size,
        max_transform_hierarchy_depth_intra: sps.max_transform_hierarchy_depth_intra,
        max_transform_hierarchy_depth_inter: sps.max_transform_hierarchy_depth_inter,
        init_qp_minus26: pps.init_qp_minus26,
        diff_cu_qp_delta_depth: pps.diff_cu_qp_delta_depth,
        pps_cb_qp_offset: pps.cb_qp_offset,
        pps_cr_qp_offset: pps.cr_qp_offset,
        log2_parallel_merge_level_minus2: pps.log2_parallel_merge_level_minus2,
        num_tile_columns_minus1: pps.num_tile_columns_minus1,
        num_tile_rows_minus1: pps.num_tile_rows_minus1,
        log2_max_pic_order_cnt_lsb_minus4: sps.log2_max_pic_order_cnt_lsb_minus4,
        num_short_term_ref_pic_sets: sps.num_short_term_ref_pic_sets,
        num_long_term_ref_pic_sps: sps.num_long_term_ref_pics_sps,
        num_ref_idx_l0_default_active_minus1: pps.num_ref_idx_l0_default_active_minus1,
        num_ref_idx_l1_default_active_minus1: pps.num_ref_idx_l1_default_active_minus1,
        pps_beta_offset_div2: pps.beta_offset_div2,
        pps_tc_offset_div2: pps.tc_offset_div2,
        num_extra_slice_header_bits: pps.num_extra_slice_header_bits,
        st_rps_bits: header.st_rps_bits,
        ..Default::default()
    };

    for (target, &width) in parameters
        .column_width_minus1
        .iter_mut()
        .zip(&pps.column_width_minus1)
    {
        *target = u16::try_from(width).unwrap_or(u16::MAX);
    }
    for (target, &height) in parameters
        .row_height_minus1
        .iter_mut()
        .zip(&pps.row_height_minus1)
    {
        *target = u16::try_from(height).unwrap_or(u16::MAX);
    }

    set_picture_fields(&mut parameters, sps, pps);
    set_slice_parsing_fields(&mut parameters, sps, pps, picture.nalu_type);

    parameters
}

fn set_picture_fields(parameters: &mut ffi::VAPictureParameterBufferHEVC, sps: &Sps, pps: &Pps) {
    let fields = unsafe { &mut parameters.pic_fields.bits };
    fields.set_chroma_format_idc(u32::from(sps.chroma_format_idc));
    fields.set_separate_colour_plane_flag(u32::from(sps.separate_colour_plane_flag));
    fields.set_pcm_enabled_flag(u32::from(sps.pcm_enabled_flag));
    fields.set_scaling_list_enabled_flag(u32::from(sps.scaling_list_enabled_flag));
    fields.set_transform_skip_enabled_flag(u32::from(pps.transform_skip_enabled_flag));
    fields.set_amp_enabled_flag(u32::from(sps.amp_enabled_flag));
    fields.set_strong_intra_smoothing_enabled_flag(u32::from(
        sps.strong_intra_smoothing_enabled_flag,
    ));
    fields.set_sign_data_hiding_enabled_flag(u32::from(pps.sign_data_hiding_enabled_flag));
    fields.set_constrained_intra_pred_flag(u32::from(pps.constrained_intra_pred_flag));
    fields.set_cu_qp_delta_enabled_flag(u32::from(pps.cu_qp_delta_enabled_flag));
    fields.set_weighted_pred_flag(u32::from(pps.weighted_pred_flag));
    fields.set_weighted_bipred_flag(u32::from(pps.weighted_bipred_flag));
    fields.set_transquant_bypass_enabled_flag(u32::from(pps.transquant_bypass_enabled_flag));
    fields.set_tiles_enabled_flag(u32::from(pps.tiles_enabled_flag));
    fields.set_entropy_coding_sync_enabled_flag(u32::from(pps.entropy_coding_sync_enabled_flag));
    fields.set_pps_loop_filter_across_slices_enabled_flag(u32::from(
        pps.loop_filter_across_slices_enabled_flag,
    ));
    fields.set_loop_filter_across_tiles_enabled_flag(u32::from(
        pps.loop_filter_across_tiles_enabled_flag,
    ));
    fields.set_pcm_loop_filter_disabled_flag(u32::from(sps.pcm_loop_filter_disabled_flag));
}

fn set_slice_parsing_fields(
    parameters: &mut ffi::VAPictureParameterBufferHEVC,
    sps: &Sps,
    pps: &Pps,
    nalu_type: NaluType,
) {
    let random_access =
        (NaluType::BlaWLp as u32..=NaluType::CraNut as u32).contains(&(nalu_type as u32));

    let parsing = unsafe { &mut parameters.slice_parsing_fields.bits };
    parsing.set_lists_modification_present_flag(u32::from(pps.lists_modification_present_flag));
    parsing.set_long_term_ref_pics_present_flag(u32::from(sps.long_term_ref_pics_present_flag));
    parsing.set_sps_temporal_mvp_enabled_flag(u32::from(sps.temporal_mvp_enabled_flag));
    parsing.set_cabac_init_present_flag(u32::from(pps.cabac_init_present_flag));
    parsing.set_output_flag_present_flag(u32::from(pps.output_flag_present_flag));
    parsing.set_dependent_slice_segments_enabled_flag(u32::from(
        pps.dependent_slice_segments_enabled_flag,
    ));
    parsing.set_pps_slice_chroma_qp_offsets_present_flag(u32::from(
        pps.slice_chroma_qp_offsets_present_flag,
    ));
    parsing.set_sample_adaptive_offset_enabled_flag(u32::from(
        sps.sample_adaptive_offset_enabled_flag,
    ));
    parsing.set_deblocking_filter_override_enabled_flag(u32::from(
        pps.deblocking_filter_override_enabled_flag,
    ));
    parsing.set_pps_disable_deblocking_filter_flag(u32::from(pps.deblocking_filter_disabled_flag));
    parsing.set_slice_segment_header_extension_present_flag(u32::from(
        pps.slice_segment_header_extension_present_flag,
    ));
    parsing.set_RapPicFlag(u32::from(random_access));
    parsing.set_IdrPicFlag(u32::from(nalu_type.is_idr()));
    parsing.set_IntraPicFlag(u32::from(nalu_type.is_irap()));
}

fn picture_range_extension(sps: &Sps, pps: &Pps) -> ffi::VAPictureParameterBufferHEVCRext {
    let sequence = &sps.range_extension;
    let picture = &pps.range_extension;

    let mut extension = ffi::VAPictureParameterBufferHEVCRext {
        diff_cu_chroma_qp_offset_depth: u8::try_from(picture.diff_cu_chroma_qp_offset_depth)
            .unwrap_or(0),
        chroma_qp_offset_list_len_minus1: u8::try_from(picture.chroma_qp_offset_list_len_minus1)
            .unwrap_or(0),
        log2_sao_offset_scale_luma: u8::try_from(picture.log2_sao_offset_scale_luma).unwrap_or(0),
        log2_sao_offset_scale_chroma: u8::try_from(picture.log2_sao_offset_scale_chroma)
            .unwrap_or(0),
        log2_max_transform_skip_block_size_minus2: u8::try_from(
            picture.log2_max_transform_skip_block_size_minus2,
        )
        .unwrap_or(0),
        ..Default::default()
    };
    for (target, &offset) in extension
        .cb_qp_offset_list
        .iter_mut()
        .zip(&picture.cb_qp_offset_list)
    {
        *target = i8::try_from(offset).unwrap_or(0);
    }
    for (target, &offset) in extension
        .cr_qp_offset_list
        .iter_mut()
        .zip(&picture.cr_qp_offset_list)
    {
        *target = i8::try_from(offset).unwrap_or(0);
    }

    let fields = unsafe { &mut extension.range_extension_pic_fields.bits };
    fields.set_transform_skip_rotation_enabled_flag(u32::from(
        sequence.transform_skip_rotation_enabled_flag,
    ));
    fields.set_transform_skip_context_enabled_flag(u32::from(
        sequence.transform_skip_context_enabled_flag,
    ));
    fields.set_implicit_rdpcm_enabled_flag(u32::from(sequence.implicit_rdpcm_enabled_flag));
    fields.set_explicit_rdpcm_enabled_flag(u32::from(sequence.explicit_rdpcm_enabled_flag));
    fields.set_extended_precision_processing_flag(u32::from(
        sequence.extended_precision_processing_flag,
    ));
    fields.set_intra_smoothing_disabled_flag(u32::from(sequence.intra_smoothing_disabled_flag));
    fields.set_high_precision_offsets_enabled_flag(u32::from(
        sequence.high_precision_offsets_enabled_flag,
    ));
    fields.set_persistent_rice_adaptation_enabled_flag(u32::from(
        sequence.persistent_rice_adaptation_enabled_flag,
    ));
    fields.set_cabac_bypass_alignment_enabled_flag(u32::from(
        sequence.cabac_bypass_alignment_enabled_flag,
    ));
    fields.set_cross_component_prediction_enabled_flag(u32::from(
        picture.cross_component_prediction_enabled_flag,
    ));
    fields.set_chroma_qp_offset_list_enabled_flag(u32::from(
        picture.chroma_qp_offset_list_enabled_flag,
    ));

    extension
}

fn scaling_lists<'a>(sps: &'a Sps, pps: &'a Pps) -> Option<&'a ScalingLists> {
    if !sps.scaling_list_enabled_flag {
        None
    } else if pps.scaling_list_data_present_flag || !sps.scaling_list_data_present_flag {
        Some(&pps.scaling_list)
    } else {
        Some(&sps.scaling_list)
    }
}

fn iq_matrix(lists: &ScalingLists) -> ffi::VAIQMatrixBufferHEVC {
    let mut matrix = ffi::VAIQMatrixBufferHEVC::default();

    for (raster, diagonal) in matrix
        .ScalingList4x4
        .iter_mut()
        .zip(&lists.scaling_list_4x4)
    {
        for (index, &position) in DIAGONAL_4X4.iter().enumerate() {
            raster[position] = diagonal[index];
        }
    }
    for (raster, diagonal) in matrix
        .ScalingList8x8
        .iter_mut()
        .zip(&lists.scaling_list_8x8)
    {
        for (index, &position) in DIAGONAL_8X8.iter().enumerate() {
            raster[position] = diagonal[index];
        }
    }
    for (raster, diagonal) in matrix
        .ScalingList16x16
        .iter_mut()
        .zip(&lists.scaling_list_16x16)
    {
        for (index, &position) in DIAGONAL_8X8.iter().enumerate() {
            raster[position] = diagonal[index];
        }
    }
    for (list, source) in [0, 3].into_iter().enumerate() {
        for (index, &position) in DIAGONAL_8X8.iter().enumerate() {
            matrix.ScalingList32x32[list][position] = lists.scaling_list_32x32[source][index];
        }
        matrix.ScalingListDC32x32[list] =
            u8::try_from(lists.scaling_list_dc_coef_minus8_32x32[source] + 8).unwrap_or(16);
    }
    for (target, &coefficient) in matrix
        .ScalingListDC16x16
        .iter_mut()
        .zip(&lists.scaling_list_dc_coef_minus8_16x16)
    {
        *target = u8::try_from(coefficient + 8).unwrap_or(16);
    }

    matrix
}

fn reference_lists(header: &SliceHeader, set: &ReferenceSet) -> [Vec<Option<usize>>; 2] {
    if header.type_ == SliceType::I {
        return [Vec::new(), Vec::new()];
    }

    let total = usize::try_from(header.num_pic_total_curr).unwrap_or(0);
    let build = |order: [&Vec<Option<usize>>; 3],
                 active: usize,
                 modified: bool,
                 entries: &[u32]|
     -> Vec<Option<usize>> {
        let candidates: Vec<Option<usize>> = order.into_iter().flatten().copied().collect();
        if candidates.is_empty() {
            return vec![None; active];
        }

        let length = active.max(total);
        let temporary: Vec<Option<usize>> =
            candidates.iter().copied().cycle().take(length).collect();

        (0..active)
            .map(|index| {
                let source = if modified {
                    entries
                        .get(index)
                        .and_then(|&entry| usize::try_from(entry).ok())
                        .unwrap_or(index)
                } else {
                    index
                };
                temporary.get(source).copied().flatten()
            })
            .collect()
    };

    let modification = &header.ref_pic_list_modification;
    let list0 = build(
        [&set.before, &set.after, &set.long_term],
        usize::from(header.num_ref_idx_l0_active_minus1) + 1,
        modification.ref_pic_list_modification_flag_l0,
        &modification.list_entry_l0,
    );
    let list1 = if header.type_ == SliceType::B {
        build(
            [&set.after, &set.before, &set.long_term],
            usize::from(header.num_ref_idx_l1_active_minus1) + 1,
            modification.ref_pic_list_modification_flag_l1,
            &modification.list_entry_l1,
        )
    } else {
        Vec::new()
    };

    [list0, list1]
}

fn chroma_offset(sps: &Sps, denominator: u8, delta_weight: i8, delta_offset: i16) -> i8 {
    let half_range = i32::try_from(sps.wp_offset_half_range_c).unwrap_or(128);
    let weight = (1_i32 << denominator) + i32::from(delta_weight);
    let offset = half_range + i32::from(delta_offset) - ((half_range * weight) >> denominator);
    i8::try_from(offset.clamp(-half_range, half_range - 1)).unwrap_or(0)
}

fn slice_parameters(
    slice: &Slice<'_>,
    sps: &Sps,
    lists: &[Vec<Option<usize>>; 2],
    last: bool,
) -> ffi::VASliceParameterBufferHEVC {
    let header = &slice.header;
    let table = &header.pred_weight_table;

    let mut parameters = ffi::VASliceParameterBufferHEVC {
        slice_data_size: u32::try_from(slice.nalu.size).unwrap_or(u32::MAX),
        slice_data_offset: 0,
        slice_data_flag: ffi::VA_SLICE_DATA_FLAG_ALL,
        slice_data_byte_offset: header.header_bit_size / 8,
        slice_segment_address: header.segment_address,
        RefPicList: [[NO_REFERENCE; MAX_REFERENCE_FRAMES]; 2],
        collocated_ref_idx: if header.temporal_mvp_enabled_flag {
            header.collocated_ref_idx
        } else {
            NO_REFERENCE
        },
        num_ref_idx_l0_active_minus1: header.num_ref_idx_l0_active_minus1,
        num_ref_idx_l1_active_minus1: header.num_ref_idx_l1_active_minus1,
        slice_qp_delta: header.qp_delta,
        slice_cb_qp_offset: header.cb_qp_offset,
        slice_cr_qp_offset: header.cr_qp_offset,
        slice_beta_offset_div2: header.beta_offset_div2,
        slice_tc_offset_div2: header.tc_offset_div2,
        luma_log2_weight_denom: table.luma_log2_weight_denom,
        delta_chroma_log2_weight_denom: table.delta_chroma_log2_weight_denom,
        five_minus_max_num_merge_cand: header.five_minus_max_num_merge_cand,
        num_entry_point_offsets: u16::try_from(header.num_entry_point_offsets).unwrap_or(0),
        entry_offset_to_subset_array: 0,
        slice_data_num_emu_prevn_bytes: u16::try_from(header.n_emulation_prevention_bytes)
            .unwrap_or(0),
        ..Default::default()
    };

    for (target, list) in parameters.RefPicList.iter_mut().zip(lists) {
        for (slot, entry) in target.iter_mut().zip(list) {
            if let Some(index) = entry.and_then(|index| u8::try_from(index).ok()) {
                *slot = index;
            }
        }
    }

    let denominator = table.chroma_log2_weight_denom;
    for index in 0..MAX_REFERENCE_FRAMES {
        parameters.delta_luma_weight_l0[index] = table.delta_luma_weight_l0[index];
        parameters.luma_offset_l0[index] = table.luma_offset_l0[index];
        for component in 0..2 {
            parameters.delta_chroma_weight_l0[index][component] =
                table.delta_chroma_weight_l0[index][component];
            parameters.ChromaOffsetL0[index][component] = chroma_offset(
                sps,
                denominator,
                table.delta_chroma_weight_l0[index][component],
                table.delta_chroma_offset_l0[index][component],
            );
        }

        if header.type_ == SliceType::B {
            parameters.delta_luma_weight_l1[index] = table.delta_luma_weight_l1[index];
            parameters.luma_offset_l1[index] = table.luma_offset_l1[index];
            for component in 0..2 {
                parameters.delta_chroma_weight_l1[index][component] =
                    table.delta_chroma_weight_l1[index][component];
                parameters.ChromaOffsetL1[index][component] = chroma_offset(
                    sps,
                    denominator,
                    table.delta_chroma_weight_l1[index][component],
                    table.delta_chroma_offset_l1[index][component],
                );
            }
        }
    }

    let flags = unsafe { &mut parameters.LongSliceFlags.fields };
    flags.set_LastSliceOfPic(u32::from(last));
    flags.set_dependent_slice_segment_flag(u32::from(header.dependent_slice_segment_flag));
    flags.set_slice_type(header.type_ as u32);
    flags.set_color_plane_id(u32::from(header.colour_plane_id));
    flags.set_slice_sao_luma_flag(u32::from(header.sao_luma_flag));
    flags.set_slice_sao_chroma_flag(u32::from(header.sao_chroma_flag));
    flags.set_mvd_l1_zero_flag(u32::from(header.mvd_l1_zero_flag));
    flags.set_cabac_init_flag(u32::from(header.cabac_init_flag));
    flags.set_slice_temporal_mvp_enabled_flag(u32::from(header.temporal_mvp_enabled_flag));
    flags.set_slice_deblocking_filter_disabled_flag(u32::from(
        header.deblocking_filter_disabled_flag,
    ));
    flags.set_collocated_from_l0_flag(u32::from(header.collocated_from_l0_flag));
    flags.set_slice_loop_filter_across_slices_enabled_flag(u32::from(
        header.loop_filter_across_slices_enabled_flag,
    ));

    parameters
}

fn slice_range_extension(
    header: &SliceHeader,
    base: &ffi::VASliceParameterBufferHEVC,
) -> ffi::VASliceParameterBufferHEVCRext {
    let mut extension = ffi::VASliceParameterBufferHEVCRext {
        slice_act_y_qp_offset: header.slice_act_y_qp_offset,
        slice_act_cb_qp_offset: header.slice_act_cb_qp_offset,
        slice_act_cr_qp_offset: header.slice_act_cr_qp_offset,
        ..Default::default()
    };

    for index in 0..MAX_REFERENCE_FRAMES {
        extension.luma_offset_l0[index] = i16::from(base.luma_offset_l0[index]);
        extension.luma_offset_l1[index] = i16::from(base.luma_offset_l1[index]);
        for component in 0..2 {
            extension.ChromaOffsetL0[index][component] =
                i16::from(base.ChromaOffsetL0[index][component]);
            extension.ChromaOffsetL1[index][component] =
                i16::from(base.ChromaOffsetL1[index][component]);
        }
    }

    let flags = unsafe { &mut extension.slice_ext_flags.bits };
    flags.set_cu_chroma_qp_offset_enabled_flag(u32::from(header.cu_chroma_qp_offset_enabled_flag));
    flags.set_use_integer_mv_flag(u32::from(header.use_integer_mv_flag));

    extension
}
