use std::ffi::CStr;

use rusplay_config::SoftwarePreset;
use rusplay_core::{ColorPrimaries, TransferFunction};

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
