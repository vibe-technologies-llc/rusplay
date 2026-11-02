use std::rc::Rc;

use cros_codecs::codec::h264::parser::{
    Level, MaxLongTermFrameIdx, Profile, RefPicListModification, RefPicMarking, RefPicMarkingInner,
    SliceHeader, SliceType, Sps, SpsBuilder,
};

use super::*;

const MAX_FRAME_NUM: u32 = 16;

fn sps(pic_order_cnt_type: u8) -> Sps {
    let built = SpsBuilder::new()
        .profile_idc(Profile::High)
        .level_idc(Level::L4)
        .frame_mbs_only_flag(true)
        .resolution(64, 64)
        .max_frame_num(MAX_FRAME_NUM)
        .pic_order_cnt_type(pic_order_cnt_type)
        .build();

    Rc::into_inner(built).unwrap()
}

fn operation(kind: u8) -> RefPicMarkingInner {
    RefPicMarkingInner {
        memory_management_control_operation: kind,
        ..RefPicMarkingInner::default()
    }
}

fn adaptive(inner: Vec<RefPicMarkingInner>) -> RefPicMarking {
    RefPicMarking {
        adaptive_ref_pic_marking_mode_flag: true,
        inner,
        ..RefPicMarking::default()
    }
}

fn picture(marking: &RefPicMarking, id: u32, frame_num: u32, poc: i32) -> Decoded<'_> {
    Decoded {
        marking,
        idr: false,
        nal_ref_idc: 1,
        max_num_ref_frames: 4,
        surface: PoolSurface::detached(id),
        structure: Structure::Frame,
        second_field: false,
        partner_poc: None,
        frame_num,
        pic_order_cnt_lsb: poc,
        order: OrderCount::of(Structure::Frame, poc, poc, 0, 0),
    }
}

fn store(dpb: &mut Dpb, decoded: &Decoded<'_>) {
    dpb.update_frame_num_wrap(decoded.frame_num, MAX_FRAME_NUM);
    dpb.finish(decoded);
}

fn store_plain(dpb: &mut Dpb, id: u32, frame_num: u32, poc: i32) {
    let marking = RefPicMarking::default();
    store(dpb, &picture(&marking, id, frame_num, poc));
}

fn idr(dpb: &mut Dpb, id: u32) {
    let marking = RefPicMarking::default();
    let decoded = Decoded {
        idr: true,
        ..picture(&marking, id, 0, 0)
    };

    store(dpb, &decoded);
}

fn reference(id: u32, frame_num: u32, wrap: i32, marking: Marking, poc: i32) -> Reference {
    Reference {
        surface: PoolSurface::detached(id),
        frame_num,
        frame_num_wrap: wrap,
        long_term_frame_idx: 0,
        fields: [FieldState {
            poc: Some(poc),
            marking,
        }; 2],
    }
}

fn long_term(id: u32, index: u32) -> Reference {
    Reference {
        long_term_frame_idx: index,
        ..reference(id, 0, 0, Marking::LongTerm, 0)
    }
}

fn dpb_of(references: Vec<Reference>) -> Dpb {
    Dpb {
        references,
        ..Dpb::default()
    }
}

fn header(slice_type: SliceType, active_l0: u8) -> SliceHeader {
    SliceHeader {
        slice_type,
        num_ref_idx_l0_active_minus1: active_l0 - 1,
        num_ref_idx_l1_active_minus1: active_l0 - 1,
        ..SliceHeader::default()
    }
}

fn modification(idc: u8, abs_diff_minus1: u32, long_term_pic_num: u32) -> RefPicListModification {
    RefPicListModification {
        modification_of_pic_nums_idc: idc,
        abs_diff_pic_num_minus1: abs_diff_minus1,
        long_term_pic_num,
        ..RefPicListModification::default()
    }
}

#[allow(clippy::unnecessary_wraps)]
fn frame(index: usize) -> Option<ListEntry> {
    Some(ListEntry {
        index,
        structure: Structure::Frame,
    })
}

fn surfaces(dpb: &Dpb) -> Vec<u32> {
    dpb.references
        .iter()
        .map(|reference| reference.surface.id)
        .collect()
}

