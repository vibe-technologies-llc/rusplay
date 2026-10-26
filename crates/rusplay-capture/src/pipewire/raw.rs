use std::{
    mem,
    os::fd::BorrowedFd,
    ptr::{self, NonNull},
    slice,
};

use pipewire::{
    spa::{buffer::Data, sys as spa_sys},
    stream::Stream,
    sys as pw_sys,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct BufferHandle {
    address: usize,
    serial: u64,
}

pub(crate) struct DequeuedBuffer<'s> {
    stream: &'s Stream,
    raw: NonNull<pw_sys::pw_buffer>,
}

impl<'s> DequeuedBuffer<'s> {
    pub fn dequeue(stream: &'s Stream) -> Option<Self> {
        let raw = unsafe { stream.dequeue_raw_buffer() };
        NonNull::new(raw).map(|raw| Self { stream, raw })
    }

    fn spa(&self) -> Option<&spa_sys::spa_buffer> {
        unsafe { self.raw.as_ref().buffer.as_ref() }
    }

    pub fn datas_mut(&mut self) -> &mut [Data] {
        let Some(buffer) = self.spa() else {
            return &mut [];
        };
        let count = usize::try_from(buffer.n_datas).unwrap_or(0);
        if count == 0 || buffer.datas.is_null() {
            return &mut [];
        }
        let datas = buffer.datas.cast::<Data>();

        unsafe { slice::from_raw_parts_mut(datas, count) }
    }

    pub fn meta(&self, meta_type: u32) -> Option<&[u8]> {
        let buffer = self.spa()?;
        let count = usize::try_from(buffer.n_metas).ok()?;
        if count == 0 || buffer.metas.is_null() {
            return None;
        }

        let metas = unsafe { slice::from_raw_parts(buffer.metas, count) };
        let meta = metas.iter().find(|meta| meta.type_ == meta_type)?;
        if meta.data.is_null() {
            return None;
        }
        let size = usize::try_from(meta.size).ok()?;

        Some(unsafe { slice::from_raw_parts(meta.data.cast::<u8>(), size) })
    }

    fn hold(self) -> usize {
        let address = self.raw.as_ptr().expose_provenance();
        mem::forget(self);
        address
    }
}

impl Drop for DequeuedBuffer<'_> {
    fn drop(&mut self) {
        unsafe { self.stream.queue_raw_buffer(self.raw.as_ptr()) };
    }
}

pub(crate) fn borrow_fd(data: &Data) -> Option<BorrowedFd<'_>> {
    let fd = data.fd();
    if fd < 0 {
        return None;
    }

    Some(unsafe { BorrowedFd::borrow_raw(fd) })
}

#[derive(Debug, Default)]
pub(crate) struct HeldBuffers {
    handles: Vec<BufferHandle>,
    next_serial: u64,
}

impl HeldBuffers {
    pub fn hold(&mut self, buffer: DequeuedBuffer<'_>) -> BufferHandle {
        let handle = BufferHandle {
            address: buffer.hold(),
            serial: self.next_serial,
        };
        self.next_serial += 1;
        self.handles.push(handle);
        handle
    }

    pub fn forget(&mut self, raw: *mut pw_sys::pw_buffer) {
        let address = raw.expose_provenance();
        self.handles.retain(|held| held.address != address);
    }

    pub fn release(&mut self, handle: BufferHandle, stream: &Stream) {
        let Some(index) = self.handles.iter().position(|held| *held == handle) else {
            return;
        };
        self.handles.swap_remove(index);

        let raw = ptr::with_exposed_provenance_mut::<pw_sys::pw_buffer>(handle.address);
        unsafe { stream.queue_raw_buffer(raw) };
    }
}

pub(crate) fn header_is_corrupted(header: &[u8]) -> bool {
    header
        .first_chunk::<4>()
        .map(|flags| u32::from_ne_bytes(*flags))
        .is_some_and(|flags| flags & spa_sys::SPA_META_HEADER_FLAG_CORRUPTED != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupted_header_flag_is_detected() {
        let mut header = [0u8; 32];

        let clean = header_is_corrupted(&header);
        header[..4].copy_from_slice(&spa_sys::SPA_META_HEADER_FLAG_CORRUPTED.to_ne_bytes());
        let corrupted = header_is_corrupted(&header);

        assert!(!clean);
        assert!(corrupted);
        assert!(!header_is_corrupted(&[]));
    }
}
