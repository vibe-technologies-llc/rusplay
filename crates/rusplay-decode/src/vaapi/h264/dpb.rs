use std::cmp::Reverse;

use cros_codecs::codec::h264::parser::{
    MaxLongTermFrameIdx, RefPicListModification, RefPicMarking, RefPicMarkingInner, Slice,
    SliceType, Sps,
};

use crate::vaapi::{
    ffi,
    va::{INVALID_SURFACE, PoolSurface, SurfaceId},
};

pub(super) const MAX_REFERENCE_FRAMES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Structure {
    Frame,
    Top,
    Bottom,
}

impl Structure {
    pub(super) fn of(field_pic_flag: bool, bottom_field_flag: bool) -> Self {
        match (field_pic_flag, bottom_field_flag) {
            (false, _) => Self::Frame,
            (true, false) => Self::Top,
            (true, true) => Self::Bottom,
        }
    }

    pub(super) fn is_field(self) -> bool {
        self != Self::Frame
    }

    pub(super) fn opposite(self) -> Self {
        match self {
            Self::Frame => Self::Frame,
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }

    pub(super) fn parity(self) -> Option<usize> {
        match self {
            Self::Frame => None,
            Self::Top => Some(0),
            Self::Bottom => Some(1),
        }
    }

    pub(super) fn va_flags(self) -> u32 {
        match self {
            Self::Frame => 0,
            Self::Top => ffi::VA_PICTURE_H264_TOP_FIELD,
            Self::Bottom => ffi::VA_PICTURE_H264_BOTTOM_FIELD,
        }
    }

    fn parities(self) -> &'static [usize] {
        match self {
            Self::Frame => &[0, 1],
            Self::Top => &[0],
            Self::Bottom => &[1],
        }
    }

