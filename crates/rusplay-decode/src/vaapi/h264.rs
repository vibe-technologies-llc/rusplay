use std::{cmp::Reverse, io::Cursor, rc::Rc};

use cros_codecs::codec::h264::parser::{
    MaxLongTermFrameIdx, NaluHeader, NaluType, Parser, Pps, RefPicListModification, RefPicMarking,
    RefPicMarkingInner, Slice, SliceType, Sps,
};
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, EncodedFrame, PixelFormat, Resolution, Timestamp,
};

use crate::{
    BitstreamUnit, DecodeError, DecodedFrame,
    vaapi::{
        Stream, StreamFormat, annexb, ffi,
        va::{Display, INVALID_SURFACE, PoolSurface, Profile},
    },
};

const CODEC: Codec = Codec::H264;
const MAX_REFERENCE_FRAMES: usize = 16;
const MAX_LIST_ENTRIES: usize = 32;
const LEVEL_3_1: u8 = 31;
const ZIGZAG_4X4: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];
const ZIGZAG_8X8: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

fn malformed(unit: BitstreamUnit) -> impl FnOnce(String) -> DecodeError {
    annexb::malformed(CODEC, unit)
}

#[derive(Debug, Clone)]
struct Reference {
    surface: PoolSurface,
    frame_num: u32,
    frame_num_wrap: i32,
    long_term_frame_idx: Option<u32>,
    top: i32,
    bottom: i32,
}

impl Reference {
    fn poc(&self) -> i32 {
        self.top.min(self.bottom)
    }

    fn is_short_term(&self) -> bool {
        self.long_term_frame_idx.is_none()
    }

    fn va_picture(&self) -> ffi::VAPictureH264 {
        let (frame_idx, flags) = match self.long_term_frame_idx {
            Some(index) => (index, ffi::VA_PICTURE_H264_LONG_TERM_REFERENCE),
            None => (self.frame_num, ffi::VA_PICTURE_H264_SHORT_TERM_REFERENCE),
        };

        ffi::VAPictureH264 {
            picture_id: self.surface.id,
            frame_idx,
            flags,
            TopFieldOrderCnt: self.top,
            BottomFieldOrderCnt: self.bottom,
            ..Default::default()
        }
    }
}

