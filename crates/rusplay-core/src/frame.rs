use std::{
    fmt, mem,
    ops::{Deref, DerefMut},
    os::fd::OwnedFd,
};

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
        data: ShmBytes,
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
type Recycle = Box<dyn FnOnce(Vec<u8>) + Send>;

pub struct ShmBytes {
    bytes: Vec<u8>,
    recycle: Option<Recycle>,
}

impl ShmBytes {
    pub fn recycled(bytes: Vec<u8>, recycle: impl FnOnce(Vec<u8>) + Send + 'static) -> Self {
        Self {
            bytes,
            recycle: Some(Box::new(recycle)),
        }
    }

    pub fn into_vec(mut self) -> Vec<u8> {
        self.recycle = None;
        mem::take(&mut self.bytes)
    }
}

impl From<Vec<u8>> for ShmBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            recycle: None,
        }
    }
}

impl FromIterator<u8> for ShmBytes {
    fn from_iter<I: IntoIterator<Item = u8>>(bytes: I) -> Self {
        Self::from(bytes.into_iter().collect::<Vec<u8>>())
    }
}

impl Deref for ShmBytes {
    type Target = Vec<u8>;

    fn deref(&self) -> &Vec<u8> {
        &self.bytes
    }
}

impl DerefMut for ShmBytes {
    fn deref_mut(&mut self) -> &mut Vec<u8> {
        &mut self.bytes
    }
}

impl Drop for ShmBytes {
    fn drop(&mut self) {
        if let Some(recycle) = self.recycle.take() {
            recycle(mem::take(&mut self.bytes));
        }
    }
}

impl fmt::Debug for ShmBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ShmBytes")
            .field("len", &self.bytes.len())
            .field("recycled", &self.recycle.is_some())
            .finish()
    }
}

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
        mpsc,
    };

    use super::*;

    #[test]
    fn dropped_bytes_go_back_to_their_pool_and_detached_bytes_do_not() {
        let (returned, pool) = mpsc::channel();

        let bytes = ShmBytes::recycled(vec![1, 2, 3], {
            let returned = returned.clone();
            move |bytes| returned.send(bytes).unwrap()
        });
        let before = pool.try_recv();
        drop(bytes);
        let after = pool.try_recv();

        let kept = ShmBytes::recycled(vec![4, 5], move |bytes| returned.send(bytes).unwrap());
        let detached = kept.into_vec();

        assert!(before.is_err());
        assert_eq!(after.unwrap(), vec![1, 2, 3]);
        assert!(pool.try_recv().is_err());
        assert_eq!(detached, vec![4, 5]);
        assert_eq!(ShmBytes::from(vec![7]).len(), 1);
    }

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
