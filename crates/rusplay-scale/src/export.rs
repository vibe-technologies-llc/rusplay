#[cfg(test)]
mod tests;
mod vulkan;

use std::{
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{ColorConverter, ScaleError, YuvFrame};

static NEXT_ALLOCATION: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryHandle {
    DmaBuf,
    OpaqueFd,
}

#[derive(Debug)]
pub struct ExportedFrame {
    frame: YuvFrame,
    fd: OwnedFd,
    handle: MemoryHandle,
    allocation: u64,
    allocation_size: u64,
    dedicated: bool,
}

impl ExportedFrame {
    pub fn frame(&self) -> &YuvFrame {
        &self.frame
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    pub fn handle(&self) -> MemoryHandle {
        self.handle
    }

    pub fn allocation(&self) -> u64 {
        self.allocation
    }

    pub fn allocation_size(&self) -> u64 {
        self.allocation_size
    }

    pub fn dedicated(&self) -> bool {
        self.dedicated
    }
}

pub fn can_export(device: &wgpu::Device, handle: MemoryHandle) -> bool {
    vulkan::Exporter::new(device, handle).is_ok_and(|exporter| exporter.exportable())
}

impl ColorConverter {
    pub fn create_exported_frame(
        &self,
        device: &wgpu::Device,
        handle: MemoryHandle,
    ) -> Result<ExportedFrame, ScaleError> {
        let exporter = vulkan::Exporter::new(device, handle)?;
        let buffer = exporter.export(device, self.layout().size)?;

        Ok(ExportedFrame {
            frame: YuvFrame::from_parts(buffer.buffer, self.layout().clone()),
            fd: buffer.fd,
            handle,
            allocation: NEXT_ALLOCATION.fetch_add(1, Ordering::Relaxed),
            allocation_size: buffer.allocation_size,
            dedicated: buffer.dedicated,
        })
    }
}