fn frame_nums(dpb: &Dpb) -> Vec<u32> {
    dpb.references
        .iter()
        .map(|reference| reference.frame_num)
        .collect()
}

fn marking_of(dpb: &Dpb, id: u32) -> Marking {
    dpb.find_surface(id).unwrap().marking_of(Structure::Frame)
}

fn lists(dpb: &Dpb, header: &SliceHeader, poc: i32, frame_num: u32) -> [ReferenceList; 2] {
    dpb.reference_lists(header, Structure::Frame, poc, frame_num, MAX_FRAME_NUM)
}

fn three_frames() -> Dpb {
    let mut dpb = Dpb::default();

    idr(&mut dpb, 100);
    store_plain(&mut dpb, 101, 1, 2);
    store_plain(&mut dpb, 102, 2, 4);

    dpb
}

#[test]
fn an_idr_picture_replaces_every_reference() {
    let mut dpb = three_frames();

    idr(&mut dpb, 200);

    assert_eq!(surfaces(&dpb), [200]);
    assert_eq!(marking_of(&dpb, 200), Marking::ShortTerm);
}

#[test]
fn a_long_term_idr_takes_long_term_index_zero() {
    let mut dpb = Dpb::default();
    let marking = RefPicMarking {
        long_term_reference_flag: true,
        ..RefPicMarking::default()
    };
    let decoded = Decoded {
        idr: true,
        ..picture(&marking, 7, 0, 0)
    };

    store(&mut dpb, &decoded);

    assert_eq!(marking_of(&dpb, 7), Marking::LongTerm);
    assert_eq!(dpb.max_long_term_frame_idx, Some(0));
}

#[test]
fn a_non_reference_picture_is_not_stored() {
    let mut dpb = three_frames();
    let marking = RefPicMarking::default();
    let decoded = Decoded {
        nal_ref_idc: 0,
        ..picture(&marking, 9, 3, 6)
    };

    store(&mut dpb, &decoded);

    assert_eq!(surfaces(&dpb), [100, 101, 102]);
}

#[test]
fn the_sliding_window_drops_the_oldest_short_term_frame() {
    let mut dpb = Dpb::default();
    let marking = RefPicMarking::default();

    for (id, frame_num) in [(100, 0), (101, 1), (102, 2), (103, 3)] {
        let decoded = Decoded {
            idr: frame_num == 0,
            max_num_ref_frames: 2,
            ..picture(&marking, id, frame_num, 0)
        };
        store(&mut dpb, &decoded);
    }

    assert_eq!(surfaces(&dpb), [102, 103]);
}

#[test]
fn the_sliding_window_never_drops_long_term_frames() {
    let mut dpb = dpb_of(vec![
        long_term(1, 0),
        reference(2, 1, 1, Marking::ShortTerm, 2),
    ]);
    let marking = RefPicMarking::default();
    let decoded = Decoded {
        max_num_ref_frames: 2,
        ..picture(&marking, 3, 2, 4)
    };

    store(&mut dpb, &decoded);

    assert_eq!(surfaces(&dpb), [1, 3]);
    assert_eq!(marking_of(&dpb, 1), Marking::LongTerm);
}

#[test]
fn frame_num_wrap_goes_negative_once_the_counter_wraps() {
    let mut dpb = Dpb::default();

    idr(&mut dpb, 100);
    store_plain(&mut dpb, 101, 14, 2);
    store_plain(&mut dpb, 102, 15, 4);
    dpb.update_frame_num_wrap(1, MAX_FRAME_NUM);

    let wraps: Vec<i32> = dpb
        .references
        .iter()
        .map(|reference| reference.frame_num_wrap)
        .collect();

    assert_eq!(frame_nums(&dpb), [0, 14, 15]);
    assert_eq!(wraps, [0, -2, -1]);
}

#[test]
fn mmco_1_marks_a_short_term_frame_unused() {
    let mut dpb = three_frames();
    let marking = adaptive(vec![RefPicMarkingInner {
        difference_of_pic_nums_minus1: 1,
        ..operation(1)
    }]);

    store(&mut dpb, &picture(&marking, 103, 3, 6));

    assert_eq!(surfaces(&dpb), [100, 102, 103]);
}

