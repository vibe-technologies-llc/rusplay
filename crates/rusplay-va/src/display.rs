use std::{
    ffi::{c_int, c_uint, c_void},
    fs::{File, OpenOptions},
    mem,
    os::fd::AsRawFd,
    path::Path,
    ptr,
    rc::Rc,
};

use rusplay_core::Resolution;

use crate::{
    error::{VaCall, VaError, VaStatus},
    ffi,
    surface::SurfaceId,
};

pub type Profile = ffi::VAProfile;
pub type Entrypoint = ffi::VAEntrypoint;
pub type AttributeType = ffi::VAConfigAttribType;

pub(crate) fn check(call: VaCall, status: ffi::VAStatus) -> Result<(), VaError> {
    if status == 0 {
        Ok(())
    } else {
        Err(VaError::Call {
            call,
            status: VaStatus(status),
        })
    }
}

pub(crate) fn c_int_of(value: impl TryInto<c_int>) -> c_int {
    value.try_into().unwrap_or(c_int::MAX)
}

pub(crate) fn c_uint_of(value: impl TryInto<c_uint>) -> c_uint {
    value.try_into().unwrap_or(c_uint::MAX)
}

pub(crate) fn usize_of(value: impl TryInto<usize>) -> usize {
    value.try_into().unwrap_or(0)
}

pub struct Display {
    raw: ffi::VADisplay,
    _device: Option<File>,
}

impl Display {
    pub fn open(path: &Path) -> Result<Rc<Self>, VaError> {
        let device = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| VaError::DeviceOpen {
                path: path.to_owned(),
                source,
            })?;

        let raw = unsafe { ffi::vaGetDisplayDRM(device.as_raw_fd()) };
        if raw.is_null() {
            return Err(VaError::DisplayUnavailable {
                path: path.to_owned(),
            });
        }

        let display = Rc::new(Self {
            raw,
            _device: Some(device),
        });

        let (mut major, mut minor) = (0, 0);
        check(VaCall::Initialize, unsafe {
            ffi::vaInitialize(display.raw, &raw mut major, &raw mut minor)
        })?;

