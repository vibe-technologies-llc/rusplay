use cros_codecs::codec::h265::parser::{ShortTermRefPicSet, SliceHeader, SliceType, Sps};
use rusplay_core::{BitDepth, ChromaSubsampling, PixelFormat};

use super::*;

const MAX_LSB: i32 = 16;

fn rps(negative: &[(i32, bool)], positive: &[(i32, bool)]) -> ShortTermRefPicSet {
    let mut set = ShortTermRefPicSet {
        num_negative_pics: u8::try_from(negative.len()).unwrap(),
        num_positive_pics: u8::try_from(positive.len()).unwrap(),
        ..ShortTermRefPicSet::default()
    };

    for (index, &(delta, used)) in negative.iter().enumerate() {
        set.delta_poc_s0[index] = delta;
        set.used_by_curr_pic_s0[index] = used;
    }
    for (index, &(delta, used)) in positive.iter().enumerate() {
        set.delta_poc_s1[index] = delta;
        set.used_by_curr_pic_s1[index] = used;
    }

    set
}

fn header(negative: &[(i32, bool)], positive: &[(i32, bool)]) -> SliceHeader {
    SliceHeader {
        short_term_ref_pic_set: rps(negative, positive),
        ..SliceHeader::default()
    }
}

fn long_term_entry(header: &mut SliceHeader, index: usize, lsb: u32, used: bool) {
    header.num_long_term_pics = u8::try_from(index + 1).unwrap();
    header.poc_lsb_lt[index] = lsb;
    header.used_by_curr_pic_lt[index] = used;
}

fn full_long_term_entry(header: &mut SliceHeader, index: usize, lsb: u32, cycle: u32, used: bool) {
    long_term_entry(header, index, lsb, used);
    header.delta_poc_msb_present_flag[index] = true;
    header.delta_poc_msb_cycle_lt[index] = cycle;
}

fn reference(id: u32, poc: i32) -> Reference {
    Reference {
        surface: PoolSurface::detached(id),
        poc,
        long_term: false,
    }
}

fn references(pocs: &[i32]) -> Vec<Reference> {
    (1..)
        .zip(pocs)
        .map(|(id, &poc)| reference(id, poc))
        .collect()
}

fn ids(references: &[Reference]) -> Vec<u32> {
    references
        .iter()
        .map(|reference| reference.surface.id)
        .collect()
}

fn some(indices: &[usize]) -> Vec<Option<usize>> {
    indices.iter().copied().map(Some).collect()
}

#[test]
fn the_order_count_follows_the_most_significant_bits_across_a_wrap() {
    assert_eq!(following_order_count(14, 2, MAX_LSB), 18);
    assert_eq!(following_order_count(18, 14, MAX_LSB), 14);
    assert_eq!(following_order_count(5, 7, MAX_LSB), 7);
    assert_eq!(following_order_count(7, 5, MAX_LSB), 5);
}

#[test]
fn the_order_count_wraps_exactly_at_half_the_lsb_range() {
    assert_eq!(following_order_count(8, 0, MAX_LSB), 16);
    assert_eq!(following_order_count(0, 8, MAX_LSB), 8);
    assert_eq!(following_order_count(0, 9, MAX_LSB), -7);
    assert_eq!(following_order_count(9, 0, MAX_LSB), 16);
}

#[test]
fn the_order_count_of_negative_pictures_wraps_toward_zero() {
    assert_eq!(following_order_count(-3, 1, MAX_LSB), 1);
    assert_eq!(following_order_count(-3, 11, MAX_LSB), -5);
    assert_eq!(following_order_count(-20, 2, MAX_LSB), -14);
}

#[test]
fn the_order_count_keeps_climbing_over_many_cycles() {
    let mut poc = 0;

    for step in 1..=100 {
        let lsb = u16::try_from((step * 5) % MAX_LSB).unwrap();
        poc = following_order_count(poc, lsb, MAX_LSB);

        assert_eq!(poc, step * 5);
    }
}

