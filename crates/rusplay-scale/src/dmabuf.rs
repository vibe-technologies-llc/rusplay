mod fence;
#[cfg(test)]
mod tests;
mod vulkan;

use std::{collections::VecDeque, os::fd::BorrowedFd};

use ash::vk;
use rusplay_core::{BitDepth, ChromaSubsampling, PixelFormat, Resolution};

pub use self::fence::{WriteFence, wait_for_writes};
use self::vulkan::{ImageRequest, MemoryPlane, ModifierProperties, VulkanImporter};
use crate::{ChannelOrder, ScaleError, SourceChannels};

pub const DMABUF_FEATURES: wgpu::Features = wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF;

const IMPORT_CACHE_ENTRIES: usize = 16;

pub fn device_descriptor(adapter: &wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> {
    wgpu::DeviceDescriptor {
        label: Some("rusplay"),
        required_features: adapter.features() & DMABUF_FEATURES,
        ..wgpu::DeviceDescriptor::default()
    }
}

pub fn release_after_submission(queue: &wgpu::Queue, lease: impl Send + 'static) {
    queue.on_submitted_work_done(move || drop(lease));
}

pub(crate) fn fourcc_name(fourcc: u32) -> String {
    fourcc.to_le_bytes().escape_ascii().to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DmaBufFormat {
    Xrgb8888,
    Argb8888,
    Xbgr8888,
    Abgr8888,
    Xbgr2101010,
    Abgr2101010,
    Xrgb2101010,
    Argb2101010,
    Bgrx8888,
    Rgbx8888,
    Rgbx1010102,
    Bgrx1010102,
    Nv12,
    P010,
}

#[derive(Debug, Clone, Copy)]
struct PlaneSpec {
    vulkan: vk::Format,
    texture: wgpu::TextureFormat,
    linear: Option<wgpu::TextureFormat>,
    view_formats: &'static [vk::Format],
    subsampled: bool,
}

const BGRA8: PlaneSpec = PlaneSpec {
    vulkan: vk::Format::B8G8R8A8_UNORM,
    texture: wgpu::TextureFormat::Bgra8Unorm,
    linear: Some(wgpu::TextureFormat::Bgra8UnormSrgb),
    view_formats: &[vk::Format::B8G8R8A8_UNORM, vk::Format::B8G8R8A8_SRGB],
    subsampled: false,
};
const RGBA8: PlaneSpec = PlaneSpec {
    vulkan: vk::Format::R8G8B8A8_UNORM,
    texture: wgpu::TextureFormat::Rgba8Unorm,
    linear: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
    view_formats: &[vk::Format::R8G8B8A8_UNORM, vk::Format::R8G8B8A8_SRGB],
    subsampled: false,
};
const RGB10A2: PlaneSpec = PlaneSpec {
    vulkan: vk::Format::A2B10G10R10_UNORM_PACK32,
    texture: wgpu::TextureFormat::Rgb10a2Unorm,
    linear: None,
    view_formats: &[],
    subsampled: false,
};
const RGBA8_PLAIN: PlaneSpec = PlaneSpec {
    vulkan: vk::Format::R8G8B8A8_UNORM,
    texture: wgpu::TextureFormat::Rgba8Unorm,
    linear: None,
    view_formats: &[],
    subsampled: false,
};
const PACKED32: PlaneSpec = PlaneSpec {
    vulkan: vk::Format::R32_UINT,
    texture: wgpu::TextureFormat::R32Uint,
    linear: None,
    view_formats: &[],
    subsampled: false,
};
const LUMA8: PlaneSpec = PlaneSpec {
    vulkan: vk::Format::R8_UINT,
    texture: wgpu::TextureFormat::R8Uint,
    linear: None,
    view_formats: &[],
    subsampled: false,
};
const CHROMA8: PlaneSpec = PlaneSpec {
    vulkan: vk::Format::R8G8_UINT,
    texture: wgpu::TextureFormat::Rg8Uint,
    linear: None,
    view_formats: &[],
    subsampled: true,
};
const LUMA16: PlaneSpec = PlaneSpec {
    vulkan: vk::Format::R16_UINT,
    texture: wgpu::TextureFormat::R16Uint,
    linear: None,
    view_formats: &[],
    subsampled: false,
};
const CHROMA16: PlaneSpec = PlaneSpec {
    vulkan: vk::Format::R16G16_UINT,
    texture: wgpu::TextureFormat::Rg16Uint,
    linear: None,
    view_formats: &[],
    subsampled: true,
};

impl DmaBufFormat {
    pub const ALL: [Self; 14] = [
        Self::Xrgb8888,
        Self::Argb8888,
        Self::Xbgr8888,
        Self::Abgr8888,
        Self::Xbgr2101010,
        Self::Abgr2101010,
        Self::Xrgb2101010,
        Self::Argb2101010,
        Self::Bgrx8888,
        Self::Rgbx8888,
        Self::Rgbx1010102,
        Self::Bgrx1010102,
        Self::Nv12,
        Self::P010,
    ];

    pub const fn fourcc(self) -> u32 {
        u32::from_le_bytes(match self {
            Self::Xrgb8888 => *b"XR24",
            Self::Argb8888 => *b"AR24",
            Self::Xbgr8888 => *b"XB24",
            Self::Abgr8888 => *b"AB24",
            Self::Xbgr2101010 => *b"XB30",
            Self::Abgr2101010 => *b"AB30",
            Self::Xrgb2101010 => *b"XR30",
            Self::Argb2101010 => *b"AR30",
            Self::Bgrx8888 => *b"BX24",
            Self::Rgbx8888 => *b"RX24",
            Self::Rgbx1010102 => *b"RX30",
            Self::Bgrx1010102 => *b"BX30",
            Self::Nv12 => *b"NV12",
            Self::P010 => *b"P010",
        })
    }

    pub fn from_fourcc(fourcc: u32) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|format| format.fourcc() == fourcc)
    }

    pub const fn pixel_format(self) -> Option<PixelFormat> {
        let depth = match self {
            Self::Nv12 => BitDepth::Eight,
            Self::P010 => BitDepth::Ten,
            Self::Xrgb8888
            | Self::Argb8888
            | Self::Xbgr8888
            | Self::Abgr8888
            | Self::Xbgr2101010
            | Self::Abgr2101010
            | Self::Xrgb2101010
            | Self::Argb2101010
            | Self::Bgrx8888
            | Self::Rgbx8888
            | Self::Rgbx1010102
            | Self::Bgrx1010102 => return None,
        };

        Some(PixelFormat {
            chroma: ChromaSubsampling::Yuv420,
            depth,
        })
    }

    pub const fn channels(self) -> Option<SourceChannels> {
        Some(match self {
            Self::Xrgb8888 | Self::Xbgr8888 | Self::Xbgr2101010 => SourceChannels::RGBX,
            Self::Argb8888 | Self::Abgr8888 | Self::Abgr2101010 => SourceChannels::RGBA,
            Self::Xrgb2101010 => SourceChannels::float(ChannelOrder::Bgra, true),
            Self::Argb2101010 => SourceChannels::float(ChannelOrder::Bgra, false),
            Self::Bgrx8888 => SourceChannels::float(ChannelOrder::Argb, true),
            Self::Rgbx8888 => SourceChannels::float(ChannelOrder::Abgr, true),
            Self::Rgbx1010102 => SourceChannels::packed(ChannelOrder::Bgra, true),
            Self::Bgrx1010102 => SourceChannels::packed(ChannelOrder::Rgba, true),
            Self::Nv12 | Self::P010 => return None,
        })
    }

    const fn planes(self) -> &'static [PlaneSpec] {
        match self {
            Self::Xrgb8888 | Self::Argb8888 => &[BGRA8],
            Self::Xbgr8888 | Self::Abgr8888 => &[RGBA8],
            Self::Xbgr2101010 | Self::Abgr2101010 | Self::Xrgb2101010 | Self::Argb2101010 => {
                &[RGB10A2]
            }
            Self::Bgrx8888 | Self::Rgbx8888 => &[RGBA8_PLAIN],
            Self::Rgbx1010102 | Self::Bgrx1010102 => &[PACKED32],
            Self::Nv12 => &[LUMA8, CHROMA8],
            Self::P010 => &[LUMA16, CHROMA16],
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DmaBufPlane<'a> {
    pub fd: BorrowedFd<'a>,
    pub offset: u64,
    pub stride: u64,
}

#[derive(Debug, Clone)]
pub struct DmaBuf<'a> {
    pub fourcc: u32,
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
    pub planes: Vec<DmaBufPlane<'a>>,
}

