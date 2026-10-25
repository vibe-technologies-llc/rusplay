use std::{
    ffi::{c_int, c_uint, c_void},
    fs::{File, OpenOptions},
    mem,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::Path,
    ptr,
    rc::Rc,
    sync::Arc,
};

use crate::{
    DecodeError, DmaBufImage, DmaBufObject, DmaBufPlane, DrmFourcc, VaCall, VaStatus, vaapi::ffi,
};

pub(super) type SurfaceId = ffi::VASurfaceID;
pub(super) type Profile = ffi::VAProfile;

pub(super) const INVALID_SURFACE: SurfaceId = ffi::VA_INVALID_SURFACE;

fn check(call: VaCall, status: ffi::VAStatus) -> Result<(), DecodeError> {
    if status == 0 {
        Ok(())
    } else {
        Err(DecodeError::Va {
            call,
            status: VaStatus(status),
        })
    }
}

fn to_c_int(value: usize) -> c_int {
    c_int::try_from(value).unwrap_or(c_int::MAX)
}

pub(super) struct Display {
    raw: ffi::VADisplay,
    _device: File,
}

impl Display {
    pub(super) fn open(path: &Path) -> Result<Rc<Self>, DecodeError> {
        let device = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| DecodeError::DeviceOpen {
                path: path.to_owned(),
                source,
            })?;