#[test]
fn short_term_entries_split_by_direction_and_use() {
    let header = header(&[(-1, true), (-3, false)], &[(2, true), (4, false)]);

    let counts = reference_order_counts(&Sps::default(), &header, 10, MAX_LSB);

    assert_eq!(counts.before, [9]);
    assert_eq!(counts.after, [12]);
    assert_eq!(counts.following, [7, 14]);
    assert_eq!(counts.long_term_current.len(), 0);
    assert_eq!(counts.long_term_following.len(), 0);
}

#[test]
fn the_sps_set_named_by_the_slice_replaces_the_inline_one() {
    let sps = Sps {
        short_term_ref_pic_set: vec![rps(&[(-1, true)], &[]), rps(&[(-2, true)], &[(3, true)])],
        ..Sps::default()
    };
    let mut header = header(&[(-9, true)], &[]);
    header.short_term_ref_pic_set_sps_flag = true;
    header.curr_rps_idx = 1;

    let counts = reference_order_counts(&sps, &header, 10, MAX_LSB);

    assert_eq!(counts.before, [8]);
    assert_eq!(counts.after, [13]);
}

#[test]
fn an_sps_set_index_beyond_the_list_falls_back_to_the_inline_set() {
    let sps = Sps::default();
    let mut header = header(&[(-4, true)], &[]);
    header.short_term_ref_pic_set_sps_flag = true;
    header.curr_rps_idx = 3;

    let counts = reference_order_counts(&sps, &header, 10, MAX_LSB);

    assert_eq!(counts.before, [6]);
}

#[test]
fn long_term_entries_without_the_msb_match_the_lsb_only() {
    let mut header = header(&[], &[]);
    long_term_entry(&mut header, 0, 5, true);

    let counts = reference_order_counts(&Sps::default(), &header, 100, MAX_LSB);

    assert_eq!(counts.long_term_current, [(5, false)]);
}

#[test]
fn long_term_entries_with_the_msb_cycle_name_the_full_order_count() {
    let mut header = header(&[], &[]);
    full_long_term_entry(&mut header, 0, 5, 2, true);
    full_long_term_entry(&mut header, 1, 9, 0, false);

    let counts = reference_order_counts(&Sps::default(), &header, 100, MAX_LSB);

    assert_eq!(counts.long_term_current, [(69, true)]);
    assert_eq!(counts.long_term_following, [(105, true)]);
}

#[test]
fn sps_and_slice_long_term_entries_are_both_counted() {
    let mut header = header(&[], &[]);
    header.num_long_term_sps = 1;
    header.num_long_term_pics = 1;
    header.poc_lsb_lt[0] = 3;
    header.poc_lsb_lt[1] = 4;
    header.used_by_curr_pic_lt[0] = true;
    header.used_by_curr_pic_lt[1] = false;

    let counts = reference_order_counts(&Sps::default(), &header, 100, MAX_LSB);

    assert_eq!(counts.long_term_current, [(3, false)]);
    assert_eq!(counts.long_term_following, [(4, false)]);
}

#[test]
fn retaining_references_remaps_the_kept_indices_in_order() {
    let mut held = references(&[8, 4, 0, 16]);

    let remap = retain_references(&mut held, &[3, 0, 1]);

    assert_eq!(ids(&held), [1, 2, 4]);
    assert_eq!(remap, [Some(0), Some(1), None, Some(2)]);
}

#[test]
fn retaining_nothing_empties_the_list() {
    let mut held = references(&[8, 4]);

    let remap = retain_references(&mut held, &[]);

    assert_eq!(held.len(), 0);
    assert_eq!(remap, [None, None]);
}

#[test]
fn the_reference_set_resolves_pictures_and_drops_the_unused() {
    let mut held = references(&[8, 4, 0, 16]);
    let header = header(&[(-2, true), (-6, false)], &[(6, true)]);

    let set = apply_reference_set(&mut held, &Sps::default(), &header, 10, MAX_LSB);

    assert_eq!(ids(&held), [1, 2, 4]);
    assert_eq!(set.before, some(&[0]));
    assert_eq!(set.after, some(&[2]));
    assert_eq!(set.long_term.len(), 0);
}

