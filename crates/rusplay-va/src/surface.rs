use std::{
    ffi::c_void,
    os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
    rc::Rc,
};

use rusplay_core::Resolution;

use crate::{
    display::{Display, c_uint_of, check, usize_of},
    error::{VaCall, VaError},
    ffi,
};

pub type SurfaceId = ffi::VASurfaceID;

pub const INVALID_SURFACE: SurfaceId = ffi::VA_INVALID_SURFACE;

const MAX_OBJECTS: usize = 4;
const MAX_PLANES: usize = 4;

#[derive(Debug, Clone, Copy)]
pub struct DmaBufObjectRef<'a> {
    pub fd: BorrowedFd<'a>,
    pub size: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct DmaBufPlaneLayout {
    pub object: u32,
    pub offset: u32,
    pub pitch: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct DmaBufImport<'a> {
    pub va_fourcc: u32,
    pub drm_fourcc: u32,
    pub resolution: Resolution,
    pub modifier: u64,
    pub objects: &'a [DmaBufObjectRef<'a>],
    pub planes: &'a [DmaBufPlaneLayout],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportAccess {
    ReadOnly,
    ReadWrite,
}

impl ExportAccess {
    fn flags(self) -> u32 {
        let access = match self {
            Self::ReadOnly => ffi::VA_EXPORT_SURFACE_READ_ONLY,
            Self::ReadWrite => ffi::VA_EXPORT_SURFACE_READ_WRITE,
        };

        access | ffi::VA_EXPORT_SURFACE_COMPOSED_LAYERS
    }
}

#[derive(Debug)]
pub struct ExportedObject {
    pub fd: OwnedFd,
    pub size: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct ExportedPlane {
    pub object: u32,
    pub offset: u32,
    pub pitch: u32,
}

#[derive(Debug)]
pub struct ExportedSurface {
    pub fourcc: u32,
    pub width: u32,
    pub height: u32,
    pub modifier: u64,
    pub layers: u32,
    pub objects: Vec<ExportedObject>,
    pub planes: Vec<ExportedPlane>,
}

pub struct Surface {
    display: Rc<Display>,
    id: SurfaceId,
}

impl Surface {
    pub fn create(
        display: &Rc<Display>,
        rt_format: u32,
        resolution: Resolution,
        usage_hint: u32,
        pixel_format: Option<u32>,
    ) -> Result<Self, VaError> {
        let mut attributes = vec![integer_attribute(
            ffi::VASurfaceAttribType_VASurfaceAttribUsageHint,
            usage_hint,
        )];
        if let Some(fourcc) = pixel_format {
            attributes.push(integer_attribute(
                ffi::VASurfaceAttribType_VASurfaceAttribPixelFormat,
                fourcc,
            ));
        }

        Self::create_with(display, rt_format, resolution, &mut attributes)
    }

    pub fn import(
        display: &Rc<Display>,
        rt_format: u32,
        import: &DmaBufImport<'_>,
        usage_hint: Option<u32>,
    ) -> Result<Self, VaError> {
        let mut descriptor = ffi::VADRMPRIMESurfaceDescriptor {
            fourcc: import.va_fourcc,
            width: import.resolution.width,
            height: import.resolution.height,
            num_objects: c_uint_of(import.objects.len().min(MAX_OBJECTS)),
            num_layers: 1,
            ..Default::default()
        };
        for (slot, object) in descriptor.objects.iter_mut().zip(import.objects) {
            slot.fd = object.fd.as_raw_fd();
            slot.size = object.size;
            slot.drm_format_modifier = import.modifier;
        }

        let layer = &mut descriptor.layers[0];
        layer.drm_format = import.drm_fourcc;
        layer.num_planes = c_uint_of(import.planes.len().min(MAX_PLANES));
        for (index, plane) in import.planes.iter().enumerate().take(layer.offset.len()) {
            layer.object_index[index] = plane.object;
            layer.offset[index] = plane.offset;
            layer.pitch[index] = plane.pitch;
        }

        let mut attributes = vec![
            integer_attribute(
                ffi::VASurfaceAttribType_VASurfaceAttribMemoryType,
                ffi::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
            ),
            pointer_attribute(
                ffi::VASurfaceAttribType_VASurfaceAttribExternalBufferDescriptor,
                (&raw mut descriptor).cast::<c_void>(),
            ),
        ];
        if let Some(hint) = usage_hint {
            attributes.push(integer_attribute(
                ffi::VASurfaceAttribType_VASurfaceAttribUsageHint,
                hint,
            ));
        }

        Self::create_with(display, rt_format, import.resolution, &mut attributes)
    }

    fn create_with(
        display: &Rc<Display>,
        rt_format: u32,
        resolution: Resolution,
        attributes: &mut [ffi::VASurfaceAttrib],
    ) -> Result<Self, VaError> {
        let mut id = INVALID_SURFACE;
        check(VaCall::CreateSurfaces, unsafe {
            ffi::vaCreateSurfaces(
                display.raw(),
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

    pub fn id(&self) -> SurfaceId {
        self.id
    }

    pub fn export(&self, access: ExportAccess) -> Result<ExportedSurface, VaError> {
        let mut descriptor = ffi::VADRMPRIMESurfaceDescriptor::default();
        check(VaCall::ExportSurfaceHandle, unsafe {
            ffi::vaExportSurfaceHandle(
                self.display.raw(),
                self.id,
                ffi::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
                access.flags(),
                (&raw mut descriptor).cast::<c_void>(),
            )
        })?;

        let object_count = usize_of(descriptor.num_objects).min(MAX_OBJECTS);
        let objects = descriptor.objects[..object_count]
            .iter()
            .map(|object| ExportedObject {
                fd: unsafe { OwnedFd::from_raw_fd(object.fd) },
                size: object.size,
            })
            .collect();

        let layer = &descriptor.layers[0];
        let planes = (0..usize_of(layer.num_planes).min(MAX_PLANES))
            .map(|plane| ExportedPlane {
                object: layer.object_index[plane],
                offset: layer.offset[plane],
                pitch: layer.pitch[plane],
            })
            .collect();

        Ok(ExportedSurface {
            fourcc: layer.drm_format,
            width: descriptor.width,
            height: descriptor.height,
            modifier: descriptor.objects[0].drm_format_modifier,
            layers: descriptor.num_layers,
            objects,
            planes,
        })
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe { ffi::vaDestroySurfaces(self.display.raw(), &raw mut self.id, 1) };
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
