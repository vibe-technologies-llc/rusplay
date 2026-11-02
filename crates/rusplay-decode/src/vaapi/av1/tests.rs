use cros_codecs::codec::av1::parser::{
    BitDepth as Av1BitDepth, ColorConfig, Profile as Av1Profile, SequenceHeaderObu,
};

use super::*;

fn sequence(
    profile: Av1Profile,
    depth: Av1BitDepth,
    subsampling: (bool, bool),
    size: (u16, u16),
) -> SequenceHeaderObu {
    SequenceHeaderObu {
        seq_profile: profile,
        bit_depth: depth,
        max_frame_width_minus_1: size.0 - 1,
        max_frame_height_minus_1: size.1 - 1,
        color_config: ColorConfig {
            subsampling_x: subsampling.0,
            subsampling_y: subsampling.1,
            ..ColorConfig::default()
        },
        ..SequenceHeaderObu::default()
    }
}

#[test]
fn a_main_profile_sequence_reports_420_8_bit() {
    let header = sequence(
        Av1Profile::Profile0,
        Av1BitDepth::Depth8,
        (true, true),
        (1920, 1080),
    );

    let format = stream_format(&header).unwrap();

    assert_eq!(format.profile, ffi::VAProfile_VAProfileAV1Profile0);
    assert_eq!(format.profile_idc, 0);
    assert_eq!(
        format.pixel_format,
        PixelFormat {
            chroma: ChromaSubsampling::Yuv420,
            depth: BitDepth::Eight,
        }
    );
    assert_eq!(format.coded, Resolution::new(1920, 1080));
    assert_eq!(format.visible, format.coded);
    assert_eq!(format.reorder_frames, 0);
}

#[test]
fn subsampling_flags_select_the_chroma_format() {
    let chroma = |subsampling| {
        let header = sequence(
            Av1Profile::Profile2,
            Av1BitDepth::Depth10,
            subsampling,
            (64, 64),
        );

        stream_format(&header).unwrap().pixel_format.chroma
    };

    assert_eq!(chroma((true, true)), ChromaSubsampling::Yuv420);
    assert_eq!(chroma((true, false)), ChromaSubsampling::Yuv422);
    assert_eq!(chroma((false, false)), ChromaSubsampling::Yuv444);
    assert_eq!(chroma((false, true)), ChromaSubsampling::Yuv444);
}

#[test]
fn the_profile_and_depth_map_to_the_va_profile_and_bit_depth() {
    let format = |profile, depth| {
        let header = sequence(profile, depth, (false, false), (64, 64));

        stream_format(&header).unwrap()
    };

    let main = format(Av1Profile::Profile1, Av1BitDepth::Depth10);
    let professional = format(Av1Profile::Profile2, Av1BitDepth::Depth12);

    assert_eq!(main.profile, ffi::VAProfile_VAProfileAV1Profile1);
    assert_eq!(main.pixel_format.depth, BitDepth::Ten);
    assert_eq!(professional.profile, ffi::VAProfile_VAProfileAV1Profile2);
    assert_eq!(professional.profile_idc, 2);
    assert_eq!(professional.pixel_format.depth, BitDepth::Twelve);
}

#[test]
fn the_coded_size_is_the_maximum_frame_size() {
    let header = sequence(
        Av1Profile::Profile0,
        Av1BitDepth::Depth8,
        (true, true),
        (u16::MAX, 1),
    );

    let format = stream_format(&header).unwrap();

    assert_eq!(format.coded, Resolution::new(65535, 1));
}

#[test]
fn the_surface_pool_covers_every_reference_slot_and_the_grain_surface() {
    let header = sequence(
        Av1Profile::Profile0,
        Av1BitDepth::Depth8,
        (true, true),
        (64, 64),
    );

    let format = stream_format(&header).unwrap();

    assert_eq!(format.reference_frames, NUM_REF_FRAMES + GRAIN_SURFACES);
}

#[test]
fn monochrome_is_refused_by_name() {
    let mut header = sequence(
        Av1Profile::Profile0,
        Av1BitDepth::Depth8,
        (true, true),
        (64, 64),
    );
    header.color_config.mono_chrome = true;

    let error = stream_format(&header).unwrap_err();

    assert!(matches!(
        error,
        DecodeError::Monochrome {
            backend: Backend::Vaapi,
            codec: Codec::Av1,
        }
    ));
}

#[test]
fn the_cdef_strength_packs_primary_above_secondary() {
    assert_eq!(cdef_strength(0, 0), 0);
    assert_eq!(cdef_strength(5, 2), (5 << 2) | 2);
    assert_eq!(cdef_strength(15, 3), 0x3f);
}

#[test]
fn a_secondary_cdef_strength_of_four_is_coded_as_three() {
    assert_eq!(cdef_strength(1, 4), (1 << 2) | 3);
}

#[test]
fn out_of_range_cdef_strengths_are_masked() {
    assert_eq!(cdef_strength(0x1f, 0), 0x3c);
    assert_eq!(cdef_strength(0, 7), 3);
}

#[test]
fn copying_a_prefix_stops_at_the_shorter_slice() {
    let mut target = [9; 4];

    copy_prefix(&mut target, &[1, 2]);
    assert_eq!(target, [1, 2, 9, 9]);

    copy_prefix(&mut target, &[5, 6, 7, 8, 9, 10]);
    assert_eq!(target, [5, 6, 7, 8]);
}