#[test]
fn a_missing_short_term_picture_leaves_an_empty_slot() {
    let mut held = references(&[8]);
    let header = header(&[(-2, true), (-5, true)], &[]);

    let set = apply_reference_set(&mut held, &Sps::default(), &header, 10, MAX_LSB);

    assert_eq!(set.before, [Some(0), None]);
    assert_eq!(ids(&held), [1]);
}

#[test]
fn a_long_term_lsb_match_marks_the_picture_long_term_and_keeps_it() {
    let mut held = references(&[8, 21, 4]);
    let mut header = header(&[(-6, true)], &[]);
    long_term_entry(&mut header, 0, 5, true);

    let set = apply_reference_set(&mut held, &Sps::default(), &header, 10, MAX_LSB);

    assert_eq!(ids(&held), [2, 3]);
    assert_eq!(set.before, some(&[1]));
    assert_eq!(set.long_term, some(&[0]));
    assert!(held[0].long_term);
    assert!(!held[1].long_term);
}

#[test]
fn a_long_term_full_order_count_matches_only_the_exact_picture() {
    let mut held = references(&[5, 21]);
    let mut header = header(&[], &[]);
    full_long_term_entry(&mut header, 0, 5, 0, true);

    let set = apply_reference_set(&mut held, &Sps::default(), &header, 21, MAX_LSB);

    assert_eq!(set.long_term, some(&[0]));
    assert_eq!(ids(&held), [2]);
}

#[test]
fn a_following_long_term_picture_is_kept_without_joining_the_set() {
    let mut held = references(&[5, 9]);
    let mut header = header(&[], &[]);
    long_term_entry(&mut header, 0, 5, false);

    let set = apply_reference_set(&mut held, &Sps::default(), &header, 10, MAX_LSB);

    assert_eq!(set.long_term.len(), 0);
    assert_eq!(ids(&held), [1]);
    assert!(held[0].long_term);
}

#[test]
fn a_long_term_picture_is_not_found_as_a_short_term_one() {
    let mut held = references(&[8]);
    held[0].long_term = true;
    let header = header(&[(-2, true)], &[]);

    let set = apply_reference_set(&mut held, &Sps::default(), &header, 10, MAX_LSB);

    assert_eq!(set.before, [None]);
    assert_eq!(held.len(), 0);
}

#[test]
fn an_empty_reference_set_clears_every_picture() {
    let mut held = references(&[8, 4]);

    let set = apply_reference_set(&mut held, &Sps::default(), &header(&[], &[]), 10, MAX_LSB);

    assert_eq!(held.len(), 0);
    assert_eq!(set.before.len(), 0);
    assert_eq!(set.after.len(), 0);
}

#[test]
fn reference_set_flags_name_the_list_each_picture_belongs_to() {
    let set = ReferenceSet {
        before: some(&[0]),
        after: some(&[1]),
        long_term: some(&[2]),
    };

    assert_eq!(set.flags(0), ffi::VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE);
    assert_eq!(set.flags(1), ffi::VA_PICTURE_HEVC_RPS_ST_CURR_AFTER);
    assert_eq!(set.flags(2), ffi::VA_PICTURE_HEVC_RPS_LT_CURR);
    assert_eq!(set.flags(3), 0);
}

