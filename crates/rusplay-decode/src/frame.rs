use std::{
    fmt,
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    sync::Arc,
};

use rusplay_core::{PixelFormat, Resolution, SignalledColor, Timestamp};

#[derive(Debug)]
pub struct DecodedFrame {
    pub resolution: Resolution,
    pub format: PixelFormat,
    pub color: SignalledColor,
    pub timestamp: Timestamp,
    pub buffer: DecodedBuffer,
}

#[derive(Debug)]
pub enum DecodedBuffer {
    DmaBuf(DmaBufFrame),
    Cpu(CpuFrame),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DrmFourcc(u32);

impl DrmFourcc {
    pub const NV12: Self = Self::from_bytes(*b"NV12");
    pub const P010: Self = Self::from_bytes(*b"P010");
    pub const P012: Self = Self::from_bytes(*b"P012");
    pub const NV16: Self = Self::from_bytes(*b"NV16");
    pub const P210: Self = Self::from_bytes(*b"P210");
    pub const NV24: Self = Self::from_bytes(*b"NV24");
    pub const Y210: Self = Self::from_bytes(*b"Y210");
    pub const Y410: Self = Self::from_bytes(*b"Y410");
    pub const Y412: Self = Self::from_bytes(*b"Y412");
    pub const AYUV: Self = Self::from_bytes(*b"AYUV");

    pub const fn from_bytes(code: [u8; 4]) -> Self {
        Self(u32::from_le_bytes(code))
    }

    pub const fn from_u32(code: u32) -> Self {
        Self(code)
    }

    pub const fn as_u32(self) -> u32 {
        self.0
    }

    pub const fn to_bytes(self) -> [u8; 4] {
        self.0.to_le_bytes()
    }
}

impl fmt::Display for DrmFourcc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.to_bytes() {
            write!(f, "{}", char::from(byte))?;
        }
        Ok(())
    }
}

impl fmt::Debug for DrmFourcc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DrmFourcc({self})")
    }
}

#[derive(Debug)]
pub struct DmaBufObject {
    pub fd: OwnedFd,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaBufPlane {
    pub object: usize,
    pub offset: u64,
    pub pitch: u64,
}

#[derive(Debug)]
pub struct DmaBufImage {
    pub fourcc: DrmFourcc,
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
    pub objects: Vec<DmaBufObject>,
    pub planes: Vec<DmaBufPlane>,
}

impl DmaBufImage {
    pub fn plane_fd(&self, plane: usize) -> Option<BorrowedFd<'_>> {
        let object = self.planes.get(plane)?.object;
        self.objects.get(object).map(|object| object.fd.as_fd())
    }
}

#[derive(Debug)]
pub struct DmaBufFrame {
    image: Arc<DmaBufImage>,
    _lease: Box<dyn fmt::Debug + Send + Sync>,
}

impl DmaBufFrame {
    pub fn new(image: Arc<DmaBufImage>, lease: impl fmt::Debug + Send + Sync + 'static) -> Self {
        Self {
            image,
            _lease: Box::new(lease),
        }
    }

    pub fn image(&self) -> &DmaBufImage {
        &self.image
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CpuPlane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
    pub width: u32,
    pub height: u32,
    pub bytes_per_sample: u8,
}

pub(crate) trait PlaneStorage: fmt::Debug + Send + Sync {
    fn plane_count(&self) -> usize;

    fn plane(&self, index: usize) -> Option<CpuPlane<'_>>;
}

#[derive(Debug)]
pub struct CpuFrame {
    storage: Box<dyn PlaneStorage>,
}

impl CpuFrame {
    #[cfg_attr(not(feature = "software"), allow(dead_code))]
    pub(crate) fn new(storage: impl PlaneStorage + 'static) -> Self {
        Self {
            storage: Box::new(storage),
        }
    }

    pub fn plane_count(&self) -> usize {
        self.storage.plane_count()
    }

    pub fn plane(&self, index: usize) -> Option<CpuPlane<'_>> {
        self.storage.plane(index)
    }

    pub fn planes(&self) -> impl Iterator<Item = CpuPlane<'_>> {
        (0..self.plane_count()).filter_map(|index| self.plane(index))
    }
}