#[test]
fn mmco_2_marks_a_long_term_frame_unused() {
    let mut dpb = dpb_of(vec![long_term(1, 0), long_term(2, 1)]);
    let marking = adaptive(vec![RefPicMarkingInner {
        long_term_pic_num: 1,
        ..operation(2)
    }]);

    store(&mut dpb, &picture(&marking, 3, 1, 2));

    assert_eq!(surfaces(&dpb), [1, 3]);
}

#[test]
fn mmco_3_turns_a_short_term_frame_into_a_long_term_one() {
    let mut dpb = three_frames();
    let marking = adaptive(vec![RefPicMarkingInner {
        difference_of_pic_nums_minus1: 0,
        long_term_frame_idx: 2,
        ..operation(3)
    }]);

    store(&mut dpb, &picture(&marking, 103, 3, 6));

    let converted = dpb.find_surface(102).unwrap();
    assert_eq!(converted.marking_of(Structure::Frame), Marking::LongTerm);
    assert_eq!(converted.long_term_frame_idx, 2);
    assert_eq!(marking_of(&dpb, 101), Marking::ShortTerm);
}

#[test]
fn mmco_3_evicts_the_frame_that_held_the_long_term_index() {
    let mut dpb = dpb_of(vec![
        long_term(1, 2),
        reference(2, 1, 1, Marking::ShortTerm, 2),
    ]);
    let marking = adaptive(vec![RefPicMarkingInner {
        difference_of_pic_nums_minus1: 0,
        long_term_frame_idx: 2,
        ..operation(3)
    }]);

    store(&mut dpb, &picture(&marking, 3, 2, 4));

    assert_eq!(surfaces(&dpb), [2, 3]);
    assert_eq!(marking_of(&dpb, 2), Marking::LongTerm);
}

#[test]
fn mmco_4_drops_long_term_frames_above_the_new_maximum() {
    let mut dpb = dpb_of(vec![long_term(1, 0), long_term(2, 1), long_term(3, 2)]);
    let marking = adaptive(vec![RefPicMarkingInner {
        max_long_term_frame_idx: MaxLongTermFrameIdx::Idx(0),
        ..operation(4)
    }]);

    store(&mut dpb, &picture(&marking, 4, 1, 2));

    assert_eq!(surfaces(&dpb), [1, 4]);
    assert_eq!(dpb.max_long_term_frame_idx, Some(0));
}

#[test]
fn mmco_4_without_indices_drops_every_long_term_frame() {
    let mut dpb = dpb_of(vec![long_term(1, 0), long_term(2, 1)]);
    let marking = adaptive(vec![RefPicMarkingInner {
        max_long_term_frame_idx: MaxLongTermFrameIdx::NoLongTermFrameIndices,
        ..operation(4)
    }]);

    store(&mut dpb, &picture(&marking, 3, 1, 2));

    assert_eq!(surfaces(&dpb), [3]);
    assert_eq!(dpb.max_long_term_frame_idx, None);
}

#[test]
fn mmco_5_clears_the_buffer_and_restarts_the_counters() {
    let mut dpb = three_frames();
    let marking = adaptive(vec![operation(5)]);

    store(&mut dpb, &picture(&marking, 103, 3, 10));

    let current = dpb.find_surface(103).unwrap();
    assert_eq!(surfaces(&dpb), [103]);
    assert_eq!(current.frame_num, 0);
    assert_eq!(current.fields[0].poc, Some(0));
    assert!(dpb.previous.had_mmco5);
}

#[test]
fn mmco_6_marks_the_current_picture_long_term() {
    let mut dpb = three_frames();
    let marking = adaptive(vec![RefPicMarkingInner {
        long_term_frame_idx: 3,
        ..operation(6)
    }]);

    store(&mut dpb, &picture(&marking, 103, 3, 6));

    let current = dpb.find_surface(103).unwrap();
    assert_eq!(current.marking_of(Structure::Frame), Marking::LongTerm);
    assert_eq!(current.long_term_frame_idx, 3);
    assert_eq!(surfaces(&dpb), [100, 101, 102, 103]);
}