#[test]
fn va_references_flag_each_picture_and_invalidate_the_unused_slots() {
    let mut held = references(&[8, 4, 21]);
    held[2].long_term = true;
    let set = ReferenceSet {
        before: some(&[0]),
        after: some(&[1]),
        long_term: some(&[2]),
    };

    let frames = va_references(&held, &set);

    assert_eq!(frames[0].picture_id, 1);
    assert_eq!(frames[0].pic_order_cnt, 8);
    assert_eq!(frames[0].flags, ffi::VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE);
    assert_eq!(frames[1].flags, ffi::VA_PICTURE_HEVC_RPS_ST_CURR_AFTER);
    assert_eq!(
        frames[2].flags,
        ffi::VA_PICTURE_HEVC_RPS_LT_CURR | ffi::VA_PICTURE_HEVC_LONG_TERM_REFERENCE
    );
    for unused in &frames[3..] {
        assert_eq!(unused.picture_id, INVALID_SURFACE);
        assert_eq!(unused.flags, ffi::VA_PICTURE_HEVC_INVALID);
    }
}

fn slice(slice_type: SliceType, active_l0: u8, active_l1: u8, total: u32) -> SliceHeader {
    SliceHeader {
        type_: slice_type,
        num_ref_idx_l0_active_minus1: active_l0 - 1,
        num_ref_idx_l1_active_minus1: active_l1 - 1,
        num_pic_total_curr: total,
        ..SliceHeader::default()
    }
}

fn four_way_set() -> ReferenceSet {
    ReferenceSet {
        before: some(&[0, 1]),
        after: some(&[2]),
        long_term: some(&[3]),
    }
}

#[test]
fn an_intra_slice_has_no_lists() {
    let [list0, list1] = reference_lists(&slice(SliceType::I, 1, 1, 4), &four_way_set());

    assert_eq!(list0.len(), 0);
    assert_eq!(list1.len(), 0);
}

#[test]
fn a_p_slice_lists_before_then_after_then_long_term() {
    let [list0, list1] = reference_lists(&slice(SliceType::P, 4, 1, 4), &four_way_set());

    assert_eq!(list0, some(&[0, 1, 2, 3]));
    assert_eq!(list1.len(), 0);
}

#[test]
fn a_b_slice_puts_the_following_pictures_first_in_list_one() {
    let [list0, list1] = reference_lists(&slice(SliceType::B, 4, 4, 4), &four_way_set());

    assert_eq!(list0, some(&[0, 1, 2, 3]));
    assert_eq!(list1, some(&[2, 0, 1, 3]));
}

#[test]
fn an_active_count_above_the_candidates_cycles_through_them() {
    let [list0, _] = reference_lists(&slice(SliceType::P, 6, 1, 4), &four_way_set());

    assert_eq!(list0, some(&[0, 1, 2, 3, 0, 1]));
}

#[test]
fn an_active_count_below_the_candidates_truncates() {
    let [list0, list1] = reference_lists(&slice(SliceType::B, 2, 1, 4), &four_way_set());

    assert_eq!(list0, some(&[0, 1]));
    assert_eq!(list1, some(&[2]));
}

#[test]
fn list_modification_picks_entries_of_the_temporary_list() {
    let mut header = slice(SliceType::B, 3, 3, 4);
    header
        .ref_pic_list_modification
        .ref_pic_list_modification_flag_l0 = true;
    header.ref_pic_list_modification.list_entry_l0 = vec![3, 3, 0];
    header
        .ref_pic_list_modification
        .ref_pic_list_modification_flag_l1 = true;
    header.ref_pic_list_modification.list_entry_l1 = vec![1];

    let [list0, list1] = reference_lists(&header, &four_way_set());

    assert_eq!(list0, some(&[3, 3, 0]));
    assert_eq!(list1, some(&[0, 0, 1]));
}

#[test]
fn list_modification_indexes_a_temporary_list_longer_than_the_candidates() {
    let mut header = slice(SliceType::P, 2, 1, 6);
    header
        .ref_pic_list_modification
        .ref_pic_list_modification_flag_l0 = true;
    header.ref_pic_list_modification.list_entry_l0 = vec![5, 4];

    let [list0, _] = reference_lists(&header, &four_way_set());

    assert_eq!(list0, some(&[1, 0]));
}

#[test]
fn a_missing_reference_stays_empty_in_the_lists() {
    let set = ReferenceSet {
        before: vec![Some(0), None],
        ..ReferenceSet::default()
    };

    let [list0, _] = reference_lists(&slice(SliceType::P, 2, 1, 2), &set);

    assert_eq!(list0, [Some(0), None]);
}

