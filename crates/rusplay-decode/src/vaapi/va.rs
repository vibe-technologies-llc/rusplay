use std::{rc::Rc, sync::Arc};

use rusplay_core::Resolution;
pub(super) use rusplay_va::{Buffer, Display, INVALID_SURFACE, Profile, SurfaceId};
use rusplay_va::{Config, Context, ExportAccess, Surface};

use crate::{DecodeError, DmaBufImage, DmaBufObject, DmaBufPlane, DrmFourcc, vaapi::ffi};

pub(super) trait DecoderDisplay {
    fn has_decoder(&self, profile: Profile) -> Result<bool, DecodeError>;

    fn rt_formats(&self, profile: Profile) -> Result<u32, DecodeError>;
}

impl DecoderDisplay for Rc<Display> {
    fn has_decoder(&self, profile: Profile) -> Result<bool, DecodeError> {
        if !self.profiles()?.contains(&profile) {
            return Ok(false);
        }

        Ok(self
            .entrypoints(profile)?
            .contains(&ffi::VAEntrypoint_VAEntrypointVLD))
    }

    fn rt_formats(&self, profile: Profile) -> Result<u32, DecodeError> {
        let [rt_formats] = self.attributes(
            profile,
            ffi::VAEntrypoint_VAEntrypointVLD,
            [ffi::VAConfigAttribType_VAConfigAttribRTFormat],
        )?;

        Ok(rt_formats.unwrap_or(0))
    }
}

pub(super) struct Session {
    context: Context,
    _config: Config,
}

impl Session {
    pub(super) fn new(
        display: &Rc<Display>,
        profile: Profile,
        rt_format: u32,
        width: u32,
        height: u32,
    ) -> Result<Self, DecodeError> {
        let config = Config::new(
            display,
            profile,
            ffi::VAEntrypoint_VAEntrypointVLD,
            &[(ffi::VAConfigAttribType_VAConfigAttribRTFormat, rt_format)],
        )?;
        let context = Context::new(&config, width, height, &[])?;

        Ok(Self {
            context,
            _config: config,
        })
    }

    pub(super) fn display(&self) -> &Rc<Display> {
        self.context.display()
    }

    pub(super) fn parameter<T: Copy>(
        &self,
        kind: ffi::VABufferType,
        value: &T,
    ) -> Result<Buffer, DecodeError> {
        Ok(self.context.parameter(kind, value)?)
    }

    pub(super) fn parameters<T: Copy>(
        &self,
        kind: ffi::VABufferType,
        values: &[T],
    ) -> Result<Buffer, DecodeError> {
        Ok(self.context.parameters(kind, values)?)
    }

    pub(super) fn slice_data(&self, data: &[u8]) -> Result<Buffer, DecodeError> {
        Ok(self
            .context
            .data(ffi::VABufferType_VASliceDataBufferType, data)?)
    }

    pub(super) fn decode(&self, target: SurfaceId, buffers: &[Buffer]) -> Result<(), DecodeError> {
        Ok(self.context.render(target, buffers)?)
    }
}

#[derive(Debug, Clone)]
pub(super) struct PoolSurface {
    pub(super) id: SurfaceId,
    pub(super) image: Arc<DmaBufImage>,
}

#[cfg(test)]
impl PoolSurface {
    pub(super) fn detached(id: SurfaceId) -> Self {
        Self {
            id,
            image: Arc::new(DmaBufImage {
                fourcc: DrmFourcc::NV12,
                modifier: 0,
                width: 0,
                height: 0,
                objects: Vec::new(),
                planes: Vec::new(),
            }),
        }
    }
}

pub(super) struct SurfacePool {
    display: Rc<Display>,
    rt_format: u32,
    width: u32,
    height: u32,
    capacity: usize,
    surfaces: Vec<PoolSurface>,
    owned: Vec<Surface>,
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
            owned: Vec::new(),
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

        let owned = Surface::create(
            &self.display,
            self.rt_format,
            Resolution::new(self.width, self.height),
            ffi::VA_SURFACE_ATTRIB_USAGE_HINT_DECODER | ffi::VA_SURFACE_ATTRIB_USAGE_HINT_EXPORT,
            None,
        )?;
        let image = export(&owned)?;

