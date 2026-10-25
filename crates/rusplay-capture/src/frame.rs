use std::os::fd::OwnedFd;

use rusplay_core::{ColorSpace, Resolution, Timestamp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DrmFourcc(pub u32);

impl DrmFourcc {
    pub const fn from_code(code: [u8; 4]) -> Self {
        Self(u32::from_le_bytes(code))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plane {
    pub offset: u32,
    pub stride: u32,
}

#[derive(Debug)]
pub enum FrameBuffer {
    DmaBuf {
        fd: OwnedFd,
        modifier: u64,
        planes: Vec<Plane>,
    },
    Shm {
        data: Vec<u8>,
        stride: u32,
    },
}

#[derive(Debug)]
pub struct Frame {
    pub resolution: Resolution,
    pub format: DrmFourcc,
    pub timestamp: Timestamp,
    pub color: ColorSpace,
    pub buffer: FrameBuffer,
}
