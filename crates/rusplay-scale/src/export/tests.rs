use std::os::fd::AsFd;

use rusplay_core::{
    BitDepth, ChromaSubsampling, ColorRange, HdrLevels, MatrixCoefficients, PixelFormat,
    Resolution, ScaleFilter, TransferFunction,
};
use rustix::fs::{SeekFrom, seek};

use super::*;
use crate::{
    ChromaSiting, ConvertSettings, DEFAULT_ROW_ALIGNMENT, DmaBuf, DmaBufFormat, DmaBufImporter,
    DmaBufPlane, FrameLayout, InputLight, OutputLight, PlaneLayout, RgbConvertSettings,
    RgbConverter, ScalePipelines,
    testing::{self, Gpu, Image},
};

const DRM_FORMAT_MOD_LINEAR: u64 = 0;

fn convert_settings(size: (u32, u32), depth: BitDepth, layout: PlaneLayout) -> ConvertSettings {
    ConvertSettings {
        resolution: Resolution::new(size.0, size.1),
        format: PixelFormat {
            chroma: ChromaSubsampling::Yuv420,
            depth,
        },
        layout,
        matrix: MatrixCoefficients::Bt709,
        range: ColorRange::Limited,
        transfer: TransferFunction::Srgb,
        hdr: HdrLevels::default(),
        input: InputLight::Linear,
        chroma_filter: ScaleFilter::Hermite,
        chroma_siting: ChromaSiting::Left,
        row_alignment: DEFAULT_ROW_ALIGNMENT,
    }
}

fn unpack_settings(settings: &ConvertSettings) -> RgbConvertSettings {
    RgbConvertSettings {
        resolution: settings.resolution,
        format: settings.format,
        layout: settings.layout,
        matrix: settings.matrix,
        range: settings.range,
        transfer: settings.transfer,
        output: OutputLight::Linear,
        chroma_filter: ScaleFilter::Lanczos3,
        chroma_siting: settings.chroma_siting,
        row_alignment: settings.row_alignment,
    }
}

fn convert_into(gpu: &Gpu, converter: &mut ColorConverter, image: &Image, frame: &YuvFrame) {
    let source = testing::upload(gpu, image);

    testing::submit(gpu, |encoder| {
        converter
            .encode(&gpu.device, encoder, &testing::view(&source), frame)
            .unwrap();
    });
}

fn exported_planes<'a>(
    exported: &'a ExportedFrame,
    format: DmaBufFormat,
    layout: &FrameLayout,
) -> DmaBuf<'a> {
    DmaBuf {
        fourcc: format.fourcc(),
        modifier: DRM_FORMAT_MOD_LINEAR,
        width: layout.planes[0].width,
        height: layout.planes[0].height,
        planes: layout.planes[..2]
            .iter()
            .map(|plane| DmaBufPlane {
                fd: exported.fd(),
                offset: plane.offset,
                stride: u64::from(plane.stride),
            })
            .collect(),
    }
}

#[test]
fn exported_dma_bufs_hold_what_the_converter_writes() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let Ok(mut importer) = DmaBufImporter::new(&gpu.device) else {
        return;
    };
    if !can_export(&gpu.device, MemoryHandle::DmaBuf) {
        return;
    }
    let pipelines = ScalePipelines::new(&gpu.device);

    for (format, depth) in [
        (DmaBufFormat::Nv12, BitDepth::Eight),
        (DmaBufFormat::P010, BitDepth::Ten),
    ] {
        for size in [(64, 36), (37, 23)] {
            let settings = convert_settings(size, depth, PlaneLayout::SemiPlanar);
            let image = Image::pattern(size.0, size.1, 7);
            let mut converter = ColorConverter::new(&pipelines, &gpu.device, settings).unwrap();
            let plain = converter.create_frame(&gpu.device);
            let exported = converter
                .create_exported_frame(&gpu.device, MemoryHandle::DmaBuf)
                .unwrap();

            convert_into(gpu, &mut converter, &image, &plain);
            convert_into(gpu, &mut converter, &image, exported.frame());
            let expected = testing::download_buffer(gpu, plain.buffer());
            let actual = testing::download_buffer(gpu, exported.frame().buffer());
            let dma_buf_bytes = seek(exported.fd(), SeekFrom::End(0)).unwrap();

            assert_eq!(actual, expected, "{format:?} {size:?}");
            assert_eq!(exported.handle(), MemoryHandle::DmaBuf);
            assert!(exported.allocation_size() >= converter.layout().size);
            assert!(dma_buf_bytes >= converter.layout().size);

            let unpack = unpack_settings(&settings);
            let mut unpacker = RgbConverter::new(&pipelines, &gpu.device, unpack).unwrap();
            let upload = unpacker.create_frame(&gpu.device);
            let texture = importer
                .import(&exported_planes(&exported, format, converter.layout()))
                .unwrap();
            let from_buffer = testing::storage_target(gpu, size.0, size.1);
            let from_dma_buf = testing::storage_target(gpu, size.0, size.1);

            gpu.queue.write_buffer(upload.buffer(), 0, &expected);
            testing::submit(gpu, |encoder| {
                unpacker
                    .encode(&gpu.device, encoder, &upload, &testing::view(&from_buffer))
                    .unwrap();
                unpacker
                    .encode_planes(
                        &gpu.device,
                        encoder,
                        &texture,
                        &testing::view(&from_dma_buf),
                    )
                    .unwrap();
            });

            assert_eq!(
                testing::download(gpu, &from_dma_buf)
                    .max_difference(&testing::download(gpu, &from_buffer)),
                0.0,
                "{format:?} {size:?} read back through the dma-buf"
            );
        }
    }
}

#[test]
fn opaque_fd_frames_hold_what_the_converter_writes() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    if !can_export(&gpu.device, MemoryHandle::OpaqueFd) {
        return;
    }
    let pipelines = ScalePipelines::new(&gpu.device);
    let settings = convert_settings((48, 20), BitDepth::Ten, PlaneLayout::PlanarHigh);
    let image = Image::pattern(48, 20, 3);
    let mut converter = ColorConverter::new(&pipelines, &gpu.device, settings).unwrap();

    let plain = converter.create_frame(&gpu.device);
    let first = converter
        .create_exported_frame(&gpu.device, MemoryHandle::OpaqueFd)
        .unwrap();
    let second = converter
        .create_exported_frame(&gpu.device, MemoryHandle::OpaqueFd)
        .unwrap();
    convert_into(gpu, &mut converter, &image, &plain);
    convert_into(gpu, &mut converter, &image, first.frame());

    assert_eq!(
        testing::download_buffer(gpu, first.frame().buffer()),
        testing::download_buffer(gpu, plain.buffer())
    );
    assert_eq!(first.handle(), MemoryHandle::OpaqueFd);
    assert!(first.dedicated());
    assert!(first.allocation_size() >= converter.layout().size);
    assert_ne!(first.allocation(), second.allocation());
    assert!(first.fd().as_fd().try_clone_to_owned().is_ok());
}

#[test]
fn devices_without_dma_buf_features_cannot_export_them() {
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
    let pipelines = ScalePipelines::new(&plain);
    let converter = ColorConverter::new(
        &pipelines,
        &plain,
        convert_settings((16, 16), BitDepth::Eight, PlaneLayout::SemiPlanar),
    )
    .unwrap();

    let refused = converter.create_exported_frame(&plain, MemoryHandle::DmaBuf);

    assert!(!can_export(&plain, MemoryHandle::DmaBuf));
    assert_eq!(
        refused.unwrap_err(),
        ScaleError::MemoryExportUnavailable(MemoryHandle::DmaBuf)
    );
}
