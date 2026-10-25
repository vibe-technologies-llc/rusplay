use std::ffi::{CStr, CString};

use rusplay_config::SoftwarePreset;
use rusplay_core::FrameRate;

use crate::{Backend, EncodeError};

pub const VBV_BUFFER_FRAMES: u64 = 2;

pub struct CParam {
    pub key: CString,
    pub value: CString,
}

impl CParam {
    pub fn new(backend: Backend, key: &str, value: &str) -> Result<Self, EncodeError> {
        let invalid = || EncodeError::InvalidParameterValue {
            backend,
            key: key.to_owned(),
            value: value.to_owned(),
        };

        Ok(Self {
            key: CString::new(key).map_err(|_| invalid())?,
            value: CString::new(value).map_err(|_| invalid())?,
        })
    }
}

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

pub fn vbv_buffer_kbit(bitrate_kbps: u32, frame_rate: FrameRate) -> u32 {
    let numerator = u64::from(frame_rate.numerator).max(1);
    let kbit =
        u64::from(bitrate_kbps) * u64::from(frame_rate.denominator) * VBV_BUFFER_FRAMES / numerator;

    u32::try_from(kbit.max(1)).unwrap_or(u32::MAX)
}

#[cfg(test)]
pub mod testing {
    use std::{fs::File, os::fd::OwnedFd};

    use rusplay_capture::{DrmFourcc, Frame, FrameBuffer};
    use rusplay_core::{Resolution, Timestamp};

    pub fn start_code_offsets(data: &[u8]) -> Vec<usize> {
        data.windows(3)
            .enumerate()
            .filter(|(_, window)| *window == [0, 0, 1])
            .map(|(offset, _)| offset + 3)
            .collect()
    }

    pub fn dmabuf_frame() -> Frame {
        let fd = OwnedFd::from(File::open("/dev/null").unwrap());

        Frame {
            resolution: Resolution::new(64, 64),
            format: DrmFourcc::from_code(*b"XR24"),
            timestamp: Timestamp::ZERO,
            buffer: FrameBuffer::DmaBuf {
                fd,
                modifier: 0,
                planes: Vec::new(),
            },
        }
    }
}