        let surface = PoolSurface {
            id: owned.id(),
            image: Arc::new(image),
        };
        self.owned.push(owned);
        self.surfaces.push(surface.clone());

        Ok(surface)
    }
}

fn export(surface: &Surface) -> Result<DmaBufImage, DecodeError> {
    let exported = surface.export(ExportAccess::ReadOnly)?;

    let objects: Vec<DmaBufObject> = exported
        .objects
        .into_iter()
        .map(|object| DmaBufObject {
            fd: object.fd,
            size: u64::from(object.size),
        })
        .collect();

    if exported.layers != 1 {
        return Err(DecodeError::ExportLayers {
            layers: exported.layers,
        });
    }

    let planes = exported
        .planes
        .iter()
        .map(|plane| DmaBufPlane {
            object: usize::try_from(plane.object).unwrap_or(0),
            offset: u64::from(plane.offset),
            pitch: u64::from(plane.pitch),
        })
        .collect();

    Ok(DmaBufImage {
        fourcc: DrmFourcc::from_u32(exported.fourcc),
        modifier: exported.modifier,
        width: exported.width,
        height: exported.height,
        objects,
        planes,
    })
}

#[cfg(test)]
pub(super) trait ReadBack {
    fn read_back(
        &self,
        image: &DmaBufImage,
        width: u32,
        height: u32,
    ) -> Result<Vec<Vec<u8>>, DecodeError>;
}

#[cfg(test)]
impl ReadBack for Rc<Display> {
    fn read_back(
        &self,
        image: &DmaBufImage,
        width: u32,
        height: u32,
    ) -> Result<Vec<Vec<u8>>, DecodeError> {
        use rusplay_va::{DmaBufImport, DmaBufObjectRef, DmaBufPlaneLayout, Image};

        let objects: Vec<DmaBufObjectRef<'_>> = image
            .objects
            .iter()
            .map(|object| DmaBufObjectRef {
                fd: std::os::fd::AsFd::as_fd(&object.fd),
                size: u32::try_from(object.size).unwrap_or(u32::MAX),
            })
            .collect();
        let planes: Vec<DmaBufPlaneLayout> = image
            .planes
            .iter()
            .map(|plane| DmaBufPlaneLayout {
                object: u32::try_from(plane.object).unwrap_or(0),
                offset: u32::try_from(plane.offset).unwrap_or(0),
                pitch: u32::try_from(plane.pitch).unwrap_or(0),
            })
            .collect();
        let rt_format = if image.fourcc == DrmFourcc::NV12 {
            ffi::VA_RT_FORMAT_YUV420
        } else {
            ffi::VA_RT_FORMAT_YUV420_10
        };

        let surface = Surface::import(
            self,
            rt_format,
            &DmaBufImport {
                va_fourcc: image.fourcc.as_u32(),
                drm_fourcc: image.fourcc.as_u32(),
                resolution: Resolution::new(image.width, image.height),
                modifier: image.modifier,
                objects: &objects,
                planes: &planes,
            },
            None,
        )?;

        let mut va_image = Image::new(self, image.fourcc.as_u32(), Resolution::new(width, height))?;
        let data = va_image.get(&surface)?;
        let layout = va_image.layout();

        let bytes_per_sample = if image.fourcc == DrmFourcc::NV12 {
            1
        } else {
            2
        };
        let row_bytes = usize::try_from(width).unwrap_or(0) * bytes_per_sample;
        let plane_rows = [
            usize::try_from(height).unwrap_or(0),
            usize::try_from(height.div_ceil(2)).unwrap_or(0),
        ];

        Ok((0..2)
            .map(|plane| {
                (0..plane_rows[plane])
                    .flat_map(|row| {
                        let start = layout.offsets[plane] + row * layout.pitches[plane];
                        data[start..start + row_bytes].iter().copied()
                    })
                    .collect()
            })
            .collect())
    }
}
