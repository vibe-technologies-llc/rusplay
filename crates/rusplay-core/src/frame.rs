use std::{fmt, os::fd::OwnedFd};

use crate::{ColorSpace, Resolution, Timestamp};

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
        lease: BufferLease,
    },
    Shm {
        data: Vec<u8>,
        stride: u32,
    },
    OpaqueFd {
        fd: OwnedFd,
        allocation: u64,
        size: u64,
        dedicated: bool,
        planes: Vec<Plane>,
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

type Release = Box<dyn FnOnce() + Send>;

pub struct BufferLease {
    release: Option<Release>,
}

impl BufferLease {
    pub fn new(release: impl FnOnce() + Send + 'static) -> Self {
        Self {
            release: Some(Box::new(release)),
        }
    }

    pub const fn detached() -> Self {
        Self { release: None }
    }

    pub const fn is_detached(&self) -> bool {
        self.release.is_none()
    }
}

impl Drop for BufferLease {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

impl fmt::Debug for BufferLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BufferLease")
            .field("detached", &self.is_detached())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    #[test]
    fn a_lease_releases_its_buffer_once_when_dropped() {
        let released = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&released);

        let lease = BufferLease::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        let held = released.load(Ordering::Relaxed);
        drop(lease);

        assert_eq!(held, 0);
        assert_eq!(released.load(Ordering::Relaxed), 1);
        assert!(BufferLease::detached().is_detached());
    }
}
