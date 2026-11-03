use std::{ffi::c_uint, mem, os::fd::BorrowedFd, rc::Rc, slice};

use rusplay_core::Resolution;
pub(super) use rusplay_va::{
    Buffer, Config, Context, Display, Entrypoint, INVALID_SURFACE, Image, ImageLayout, Profile,
    SizeLimits, Surface, SurfaceId,
};
use rusplay_va::{DmaBufObjectRef, DmaBufPlaneLayout as VaPlaneLayout};

use crate::{EncodeError, obu::AccessUnit, vaapi::ffi};

fn c_uint_of(value: impl TryInto<c_uint>) -> c_uint {
    value.try_into().unwrap_or(c_uint::MAX)
}

fn usize_of(value: impl TryInto<usize>) -> usize {
    value.try_into().unwrap_or(0)
}

pub(super) trait EncoderContext {
    fn misc<T: Copy>(
        &self,
        kind: ffi::VAEncMiscParameterType,
        value: &T,
    ) -> Result<Buffer, EncodeError>;

    fn packed_header(
        &self,
        kind: ffi::VAEncPackedHeaderType,
        header: &PackedHeader,
    ) -> Result<[Buffer; 2], EncodeError>;

    fn coded(&self, capacity: usize) -> Result<CodedBuffer, EncodeError>;
}

impl EncoderContext for Context {
    fn misc<T: Copy>(
        &self,
        kind: ffi::VAEncMiscParameterType,
        value: &T,
    ) -> Result<Buffer, EncodeError> {
        let header = mem::size_of::<ffi::VAEncMiscParameterType>();
        let buffer = self.allocate(
            ffi::VABufferType_VAEncMiscParameterBufferType,
            header + mem::size_of::<T>(),
        )?;

        let mapped = buffer.map()?;
        unsafe {
            mapped
                .cast::<ffi::VAEncMiscParameterType>()
                .write_unaligned(kind);
            mapped
                .cast::<u8>()
                .add(header)
                .cast::<T>()
                .write_unaligned(*value);
        }
        buffer.unmap()?;

        Ok(buffer)
    }

    fn packed_header(
        &self,
        kind: ffi::VAEncPackedHeaderType,
        header: &PackedHeader,
    ) -> Result<[Buffer; 2], EncodeError> {
        let parameter = ffi::VAEncPackedHeaderParameterBuffer {
            type_: kind,
            bit_length: c_uint_of(header.bits),
            has_emulation_bytes: 1,
            ..Default::default()
        };
        let parameter = self.parameter(
            ffi::VABufferType_VAEncPackedHeaderParameterBufferType,
            &parameter,
        )?;
        let data = self.data(
            ffi::VABufferType_VAEncPackedHeaderDataBufferType,
            &header.bytes,
        )?;

        Ok([parameter, data])
    }

    fn coded(&self, capacity: usize) -> Result<CodedBuffer, EncodeError> {
        let buffer = self.allocate(ffi::VABufferType_VAEncCodedBufferType, capacity)?;

        Ok(CodedBuffer { buffer, capacity })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct PackedHeader {
    pub(super) bytes: Vec<u8>,
    pub(super) bits: usize,
}

pub(super) struct CodedBuffer {
    buffer: Buffer,
    capacity: usize,
}

impl CodedBuffer {
    pub(super) fn id(&self) -> ffi::VABufferID {
        self.buffer.id()
    }

    pub(super) fn read(&self, delimit: bool) -> Result<Vec<u8>, EncodeError> {
        let mut segment = self.buffer.map()?.cast::<ffi::VACodedBufferSegment>();
        let mut unit = AccessUnit::new(delimit);
        let mut overflow = false;

        while !segment.is_null() {
            let current = unsafe { segment.read() };
            overflow |= current.status & ffi::VA_CODED_BUF_STATUS_SLICE_OVERFLOW_MASK != 0;
            if !current.buf.is_null() {
                unit.push(unsafe {
                    slice::from_raw_parts(current.buf.cast::<u8>(), usize_of(current.size))
                });
            }
            segment = current.next.cast();
        }

        self.buffer.unmap()?;

        if overflow {
            return Err(EncodeError::CodedBufferOverflow {
                capacity: self.capacity,
            });
        }

        Ok(unit.finish())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SurfaceFormat {
    pub(super) rt_format: u32,
    pub(super) va_fourcc: u32,
    pub(super) drm_fourcc: u32,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct DmaBufPlaneLayout {
    pub(super) offset: u32,
    pub(super) pitch: u32,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct DmaBufImport<'a> {
    pub(super) fd: BorrowedFd<'a>,
    pub(super) size: u32,
    pub(super) modifier: u64,
    pub(super) resolution: Resolution,
    pub(super) planes: &'a [DmaBufPlaneLayout],
}

pub(super) fn encoder_surface(
    display: &Rc<Display>,
    format: SurfaceFormat,
    resolution: Resolution,
) -> Result<Surface, EncodeError> {
    Ok(Surface::create(
        display,
        format.rt_format,
        resolution,
        ffi::VA_SURFACE_ATTRIB_USAGE_HINT_ENCODER,
        Some(format.va_fourcc),
    )?)
}

pub(super) fn imported_surface(
    display: &Rc<Display>,
    format: SurfaceFormat,
    import: &DmaBufImport<'_>,
) -> Result<Surface, EncodeError> {
    let planes: Vec<VaPlaneLayout> = import
        .planes
        .iter()
        .map(|plane| VaPlaneLayout {
            object: 0,
            offset: plane.offset,
            pitch: plane.pitch,
        })
        .collect();

    Ok(Surface::import(
        display,
        format.rt_format,
        &rusplay_va::DmaBufImport {
            va_fourcc: format.va_fourcc,
            drm_fourcc: format.drm_fourcc,
            resolution: import.resolution,
            modifier: import.modifier,
            objects: &[DmaBufObjectRef {
                fd: import.fd,
                size: import.size,
            }],
            planes: &planes,
        },
        Some(ffi::VA_SURFACE_ATTRIB_USAGE_HINT_ENCODER),
    )?)
}

#[cfg(test)]
#[derive(Debug)]
pub(super) struct ExportedSurface {
    pub(super) fd: std::os::fd::OwnedFd,
    pub(super) modifier: u64,
    pub(super) planes: Vec<DmaBufPlaneLayout>,
}

#[cfg(test)]
pub(super) fn exportable_surface(
    display: &Rc<Display>,
    format: SurfaceFormat,
    resolution: Resolution,
) -> Result<Surface, EncodeError> {
    Ok(Surface::create(
        display,
        format.rt_format,
        resolution,
        ffi::VA_SURFACE_ATTRIB_USAGE_HINT_EXPORT,
        Some(format.va_fourcc),
    )?)
}

#[cfg(test)]
pub(super) fn export_surface(surface: &Surface) -> Result<ExportedSurface, EncodeError> {
    let exported = surface.export(rusplay_va::ExportAccess::ReadWrite)?;
    let modifier = exported.modifier;
    let planes = exported
        .planes
        .iter()
        .map(|plane| DmaBufPlaneLayout {
            offset: plane.offset,
            pitch: plane.pitch,
        })
        .collect();
    let fd = exported
        .objects
        .into_iter()
        .next()
        .expect("an exported surface has an object")
        .fd;

    Ok(ExportedSurface {
        fd,
        modifier,
        planes,
    })
}
