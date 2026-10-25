pub const VBV_BUFFER_FRAMES: u64 = 2;

#[cfg(any(feature = "x264", feature = "x265", feature = "vaapi"))]
pub fn vbv_buffer_kbit(bitrate_kbps: u32, frame_rate: rusplay_core::FrameRate) -> u32 {
    let numerator = u64::from(frame_rate.numerator).max(1);
    let kbit =
        u64::from(bitrate_kbps) * u64::from(frame_rate.denominator) * VBV_BUFFER_FRAMES / numerator;

    u32::try_from(kbit.max(1)).unwrap_or(u32::MAX)
}