        let raw = unsafe { ffi::vaGetDisplayDRM(device.as_raw_fd()) };
        if raw.is_null() {
            return Err(DecodeError::DisplayUnavailable {
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

    pub(super) fn has_decoder(&self, profile: Profile) -> Result<bool, DecodeError> {
        let mut profiles =
            vec![0; usize::try_from(unsafe { ffi::vaMaxNumProfiles(self.raw) }).unwrap_or(0)];
        let mut count = 0;
        check(VaCall::QueryConfigProfiles, unsafe {
            ffi::vaQueryConfigProfiles(self.raw, profiles.as_mut_ptr(), &raw mut count)
        })?;
        profiles.truncate(usize::try_from(count).unwrap_or(0));

        if !profiles.contains(&profile) {
            return Ok(false);
        }

        let mut entrypoints =
            vec![0; usize::try_from(unsafe { ffi::vaMaxNumEntrypoints(self.raw) }).unwrap_or(0)];
        let mut count = 0;
        check(VaCall::QueryConfigEntrypoints, unsafe {
            ffi::vaQueryConfigEntrypoints(
                self.raw,
                profile,
                entrypoints.as_mut_ptr(),
                &raw mut count,
            )
        })?;
        entrypoints.truncate(usize::try_from(count).unwrap_or(0));

        Ok(entrypoints.contains(&ffi::VAEntrypoint_VAEntrypointVLD))
    }

    pub(super) fn rt_formats(&self, profile: Profile) -> Result<u32, DecodeError> {
        let mut attribute = ffi::VAConfigAttrib {
            type_: ffi::VAConfigAttribType_VAConfigAttribRTFormat,
            value: 0,
        };
        check(VaCall::GetConfigAttributes, unsafe {
            ffi::vaGetConfigAttributes(
                self.raw,
                profile,
                ffi::VAEntrypoint_VAEntrypointVLD,
                &raw mut attribute,
                1,
            )
        })?;

        Ok(if attribute.value == ffi::VA_ATTRIB_NOT_SUPPORTED {
            0
        } else {
            attribute.value
        })
    }

    pub(super) fn sync(&self, surface: SurfaceId) -> Result<(), DecodeError> {
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

pub(super) struct Session {
    display: Rc<Display>,
    config: ffi::VAConfigID,
    context: ffi::VAContextID,
}

impl Session {
    pub(super) fn new(
        display: &Rc<Display>,
        profile: Profile,
        rt_format: u32,
        width: u32,
        height: u32,
    ) -> Result<Self, DecodeError> {
        let mut attribute = ffi::VAConfigAttrib {
            type_: ffi::VAConfigAttribType_VAConfigAttribRTFormat,
            value: rt_format,
        };
        let mut config = ffi::VA_INVALID_ID;
        check(VaCall::CreateConfig, unsafe {
            ffi::vaCreateConfig(
                display.raw,
                profile,
                ffi::VAEntrypoint_VAEntrypointVLD,
                &raw mut attribute,
                1,
                &raw mut config,
            )
        })?;

        let mut context = ffi::VA_INVALID_ID;
        let status = unsafe {
            ffi::vaCreateContext(
                display.raw,
                config,
                c_int::try_from(width).unwrap_or(c_int::MAX),
                c_int::try_from(height).unwrap_or(c_int::MAX),
                c_int::try_from(ffi::VA_PROGRESSIVE).unwrap_or(1),
                ptr::null_mut(),
                0,
                &raw mut context,
            )
        };
        if let Err(error) = check(VaCall::CreateContext, status) {
            unsafe { ffi::vaDestroyConfig(display.raw, config) };
            return Err(error);
        }

        Ok(Self {
            display: Rc::clone(display),
            config,
            context,
        })
    }

    pub(super) fn display(&self) -> &Rc<Display> {
        &self.display
    }

    pub(super) fn parameter<T: Copy>(
        &self,
        kind: ffi::VABufferType,
        value: &T,
    ) -> Result<Buffer, DecodeError> {
        self.parameters(kind, std::slice::from_ref(value))
    }

    pub(super) fn parameters<T: Copy>(
        &self,
        kind: ffi::VABufferType,
        values: &[T],
    ) -> Result<Buffer, DecodeError> {
        self.create_buffer(
            kind,
            mem::size_of::<T>(),
            values.len(),
            values.as_ptr().cast::<c_void>().cast_mut(),
        )
    }

    pub(super) fn slice_data(&self, data: &[u8]) -> Result<Buffer, DecodeError> {
        self.create_buffer(
            ffi::VABufferType_VASliceDataBufferType,
            data.len(),
            1,
            data.as_ptr().cast::<c_void>().cast_mut(),
        )
    }

    fn create_buffer(
        &self,
        kind: ffi::VABufferType,
        size: usize,
        count: usize,
        data: *mut c_void,
    ) -> Result<Buffer, DecodeError> {
        let mut id = ffi::VA_INVALID_ID;
        check(VaCall::CreateBuffer, unsafe {
            ffi::vaCreateBuffer(
                self.display.raw,
                self.context,
                kind,
                c_uint::try_from(size).unwrap_or(c_uint::MAX),
                c_uint::try_from(count).unwrap_or(c_uint::MAX),
                data,
                &raw mut id,
            )
        })?;

        Ok(Buffer {
            display: Rc::clone(&self.display),
            id,
        })
    }

    pub(super) fn decode(&self, target: SurfaceId, buffers: &[Buffer]) -> Result<(), DecodeError> {
        let mut ids: Vec<ffi::VABufferID> = buffers.iter().map(|buffer| buffer.id).collect();

        check(VaCall::BeginPicture, unsafe {
            ffi::vaBeginPicture(self.display.raw, self.context, target)
        })?;
        let rendered = check(VaCall::RenderPicture, unsafe {
            ffi::vaRenderPicture(
                self.display.raw,
                self.context,
                ids.as_mut_ptr(),
                to_c_int(ids.len()),
            )
        });
        let ended = check(VaCall::EndPicture, unsafe {
            ffi::vaEndPicture(self.display.raw, self.context)
        });

        rendered.and(ended)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            ffi::vaDestroyContext(self.display.raw, self.context);
            ffi::vaDestroyConfig(self.display.raw, self.config);
        }
    }
}

pub(super) struct Buffer {
    display: Rc<Display>,
    id: ffi::VABufferID,
}

impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe { ffi::vaDestroyBuffer(self.display.raw, self.id) };
    }
}

#[derive(Debug, Clone)]
pub(super) struct PoolSurface {
    pub(super) id: SurfaceId,
    pub(super) image: Arc<DmaBufImage>,
}

pub(super) struct SurfacePool {
    display: Rc<Display>,
    rt_format: u32,
    width: u32,
    height: u32,
    capacity: usize,
    surfaces: Vec<PoolSurface>,
}

impl SurfacePool {
    pub(super) fn new(
        display: &Rc<Display>,
        rt_format: u32,
        width: u32,
        height: u32,
        capacity: usize,
    ) -> Self {
        Self {
            display: Rc::clone(display),
            rt_format,
            width,
            height,
            capacity,
            surfaces: Vec::new(),
        }
    }

    pub(super) fn acquire(&mut self) -> Result<PoolSurface, DecodeError> {
        if let Some(free) = self
            .surfaces
            .iter()
            .find(|surface| Arc::strong_count(&surface.image) == 1)
        {
            return Ok(free.clone());
        }

        if self.surfaces.len() >= self.capacity {
            return Err(DecodeError::SurfacePoolExhausted {
                capacity: self.capacity,
            });
        }

        let id = self.create_surface()?;
        let image = match self.export(id) {
            Ok(image) => image,
            Err(error) => {
                let mut id = id;
                unsafe { ffi::vaDestroySurfaces(self.display.raw, &raw mut id, 1) };
                return Err(error);
            }
        };

        let surface = PoolSurface {
            id,
            image: Arc::new(image),
        };
        self.surfaces.push(surface.clone());

        Ok(surface)
    }

    fn create_surface(&self) -> Result<SurfaceId, DecodeError> {
        let mut usage = ffi::VASurfaceAttrib {
            type_: ffi::VASurfaceAttribType_VASurfaceAttribUsageHint,
            flags: ffi::VA_SURFACE_ATTRIB_SETTABLE,
            ..Default::default()
        };
        usage.value.type_ = ffi::VAGenericValueType_VAGenericValueTypeInteger;
        usage.value.value.i = i32::try_from(
            ffi::VA_SURFACE_ATTRIB_USAGE_HINT_DECODER | ffi::VA_SURFACE_ATTRIB_USAGE_HINT_EXPORT,
        )
        .unwrap_or(0);

        let mut id = INVALID_SURFACE;
        check(VaCall::CreateSurfaces, unsafe {
            ffi::vaCreateSurfaces(
                self.display.raw,
                self.rt_format,
                self.width,
                self.height,
                &raw mut id,
                1,
                &raw mut usage,
                1,
            )
        })?;

        Ok(id)
    }

    fn export(&self, id: SurfaceId) -> Result<DmaBufImage, DecodeError> {
        let mut descriptor = ffi::VADRMPRIMESurfaceDescriptor::default();
        check(VaCall::ExportSurfaceHandle, unsafe {
            ffi::vaExportSurfaceHandle(
                self.display.raw,
                id,
                ffi::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
                ffi::VA_EXPORT_SURFACE_READ_ONLY | ffi::VA_EXPORT_SURFACE_COMPOSED_LAYERS,
                (&raw mut descriptor).cast::<c_void>(),
            )
        })?;

        let object_count = usize::try_from(descriptor.num_objects).unwrap_or(0).min(4);
        let objects: Vec<DmaBufObject> = descriptor.objects[..object_count]
            .iter()
            .map(|object| DmaBufObject {
                fd: unsafe { OwnedFd::from_raw_fd(object.fd) },
                size: u64::from(object.size),
            })
            .collect();

        if descriptor.num_layers != 1 {
            return Err(DecodeError::ExportLayers {
                layers: descriptor.num_layers,
            });
        }

        let layer = &descriptor.layers[0];
        let plane_count = usize::try_from(layer.num_planes).unwrap_or(0).min(4);
        let planes = (0..plane_count)
            .map(|plane| DmaBufPlane {
                object: usize::try_from(layer.object_index[plane]).unwrap_or(0),
                offset: u64::from(layer.offset[plane]),
                pitch: u64::from(layer.pitch[plane]),
            })
            .collect();

        Ok(DmaBufImage {
            fourcc: DrmFourcc::from_u32(layer.drm_format),
            modifier: descriptor.objects[0].drm_format_modifier,
            width: descriptor.width,
            height: descriptor.height,
            objects,
            planes,
        })
    }
}

impl Drop for SurfacePool {
    fn drop(&mut self) {
        let mut ids: Vec<SurfaceId> = self.surfaces.iter().map(|surface| surface.id).collect();
        if !ids.is_empty() {
            unsafe {
                ffi::vaDestroySurfaces(self.display.raw, ids.as_mut_ptr(), to_c_int(ids.len()))
            };
        }
    }
}
#[cfg(test)]
impl Display {
    pub(super) fn read_back(
        &self,
        image: &DmaBufImage,
        width: u32,
        height: u32,
    ) -> Result<Vec<Vec<u8>>, DecodeError> {
        let mut descriptor = ffi::VADRMPRIMESurfaceDescriptor {
            fourcc: image.fourcc.as_u32(),
            width: image.width,
            height: image.height,
            num_objects: u32::try_from(image.objects.len()).unwrap_or(0),
            num_layers: 1,
            ..Default::default()
        };
        for (slot, object) in descriptor.objects.iter_mut().zip(&image.objects) {
            slot.fd = object.fd.as_raw_fd();
            slot.size = u32::try_from(object.size).unwrap_or(u32::MAX);
            slot.drm_format_modifier = image.modifier;
        }
        let layer = &mut descriptor.layers[0];
        layer.drm_format = image.fourcc.as_u32();
        layer.num_planes = u32::try_from(image.planes.len()).unwrap_or(0);
        for (index, plane) in image.planes.iter().enumerate() {
            layer.object_index[index] = u32::try_from(plane.object).unwrap_or(0);
            layer.offset[index] = u32::try_from(plane.offset).unwrap_or(0);
            layer.pitch[index] = u32::try_from(plane.pitch).unwrap_or(0);
        }

        let mut attributes = [
            ffi::VASurfaceAttrib {
                type_: ffi::VASurfaceAttribType_VASurfaceAttribMemoryType,
                flags: ffi::VA_SURFACE_ATTRIB_SETTABLE,
                ..Default::default()
            },
            ffi::VASurfaceAttrib {
                type_: ffi::VASurfaceAttribType_VASurfaceAttribExternalBufferDescriptor,
                flags: ffi::VA_SURFACE_ATTRIB_SETTABLE,
                ..Default::default()
            },
        ];
        attributes[0].value.type_ = ffi::VAGenericValueType_VAGenericValueTypeInteger;
        attributes[0].value.value.i =
            i32::try_from(ffi::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2).unwrap_or(0);
        attributes[1].value.type_ = ffi::VAGenericValueType_VAGenericValueTypePointer;
        attributes[1].value.value.p = (&raw mut descriptor).cast::<c_void>();

        let rt_format = if image.fourcc == DrmFourcc::NV12 {
            ffi::VA_RT_FORMAT_YUV420
        } else {
            ffi::VA_RT_FORMAT_YUV420_10
        };

        let mut surface = INVALID_SURFACE;
        check(VaCall::CreateSurfaces, unsafe {
            ffi::vaCreateSurfaces(
                self.raw,
                rt_format,
                image.width,
                image.height,
                &raw mut surface,
                1,
                attributes.as_mut_ptr(),
                2,
            )
        })?;

        let planes = self.read_surface(surface, image.fourcc, width, height);
        unsafe { ffi::vaDestroySurfaces(self.raw, &raw mut surface, 1) };
        planes
    }

    fn read_surface(
        &self,
        surface: SurfaceId,
        fourcc: DrmFourcc,
        width: u32,
        height: u32,
    ) -> Result<Vec<Vec<u8>>, DecodeError> {
        let mut format = ffi::VAImageFormat {
            fourcc: fourcc.as_u32(),
            ..Default::default()
        };
        let mut va_image = ffi::VAImage::default();
        check(VaCall::CreateImage, unsafe {
            ffi::vaCreateImage(
                self.raw,
                &raw mut format,
                c_int::try_from(width).unwrap_or(0),
                c_int::try_from(height).unwrap_or(0),
                &raw mut va_image,
            )
        })?;

        let result = (|| {
            check(VaCall::GetImage, unsafe {
                ffi::vaGetImage(self.raw, surface, 0, 0, width, height, va_image.image_id)
            })?;

            let mut mapped = ptr::null_mut();
            check(VaCall::MapBuffer, unsafe {
                ffi::vaMapBuffer(self.raw, va_image.buf, &raw mut mapped)
            })?;
            let data = unsafe {
                std::slice::from_raw_parts(
                    mapped.cast::<u8>(),
                    usize::try_from(va_image.data_size).unwrap_or(0),
                )
            };

            let bytes_per_sample = if fourcc == DrmFourcc::NV12 { 1 } else { 2 };
            let row_bytes = usize::try_from(width).unwrap_or(0) * bytes_per_sample;
            let plane_rows = [
                usize::try_from(height).unwrap_or(0),
                usize::try_from(height.div_ceil(2)).unwrap_or(0),
            ];

            let planes = (0..2)
                .map(|plane| {
                    let offset = usize::try_from(va_image.offsets[plane]).unwrap_or(0);
                    let pitch = usize::try_from(va_image.pitches[plane]).unwrap_or(0);
                    (0..plane_rows[plane])
                        .flat_map(|row| {
                            let start = offset + row * pitch;
                            data[start..start + row_bytes].iter().copied()
                        })
                        .collect()
                })
                .collect();

            unsafe { ffi::vaUnmapBuffer(self.raw, va_image.buf) };
            Ok(planes)
        })();

        unsafe { ffi::vaDestroyImage(self.raw, va_image.image_id) };
        result
    }
}
