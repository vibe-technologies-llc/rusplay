use std::{
    ffi::{c_int, c_uint, c_void},
    fs::{File, OpenOptions},
    mem,
    os::fd::{AsRawFd, BorrowedFd},
    path::Path,
    ptr,
    rc::Rc,
    slice,
};

use rusplay_core::Resolution;

use crate::{EncodeError, VaCall, VaStatus, vaapi::ffi};

pub(super) type Profile = ffi::VAProfile;
pub(super) type Entrypoint = ffi::VAEntrypoint;
pub(super) type AttributeType = ffi::VAConfigAttribType;
pub(super) type SurfaceId = ffi::VASurfaceID;

pub(super) const INVALID_SURFACE: SurfaceId = ffi::VA_INVALID_SURFACE;

fn check(call: VaCall, status: ffi::VAStatus) -> Result<(), EncodeError> {
    if status == 0 {
        Ok(())
    } else {
        Err(EncodeError::Va {
            call,
            status: VaStatus(status),
        })
    }
}

fn c_int_of(value: impl TryInto<c_int>) -> c_int {
    value.try_into().unwrap_or(c_int::MAX)
}

fn c_uint_of(value: impl TryInto<c_uint>) -> c_uint {
    value.try_into().unwrap_or(c_uint::MAX)
}

fn usize_of(value: impl TryInto<usize>) -> usize {
    value.try_into().unwrap_or(0)
}

pub(super) struct Display {
    raw: ffi::VADisplay,
    _device: File,
}

impl Display {
    pub(super) fn open(path: &Path) -> Result<Rc<Self>, EncodeError> {
        let device = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| EncodeError::DeviceOpen {
                path: path.to_owned(),
                source,
            })?;

        let raw = unsafe { ffi::vaGetDisplayDRM(device.as_raw_fd()) };
        if raw.is_null() {
            return Err(EncodeError::DisplayUnavailable {
                path: path.to_owned(),
            });
        }

        let display = Rc::new(Self {
            raw,
            _device: device,
        });

        let (mut major, mut minor) = (0, 0);
        check(VaCall::Initialize, unsafe {
            ffi::vaInitialize(display.raw, &raw mut major, &raw mut minor)
        })?;

