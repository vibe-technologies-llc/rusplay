use std::ffi::CStr;

use rusplay_config::SoftwarePreset;
use rusplay_core::{ColorPrimaries, FrameRate, TransferFunction};

use crate::software::VBV_BUFFER_FRAMES;

pub const fn preset_name(preset: SoftwarePreset) -> &'static CStr {
    match preset {
        SoftwarePreset::Ultrafast => c"ultrafast",
        SoftwarePreset::Superfast => c"superfast",
        SoftwarePreset::Veryfast => c"veryfast",
        SoftwarePreset::Faster => c"faster",
        SoftwarePreset::Fast => c"fast",
        SoftwarePreset::Medium => c"medium",
        SoftwarePreset::Slow => c"slow",
        SoftwarePreset::Slower => c"slower",
        SoftwarePreset::Veryslow => c"veryslow",
        SoftwarePreset::Placebo => c"placebo",
    }
}

pub const fn colour_primaries_name(primaries: ColorPrimaries) -> &'static str {
    match primaries {
        ColorPrimaries::Bt709 => "bt709",
        ColorPrimaries::Bt2020 => "bt2020",
    }
}

pub const fn colour_matrix_name(primaries: ColorPrimaries) -> &'static str {
    match primaries {
        ColorPrimaries::Bt709 => "bt709",
        ColorPrimaries::Bt2020 => "bt2020nc",
    }
}

pub const fn transfer_name(transfer: TransferFunction) -> &'static str {
    match transfer {
        TransferFunction::Bt709 => "bt709",
        TransferFunction::Srgb => "iec61966-2-1",
        TransferFunction::Pq => "smpte2084",
        TransferFunction::Hlg => "arib-std-b67",
    }
}

pub fn vbv_buffer_kbit(bitrate_kbps: u32, frame_rate: FrameRate) -> u32 {
    let numerator = u64::from(frame_rate.numerator).max(1);
    let kbit =
        u64::from(bitrate_kbps) * u64::from(frame_rate.denominator) * VBV_BUFFER_FRAMES / numerator;

    u32::try_from(kbit.max(1)).unwrap_or(u32::MAX)
}
