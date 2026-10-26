use std::{
    ffi::c_void,
    fs::File,
    io::Write,
    os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd},
};

use rusplay_core::{
    BitDepth, ChromaSubsampling, ColorRange, HdrLevels, MatrixCoefficients, PixelFormat,
    Resolution, ScaleFilter, TransferFunction,
};
use rustix::{
    fs::{Mode, OFlags, SealFlags},
    ioctl::{Ioctl, IoctlOutput, Opcode, opcode},
};

use super::*;
use crate::{
    ChromaSiting, ColorConverter, ConvertSettings, DEFAULT_ROW_ALIGNMENT, FrameLayout, InputLight,
    LineariseSettings, Lineariser, OutputLight, PlaneLayout, RgbConvertSettings, RgbConverter,
    ScalePipelines,
    testing::{self, Gpu, Image, RGBA16F_TOLERANCE},
};

const DRM_FORMAT_MOD_LINEAR: u64 = 0;
const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;
const PAGE_BYTES: usize = 4096;
const UDMABUF_FLAGS_CLOEXEC: u32 = 1;
const RGB_STRIDE: u64 = 256;
const RGB_TEXEL_BYTES: u32 = 4;

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
        opcode::write::<UdmabufCreate>(b'u', 0x42)
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

fn udmabuf(bytes: &[u8]) -> Option<OwnedFd> {
    let device = rustix::fs::open(
        "/dev/udmabuf",
        OFlags::RDWR | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    let size = bytes.len().next_multiple_of(PAGE_BYTES);

    let memfd = rustix::fs::memfd_create(
        "rusplay-dmabuf-test",
        rustix::fs::MemfdFlags::ALLOW_SEALING | rustix::fs::MemfdFlags::CLOEXEC,
    )
    .unwrap();
    rustix::fs::ftruncate(&memfd, size as u64).unwrap();
    File::from(memfd.try_clone().unwrap())
        .write_all(bytes)
        .unwrap();
    rustix::fs::fcntl_add_seals(&memfd, SealFlags::SHRINK).unwrap();

    let create = UdmabufCreate {
        memfd: u32::try_from(memfd.as_raw_fd()).unwrap(),
        flags: UDMABUF_FLAGS_CLOEXEC,
        offset: 0,
        size: size as u64,
    };
    unsafe { rustix::ioctl::ioctl(&device, CreateUdmabuf(create)) }.ok()
}

struct Fixture {
    gpu: &'static Gpu,
    importer: DmaBufImporter,
}

fn fixture() -> Option<Fixture> {
    let gpu = testing::gpu()?;
    let importer = DmaBufImporter::new(&gpu.device).ok()?;
    udmabuf(&[0]).map(|_| Fixture { gpu, importer })
}

fn single_plane(fd: &OwnedFd, fourcc: u32, size: (u32, u32), stride: u64) -> DmaBuf<'_> {
    DmaBuf {
        fourcc,
        modifier: DRM_FORMAT_MOD_LINEAR,
        width: size.0,
        height: size.1,
        planes: vec![DmaBufPlane {
            fd: fd.as_fd(),
            offset: 0,
            stride,
        }],
    }
}

fn rgb_bytes(size: (u32, u32), seed: u32) -> Vec<u8> {
    let stride = u32::try_from(RGB_STRIDE).unwrap();

    (0..size.1)
        .flat_map(|y| {
            (0..stride).map(move |x| {
                let mixed = x
                    .wrapping_mul(0x9E37_79B1)
                    .wrapping_add(y.wrapping_mul(0x85EB_CA77))
                    .wrapping_add(seed);
                (mixed >> 13).to_le_bytes()[0]
            })
        })
        .collect()
}

#[test]
fn rgb_formats_import_without_conversion() {
    let Some(mut fixture) = fixture() else {
        return;
    };
    let size = (37, 23);
    let tight_row = (size.0 * RGB_TEXEL_BYTES) as usize;

    for format in [
        DmaBufFormat::Xrgb8888,
        DmaBufFormat::Abgr8888,
        DmaBufFormat::Abgr2101010,
    ] {
        let bytes = rgb_bytes(size, format.fourcc());
        let fd = udmabuf(&bytes).unwrap();

        let texture = fixture
            .importer
            .import(&single_plane(&fd, format.fourcc(), size, RGB_STRIDE))
            .unwrap();
        let read = testing::download_raw(fixture.gpu, texture.texture(0).unwrap(), RGB_TEXEL_BYTES);
        let expected: Vec<u8> = bytes
            .chunks_exact(usize::try_from(RGB_STRIDE).unwrap())
            .flat_map(|row| &row[..tight_row])
            .copied()
            .collect();

        assert_eq!(texture.format(), format);
        assert_eq!(texture.plane_count(), 1);
        assert_eq!(read, expected, "{format:?}");
        assert_eq!(
            texture.linear_view().is_some(),
            format != DmaBufFormat::Abgr2101010,
            "{format:?}"
        );
    }
}