#[test]
fn an_unknown_operation_changes_nothing() {
    let mut dpb = three_frames();
    let marking = adaptive(vec![operation(9)]);

    store(&mut dpb, &picture(&marking, 103, 3, 6));

    assert_eq!(surfaces(&dpb), [100, 101, 102, 103]);
}

#[test]
fn the_buffer_never_holds_more_than_sixteen_frames() {
    let mut dpb = Dpb::default();
    let marking = adaptive(Vec::new());

    for id in 0..20 {
        let decoded = Decoded {
            max_num_ref_frames: 16,
            ..picture(&marking, id, id % MAX_FRAME_NUM, 0)
        };
        store(&mut dpb, &decoded);
    }

    assert_eq!(dpb.references.len(), MAX_REFERENCE_FRAMES);
    assert_eq!(dpb.references[0].surface.id, 4);
}

#[test]
fn the_initial_p_list_runs_from_the_newest_short_term_frame_then_long_term_by_index() {
    let dpb = dpb_of(vec![
        long_term(1, 1),
        reference(2, 1, 1, Marking::ShortTerm, 2),
        long_term(3, 0),
        reference(4, 3, 3, Marking::ShortTerm, 6),
    ]);

    let [list0, list1] = lists(&dpb, &header(SliceType::P, 4), 8, 4);

    assert_eq!(list0, [frame(3), frame(1), frame(2), frame(0)]);
    assert_eq!(list1.len(), 0);
}

#[test]
fn an_active_count_beyond_the_candidates_pads_with_empty_entries() {
    let dpb = dpb_of(vec![reference(1, 1, 1, Marking::ShortTerm, 2)]);

    let [list0, _] = lists(&dpb, &header(SliceType::P, 3), 4, 2);

    assert_eq!(list0, [frame(0), None, None]);
}

#[test]
fn an_active_count_below_the_candidates_truncates() {
    let dpb = dpb_of(vec![
        reference(1, 1, 1, Marking::ShortTerm, 2),
        reference(2, 2, 2, Marking::ShortTerm, 4),
    ]);

    let [list0, _] = lists(&dpb, &header(SliceType::P, 1), 6, 3);

    assert_eq!(list0, [frame(1)]);
}

#[test]
fn an_intra_slice_has_no_reference_lists() {
    let dpb = three_frames();

    let [list0, list1] = lists(&dpb, &header(SliceType::I, 2), 6, 3);

    assert_eq!(list0.len(), 0);
    assert_eq!(list1.len(), 0);
}

fn b_references() -> Dpb {
    dpb_of(vec![
        reference(1, 1, 1, Marking::ShortTerm, 4),
        reference(2, 2, 2, Marking::ShortTerm, 6),
        reference(3, 3, 3, Marking::ShortTerm, 10),
        reference(4, 4, 4, Marking::ShortTerm, 12),
        long_term(5, 0),
    ])
}

#[test]
fn the_initial_b_lists_order_by_distance_around_the_current_order_count() {
    let dpb = b_references();

    let [list0, list1] = lists(&dpb, &header(SliceType::B, 5), 8, 5);

    assert_eq!(list0, [frame(1), frame(0), frame(2), frame(3), frame(4)]);
    assert_eq!(list1, [frame(2), frame(3), frame(1), frame(0), frame(4)]);
}

#[test]
fn identical_b_lists_swap_their_first_two_entries() {
    let dpb = dpb_of(vec![
        reference(1, 1, 1, Marking::ShortTerm, 4),
        reference(2, 2, 2, Marking::ShortTerm, 6),
    ]);

    let [list0, list1] = lists(&dpb, &header(SliceType::B, 2), 8, 3);

    assert_eq!(list0, [frame(1), frame(0)]);
    assert_eq!(list1, [frame(0), frame(1)]);
}

#[test]
fn a_single_reference_b_list_is_not_swapped() {
    let dpb = dpb_of(vec![reference(1, 1, 1, Marking::ShortTerm, 4)]);

    let [list0, list1] = lists(&dpb, &header(SliceType::B, 1), 8, 2);

    assert_eq!(list0, [frame(0)]);
    assert_eq!(list1, [frame(0)]);
}