#[derive(Debug, Clone)]
struct ImportedPlane {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    linear_view: Option<wgpu::TextureView>,
}

#[derive(Debug, Clone)]
pub struct DmaBufTexture {
    format: DmaBufFormat,
    resolution: Resolution,
    planes: Vec<ImportedPlane>,
}

impl DmaBufTexture {
    pub fn format(&self) -> DmaBufFormat {
        self.format
    }

    pub fn resolution(&self) -> Resolution {
        self.resolution
    }

    pub fn plane_count(&self) -> usize {
        self.planes.len()
    }

    pub fn texture(&self, plane: usize) -> Option<&wgpu::Texture> {
        self.planes.get(plane).map(|imported| &imported.texture)
    }

    pub fn view(&self, plane: usize) -> Option<&wgpu::TextureView> {
        self.planes.get(plane).map(|imported| &imported.view)
    }

    pub fn channels(&self) -> Option<SourceChannels> {
        self.format.channels()
    }

    pub fn linear_view(&self) -> Option<&wgpu::TextureView> {
        self.planes.first()?.linear_view.as_ref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BufferIdentity {
    device: u64,
    inode: u64,
    size: u64,
}

impl BufferIdentity {
    fn of(fd: BorrowedFd<'_>) -> Result<Self, ScaleError> {
        let stat = rustix::fs::fstat(fd).map_err(ScaleError::DmaBufFdStat)?;

        Ok(Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            size: u64::try_from(stat.st_size).unwrap_or_default(),
        })
    }

    fn same_buffer(self, other: Self) -> bool {
        (self.device, self.inode) == (other.device, other.inode)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImportKey {
    fourcc: u32,
    modifier: u64,
    width: u32,
    height: u32,
    planes: Vec<(BufferIdentity, u64, u64)>,
}

#[derive(Debug)]
pub struct DmaBufImporter {
    device: wgpu::Device,
    vulkan: VulkanImporter,
    cache: VecDeque<(ImportKey, DmaBufTexture)>,
}

impl DmaBufImporter {
    pub fn new(device: &wgpu::Device) -> Result<Self, ScaleError> {
        Ok(Self {
            device: device.clone(),
            vulkan: VulkanImporter::new(device)?,
            cache: VecDeque::with_capacity(IMPORT_CACHE_ENTRIES),
        })
    }

    pub fn modifiers(&self, format: DmaBufFormat) -> Vec<u64> {
        let specs = format.planes();
        let per_plane: Vec<Vec<ModifierProperties>> =
            specs.iter().map(|spec| self.importable(spec)).collect();

        let Some((first, rest)) = per_plane.split_first() else {
            return Vec::new();
        };
        first
            .iter()
            .map(|properties| properties.modifier)
            .filter(|modifier| {
                rest.iter()
                    .all(|plane| plane.iter().any(|other| other.modifier == *modifier))
            })
            .collect()
    }

    fn importable(&self, spec: &PlaneSpec) -> Vec<ModifierProperties> {
        self.vulkan
            .modifiers(spec.vulkan)
            .into_iter()
            .filter(|properties| properties.plane_count == 1)
            .filter(|properties| self.request_for(spec, properties).is_some())
            .collect()
    }

    fn request_for(
        &self,
        spec: &PlaneSpec,
        properties: &ModifierProperties,
    ) -> Option<(ImageRequest<'static>, (u32, u32))> {
        let plain = ImageRequest {
            format: spec.vulkan,
            view_formats: &[],
            modifier: properties.modifier,
            copyable: properties.copyable,
        };
        let mutable = spec.linear.map(|_| ImageRequest {
            view_formats: spec.view_formats,
            ..plain
        });

        mutable
            .into_iter()
            .chain([plain])
            .find_map(|request| Some((request, self.vulkan.max_extent(&request)?)))
    }

    pub fn import(&mut self, dmabuf: &DmaBuf<'_>) -> Result<DmaBufTexture, ScaleError> {
        let format = DmaBufFormat::from_fourcc(dmabuf.fourcc)
            .ok_or(ScaleError::DmaBufFormatUnsupported(dmabuf.fourcc))?;
        let identities = dmabuf
            .planes
            .iter()
            .map(|plane| BufferIdentity::of(plane.fd))
            .collect::<Result<Vec<_>, _>>()?;
        let key = ImportKey {
            fourcc: dmabuf.fourcc,
            modifier: dmabuf.modifier,
            width: dmabuf.width,
            height: dmabuf.height,
            planes: identities
                .iter()
                .zip(&dmabuf.planes)
                .map(|(identity, plane)| (*identity, plane.offset, plane.stride))
                .collect(),
        };

        if let Some((_, texture)) = self.cache.iter().find(|(cached, _)| *cached == key) {
            return Ok(texture.clone());
        }

        let texture = self.import_uncached(format, dmabuf, &identities)?;
        if self.cache.len() == IMPORT_CACHE_ENTRIES {
            self.cache.pop_front();
        }
        self.cache.push_back((key, texture.clone()));
        Ok(texture)
    }

    fn import_uncached(
        &self,
        format: DmaBufFormat,
        dmabuf: &DmaBuf<'_>,
        identities: &[BufferIdentity],
    ) -> Result<DmaBufTexture, ScaleError> {
        let specs = format.planes();
        let groups = plane_groups(dmabuf, specs.len(), identities)?;

        let planes = specs
            .iter()
            .zip(groups)
            .map(|(spec, group)| self.import_plane(dmabuf, spec, group, identities))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(DmaBufTexture {
            format,
            resolution: Resolution::new(dmabuf.width, dmabuf.height),
            planes,
        })
    }

    fn import_plane(
        &self,
        dmabuf: &DmaBuf<'_>,
        spec: &PlaneSpec,
        group: PlaneGroup,
        identities: &[BufferIdentity],
    ) -> Result<ImportedPlane, ScaleError> {
        let unsupported = ScaleError::DmaBufModifierUnsupported {
            fourcc: dmabuf.fourcc,
            modifier: dmabuf.modifier,
        };
        let properties = self
            .vulkan
            .modifiers(spec.vulkan)
            .into_iter()
            .find(|properties| properties.modifier == dmabuf.modifier)
            .ok_or_else(|| unsupported.clone())?;
        if properties.plane_count != group.count {
            return Err(ScaleError::DmaBufPlaneCount {
                fourcc: dmabuf.fourcc,
                modifier: dmabuf.modifier,
                expected: properties.plane_count,
                found: group.count,
            });
        }

        let (width, height) = if spec.subsampled {
            (dmabuf.width.div_ceil(2), dmabuf.height.div_ceil(2))
        } else {
            (dmabuf.width, dmabuf.height)
        };
        let (request, max_extent) = self.request_for(spec, &properties).ok_or(unsupported)?;
        if width > max_extent.0 || height > max_extent.1 || width == 0 || height == 0 {
            return Err(ScaleError::DmaBufExtentUnsupported { width, height });
        }

        let memory_planes = (group.first..group.first + group.count)
            .map(|index| {
                let plane = dmabuf.planes[index];
                if plane.offset >= identities[index].size {
                    return Err(ScaleError::DmaBufPlaneOutOfBounds {
                        plane: index,
                        offset: plane.offset,
                        size: identities[index].size,
                    });
                }
                Ok(MemoryPlane {
                    offset: plane.offset,
                    stride: plane.stride,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let linear = spec.linear.filter(|_| !request.view_formats.is_empty());
        let view_formats: Vec<wgpu::TextureFormat> = linear.into_iter().collect();
        let usage = if request.copyable {
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC
        } else {
            wgpu::TextureUsages::TEXTURE_BINDING
        };
        let desc = wgpu::TextureDescriptor {
            label: Some("rusplay-dmabuf"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: spec.texture,
            usage,
            view_formats: &view_formats,
        };

        let texture = self.vulkan.import(
            &self.device,
            &request,
            dmabuf.planes[group.first].fd,
            &memory_planes,
            &desc,
        )?;
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let linear_view = linear.map(|format| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                format: Some(format),
                ..wgpu::TextureViewDescriptor::default()
            })
        });

        Ok(ImportedPlane {
            texture,
            view,
            linear_view,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct PlaneGroup {
    first: usize,
    count: usize,
}

fn plane_groups(
    dmabuf: &DmaBuf<'_>,
    textures: usize,
    identities: &[BufferIdentity],
) -> Result<Vec<PlaneGroup>, ScaleError> {
    let found = dmabuf.planes.len();
    if found == 0 || (textures > 1 && found != textures) {
        return Err(ScaleError::DmaBufPlaneCount {
            fourcc: dmabuf.fourcc,
            modifier: dmabuf.modifier,
            expected: textures,
            found,
        });
    }

    if textures == 1 {
        if let Some(split) = identities
            .iter()
            .position(|identity| !identity.same_buffer(identities[0]))
        {
            return Err(ScaleError::DmaBufPlanesSplit { plane: split });
        }
        return Ok(vec![PlaneGroup {
            first: 0,
            count: found,
        }]);
    }

    Ok((0..textures)
        .map(|first| PlaneGroup { first, count: 1 })
        .collect())
}