fn drm_rgb(format: DmaBufFormat, word: u32) -> Option<[f32; 3]> {
    let ten = |shift: u32| f32::from(u16::try_from((word >> shift) & 1023).unwrap()) / 1023.0;
    let eight = |shift: u32| f32::from(u8::try_from((word >> shift) & 255).unwrap()) / 255.0;

    Some(match format {
        DmaBufFormat::Xrgb8888 | DmaBufFormat::Argb8888 => [eight(16), eight(8), eight(0)],
        DmaBufFormat::Xbgr8888 | DmaBufFormat::Abgr8888 => [eight(0), eight(8), eight(16)],
        DmaBufFormat::Bgrx8888 => [eight(8), eight(16), eight(24)],
        DmaBufFormat::Rgbx8888 => [eight(24), eight(16), eight(8)],
        DmaBufFormat::Xrgb2101010 | DmaBufFormat::Argb2101010 => [ten(20), ten(10), ten(0)],
        DmaBufFormat::Xbgr2101010 | DmaBufFormat::Abgr2101010 => [ten(0), ten(10), ten(20)],
        DmaBufFormat::Rgbx1010102 => [ten(22), ten(12), ten(2)],
        DmaBufFormat::Bgrx1010102 => [ten(2), ten(12), ten(22)],
        DmaBufFormat::Nv12 | DmaBufFormat::P010 => return None,
    })
}

#[test]
fn every_rgb_format_reads_back_in_drm_channel_order() {
    let Some(mut fixture) = fixture() else {
        return;
    };
    let pipelines = ScalePipelines::new(&fixture.gpu.device);
    let size = (37, 23);

    for format in DmaBufFormat::ALL {
        let Some(channels) = format.channels() else {
            continue;
        };
        let bytes = rgb_bytes(size, format.fourcc());
        let fd = udmabuf(&bytes).unwrap();
        let texture = fixture
            .importer
            .import(&single_plane(&fd, format.fourcc(), size, RGB_STRIDE))
            .unwrap();
        let mut lineariser = Lineariser::new(
            &pipelines,
            &fixture.gpu.device,
            LineariseSettings::encoded(channels),
        )
        .unwrap();
        let target = testing::storage_target(fixture.gpu, size.0, size.1);

        testing::submit(fixture.gpu, |encoder| {
            lineariser
                .encode(
                    &fixture.gpu.device,
                    encoder,
                    texture.view(0).unwrap(),
                    &testing::view(&target),
                )
                .unwrap();
        });
        let read = testing::download(fixture.gpu, &target);

        assert_eq!(texture.channels(), Some(channels));
        for (index, texel) in read.pixels.iter().enumerate() {
            let (x, y) = (index % size.0 as usize, index / size.0 as usize);
            let at = y * usize::try_from(RGB_STRIDE).unwrap() + x * RGB_TEXEL_BYTES as usize;
            let word = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
            let want = drm_rgb(format, word).unwrap();
            for (got, want) in texel.iter().zip(want) {
                assert!(
                    (got - want).abs() <= RGBA16F_TOLERANCE / 2.0,
                    "{format:?} at {x},{y}: {texel:?} for {word:#010x}"
                );
            }
            if channels.opaque {
                assert!((texel[3] - 1.0).abs() < f32::EPSILON, "{format:?}");
            }
        }
    }
}

fn yuv_settings(size: (u32, u32), depth: BitDepth) -> RgbConvertSettings {
    RgbConvertSettings {
        resolution: Resolution::new(size.0, size.1),
        format: PixelFormat {
            chroma: ChromaSubsampling::Yuv420,
            depth,
        },
        layout: PlaneLayout::SemiPlanar,
        matrix: MatrixCoefficients::Bt709,
        range: ColorRange::Limited,
        transfer: TransferFunction::Srgb,
        output: OutputLight::Linear,
        chroma_filter: ScaleFilter::Lanczos3,
        chroma_siting: ChromaSiting::Left,
        row_alignment: DEFAULT_ROW_ALIGNMENT,
    }
}

