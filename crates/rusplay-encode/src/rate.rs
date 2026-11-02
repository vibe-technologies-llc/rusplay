#[cfg(any(
    feature = "x264",
    feature = "x265",
    feature = "vaapi",
    feature = "nvenc"
))]
pub fn vbv_buffer_kbit(
    bitrate_kbps: u32,
    frame_rate: rusplay_core::FrameRate,
    vbv_frames: u8,
) -> u32 {
    let numerator = u64::from(frame_rate.numerator).max(1);
    let kbit = u64::from(bitrate_kbps) * u64::from(frame_rate.denominator) * u64::from(vbv_frames)
        / numerator;

    u32::try_from(kbit.max(1)).unwrap_or(u32::MAX)
}

#[cfg(all(
    test,
    any(
        feature = "x264",
        feature = "x265",
        feature = "vaapi",
        feature = "nvenc"
    )
))]
mod tests {
    use rusplay_core::FrameRate;

    use super::*;

    #[test]
    fn buffer_scales_with_frames_and_bitrate() {
        let sixty = FrameRate::new(60, 1);

        assert_eq!(vbv_buffer_kbit(60_000, sixty, 1), 1_000);
        assert_eq!(vbv_buffer_kbit(60_000, sixty, 2), 2_000);
        assert_eq!(vbv_buffer_kbit(120_000, sixty, 2), 4_000);
    }

    #[test]
    fn buffer_is_at_least_one_kilobit() {
        assert_eq!(vbv_buffer_kbit(1, FrameRate::new(240, 1), 1), 1);
    }
}