#[test]
fn a_slice_without_candidates_gets_empty_lists_of_the_active_size() {
    let [list0, list1] = reference_lists(&slice(SliceType::B, 2, 3, 0), &ReferenceSet::default());

    assert_eq!(list0, [None, None]);
    assert_eq!(list1, [None, None, None]);
}

#[test]
fn every_chroma_and_depth_maps_to_a_hevc_profile() {
    let profile = |chroma, depth| profile_for(PixelFormat { chroma, depth });

    assert_eq!(
        profile(ChromaSubsampling::Yuv420, BitDepth::Eight),
        ffi::VAProfile_VAProfileHEVCMain
    );
    assert_eq!(
        profile(ChromaSubsampling::Yuv420, BitDepth::Ten),
        ffi::VAProfile_VAProfileHEVCMain10
    );
    assert_eq!(
        profile(ChromaSubsampling::Yuv422, BitDepth::Eight),
        ffi::VAProfile_VAProfileHEVCMain422_10
    );
    assert_eq!(
        profile(ChromaSubsampling::Yuv422, BitDepth::Twelve),
        ffi::VAProfile_VAProfileHEVCMain422_12
    );
    assert_eq!(
        profile(ChromaSubsampling::Yuv444, BitDepth::Eight),
        ffi::VAProfile_VAProfileHEVCMain444
    );
    assert_eq!(
        profile(ChromaSubsampling::Yuv444, BitDepth::Twelve),
        ffi::VAProfile_VAProfileHEVCMain444_12
    );
}

#[test]
fn only_the_main_and_main_10_profiles_are_not_range_extensions() {
    let formats = [
        (ChromaSubsampling::Yuv420, BitDepth::Eight, false),
        (ChromaSubsampling::Yuv420, BitDepth::Ten, false),
        (ChromaSubsampling::Yuv420, BitDepth::Twelve, true),
        (ChromaSubsampling::Yuv422, BitDepth::Ten, true),
        (ChromaSubsampling::Yuv444, BitDepth::Eight, true),
    ];

    for (chroma, depth, extended) in formats {
        let profile = profile_for(PixelFormat { chroma, depth });

        assert_eq!(
            is_range_extension(profile),
            extended,
            "{chroma:?} {depth:?}"
        );
    }
}

#[test]
fn the_reorder_depth_reads_the_highest_sub_layer_and_caps_at_the_reference_limit() {
    let mut sps = Sps {
        max_sub_layers_minus1: 2,
        max_num_reorder_pics: [1, 0, 3, 0, 0, 0, 0],
        ..Sps::default()
    };

    assert_eq!(reorder_depth(&sps), Depth::Fixed(3));

    sps.max_num_reorder_pics[2] = 200;

    assert_eq!(reorder_depth(&sps), Depth::Fixed(MAX_REFERENCE_FRAMES));
}

#[test]
fn a_conformance_window_is_scaled_by_the_chroma_format() {
    let mut sps = Sps {
        conformance_window_flag: true,
        conf_win_right_offset: 3,
        conf_win_bottom_offset: 5,
        ..Sps::default()
    };

    sps.chroma_array_type = 1;
    let subsampled = crop_offsets(&sps);
    sps.chroma_array_type = 2;
    let wide = crop_offsets(&sps);
    sps.chroma_array_type = 3;
    let full = crop_offsets(&sps);

    assert_eq!((subsampled.right, subsampled.bottom), (6, 10));
    assert_eq!((wide.right, wide.bottom), (6, 5));
    assert_eq!((full.right, full.bottom), (3, 5));
}

#[test]
fn without_a_conformance_window_nothing_is_cropped() {
    let sps = Sps {
        conf_win_right_offset: 3,
        ..Sps::default()
    };

    assert_eq!(crop_offsets(&sps), CropOffsets::default());
}