fn encode_yuv(gpu: &Gpu, pipelines: &ScalePipelines, settings: &RgbConvertSettings) -> Vec<u8> {
    let (width, height) = (settings.resolution.width, settings.resolution.height);
    let image = Image::pattern(width, height, 11);

    let mut converter = ColorConverter::new(
        pipelines,
        &gpu.device,
        ConvertSettings {
            resolution: settings.resolution,
            format: settings.format,
            layout: settings.layout,
            matrix: settings.matrix,
            range: settings.range,
            transfer: settings.transfer,
            hdr: HdrLevels::default(),
            input: InputLight::Linear,
            chroma_filter: ScaleFilter::Hermite,
            chroma_siting: settings.chroma_siting,
            row_alignment: settings.row_alignment,
        },
    )
    .unwrap();
    let frame = converter.create_frame(&gpu.device);
    let source = testing::upload(gpu, &image);

    testing::submit(gpu, |encoder| {
        converter
            .encode(&gpu.device, encoder, &testing::view(&source), &frame)
            .unwrap();
    });
    testing::download_buffer(gpu, frame.buffer())
}

fn semi_planar<'a>(fd: &'a OwnedFd, format: DmaBufFormat, layout: &FrameLayout) -> DmaBuf<'a> {
    let luma = layout.planes[0];
    let chroma = layout.planes[1];

    DmaBuf {
        fourcc: format.fourcc(),
        modifier: DRM_FORMAT_MOD_LINEAR,
        width: luma.width,
        height: luma.height,
        planes: [luma, chroma]
            .iter()
            .map(|plane| DmaBufPlane {
                fd: fd.as_fd(),
                offset: plane.offset,
                stride: u64::from(plane.stride),
            })
            .collect(),
    }
}

#[test]
fn semi_planar_dmabufs_unpack_like_the_upload_buffer() {
    let Some(mut fixture) = fixture() else {
        return;
    };
    let gpu = fixture.gpu;
    let pipelines = ScalePipelines::new(&gpu.device);

    for (format, depth) in [
        (DmaBufFormat::Nv12, BitDepth::Eight),
        (DmaBufFormat::P010, BitDepth::Ten),
    ] {
        for size in [(64, 36), (37, 23)] {
            let settings = yuv_settings(size, depth);
            let mut converter = RgbConverter::new(&pipelines, &gpu.device, settings).unwrap();
            let bytes = encode_yuv(gpu, &pipelines, &settings);
            let fd = udmabuf(&bytes).unwrap();
            let upload = converter.create_frame(&gpu.device);
            let from_buffer = testing::storage_target(gpu, size.0, size.1);
            let from_planes = testing::storage_target(gpu, size.0, size.1);

            let texture = fixture
                .importer
                .import(&semi_planar(&fd, format, converter.layout()))
                .unwrap();
            gpu.queue.write_buffer(upload.buffer(), 0, &bytes);
            testing::submit(gpu, |encoder| {
                converter
                    .encode(&gpu.device, encoder, &upload, &testing::view(&from_buffer))
                    .unwrap();
                converter
                    .encode_planes(&gpu.device, encoder, &texture, &testing::view(&from_planes))
                    .unwrap();
            });
            let expected = testing::download(gpu, &from_buffer);
            let actual = testing::download(gpu, &from_planes);

            assert_eq!(texture.plane_count(), 2);
            assert_eq!(actual.max_difference(&expected), 0.0, "{format:?} {size:?}");
        }
    }
}

#[test]
fn repeated_imports_reuse_the_texture() {
    let Some(mut fixture) = fixture() else {
        return;
    };
    let size = (16, 8);
    let fd = udmabuf(&rgb_bytes(size, 3)).unwrap();
    let fourcc = DmaBufFormat::Xrgb8888.fourcc();

    let first = fixture
        .importer
        .import(&single_plane(&fd, fourcc, size, RGB_STRIDE))
        .unwrap();
    let again = fixture
        .importer
        .import(&single_plane(&fd, fourcc, size, RGB_STRIDE))
        .unwrap();
    let narrower = fixture
        .importer
        .import(&single_plane(&fd, fourcc, (8, 8), RGB_STRIDE))
        .unwrap();

    assert_eq!(first.view(0), again.view(0));
    assert_ne!(first.view(0), narrower.view(0));
}