fn invalid_picture() -> ffi::VAPictureH264 {
    ffi::VAPictureH264 {
        picture_id: INVALID_SURFACE,
        flags: ffi::VA_PICTURE_H264_INVALID,
        ..Default::default()
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct PreviousPicture {
    frame_num: u32,
    frame_num_offset: u32,
    had_mmco5: bool,
}

#[derive(Debug, Default, Clone, Copy)]
struct PreviousReference {
    poc_msb: i32,
    poc_lsb: i32,
}

#[derive(Debug, Clone, Copy)]
struct OrderCount {
    top: i32,
    bottom: i32,
    poc_msb: i32,
    frame_num_offset: u32,
}

impl OrderCount {
    fn poc(&self) -> i32 {
        self.top.min(self.bottom)
    }
}

pub(super) struct H264Decoder {
    parser: Parser,
    stream: Option<Stream>,
    dpb: Dpb,
}

impl H264Decoder {
    pub(super) fn new() -> Self {
        Self {
            parser: Parser::default(),
            stream: None,
            dpb: Dpb::default(),
        }
    }

    pub(super) fn decode(
        &mut self,
        display: &Rc<Display>,
        frame: &EncodedFrame,
    ) -> Result<Option<DecodedFrame>, DecodeError> {
        let mut cursor = Cursor::new(frame.data.as_slice());
        let mut slices: Vec<Slice<'_>> = Vec::new();
        let mut output = None;

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

                    if slice.header.first_mb_in_slice == 0 && !slices.is_empty() {
                        output = Some(self.decode_picture(display, &slices, frame.timestamp)?);
                        slices.clear();
                    }

                    slices.push(slice);
                }
                _ => {}
            }
        }

        if !slices.is_empty() {
            output = Some(self.decode_picture(display, &slices, frame.timestamp)?);
        }

        Ok(output)
    }

    fn decode_picture(
        &mut self,
        display: &Rc<Display>,
        slices: &[Slice<'_>],
        timestamp: Timestamp,
    ) -> Result<DecodedFrame, DecodeError> {
        let first = &slices[0];
        let header = &first.header;

        if header.field_pic_flag {
            return Err(DecodeError::InterlacedUnsupported { codec: CODEC });
        }

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
            self.dpb.references.clear();
            self.dpb.max_long_term_frame_idx = None;
        }

        let idr = first.nalu.header.idr_pic_flag;
        let nal_ref_idc = first.nalu.header.ref_idc;
        let frame_num = u32::from(header.frame_num);
        let max_frame_num = sps.max_frame_num();

        if idr {
            self.dpb.references.clear();
        }

        for reference in &mut self.dpb.references {
            reference.frame_num_wrap = if reference.frame_num > frame_num {
                i32::try_from(reference.frame_num).unwrap_or(i32::MAX)
                    - i32::try_from(max_frame_num).unwrap_or(i32::MAX)
            } else {
                i32::try_from(reference.frame_num).unwrap_or(i32::MAX)
            };
        }

        let order = self.dpb.order_count(&sps, first, idr, nal_ref_idc);
        let surface = stream.pool.acquire()?;

        let picture = ffi::VAPictureH264 {
            picture_id: surface.id,
            frame_idx: frame_num,
            flags: 0,
            TopFieldOrderCnt: order.top,
            BottomFieldOrderCnt: order.bottom,
            ..Default::default()
        };

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
                ),
            )?,
            stream
                .session
                .parameter(ffi::VABufferType_VAIQMatrixBufferType, &iq_matrix(&pps))?,
        ];

        for slice in slices {
            let lists = self
                .dpb
                .reference_lists(slice, order.poc(), frame_num, max_frame_num);
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

        let output = stream.output(&surface, timestamp)?;

        self.dpb.finish(&Decoded {
            marking: &header.dec_ref_pic_marking,
            idr,
            nal_ref_idc,
            max_num_ref_frames: sps.max_num_ref_frames,
            surface,
            frame_num,
            pic_order_cnt_lsb: i32::from(header.pic_order_cnt_lsb),
            order,
        });

        Ok(output)
    }
}

struct Decoded<'a> {
    marking: &'a RefPicMarking,
    idr: bool,
    nal_ref_idc: u8,
    max_num_ref_frames: u8,
    surface: PoolSurface,
    frame_num: u32,
    pic_order_cnt_lsb: i32,
    order: OrderCount,
}

#[derive(Debug, Default)]
struct Dpb {
    references: Vec<Reference>,
    max_long_term_frame_idx: Option<u32>,
    previous: PreviousPicture,
    previous_reference: PreviousReference,
}