fn consecutive_frames() -> Dpb {
    dpb_of(vec![
        reference(1, 1, 1, Marking::ShortTerm, 2),
        reference(2, 2, 2, Marking::ShortTerm, 4),
        reference(3, 3, 3, Marking::ShortTerm, 6),
    ])
}

#[test]
fn a_subtract_modification_moves_the_named_frame_to_the_front() {
    let dpb = consecutive_frames();
    let mut slice = header(SliceType::P, 3);
    slice.ref_pic_list_modification_flag_l0 = true;
    slice.ref_pic_list_modification_l0 = vec![modification(0, 2, 0)];

    let [list0, _] = lists(&dpb, &slice, 8, 4);

    assert_eq!(list0, [frame(0), frame(2), frame(1)]);
}

#[test]
fn successive_modifications_continue_from_the_predicted_number() {
    let dpb = consecutive_frames();
    let mut slice = header(SliceType::P, 3);
    slice.ref_pic_list_modification_flag_l0 = true;
    slice.ref_pic_list_modification_l0 = vec![modification(0, 2, 0), modification(1, 0, 0)];

    let [list0, _] = lists(&dpb, &slice, 8, 4);

    assert_eq!(list0, [frame(0), frame(1), frame(2)]);
}

#[test]
fn a_modification_wraps_below_zero_into_the_previous_frame_num_cycle() {
    let dpb = dpb_of(vec![
        reference(1, 15, -1, Marking::ShortTerm, 2),
        reference(2, 0, 0, Marking::ShortTerm, 4),
    ]);
    let mut slice = header(SliceType::P, 2);
    slice.ref_pic_list_modification_flag_l0 = true;
    slice.ref_pic_list_modification_l0 = vec![modification(0, 1, 0)];

    let [list0, _] = lists(&dpb, &slice, 8, 1);

    assert_eq!(list0, [frame(0), frame(1)]);
}

#[test]
fn a_long_term_modification_selects_by_long_term_pic_num() {
    let dpb = dpb_of(vec![
        reference(1, 1, 1, Marking::ShortTerm, 2),
        long_term(2, 0),
        long_term(3, 1),
    ]);
    let mut slice = header(SliceType::P, 3);
    slice.ref_pic_list_modification_flag_l0 = true;
    slice.ref_pic_list_modification_l0 = vec![modification(2, 0, 1)];

    let [list0, _] = lists(&dpb, &slice, 8, 2);

    assert_eq!(list0, [frame(2), frame(0), frame(1)]);
}

#[test]
fn a_modification_naming_a_missing_frame_leaves_an_empty_entry() {
    let dpb = consecutive_frames();
    let mut slice = header(SliceType::P, 3);
    slice.ref_pic_list_modification_flag_l0 = true;
    slice.ref_pic_list_modification_l0 = vec![modification(0, 5, 0)];

    let [list0, _] = lists(&dpb, &slice, 8, 4);

    assert_eq!(list0, [None, frame(2), frame(1)]);
}

#[test]
fn modification_stops_at_the_end_marker() {
    let dpb = consecutive_frames();
    let mut slice = header(SliceType::P, 3);
    slice.ref_pic_list_modification_flag_l0 = true;
    slice.ref_pic_list_modification_l0 = vec![modification(3, 0, 0), modification(0, 2, 0)];

    let [list0, _] = lists(&dpb, &slice, 8, 4);

    assert_eq!(list0, [frame(2), frame(1), frame(0)]);
}

#[test]
fn field_lists_alternate_parity_starting_with_the_current_one() {
    let dpb = dpb_of(vec![
        reference(1, 1, 1, Marking::ShortTerm, 2),
        reference(2, 2, 2, Marking::ShortTerm, 4),
    ]);
    let field = |index, structure| Some(ListEntry { index, structure });

    let [list0, _] = dpb.reference_lists(
        &header(SliceType::P, 4),
        Structure::Top,
        6,
        3,
        MAX_FRAME_NUM,
    );

    assert_eq!(
        list0,
        [
            field(1, Structure::Top),
            field(1, Structure::Bottom),
            field(0, Structure::Top),
            field(0, Structure::Bottom),
        ]
    );
}

