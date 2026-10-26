use std::{
    ffi::c_void,
    fs::File,
    io::Write,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
};

use rustix::{
    fs::{MemfdFlags, Mode, OFlags, SealFlags},
    ioctl::{Ioctl, IoctlOutput, Opcode, opcode},
};

const DEVICE: &str = "/dev/udmabuf";
pub const PAGE_BYTES: usize = 4096;
const UDMABUF_FLAGS_CLOEXEC: u32 = 1;
const UDMABUF_CREATE: u8 = 0x42;

#[repr(C)]
struct UdmabufCreate {
    memfd: u32,
    flags: u32,
    offset: u64,
    size: u64,
}

struct CreateUdmabuf(UdmabufCreate);

unsafe impl Ioctl for CreateUdmabuf {
    type Output = OwnedFd;

    const IS_MUTATING: bool = false;

    fn opcode(&self) -> Opcode {
        opcode::write::<UdmabufCreate>(b'u', UDMABUF_CREATE)
    }

    fn as_ptr(&mut self) -> *mut c_void {
        (&raw mut self.0).cast()
    }

    unsafe fn output_from_ptr(
        out: IoctlOutput,
        _extract_output: *mut c_void,
    ) -> rustix::io::Result<OwnedFd> {
        Ok(unsafe { OwnedFd::from_raw_fd(out) })
    }
}

pub fn create(bytes: &[u8]) -> Option<OwnedFd> {
    let device = rustix::fs::open(DEVICE, OFlags::RDWR | OFlags::CLOEXEC, Mode::empty()).ok()?;
    let size = bytes.len().max(1).next_multiple_of(PAGE_BYTES);

    let memfd = rustix::fs::memfd_create(
        "rusplay-udmabuf",
        MemfdFlags::ALLOW_SEALING | MemfdFlags::CLOEXEC,
    )
    .ok()?;
    rustix::fs::ftruncate(&memfd, u64::try_from(size).ok()?).ok()?;
    File::from(memfd.try_clone().ok()?).write_all(bytes).ok()?;
    rustix::fs::fcntl_add_seals(&memfd, SealFlags::SHRINK).ok()?;

    let create = UdmabufCreate {
        memfd: u32::try_from(memfd.as_raw_fd()).ok()?,
        flags: UDMABUF_FLAGS_CLOEXEC,
        offset: 0,
        size: u64::try_from(size).ok()?,
    };
    unsafe { rustix::ioctl::ioctl(&device, CreateUdmabuf(create)) }.ok()
}