impl Dpb {
    fn order_count(&self, sps: &Sps, slice: &Slice<'_>, idr: bool, nal_ref_idc: u8) -> OrderCount {
        let header = &slice.header;
        let frame_num = u32::from(header.frame_num);
        let max_frame_num = sps.max_frame_num();

        let previous_offset = if self.previous.had_mmco5 {
            0
        } else {
            self.previous.frame_num_offset
        };
        let frame_num_offset = if idr {
            0
        } else if self.previous.frame_num > frame_num {
            previous_offset.wrapping_add(max_frame_num)
        } else {
            previous_offset
        };

        match sps.pic_order_cnt_type {
            0 => {
                let (previous_high, previous_low) = if idr {
                    (0, 0)
                } else {
                    (
                        self.previous_reference.poc_msb,
                        self.previous_reference.poc_lsb,
                    )
                };
                let wrap = 1_i32 << (sps.log2_max_pic_order_cnt_lsb_minus4 + 4);
                let low = i32::from(header.pic_order_cnt_lsb);

                let high = if low < previous_low && previous_low - low >= wrap / 2 {
                    previous_high + wrap
                } else if low > previous_low && low - previous_low > wrap / 2 {
                    previous_high - wrap
                } else {
                    previous_high
                };

                let top = high + low;
                OrderCount {
                    top,
                    bottom: top + header.delta_pic_order_cnt_bottom,
                    poc_msb: high,
                    frame_num_offset,
                }
            }
            1 => {
                let cycle_length = u32::from(sps.num_ref_frames_in_pic_order_cnt_cycle);
                let mut absolute = if cycle_length == 0 {
                    0
                } else {
                    frame_num_offset.wrapping_add(frame_num)
                };
                if nal_ref_idc == 0 && absolute > 0 {
                    absolute -= 1;
                }

                let mut expected = 0_i32;
                if absolute > 0 {
                    let cycles = (absolute - 1) / cycle_length;
                    let in_cycle = (absolute - 1) % cycle_length;
                    expected = i32::try_from(cycles)
                        .unwrap_or(i32::MAX)
                        .wrapping_mul(sps.expected_delta_per_pic_order_cnt_cycle);
                    for offset in
                        &sps.offset_for_ref_frame[..=usize::try_from(in_cycle).unwrap_or(0)]
                    {
                        expected = expected.wrapping_add(*offset);
                    }
                }
                if nal_ref_idc == 0 {
                    expected = expected.wrapping_add(sps.offset_for_non_ref_pic);
                }

                let top = expected + header.delta_pic_order_cnt[0];
                OrderCount {
                    top,
                    bottom: top
                        + sps.offset_for_top_to_bottom_field
                        + header.delta_pic_order_cnt[1],
                    poc_msb: 0,
                    frame_num_offset,
                }
            }
            _ => {
                let doubled = i32::try_from(frame_num_offset.wrapping_add(frame_num))
                    .unwrap_or(i32::MAX)
                    .wrapping_mul(2);
                let count = if idr {
                    0
                } else if nal_ref_idc == 0 {
                    doubled - 1
                } else {
                    doubled
                };

                OrderCount {
                    top: count,
                    bottom: count,
                    poc_msb: 0,
                    frame_num_offset,
                }
            }
        }
    }

    fn reference_lists(
        &self,
        slice: &Slice<'_>,
        poc: i32,
        frame_num: u32,
        max_frame_num: u32,
    ) -> [Vec<Option<usize>>; 2] {
        let header = &slice.header;
        let short_term: Vec<usize> = (0..self.references.len())
            .filter(|&index| self.references[index].is_short_term())
            .collect();
        let mut long_term: Vec<usize> = (0..self.references.len())
            .filter(|&index| !self.references[index].is_short_term())
            .collect();
        long_term.sort_by_key(|&index| self.references[index].long_term_frame_idx);

        let (initial0, initial1) = match header.slice_type {
            SliceType::P | SliceType::Sp => {
                let mut list = short_term;
                list.sort_by_key(|&index| Reverse(self.references[index].frame_num_wrap));
                list.extend(&long_term);
                (list, Vec::new())
            }
            SliceType::B => {
                let mut before: Vec<usize> = short_term
                    .iter()
                    .copied()
                    .filter(|&index| self.references[index].poc() < poc)
                    .collect();
                before.sort_by_key(|&index| Reverse(self.references[index].poc()));
                let mut after: Vec<usize> = short_term
                    .iter()
                    .copied()
                    .filter(|&index| self.references[index].poc() > poc)
                    .collect();
                after.sort_by_key(|&index| self.references[index].poc());

                let mut list0 = before.clone();
                list0.extend(&after);
                list0.extend(&long_term);

                let mut list1 = after;
                list1.extend(&before);
                list1.extend(&long_term);

                if list1.len() > 1 && list1 == list0 {
                    list1.swap(0, 1);
                }

                (list0, list1)
            }
            SliceType::I | SliceType::Si => (Vec::new(), Vec::new()),
        };

        let active0 = usize::from(header.num_ref_idx_l0_active_minus1) + 1;
        let active1 = usize::from(header.num_ref_idx_l1_active_minus1) + 1;

        let list0 = if header.slice_type == SliceType::I || header.slice_type == SliceType::Si {
            Vec::new()
        } else {
            self.modify_list(
                initial0,
                active0,
                header.ref_pic_list_modification_flag_l0,
                &header.ref_pic_list_modification_l0,
                frame_num,
                max_frame_num,
            )
        };
        let list1 = if header.slice_type == SliceType::B {
            self.modify_list(
                initial1,
                active1,
                header.ref_pic_list_modification_flag_l1,
                &header.ref_pic_list_modification_l1,
                frame_num,
                max_frame_num,
            )
        } else {
            Vec::new()
        };

        [list0, list1]
    }