#[test]
fn field_lists_fall_back_to_the_remaining_parity_when_one_runs_out() {
    let mut half = reference(2, 2, 2, Marking::ShortTerm, 4);
    half.fields[1].marking = Marking::Unused;
    let dpb = dpb_of(vec![reference(1, 1, 1, Marking::ShortTerm, 2), half]);

    let [list0, _] = dpb.reference_lists(
        &header(SliceType::P, 3),
        Structure::Bottom,
        6,
        3,
        MAX_FRAME_NUM,
    );

    assert_eq!(
        list0,
        [
            Some(ListEntry {
                index: 0,
                structure: Structure::Bottom
            }),
            Some(ListEntry {
                index: 1,
                structure: Structure::Top
            }),
            Some(ListEntry {
                index: 0,
                structure: Structure::Top
            }),
        ]
    );
}

#[test]
fn field_pic_nums_double_and_the_same_parity_is_odd() {
    let reference = reference(1, 3, 3, Marking::ShortTerm, 2);

    assert_eq!(reference.short_pic_num(Structure::Top, Structure::Frame), 3);
    assert_eq!(reference.short_pic_num(Structure::Top, Structure::Top), 7);
    assert_eq!(
        reference.short_pic_num(Structure::Bottom, Structure::Top),
        6
    );
}

#[test]
fn a_frame_with_one_unmarked_field_is_not_a_short_term_frame() {
    let mut half = reference(1, 1, 1, Marking::ShortTerm, 2);
    half.fields[1].marking = Marking::Unused;

    assert_eq!(half.marking_of(Structure::Frame), Marking::Unused);
    assert_eq!(half.marking_of(Structure::Top), Marking::ShortTerm);
    assert_eq!(half.marked_structure(), Structure::Top);
}

#[test]
fn va_pictures_carry_the_long_term_index_and_field_flags() {
    let long = long_term(5, 3);
    let short = reference(6, 9, 9, Marking::ShortTerm, 12);

    let long_picture = long.va_picture(Structure::Frame);
    let short_picture = short.va_picture(Structure::Top);

    assert_eq!(long_picture.picture_id, 5);
    assert_eq!(long_picture.frame_idx, 3);
    assert_eq!(long_picture.flags, ffi::VA_PICTURE_H264_LONG_TERM_REFERENCE);
    assert_eq!(short_picture.frame_idx, 9);
    assert_eq!(
        short_picture.flags,
        ffi::VA_PICTURE_H264_SHORT_TERM_REFERENCE | ffi::VA_PICTURE_H264_TOP_FIELD
    );
    assert_eq!(short_picture.TopFieldOrderCnt, 12);
}

fn order_of(
    dpb: &Dpb,
    sps: &Sps,
    frame_num: u16,
    lsb: u16,
    nal_ref_idc: u8,
    idr: bool,
) -> OrderCount {
    let slice = SliceHeader {
        frame_num,
        pic_order_cnt_lsb: lsb,
        ..SliceHeader::default()
    };

    dpb.order_count(sps, &slice, idr, nal_ref_idc, Structure::Frame)
}

fn follow(dpb: &mut Dpb, order: OrderCount, frame_num: u32, lsb: u16) {
    let marking = RefPicMarking::default();
    let decoded = Decoded {
        order,
        pic_order_cnt_lsb: i32::from(lsb),
        ..picture(&marking, frame_num, frame_num, 0)
    };

    store(dpb, &decoded);
}

#[test]
fn order_count_type_0_unwraps_the_least_significant_bits() {
    let sps = sps(0);
    let mut dpb = Dpb::default();

    let first = order_of(&dpb, &sps, 0, 0, 1, true);
    follow(&mut dpb, first, 0, 0);
    let second = order_of(&dpb, &sps, 1, 6, 1, false);
    follow(&mut dpb, second, 1, 6);
    let third = order_of(&dpb, &sps, 2, 12, 1, false);
    follow(&mut dpb, third, 2, 12);
    let fourth = order_of(&dpb, &sps, 3, 2, 1, false);

    assert_eq!(first.poc(), 0);
    assert_eq!(second.poc(), 6);
    assert_eq!(third.poc(), 12);
    assert_eq!(fourth.poc(), 18);
}