        Ok(display)
    }

    #[cfg(feature = "detached")]
    pub fn detached() -> Rc<Self> {
        Rc::new(Self {
            raw: ptr::null_mut(),
            _device: None,
        })
    }

    #[cfg(feature = "detached")]
    pub fn is_detached(&self) -> bool {
        self.raw.is_null()
    }

    #[cfg(not(feature = "detached"))]
    pub const fn is_detached(&self) -> bool {
        false
    }

    pub(crate) fn raw(&self) -> ffi::VADisplay {
        self.raw
    }

    pub fn profiles(&self) -> Result<Vec<Profile>, VaError> {
        let mut profiles = vec![0; usize_of(unsafe { ffi::vaMaxNumProfiles(self.raw) })];
        let mut count = 0;
        check(VaCall::QueryConfigProfiles, unsafe {
            ffi::vaQueryConfigProfiles(self.raw, profiles.as_mut_ptr(), &raw mut count)
        })?;
        profiles.truncate(usize_of(count));

        Ok(profiles)
    }

    pub fn entrypoints(&self, profile: Profile) -> Result<Vec<Entrypoint>, VaError> {
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

    pub fn attributes<const N: usize>(
        &self,
        profile: Profile,
        entrypoint: Entrypoint,
        types: [AttributeType; N],
    ) -> Result<[Option<u32>; N], VaError> {
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

    pub fn sync(&self, surface: SurfaceId) -> Result<(), VaError> {
        if self.is_detached() {
            return Ok(());
        }

        check(VaCall::SyncSurface, unsafe {
            ffi::vaSyncSurface(self.raw, surface)
        })
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        if self.is_detached() {
            return;
        }

        unsafe { ffi::vaTerminate(self.raw) };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeLimits {
    pub min: Resolution,
    pub max: Resolution,
}

impl SizeLimits {
    pub fn contains(self, resolution: Resolution) -> bool {
        (self.min.width..=self.max.width).contains(&resolution.width)
            && (self.min.height..=self.max.height).contains(&resolution.height)
    }
}

pub struct Config {
    display: Rc<Display>,
    id: ffi::VAConfigID,
}

impl Config {
    pub fn new(
        display: &Rc<Display>,
        profile: Profile,
        entrypoint: Entrypoint,
        attributes: &[(AttributeType, u32)],
    ) -> Result<Self, VaError> {
        let mut list: Vec<ffi::VAConfigAttrib> = attributes
            .iter()
            .map(|&(type_, value)| ffi::VAConfigAttrib { type_, value })
            .collect();
        let mut id = ffi::VA_INVALID_ID;
        if display.is_detached() {
            return Ok(Self {
                display: Rc::clone(display),
                id,
            });
        }

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

    pub fn size_limits(&self) -> Result<SizeLimits, VaError> {
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
        if self.display.is_detached() {
            return;
        }

        unsafe { ffi::vaDestroyConfig(self.display.raw, self.id) };
    }
}

pub struct Context {
    display: Rc<Display>,
    id: ffi::VAContextID,
}

impl Context {
    pub fn new(
        config: &Config,
        width: u32,
        height: u32,
        targets: &[SurfaceId],
    ) -> Result<Self, VaError> {
        let mut targets = targets.to_vec();
        let target_list = if targets.is_empty() {
            ptr::null_mut()
        } else {
            targets.as_mut_ptr()
        };
        let mut id = ffi::VA_INVALID_ID;
        if config.display.is_detached() {
            return Ok(Self {
                display: Rc::clone(&config.display),
                id,
            });
        }

        check(VaCall::CreateContext, unsafe {
            ffi::vaCreateContext(
                config.display.raw,
                config.id,
                c_int_of(width),
                c_int_of(height),
                c_int_of(ffi::VA_PROGRESSIVE),
                target_list,
                c_int_of(targets.len()),
                &raw mut id,
            )
        })?;

        Ok(Self {
            display: Rc::clone(&config.display),
            id,
        })
    }

    pub fn display(&self) -> &Rc<Display> {
        &self.display
    }

    pub fn parameter<T: Copy>(
        &self,
        kind: ffi::VABufferType,
        value: &T,
    ) -> Result<Buffer, VaError> {
        self.parameters(kind, std::slice::from_ref(value))
    }

    pub fn parameters<T: Copy>(
        &self,
        kind: ffi::VABufferType,
        values: &[T],
    ) -> Result<Buffer, VaError> {
        self.create_buffer(
            kind,
            mem::size_of::<T>(),
            values.len(),
            values.as_ptr().cast::<c_void>().cast_mut(),
        )
    }

    pub fn data(&self, kind: ffi::VABufferType, data: &[u8]) -> Result<Buffer, VaError> {
        self.create_buffer(
            kind,
            data.len(),
            1,
            data.as_ptr().cast::<c_void>().cast_mut(),
        )
    }

    pub fn allocate(&self, kind: ffi::VABufferType, size: usize) -> Result<Buffer, VaError> {
        self.create_buffer(kind, size, 1, ptr::null_mut())
    }

    fn create_buffer(
        &self,
        kind: ffi::VABufferType,
        size: usize,
        count: usize,
        data: *mut c_void,
    ) -> Result<Buffer, VaError> {
        let mut id = ffi::VA_INVALID_ID;
        if self.display.is_detached() {
            return Ok(Buffer {
                display: Rc::clone(&self.display),
                id,
            });
        }

        check(VaCall::CreateBuffer, unsafe {
            ffi::vaCreateBuffer(
                self.display.raw,
                self.id,
                kind,
                c_uint_of(size),
                c_uint_of(count),
                data,
                &raw mut id,
            )
        })?;

        Ok(Buffer {
            display: Rc::clone(&self.display),
            id,
        })
    }

    pub fn render(&self, target: SurfaceId, buffers: &[Buffer]) -> Result<(), VaError> {
        if self.display.is_detached() {
            return Ok(());
        }

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
        if self.display.is_detached() {
            return;
        }

        unsafe { ffi::vaDestroyContext(self.display.raw, self.id) };
    }
}

pub struct Buffer {
    display: Rc<Display>,
    id: ffi::VABufferID,
}

impl Buffer {
    pub fn id(&self) -> ffi::VABufferID {
        self.id
    }

    pub fn map(&self) -> Result<*mut c_void, VaError> {
        let mut mapped = ptr::null_mut();
        check(VaCall::MapBuffer, unsafe {
            ffi::vaMapBuffer(self.display.raw, self.id, &raw mut mapped)
        })?;

        Ok(mapped)
    }

    pub fn unmap(&self) -> Result<(), VaError> {
        check(VaCall::UnmapBuffer, unsafe {
            ffi::vaUnmapBuffer(self.display.raw, self.id)
        })
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        if self.display.is_detached() {
            return;
        }

        unsafe { ffi::vaDestroyBuffer(self.display.raw, self.id) };
    }
}