    fn modify_list(
        &self,
        initial: Vec<usize>,
        active: usize,
        modified: bool,
        modifications: &[RefPicListModification],
        frame_num: u32,
        max_frame_num: u32,
    ) -> Vec<Option<usize>> {
        let mut list: Vec<Option<usize>> = initial.into_iter().map(Some).collect();
        list.resize(active, None);

        if !modified {
            return list;
        }

        let current_pic_num = i64::from(frame_num);
        let max_pic_num = i64::from(max_frame_num);
        let mut predicted = current_pic_num;

        for (position, modification) in modifications.iter().enumerate() {
            if position >= active {
                break;
            }

            let target = match modification.modification_of_pic_nums_idc {
                0 | 1 => {
                    let difference = i64::from(modification.abs_diff_pic_num_minus1) + 1;
                    let mut no_wrap = if modification.modification_of_pic_nums_idc == 0 {
                        predicted - difference
                    } else {
                        predicted + difference
                    };
                    if no_wrap < 0 {
                        no_wrap += max_pic_num;
                    } else if no_wrap >= max_pic_num {
                        no_wrap -= max_pic_num;
                    }
                    predicted = no_wrap;

                    let pic_num = if no_wrap > current_pic_num {
                        no_wrap - max_pic_num
                    } else {
                        no_wrap
                    };
                    self.references.iter().position(|reference| {
                        reference.is_short_term() && i64::from(reference.frame_num_wrap) == pic_num
                    })
                }
                2 => self.references.iter().position(|reference| {
                    reference.long_term_frame_idx == Some(modification.long_term_pic_num)
                }),
                _ => break,
            };

            list.insert(position, target);
            if let Some(duplicate) = list
                .iter()
                .skip(position + 1)
                .position(|entry| entry.is_some() && *entry == target)
            {
                list.remove(position + 1 + duplicate);
            }
            list.truncate(active);
        }

        list
    }

    fn finish(&mut self, picture: &Decoded<'_>) {
        let had_mmco5 = self.mark_references(picture);
        let order = picture.order;

        self.previous = PreviousPicture {
            frame_num: if had_mmco5 { 0 } else { picture.frame_num },
            frame_num_offset: if had_mmco5 { 0 } else { order.frame_num_offset },
            had_mmco5,
        };

        if picture.nal_ref_idc == 0 {
            return;
        }

        self.previous_reference = if had_mmco5 {
            PreviousReference {
                poc_msb: 0,
                poc_lsb: order.top - order.poc(),
            }
        } else {
            PreviousReference {
                poc_msb: order.poc_msb,
                poc_lsb: picture.pic_order_cnt_lsb,
            }
        };
    }

    fn mark_references(&mut self, picture: &Decoded<'_>) -> bool {
        let Decoded {
            marking,
            idr,
            nal_ref_idc,
            max_num_ref_frames,
            ref surface,
            frame_num,
            order,
            ..
        } = *picture;

        if nal_ref_idc == 0 {
            return false;
        }

        let mut current_long_term = None;
        let mut had_mmco5 = false;

        if idr {
            self.references.clear();
            if marking.long_term_reference_flag {
                current_long_term = Some(0);
                self.max_long_term_frame_idx = Some(0);
            } else {
                self.max_long_term_frame_idx = None;
            }
        } else if marking.adaptive_ref_pic_marking_mode_flag {
            for operation in &marking.inner {
                match self.apply_operation(operation, frame_num) {
                    Marked::Nothing => {}
                    Marked::AllUnused => had_mmco5 = true,
                    Marked::CurrentLongTerm(index) => current_long_term = Some(index),
                }
            }
        } else {
            self.slide_window(max_num_ref_frames);
        }

        if self.references.len() >= MAX_REFERENCE_FRAMES {
            self.references.remove(0);
        }

        let shift = if had_mmco5 { order.poc() } else { 0 };
        self.references.push(Reference {
            surface: surface.clone(),
            frame_num: if had_mmco5 { 0 } else { frame_num },
            frame_num_wrap: 0,
            long_term_frame_idx: current_long_term,
            top: order.top - shift,
            bottom: order.bottom - shift,
        });

        had_mmco5
    }