    fn pictures(self) -> &'static [Self] {
        match self {
            Self::Frame => &[Self::Frame],
            Self::Top | Self::Bottom => &[Self::Top, Self::Bottom],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Marking {
    Unused,
    ShortTerm,
    LongTerm,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct FieldState {
    pub(super) poc: Option<i32>,
    pub(super) marking: Marking,
}

#[derive(Debug, Clone)]
pub(super) struct Reference {
    pub(super) surface: PoolSurface,
    pub(super) frame_num: u32,
    pub(super) frame_num_wrap: i32,
    pub(super) long_term_frame_idx: u32,
    pub(super) fields: [FieldState; 2],
}

impl Reference {
    fn marking_of(&self, structure: Structure) -> Marking {
        let first = self.fields[structure.parities()[0]].marking;
        let uniform = structure
            .parities()
            .iter()
            .all(|&parity| self.fields[parity].marking == first);

        if uniform { first } else { Marking::Unused }
    }

    fn has(&self, marking: Marking) -> bool {
        self.fields.iter().any(|field| field.marking == marking)
    }

    fn is_referenced(&self) -> bool {
        self.fields
            .iter()
            .any(|field| field.marking != Marking::Unused)
    }

    fn frame_poc(&self) -> i32 {
        self.fields
            .iter()
            .filter_map(|field| field.poc)
            .min()
            .unwrap_or(0)
    }

    fn unmark(&mut self, structure: Structure, marking: Marking) {
        for &parity in structure.parities() {
            if self.fields[parity].marking == marking {
                self.fields[parity].marking = Marking::Unused;
            }
        }
    }

    fn mark(&mut self, structure: Structure, marking: Marking) {
        for &parity in structure.parities() {
            self.fields[parity].marking = marking;
        }
    }

    fn short_pic_num(&self, entry: Structure, current: Structure) -> i64 {
        let wrap = i64::from(self.frame_num_wrap);
        match (current.is_field(), entry == current) {
            (false, _) => wrap,
            (true, true) => 2 * wrap + 1,
            (true, false) => 2 * wrap,
        }
    }

    fn long_pic_num(&self, entry: Structure, current: Structure) -> i64 {
        let index = i64::from(self.long_term_frame_idx);
        match (current.is_field(), entry == current) {
            (false, _) => index,
            (true, true) => 2 * index + 1,
            (true, false) => 2 * index,
        }
    }

    fn marked_structure(&self) -> Structure {
        match (
            self.fields[0].marking != Marking::Unused,
            self.fields[1].marking != Marking::Unused,
        ) {
            (true, false) => Structure::Top,
            (false, true) => Structure::Bottom,
            _ => Structure::Frame,
        }
    }

    pub(super) fn va_frame(&self) -> ffi::VAPictureH264 {
        self.va_picture(self.marked_structure())
    }

    pub(super) fn va_picture(&self, structure: Structure) -> ffi::VAPictureH264 {
        let long_term = structure
            .parities()
            .iter()
            .any(|&parity| self.fields[parity].marking == Marking::LongTerm);
        let (frame_idx, reference) = if long_term {
            (
                self.long_term_frame_idx,
                ffi::VA_PICTURE_H264_LONG_TERM_REFERENCE,
            )
        } else {
            (self.frame_num, ffi::VA_PICTURE_H264_SHORT_TERM_REFERENCE)
        };

        ffi::VAPictureH264 {
            picture_id: self.surface.id,
            frame_idx,
            flags: reference | structure.va_flags(),
            TopFieldOrderCnt: self.fields[0].poc.unwrap_or(0),
            BottomFieldOrderCnt: self.fields[1].poc.unwrap_or(0),
            ..Default::default()
        }
    }

    pub(super) fn has_long_term(&self) -> bool {
        self.has(Marking::LongTerm)
    }
}

pub(super) fn invalid_picture() -> ffi::VAPictureH264 {
    ffi::VAPictureH264 {
        picture_id: INVALID_SURFACE,
        flags: ffi::VA_PICTURE_H264_INVALID,
        ..Default::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ListEntry {
    pub(super) index: usize,
    pub(super) structure: Structure,
}

pub(super) type ReferenceList = Vec<Option<ListEntry>>;

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct PreviousPicture {
    frame_num: u32,
    frame_num_offset: u32,
    pub(super) had_mmco5: bool,
}

#[derive(Debug, Default, Clone, Copy)]
struct PreviousReference {
    poc_msb: i32,
    poc_lsb: i32,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct OrderCount {
    pub(super) top: Option<i32>,
    pub(super) bottom: Option<i32>,
    poc_msb: i32,
    frame_num_offset: u32,
}

impl OrderCount {
    pub(super) fn poc(&self) -> i32 {
        match (self.top, self.bottom) {
            (Some(top), Some(bottom)) => top.min(bottom),
            (Some(poc), None) | (None, Some(poc)) => poc,
            (None, None) => 0,
        }
    }

    fn of(
        structure: Structure,
        top: i32,
        bottom: i32,
        poc_msb: i32,
        frame_num_offset: u32,
    ) -> Self {
        Self {
            top: (structure != Structure::Bottom).then_some(top),
            bottom: (structure != Structure::Top).then_some(bottom),
            poc_msb,
            frame_num_offset,
        }
    }
}

pub(super) struct Decoded<'a> {
    pub(super) marking: &'a RefPicMarking,
    pub(super) idr: bool,
    pub(super) nal_ref_idc: u8,
    pub(super) max_num_ref_frames: u8,
    pub(super) surface: PoolSurface,
    pub(super) structure: Structure,
    pub(super) second_field: bool,
    pub(super) partner_poc: Option<i32>,
    pub(super) frame_num: u32,
    pub(super) pic_order_cnt_lsb: i32,
    pub(super) order: OrderCount,
}

#[derive(Debug, Default)]
pub(super) struct Dpb {
    pub(super) references: Vec<Reference>,
    pub(super) max_long_term_frame_idx: Option<u32>,
    pub(super) previous: PreviousPicture,
    previous_reference: PreviousReference,
}

impl Dpb {
    pub(super) fn reset(&mut self) {
        self.references.clear();
        self.max_long_term_frame_idx = None;
    }

    pub(super) fn update_frame_num_wrap(&mut self, frame_num: u32, max_frame_num: u32) {
        let max_frame_num = i32::try_from(max_frame_num).unwrap_or(i32::MAX);

        for reference in &mut self.references {
            let own = i32::try_from(reference.frame_num).unwrap_or(i32::MAX);
            reference.frame_num_wrap = if reference.frame_num > frame_num {
                own - max_frame_num
            } else {
                own
            };
        }
    }

    pub(super) fn find_surface(&self, surface: SurfaceId) -> Option<&Reference> {
        self.references
            .iter()
            .find(|reference| reference.surface.id == surface)
    }

    pub(super) fn order_count(
        &self,
        sps: &Sps,
        slice: &Slice<'_>,
        idr: bool,
        nal_ref_idc: u8,
        structure: Structure,
    ) -> OrderCount {
        let header = &slice.header;
        let frame_num = u32::from(header.frame_num);
        let max_frame_num = sps.max_frame_num();
        let previous = self.previous;

        let previous_offset = if previous.had_mmco5 {
            0
        } else {
            previous.frame_num_offset
        };
        let frame_num_offset = if idr {
            0
        } else if previous.frame_num > frame_num {
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
                let bottom = if structure.is_field() {
                    top
                } else {
                    top + header.delta_pic_order_cnt_bottom
                };
                OrderCount::of(structure, top, bottom, high, frame_num_offset)
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
                let bottom = match structure {
                    Structure::Frame => {
                        top + sps.offset_for_top_to_bottom_field + header.delta_pic_order_cnt[1]
                    }
                    _ => {
                        expected
                            + sps.offset_for_top_to_bottom_field
                            + header.delta_pic_order_cnt[0]
                    }
                };
                OrderCount::of(structure, top, bottom, 0, frame_num_offset)
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

                OrderCount::of(structure, count, count, 0, frame_num_offset)
            }
        }
    }

    pub(super) fn reference_lists(
        &self,
        slice: &Slice<'_>,
        structure: Structure,
        poc: i32,
        frame_num: u32,
        max_frame_num: u32,
    ) -> [ReferenceList; 2] {
        let header = &slice.header;

        let [initial0, initial1] = match header.slice_type {
            SliceType::P | SliceType::Sp => [self.initial_p_list(structure), Vec::new()],
            SliceType::B => self.initial_b_lists(structure, poc),
            SliceType::I | SliceType::Si => [Vec::new(), Vec::new()],
        };

        let active0 = usize::from(header.num_ref_idx_l0_active_minus1) + 1;
        let active1 = usize::from(header.num_ref_idx_l1_active_minus1) + 1;
        let numbering = Numbering {
            structure,
            frame_num,
        };

        let list0 = if matches!(header.slice_type, SliceType::I | SliceType::Si) {
            Vec::new()
        } else {
            self.modify_list(
                initial0,
                active0,
                header.ref_pic_list_modification_flag_l0,
                &header.ref_pic_list_modification_l0,
                &numbering,
                max_frame_num,
            )
        };
        let list1 = if header.slice_type == SliceType::B {
            self.modify_list(
                initial1,
                active1,
                header.ref_pic_list_modification_flag_l1,
                &header.ref_pic_list_modification_l1,
                &numbering,
                max_frame_num,
            )
        } else {
            Vec::new()
        };

        [list0, list1]
    }

    fn frames_where(&self, structure: Structure, marking: Marking) -> Vec<usize> {
        (0..self.references.len())
            .filter(|&index| {
                let reference = &self.references[index];
                if structure.is_field() {
                    reference.has(marking)
                } else {
                    reference.marking_of(Structure::Frame) == marking
                }
            })
            .collect()
    }

    fn long_term_frames(&self, structure: Structure) -> Vec<usize> {
        let mut frames = self.frames_where(structure, Marking::LongTerm);
        frames.sort_by_key(|&index| self.references[index].long_term_frame_idx);
        frames
    }

    fn initial_p_list(&self, structure: Structure) -> Vec<ListEntry> {
        let mut short_term = self.frames_where(structure, Marking::ShortTerm);
        short_term.sort_by_key(|&index| Reverse(self.references[index].frame_num_wrap));
        let long_term = self.long_term_frames(structure);

        let mut list = self.expand(&short_term, structure, Marking::ShortTerm);
        list.extend(self.expand(&long_term, structure, Marking::LongTerm));
        list
    }

    fn initial_b_lists(&self, structure: Structure, poc: i32) -> [Vec<ListEntry>; 2] {
        let short_term = self.frames_where(structure, Marking::ShortTerm);
        let poc_of = |index: usize| self.references[index].frame_poc();

        let mut before: Vec<usize> = short_term
            .iter()
            .copied()
            .filter(|&index| {
                if structure.is_field() {
                    poc_of(index) <= poc
                } else {
                    poc_of(index) < poc
                }
            })
            .collect();
        before.sort_by_key(|&index| Reverse(poc_of(index)));
        let mut after: Vec<usize> = short_term
            .iter()
            .copied()
            .filter(|&index| poc_of(index) > poc)
            .collect();
        after.sort_by_key(|&index| poc_of(index));

        let long_term = self.expand(
            &self.long_term_frames(structure),
            structure,
            Marking::LongTerm,
        );

        let order0: Vec<usize> = before.iter().chain(&after).copied().collect();
        let order1: Vec<usize> = after.iter().chain(&before).copied().collect();

        let mut list0 = self.expand(&order0, structure, Marking::ShortTerm);
        list0.extend(&long_term);
        let mut list1 = self.expand(&order1, structure, Marking::ShortTerm);
        list1.extend(&long_term);

        if list1.len() > 1 && list1 == list0 {
            list1.swap(0, 1);
        }

        [list0, list1]
    }

    fn expand(&self, frames: &[usize], structure: Structure, marking: Marking) -> Vec<ListEntry> {
        if !structure.is_field() {
            return frames
                .iter()
                .map(|&index| ListEntry {
                    index,
                    structure: Structure::Frame,
                })
                .collect();
        }

        let fields_of = |wanted: Structure| -> Vec<ListEntry> {
            frames
                .iter()
                .copied()
                .filter(|&index| self.references[index].marking_of(wanted) == marking)
                .map(|index| ListEntry {
                    index,
                    structure: wanted,
                })
                .collect()
        };

        let mut same = fields_of(structure).into_iter();
        let mut opposite = fields_of(structure.opposite()).into_iter();
        let mut list = Vec::new();

        loop {
            match (same.next(), opposite.next()) {
                (Some(first), Some(second)) => list.extend([first, second]),
                (Some(only), None) => {
                    list.push(only);
                    list.extend(same);
                    break;
                }
                (None, Some(only)) => {
                    list.push(only);
                    list.extend(opposite);
                    break;
                }
                (None, None) => break,
            }
        }

        list
    }

    fn find_short_term(&self, pic_num: i64, current: Structure) -> Option<ListEntry> {
        self.find(current, |reference, entry| {
            reference.marking_of(entry) == Marking::ShortTerm
                && reference.short_pic_num(entry, current) == pic_num
        })
    }

    fn find_long_term(&self, pic_num: i64, current: Structure) -> Option<ListEntry> {
        self.find(current, |reference, entry| {
            reference.marking_of(entry) == Marking::LongTerm
                && reference.long_pic_num(entry, current) == pic_num
        })
    }

    fn find(
        &self,
        current: Structure,
        matches: impl Fn(&Reference, Structure) -> bool,
    ) -> Option<ListEntry> {
        self.references
            .iter()
            .enumerate()
            .flat_map(|(index, reference)| {
                current
                    .pictures()
                    .iter()
                    .map(move |&structure| (index, reference, structure))
            })
            .find(|&(_, reference, structure)| matches(reference, structure))
            .map(|(index, _, structure)| ListEntry { index, structure })
    }

    fn modify_list(
        &self,
        initial: Vec<ListEntry>,
        active: usize,
        modified: bool,
        modifications: &[RefPicListModification],
        numbering: &Numbering,
        max_frame_num: u32,
    ) -> ReferenceList {
        let mut list: ReferenceList = initial.into_iter().map(Some).collect();
        list.resize(active, None);

        if !modified {
            return list;
        }

        let current_pic_num = numbering.current_pic_num();
        let max_pic_num = numbering.max_pic_num(max_frame_num);
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
                    self.find_short_term(pic_num, numbering.structure)
                }
                2 => self.find_long_term(
                    i64::from(modification.long_term_pic_num),
                    numbering.structure,
                ),
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

    pub(super) fn finish(&mut self, picture: &Decoded<'_>) {
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
                poc_lsb: match picture.structure {
                    Structure::Frame => order.top.unwrap_or(0) - order.poc(),
                    Structure::Top | Structure::Bottom => 0,
                },
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
            structure,
            second_field,
            partner_poc,
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
            let numbering = Numbering {
                structure,
                frame_num,
            };
            for operation in &marking.inner {
                match self.apply_operation(operation, &numbering, surface.id) {
                    Marked::Nothing => {}
                    Marked::AllUnused => had_mmco5 = true,
                    Marked::CurrentLongTerm(index) => current_long_term = Some(index),
                }
            }
            self.references.retain(Reference::is_referenced);
        } else {
            let first_field_is_short_term = second_field
                && self
                    .find_surface(surface.id)
                    .is_some_and(|entry| entry.has(Marking::ShortTerm));
            if !first_field_is_short_term {
                self.slide_window(max_num_ref_frames);
            }
        }

        let shift = if had_mmco5 { order.poc() } else { 0 };
        let marking = if current_long_term.is_some() {
            Marking::LongTerm
        } else {
            Marking::ShortTerm
        };
        let own = |count: Option<i32>, parity: usize| FieldState {
            poc: count.map(|count| count - shift),
            marking: if structure.parities().contains(&parity) {
                marking
            } else {
                Marking::Unused
            },
        };

        if let Some(entry) = self
            .references
            .iter_mut()
            .find(|entry| entry.surface.id == surface.id)
        {
            let parity = structure.parity().unwrap_or(0);
            let own_state = own(if parity == 0 { order.top } else { order.bottom }, parity);
            entry.fields[parity] = own_state;
            if let Some(index) = current_long_term {
                entry.long_term_frame_idx = index;
            }
        } else {
            if self.references.len() >= MAX_REFERENCE_FRAMES {
                self.references.remove(0);
            }

            let mut fields = [own(order.top, 0), own(order.bottom, 1)];
            if let Some(parity) = structure.parity() {
                fields[1 - parity] = FieldState {
                    poc: partner_poc,
                    marking: Marking::Unused,
                };
            }
            self.references.push(Reference {
                surface: surface.clone(),
                frame_num: if had_mmco5 { 0 } else { frame_num },
                frame_num_wrap: 0,
                long_term_frame_idx: current_long_term.unwrap_or(0),
                fields,
            });
        }

        had_mmco5
    }

    fn apply_operation(
        &mut self,
        operation: &RefPicMarkingInner,
        context: &Numbering,
        current: SurfaceId,
    ) -> Marked {
        let pic_num_x =
            context.current_pic_num() - (i64::from(operation.difference_of_pic_nums_minus1) + 1);

        match operation.memory_management_control_operation {
            1 => {
                if let Some(entry) = self.find_short_term(pic_num_x, context.structure) {
                    self.references[entry.index].unmark(entry.structure, Marking::ShortTerm);
                }
            }
            2 => {
                let target = i64::from(operation.long_term_pic_num);
                if let Some(entry) = self.find_long_term(target, context.structure) {
                    self.references[entry.index].unmark(entry.structure, Marking::LongTerm);
                }
            }
            3 => {
                let index = operation.long_term_frame_idx;
                if let Some(entry) = self.find_short_term(pic_num_x, context.structure) {
                    self.free_long_term_index(index, Some(entry.index));
                    let reference = &mut self.references[entry.index];
                    reference.mark(entry.structure, Marking::LongTerm);
                    reference.long_term_frame_idx = index;
                }
            }
            4 => {
                self.max_long_term_frame_idx = match operation.max_long_term_frame_idx {
                    MaxLongTermFrameIdx::NoLongTermFrameIndices => None,
                    MaxLongTermFrameIdx::Idx(index) => Some(index),
                };
                let limit = self.max_long_term_frame_idx;
                for reference in &mut self.references {
                    let allowed = limit.is_some_and(|limit| reference.long_term_frame_idx <= limit);
                    if !allowed {
                        reference.unmark(Structure::Frame, Marking::LongTerm);
                    }
                }
            }
            5 => {
                self.references.clear();
                self.max_long_term_frame_idx = None;
                return Marked::AllUnused;
            }
            6 => {
                let index = operation.long_term_frame_idx;
                let current = self
                    .references
                    .iter()
                    .position(|reference| reference.surface.id == current);
                self.free_long_term_index(index, current);
                return Marked::CurrentLongTerm(index);
            }
            _ => {}
        }

        Marked::Nothing
    }

    fn free_long_term_index(&mut self, index: u32, keep: Option<usize>) {
        for (position, reference) in self.references.iter_mut().enumerate() {
            if Some(position) != keep && reference.long_term_frame_idx == index {
                reference.unmark(Structure::Frame, Marking::LongTerm);
            }
        }
    }

    fn slide_window(&mut self, max_num_ref_frames: u8) {
        let limit = usize::from(max_num_ref_frames).max(1);

        while self
            .references
            .iter()
            .filter(|reference| reference.is_referenced())
            .count()
            >= limit
        {
            let oldest = self
                .references
                .iter()
                .enumerate()
                .filter(|(_, reference)| reference.has(Marking::ShortTerm))
                .min_by_key(|(_, reference)| reference.frame_num_wrap)
                .map(|(index, _)| index);
            match oldest {
                Some(index) => {
                    self.references[index].unmark(Structure::Frame, Marking::ShortTerm);
                    self.references.retain(Reference::is_referenced);
                }
                None => break,
            }
        }
    }
}

struct Numbering {
    structure: Structure,
    frame_num: u32,
}

impl Numbering {
    fn current_pic_num(&self) -> i64 {
        if self.structure.is_field() {
            2 * i64::from(self.frame_num) + 1
        } else {
            i64::from(self.frame_num)
        }
    }

    fn max_pic_num(&self, max_frame_num: u32) -> i64 {
        if self.structure.is_field() {
            2 * i64::from(max_frame_num)
        } else {
            i64::from(max_frame_num)
        }
    }
}

enum Marked {
    Nothing,
    AllUnused,
    CurrentLongTerm(u32),
}