#[test]
fn order_count_type_0_unwraps_backwards_too() {
    let sps = sps(0);
    let mut dpb = Dpb::default();

    let first = order_of(&dpb, &sps, 0, 0, 1, true);
    follow(&mut dpb, first, 0, 0);
    let behind = order_of(&dpb, &sps, 1, 14, 1, false);

    assert_eq!(behind.poc(), -2);
}

#[test]
fn a_non_reference_picture_does_not_advance_the_order_count_reference() {
    let sps = sps(0);
    let mut dpb = Dpb::default();

    let first = order_of(&dpb, &sps, 0, 0, 1, true);
    follow(&mut dpb, first, 0, 0);
    let skipped = order_of(&dpb, &sps, 1, 14, 0, false);
    let marking = RefPicMarking::default();
    store(
        &mut dpb,
        &Decoded {
            nal_ref_idc: 0,
            order: skipped,
            pic_order_cnt_lsb: 14,
            ..picture(&marking, 1, 1, 0)
        },
    );
    let next = order_of(&dpb, &sps, 2, 2, 1, false);

    assert_eq!(next.poc(), 2);
}

#[test]
fn order_count_type_0_adds_the_bottom_field_delta_to_frames() {
    let sps = sps(0);
    let dpb = Dpb::default();
    let slice = SliceHeader {
        pic_order_cnt_lsb: 4,
        delta_pic_order_cnt_bottom: 1,
        ..SliceHeader::default()
    };

    let frame = dpb.order_count(&sps, &slice, true, 1, Structure::Frame);
    let bottom = dpb.order_count(&sps, &slice, true, 1, Structure::Bottom);

    assert_eq!((frame.top, frame.bottom), (Some(4), Some(5)));
    assert_eq!((bottom.top, bottom.bottom), (None, Some(4)));
}

#[test]
fn order_count_type_2_follows_decoding_order_across_frame_num_wraps() {
    let sps = sps(2);
    let mut dpb = Dpb::default();

    let first = order_of(&dpb, &sps, 15, 0, 1, true);
    follow(&mut dpb, first, 15, 0);
    let wrapped = order_of(&dpb, &sps, 0, 0, 1, false);
    let non_reference = order_of(&dpb, &sps, 0, 0, 0, false);

    assert_eq!(first.poc(), 0);
    assert_eq!(wrapped.poc(), 32);
    assert_eq!(non_reference.poc(), 31);
}

#[test]
fn order_count_type_1_walks_the_expected_cycle() {
    let mut sps = sps(1);
    sps.num_ref_frames_in_pic_order_cnt_cycle = 1;
    sps.offset_for_ref_frame[0] = 2;
    sps.expected_delta_per_pic_order_cnt_cycle = 2;
    sps.offset_for_non_ref_pic = -1;
    let dpb = Dpb::default();

    let reference = order_of(&dpb, &sps, 3, 0, 1, false);
    let non_reference = order_of(&dpb, &sps, 3, 0, 0, false);

    assert_eq!(reference.poc(), 6);
    assert_eq!(non_reference.poc(), 3);
}

#[test]
fn order_count_type_1_with_an_empty_cycle_starts_from_zero() {
    let sps = sps(1);
    let dpb = Dpb::default();

    let order = order_of(&dpb, &sps, 5, 0, 1, false);

    assert_eq!(order.poc(), 0);
}

#[test]
fn an_mmco_5_picture_restarts_the_frame_num_offset() {
    let sps = sps(2);
    let mut dpb = three_frames();
    let marking = adaptive(vec![operation(5)]);

    store(&mut dpb, &picture(&marking, 103, 3, 6));
    let next = order_of(&dpb, &sps, 1, 0, 1, false);

    assert_eq!(next.poc(), 2);
}