#[test]
fn importable_modifiers_include_linear() {
    let Some(fixture) = fixture() else {
        return;
    };

    for format in DmaBufFormat::ALL {
        let modifiers = fixture.importer.modifiers(format);

        assert!(modifiers.contains(&DRM_FORMAT_MOD_LINEAR), "{format:?}");
        assert!(!modifiers.contains(&DRM_FORMAT_MOD_INVALID), "{format:?}");
    }
}

#[test]
fn malformed_dmabufs_are_rejected() {
    let Some(mut fixture) = fixture() else {
        return;
    };
    let size = (16, 8);
    let fd = udmabuf(&rgb_bytes(size, 5)).unwrap();
    let xrgb = DmaBufFormat::Xrgb8888.fourcc();
    let nv12 = DmaBufFormat::Nv12.fourcc();
    let yuyv = u32::from_le_bytes(*b"YUYV");

    let unknown = fixture
        .importer
        .import(&single_plane(&fd, yuyv, size, RGB_STRIDE));
    let implicit = fixture.importer.import(&DmaBuf {
        modifier: DRM_FORMAT_MOD_INVALID,
        ..single_plane(&fd, xrgb, size, RGB_STRIDE)
    });
    let one_plane_nv12 = fixture
        .importer
        .import(&single_plane(&fd, nv12, size, RGB_STRIDE));
    let mut past_end = single_plane(&fd, xrgb, size, RGB_STRIDE);
    past_end.planes[0].offset = PAGE_BYTES as u64;
    let past_end = fixture.importer.import(&past_end);

    assert_eq!(
        unknown.unwrap_err(),
        ScaleError::DmaBufFormatUnsupported(yuyv)
    );
    assert_eq!(
        implicit.unwrap_err(),
        ScaleError::DmaBufModifierUnsupported {
            fourcc: xrgb,
            modifier: DRM_FORMAT_MOD_INVALID,
        }
    );
    assert_eq!(
        one_plane_nv12.unwrap_err(),
        ScaleError::DmaBufPlaneCount {
            fourcc: nv12,
            modifier: DRM_FORMAT_MOD_LINEAR,
            expected: 2,
            found: 1,
        }
    );
    assert_eq!(
        past_end.unwrap_err(),
        ScaleError::DmaBufPlaneOutOfBounds {
            plane: 0,
            offset: PAGE_BYTES as u64,
            size: PAGE_BYTES as u64,
        }
    );
}

#[test]
fn converters_refuse_planes_of_another_format() {
    let Some(mut fixture) = fixture() else {
        return;
    };
    let gpu = fixture.gpu;
    let pipelines = ScalePipelines::new(&gpu.device);
    let size = (32, 16);
    let ten_bit = yuv_settings(size, BitDepth::Ten);
    let layout = RgbConverter::new(&pipelines, &gpu.device, yuv_settings(size, BitDepth::Eight))
        .unwrap()
        .layout()
        .clone();
    let fd = udmabuf(&vec![0; usize::try_from(layout.size).unwrap()]).unwrap();
    let target = testing::storage_target(gpu, size.0, size.1);

    let texture = fixture
        .importer
        .import(&semi_planar(&fd, DmaBufFormat::Nv12, &layout))
        .unwrap();
    let mut converter = RgbConverter::new(&pipelines, &gpu.device, ten_bit).unwrap();
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    let result =
        converter.encode_planes(&gpu.device, &mut encoder, &texture, &testing::view(&target));

    assert_eq!(
        result,
        Err(ScaleError::DmaBufFormatMismatch {
            fourcc: DmaBufFormat::Nv12.fourcc(),
            format: ten_bit.format,
            layout: PlaneLayout::SemiPlanar,
        })
    );
}

#[test]
fn devices_without_dmabuf_features_cannot_import() {
    if testing::gpu().is_none() {
        return;
    }
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..wgpu::RequestAdapterOptions::default()
    }))
    .unwrap();
    let (plain, _queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();

    let without = DmaBufImporter::new(&plain);

    assert_eq!(without.unwrap_err(), ScaleError::DmaBufImportUnavailable);
}