        Ok(display)
    }

    pub(super) fn profiles(&self) -> Result<Vec<Profile>, EncodeError> {
        let mut profiles = vec![0; usize_of(unsafe { ffi::vaMaxNumProfiles(self.raw) })];
        let mut count = 0;
        check(VaCall::QueryConfigProfiles, unsafe {
            ffi::vaQueryConfigProfiles(self.raw, profiles.as_mut_ptr(), &raw mut count)
        })?;
        profiles.truncate(usize_of(count));

        Ok(profiles)
    }

    pub(super) fn entrypoints(&self, profile: Profile) -> Result<Vec<Entrypoint>, EncodeError> {
        let mut entrypoints = vec![0; usize_of(unsafe { ffi::vaMaxNumEntrypoints(self.raw) })];
        let mut count = 0;
        check(VaCall::QueryConfigEntrypoints, unsafe {
            ffi::vaQueryConfigEntrypoints(
                self.raw,
                profile,
                entrypoints.as_mut_ptr(),
                &raw mut count,
            )
        })?;
        entrypoints.truncate(usize_of(count));

        Ok(entrypoints)
    }

    pub(super) fn attributes<const N: usize>(
        &self,
        profile: Profile,
        entrypoint: Entrypoint,
        types: [AttributeType; N],
    ) -> Result<[Option<u32>; N], EncodeError> {
        let mut attributes = types.map(|type_| ffi::VAConfigAttrib { type_, value: 0 });
        check(VaCall::GetConfigAttributes, unsafe {
            ffi::vaGetConfigAttributes(
                self.raw,
                profile,
                entrypoint,
                attributes.as_mut_ptr(),
                c_int_of(N),
            )
        })?;

        Ok(attributes.map(|attribute| {
            (attribute.value != ffi::VA_ATTRIB_NOT_SUPPORTED).then_some(attribute.value)
        }))
    }

    pub(super) fn sync(&self, surface: SurfaceId) -> Result<(), EncodeError> {
        check(VaCall::SyncSurface, unsafe {
            ffi::vaSyncSurface(self.raw, surface)
        })
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        unsafe { ffi::vaTerminate(self.raw) };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SizeLimits {
    pub(super) min: Resolution,
    pub(super) max: Resolution,
}

impl SizeLimits {
    pub(super) fn contains(self, resolution: Resolution) -> bool {
        (self.min.width..=self.max.width).contains(&resolution.width)
            && (self.min.height..=self.max.height).contains(&resolution.height)
    }
}

pub(super) struct Config {
    display: Rc<Display>,
    id: ffi::VAConfigID,
}

impl Config {
    pub(super) fn new(
        display: &Rc<Display>,
        profile: Profile,
        entrypoint: Entrypoint,
        attributes: &[(AttributeType, u32)],
    ) -> Result<Self, EncodeError> {
        let mut list: Vec<ffi::VAConfigAttrib> = attributes
            .iter()
            .map(|&(type_, value)| ffi::VAConfigAttrib { type_, value })
            .collect();
        let mut id = ffi::VA_INVALID_ID;
        check(VaCall::CreateConfig, unsafe {
            ffi::vaCreateConfig(
                display.raw,
                profile,
                entrypoint,
                list.as_mut_ptr(),
                c_int_of(list.len()),
                &raw mut id,
            )
        })?;

        Ok(Self {
            display: Rc::clone(display),
            id,
        })
    }

    pub(super) fn size_limits(&self) -> Result<SizeLimits, EncodeError> {
        let mut count: c_uint = 0;
        check(VaCall::QuerySurfaceAttributes, unsafe {
            ffi::vaQuerySurfaceAttributes(
                self.display.raw,
                self.id,
                ptr::null_mut(),
                &raw mut count,
            )
        })?;

        let mut attributes = vec![ffi::VASurfaceAttrib::default(); usize_of(count)];
        check(VaCall::QuerySurfaceAttributes, unsafe {
            ffi::vaQuerySurfaceAttributes(
                self.display.raw,
                self.id,
                attributes.as_mut_ptr(),
                &raw mut count,
            )
        })?;
        attributes.truncate(usize_of(count));

        let mut limits = SizeLimits {
            min: Resolution::new(1, 1),
            max: Resolution::new(u32::MAX, u32::MAX),
        };
        for attribute in &attributes {
            if attribute.value.type_ != ffi::VAGenericValueType_VAGenericValueTypeInteger {
                continue;
            }
            let value = u32::try_from(unsafe { attribute.value.value.i }).unwrap_or(0);
            match attribute.type_ {
                ffi::VASurfaceAttribType_VASurfaceAttribMinWidth => limits.min.width = value,
                ffi::VASurfaceAttribType_VASurfaceAttribMinHeight => limits.min.height = value,
                ffi::VASurfaceAttribType_VASurfaceAttribMaxWidth => limits.max.width = value,
                ffi::VASurfaceAttribType_VASurfaceAttribMaxHeight => limits.max.height = value,
                _ => {}
            }
        }

        Ok(limits)
    }
}

impl Drop for Config {
    fn drop(&mut self) {
        unsafe { ffi::vaDestroyConfig(self.display.raw, self.id) };
    }
}

pub(super) struct Context {
    display: Rc<Display>,
    id: ffi::VAContextID,
}

impl Context {
    pub(super) fn new(
        config: &Config,
        width: u32,
        height: u32,
        targets: &[SurfaceId],
    ) -> Result<Self, EncodeError> {
        let mut targets = targets.to_vec();
        let mut id = ffi::VA_INVALID_ID;
        check(VaCall::CreateContext, unsafe {
            ffi::vaCreateContext(
                config.display.raw,
                config.id,
                c_int_of(width),
                c_int_of(height),
                c_int_of(ffi::VA_PROGRESSIVE),
                targets.as_mut_ptr(),
                c_int_of(targets.len()),
                &raw mut id,
            )
        })?;

        Ok(Self {
            display: Rc::clone(&config.display),
            id,
        })
    }

    pub(super) fn parameter<T: Copy>(
        &self,
        kind: ffi::VABufferType,
        value: &T,
    ) -> Result<Buffer, EncodeError> {
        self.create_buffer(
            kind,
            mem::size_of::<T>(),
            ptr::from_ref(value).cast::<c_void>().cast_mut(),
        )
    }

    pub(super) fn misc<T: Copy>(
        &self,
        kind: ffi::VAEncMiscParameterType,
        value: &T,
    ) -> Result<Buffer, EncodeError> {
        let header = mem::size_of::<ffi::VAEncMiscParameterType>();
        let buffer = self.create_buffer(
            ffi::VABufferType_VAEncMiscParameterBufferType,
            header + mem::size_of::<T>(),
            ptr::null_mut(),
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

    pub(super) fn packed_header(
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
        let data = self.create_buffer(
            ffi::VABufferType_VAEncPackedHeaderDataBufferType,
            header.bytes.len(),
            header.bytes.as_ptr().cast::<c_void>().cast_mut(),
        )?;

        Ok([parameter, data])
    }

    pub(super) fn coded(&self, capacity: usize) -> Result<CodedBuffer, EncodeError> {
        let buffer = self.create_buffer(
            ffi::VABufferType_VAEncCodedBufferType,
            capacity,
            ptr::null_mut(),
        )?;

        Ok(CodedBuffer { buffer, capacity })
    }

    fn create_buffer(
        &self,
        kind: ffi::VABufferType,
        size: usize,
        data: *mut c_void,
    ) -> Result<Buffer, EncodeError> {
        let mut id = ffi::VA_INVALID_ID;
        check(VaCall::CreateBuffer, unsafe {
            ffi::vaCreateBuffer(
                self.display.raw,
                self.id,
                kind,
                c_uint_of(size),
                1,
                data,
                &raw mut id,
            )
        })?;

        Ok(Buffer {
            display: Rc::clone(&self.display),
            id,
        })
    }

    pub(super) fn render(&self, target: SurfaceId, buffers: &[Buffer]) -> Result<(), EncodeError> {
        let mut ids: Vec<ffi::VABufferID> = buffers.iter().map(|buffer| buffer.id).collect();

        check(VaCall::BeginPicture, unsafe {
            ffi::vaBeginPicture(self.display.raw, self.id, target)
        })?;
        let rendered = check(VaCall::RenderPicture, unsafe {
            ffi::vaRenderPicture(
                self.display.raw,
                self.id,
                ids.as_mut_ptr(),
                c_int_of(ids.len()),
            )
        });
        let ended = check(VaCall::EndPicture, unsafe {
            ffi::vaEndPicture(self.display.raw, self.id)
        });

        rendered.and(ended)
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe { ffi::vaDestroyContext(self.display.raw, self.id) };
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct PackedHeader {
    pub(super) bytes: Vec<u8>,
    pub(super) bits: usize,
}

pub(super) struct Buffer {
    display: Rc<Display>,
    id: ffi::VABufferID,
}

impl Buffer {
    fn map(&self) -> Result<*mut c_void, EncodeError> {
        let mut mapped = ptr::null_mut();
        check(VaCall::MapBuffer, unsafe {
            ffi::vaMapBuffer(self.display.raw, self.id, &raw mut mapped)
        })?;

        Ok(mapped)
    }

    fn unmap(&self) -> Result<(), EncodeError> {
        check(VaCall::UnmapBuffer, unsafe {
            ffi::vaUnmapBuffer(self.display.raw, self.id)
        })
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe { ffi::vaDestroyBuffer(self.display.raw, self.id) };
    }
}

pub(super) struct CodedBuffer {
    buffer: Buffer,
    capacity: usize,
}

impl CodedBuffer {
    pub(super) fn id(&self) -> ffi::VABufferID {
        self.buffer.id
    }

    pub(super) fn read(&self) -> Result<Vec<u8>, EncodeError> {
        let mut segment = self.buffer.map()?.cast::<ffi::VACodedBufferSegment>();
        let mut data = Vec::new();
        let mut overflow = false;

        while !segment.is_null() {
            let current = unsafe { segment.read() };
            overflow |= current.status & ffi::VA_CODED_BUF_STATUS_SLICE_OVERFLOW_MASK != 0;
            if !current.buf.is_null() {
                data.extend_from_slice(unsafe {
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

        Ok(data)
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

pub(super) struct Surface {
    display: Rc<Display>,
    id: SurfaceId,
}

impl Surface {
    pub(super) fn create(
        display: &Rc<Display>,
        format: SurfaceFormat,
        resolution: Resolution,
    ) -> Result<Self, EncodeError> {
        let mut attributes = [
            integer_attribute(
                ffi::VASurfaceAttribType_VASurfaceAttribUsageHint,
                ffi::VA_SURFACE_ATTRIB_USAGE_HINT_ENCODER,
            ),
            integer_attribute(
                ffi::VASurfaceAttribType_VASurfaceAttribPixelFormat,
                format.va_fourcc,
            ),
        ];

        Self::create_with(display, format.rt_format, resolution, &mut attributes)
    }

    pub(super) fn import(
        display: &Rc<Display>,
        format: SurfaceFormat,
        import: &DmaBufImport<'_>,
    ) -> Result<Self, EncodeError> {
        let mut descriptor = ffi::VADRMPRIMESurfaceDescriptor {
            fourcc: format.va_fourcc,
            width: import.resolution.width,
            height: import.resolution.height,
            num_objects: 1,
            num_layers: 1,
            ..Default::default()
        };
        descriptor.objects[0].fd = import.fd.as_raw_fd();
        descriptor.objects[0].size = import.size;
        descriptor.objects[0].drm_format_modifier = import.modifier;

        let layer = &mut descriptor.layers[0];
        layer.drm_format = format.drm_fourcc;
        layer.num_planes = c_uint_of(import.planes.len());
        for (index, plane) in import.planes.iter().enumerate().take(layer.offset.len()) {
            layer.object_index[index] = 0;
            layer.offset[index] = plane.offset;
            layer.pitch[index] = plane.pitch;
        }

        let mut attributes = [
            integer_attribute(
                ffi::VASurfaceAttribType_VASurfaceAttribMemoryType,
                ffi::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
            ),
            pointer_attribute(
                ffi::VASurfaceAttribType_VASurfaceAttribExternalBufferDescriptor,
                (&raw mut descriptor).cast::<c_void>(),
            ),
            integer_attribute(
                ffi::VASurfaceAttribType_VASurfaceAttribUsageHint,
                ffi::VA_SURFACE_ATTRIB_USAGE_HINT_ENCODER,
            ),
        ];

        Self::create_with(
            display,
            format.rt_format,
            import.resolution,
            &mut attributes,
        )
    }

    fn create_with(
        display: &Rc<Display>,
        rt_format: u32,
        resolution: Resolution,
        attributes: &mut [ffi::VASurfaceAttrib],
    ) -> Result<Self, EncodeError> {
        let mut id = INVALID_SURFACE;
        check(VaCall::CreateSurfaces, unsafe {
            ffi::vaCreateSurfaces(
                display.raw,
                rt_format,
                resolution.width,
                resolution.height,
                &raw mut id,
                1,
                attributes.as_mut_ptr(),
                c_uint_of(attributes.len()),
            )
        })?;

        Ok(Self {
            display: Rc::clone(display),
            id,
        })
    }

    pub(super) fn id(&self) -> SurfaceId {
        self.id
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(super) struct ExportedSurface {
    pub(super) fd: std::os::fd::OwnedFd,
    pub(super) modifier: u64,
    pub(super) planes: Vec<DmaBufPlaneLayout>,
}

#[cfg(test)]
impl Surface {
    pub(super) fn create_exportable(
        display: &Rc<Display>,
        format: SurfaceFormat,
        resolution: Resolution,
    ) -> Result<Self, EncodeError> {
        let mut attributes = [
            integer_attribute(
                ffi::VASurfaceAttribType_VASurfaceAttribUsageHint,
                ffi::VA_SURFACE_ATTRIB_USAGE_HINT_EXPORT,
            ),
            integer_attribute(
                ffi::VASurfaceAttribType_VASurfaceAttribPixelFormat,
                format.va_fourcc,
            ),
        ];

        Self::create_with(display, format.rt_format, resolution, &mut attributes)
    }

    pub(super) fn export(&self) -> Result<ExportedSurface, EncodeError> {
        use std::os::fd::{FromRawFd, OwnedFd};

        let mut descriptor = ffi::VADRMPRIMESurfaceDescriptor::default();
        check(VaCall::ExportSurfaceHandle, unsafe {
            ffi::vaExportSurfaceHandle(
                self.display.raw,
                self.id,
                ffi::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
                ffi::VA_EXPORT_SURFACE_READ_WRITE | ffi::VA_EXPORT_SURFACE_COMPOSED_LAYERS,
                (&raw mut descriptor).cast::<c_void>(),
            )
        })?;

        let fd = unsafe { OwnedFd::from_raw_fd(descriptor.objects[0].fd) };
        for extra in descriptor
            .objects
            .iter()
            .take(usize_of(descriptor.num_objects))
            .skip(1)
        {
            drop(unsafe { OwnedFd::from_raw_fd(extra.fd) });
        }
        let layer = &descriptor.layers[0];
        let planes = (0..usize_of(layer.num_planes).min(4))
            .map(|plane| DmaBufPlaneLayout {
                offset: layer.offset[plane],
                pitch: layer.pitch[plane],
            })
            .collect();

        Ok(ExportedSurface {
            fd,
            modifier: descriptor.objects[0].drm_format_modifier,
            planes,
        })
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe { ffi::vaDestroySurfaces(self.display.raw, &raw mut self.id, 1) };
    }
}

fn integer_attribute(type_: ffi::VASurfaceAttribType, value: u32) -> ffi::VASurfaceAttrib {
    let mut attribute = ffi::VASurfaceAttrib {
        type_,
        flags: ffi::VA_SURFACE_ATTRIB_SETTABLE,
        ..Default::default()
    };
    attribute.value.type_ = ffi::VAGenericValueType_VAGenericValueTypeInteger;
    attribute.value.value.i = i32::from_ne_bytes(value.to_ne_bytes());

    attribute
}

fn pointer_attribute(type_: ffi::VASurfaceAttribType, value: *mut c_void) -> ffi::VASurfaceAttrib {
    let mut attribute = ffi::VASurfaceAttrib {
        type_,
        flags: ffi::VA_SURFACE_ATTRIB_SETTABLE,
        ..Default::default()
    };
    attribute.value.type_ = ffi::VAGenericValueType_VAGenericValueTypePointer;
    attribute.value.value.p = value;

    attribute
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ImageLayout {
    pub(super) offsets: [usize; 3],
    pub(super) pitches: [usize; 3],
}

pub(super) struct Image {
    display: Rc<Display>,
    raw: ffi::VAImage,
    resolution: Resolution,
}

impl Image {
    pub(super) fn new(
        display: &Rc<Display>,
        va_fourcc: u32,
        resolution: Resolution,
    ) -> Result<Self, EncodeError> {
        let mut format = ffi::VAImageFormat {
            fourcc: va_fourcc,
            ..Default::default()
        };
        let mut raw = ffi::VAImage::default();
        check(VaCall::CreateImage, unsafe {
            ffi::vaCreateImage(
                display.raw,
                &raw mut format,
                c_int_of(resolution.width),
                c_int_of(resolution.height),
                &raw mut raw,
            )
        })?;

        Ok(Self {
            display: Rc::clone(display),
            raw,
            resolution,
        })
    }

    fn layout(&self) -> ImageLayout {
        ImageLayout {
            offsets: [0, 1, 2].map(|plane| usize_of(self.raw.offsets[plane])),
            pitches: [0, 1, 2].map(|plane| usize_of(self.raw.pitches[plane])),
        }
    }

    pub(super) fn write(
        &mut self,
        fill: impl FnOnce(&mut [u8], ImageLayout),
    ) -> Result<(), EncodeError> {
        let mut mapped = ptr::null_mut();
        check(VaCall::MapBuffer, unsafe {
            ffi::vaMapBuffer(self.display.raw, self.raw.buf, &raw mut mapped)
        })?;

        let layout = self.layout();
        let data =
            unsafe { slice::from_raw_parts_mut(mapped.cast::<u8>(), usize_of(self.raw.data_size)) };
        fill(data, layout);

        check(VaCall::UnmapBuffer, unsafe {
            ffi::vaUnmapBuffer(self.display.raw, self.raw.buf)
        })
    }

    pub(super) fn put(&self, surface: &Surface) -> Result<(), EncodeError> {
        let Resolution { width, height } = self.resolution;

        check(VaCall::PutImage, unsafe {
            ffi::vaPutImage(
                self.display.raw,
                surface.id,
                self.raw.image_id,
                0,
                0,
                width,
                height,
                0,
                0,
                width,
                height,
            )
        })
    }

    #[cfg(test)]
    pub(super) fn get(&mut self, surface: &Surface) -> Result<Vec<u8>, EncodeError> {
        let Resolution { width, height } = self.resolution;
        check(VaCall::GetImage, unsafe {
            ffi::vaGetImage(
                self.display.raw,
                surface.id,
                0,
                0,
                width,
                height,
                self.raw.image_id,
            )
        })?;

        let mut copy = Vec::new();
        self.write(|data, _| copy.extend_from_slice(data))?;

        Ok(copy)
    }

    #[cfg(test)]
    pub(super) fn image_layout(&self) -> ImageLayout {
        self.layout()
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        unsafe { ffi::vaDestroyImage(self.display.raw, self.raw.image_id) };
    }
}