    fn apply_operation(&mut self, operation: &RefPicMarkingInner, frame_num: u32) -> Marked {
        let pic_num =
            i64::from(frame_num) - (i64::from(operation.difference_of_pic_nums_minus1) + 1);
        let short_term_with_pic_num = |reference: &Reference| {
            reference.is_short_term() && i64::from(reference.frame_num_wrap) == pic_num
        };

        match operation.memory_management_control_operation {
            1 => {
                self.references
                    .retain(|reference| !short_term_with_pic_num(reference));
            }
            2 => {
                self.references.retain(|reference| {
                    reference.long_term_frame_idx != Some(operation.long_term_pic_num)
                });
            }
            3 => {
                let index = operation.long_term_frame_idx;
                self.references
                    .retain(|reference| reference.long_term_frame_idx != Some(index));
                if let Some(reference) = self
                    .references
                    .iter_mut()
                    .find(|reference| short_term_with_pic_num(reference))
                {
                    reference.long_term_frame_idx = Some(index);
                }
            }
            4 => {
                self.max_long_term_frame_idx = match operation.max_long_term_frame_idx {
                    MaxLongTermFrameIdx::NoLongTermFrameIndices => None,
                    MaxLongTermFrameIdx::Idx(index) => Some(index),
                };
                let limit = self.max_long_term_frame_idx;
                self.references
                    .retain(|reference| match (reference.long_term_frame_idx, limit) {
                        (Some(index), Some(limit)) => index <= limit,
                        (Some(_), None) => false,
                        (None, _) => true,
                    });
            }
            5 => {
                self.references.clear();
                self.max_long_term_frame_idx = None;
                return Marked::AllUnused;
            }
            6 => {
                let index = operation.long_term_frame_idx;
                self.references
                    .retain(|reference| reference.long_term_frame_idx != Some(index));
                return Marked::CurrentLongTerm(index);
            }
            _ => {}
        }

        Marked::Nothing
    }

    fn slide_window(&mut self, max_num_ref_frames: u8) {
        let limit = usize::from(max_num_ref_frames).max(1);
        while self.references.len() >= limit {
            let oldest = self
                .references
                .iter()
                .enumerate()
                .filter(|(_, reference)| reference.is_short_term())
                .min_by_key(|(_, reference)| reference.frame_num_wrap)
                .map(|(index, _)| index);
            match oldest {
                Some(index) => {
                    self.references.remove(index);
                }
                None => break,
            }
        }
    }
}

enum Marked {
    Nothing,
    AllUnused,
    CurrentLongTerm(u32),
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
    })
}

fn picture_parameters(
    sps: &Sps,
    pps: &Pps,
    current: ffi::VAPictureH264,
    references: &[Reference],
    frame_num: u32,
    nal_ref_idc: u8,
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
        .filter(|reference| reference.is_short_term())
        .chain(
            references
                .iter()
                .filter(|reference| !reference.is_short_term()),
        );
    for (slot, reference) in parameters.ReferenceFrames.iter_mut().zip(ordered) {
        *slot = reference.va_picture();
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
    picture.set_field_pic_flag(0);
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
    lists: &[Vec<Option<usize>>; 2],
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
            if let Some(reference) = entry.and_then(|index| references.get(index)) {
                *slot = reference.va_picture();
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
