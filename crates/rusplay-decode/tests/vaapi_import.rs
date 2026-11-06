#![cfg(feature = "vaapi")]

mod common;
#[path = "../src/hardware.rs"]
mod hardware;

use std::{os::fd::AsFd, rc::Rc};

use rusplay_config::{VaapiDecoderConfig, VideoDecoderConfig};
use rusplay_core::{ChromaSubsampling, Codec, Resolution};
use rusplay_decode::{DecodeError, DecodedBuffer, DecodedFrame, VaapiDecoder, VideoDecoder};
use rusplay_scale::{DmaBuf, DmaBufImporter, DmaBufPlane, DmaBufTexture};
use rusplay_va::{Display, DmaBufImport, DmaBufObjectRef, DmaBufPlaneLayout, Image, Surface, ffi};

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn gpu() -> Option<Gpu> {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .ok()?;
        let (device, queue) =
            rusplay_scale::request_fence_wait_device(&adapter, "vaapi-import-test")
                .await
                .ok()?;

        device
            .features()
            .contains(rusplay_scale::DMABUF_FEATURES)
            .then_some(Gpu { device, queue })
    })
}

fn reference(display: &Rc<Display>, frame: &DecodedFrame) -> Vec<Vec<u8>> {
    let DecodedBuffer::DmaBuf(buffer) = &frame.buffer else {
        panic!("expected DMA-BUF")
    };
    let image = buffer.image();
    let wide = frame.format.depth.bits() > 8;
    let rt_format = if wide {
        ffi::VA_RT_FORMAT_YUV420_10
    } else {
        ffi::VA_RT_FORMAT_YUV420
    };
    let objects: Vec<_> = image
        .objects
        .iter()
        .map(|object| DmaBufObjectRef {
            fd: object.fd.as_fd(),
            size: u32::try_from(object.size).unwrap(),
        })
        .collect();
    let planes: Vec<_> = image
        .planes
        .iter()
        .map(|plane| DmaBufPlaneLayout {
            object: u32::try_from(plane.object).unwrap(),
            offset: u32::try_from(plane.offset).unwrap(),
            pitch: u32::try_from(plane.pitch).unwrap(),
        })
        .collect();
    let imported = Surface::import(
        display,
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
    )
    .unwrap();
    let mut cpu = Image::new(display, image.fourcc.as_u32(), frame.resolution).unwrap();
    let bytes = cpu.get(&imported).unwrap();
    let layout = cpu.layout();
    let width = usize::try_from(frame.resolution.width).unwrap() * if wide { 2 } else { 1 };
    let height = usize::try_from(frame.resolution.height).unwrap();

    (0..2)
        .map(|plane| {
            let height = if plane == 0 {
                height
            } else {
                height.div_ceil(2)
            };
            (0..height)
                .flat_map(|row| {
                    let start = layout.offsets[plane] + row * layout.pitches[plane];
                    bytes[start..start + width].iter().copied()
                })
                .collect()
        })
        .collect()
}

fn download(
    gpu: &Gpu,
    imported: &DmaBufTexture,
    plane: usize,
    texel_bytes: u32,
    visible: Resolution,
) -> Vec<u8> {
    let texture = imported.texture(plane).unwrap();
    let row_bytes =
        (texture.width() * texel_bytes).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("decoded-plane-readback"),
        size: u64::from(row_bytes) * u64::from(texture.height()),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    let acquire = imported.acquire(&gpu.device).unwrap();
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes),
                rows_per_image: None,
            },
        },
        texture.size(),
    );
    let release = imported.release(&gpu.device, &mut encoder).unwrap();
    gpu.queue
        .submit(acquire.into_iter().chain([encoder.finish()]).chain(release));
    buffer.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    let mapped = buffer.get_mapped_range(..).unwrap();
    let width = usize::try_from(visible.width * texel_bytes).unwrap();

    mapped
        .chunks_exact(usize::try_from(row_bytes).unwrap())
        .take(usize::try_from(visible.height).unwrap())
        .flat_map(|row| row[..width].iter().copied())
        .collect()
}

#[test]
fn decoded_hevc_planes_survive_vulkan_import_and_surface_reuse() {
    let config = VaapiDecoderConfig::default();
    if !hardware::device_present(&config.device) {
        return;
    }
    let Some(gpu) = gpu() else {
        hardware::missing("a DMA-BUF capable Vulkan adapter");
        return;
    };
    let display = Display::open(&config.device).unwrap();
    let mut importer = DmaBufImporter::new(&gpu.device).unwrap();

    for name in ["hevc-420-8bit.hevc", "hevc-420-10bit.hevc"] {
        let packets = common::annexb_access_units(Codec::Hevc, &common::fixture(name));
        let mut decoder = match VaapiDecoder::new(Codec::Hevc, &config) {
            Ok(decoder) => decoder,
            Err(error @ DecodeError::CodecUnsupported { .. }) => {
                hardware::unsupported(error);
                continue;
            }
            Err(error) => panic!("opening HEVC: {error}"),
        };
        for (index, packet) in packets.iter().cycle().take(packets.len() * 4).enumerate() {
            let frame = match decoder.decode(packet) {
                Ok(Some(frame)) => frame,
                Err(
                    error @ (DecodeError::BitDepthUnsupported { .. }
                    | DecodeError::ChromaUnsupported { .. }
                    | DecodeError::CodecUnsupported { .. }),
                ) => {
                    hardware::unsupported(error);
                    break;
                }
                other => panic!("{name} frame {index}: {other:?}"),
            };
            let DecodedBuffer::DmaBuf(buffer) = &frame.buffer else {
                panic!("expected DMA-BUF")
            };
            let image = buffer.image();
            let planes: Vec<_> = image
                .planes
                .iter()
                .enumerate()
                .map(|(index, plane)| DmaBufPlane {
                    fd: image.plane_fd(index).unwrap(),
                    offset: plane.offset,
                    stride: plane.pitch,
                })
                .collect();
            let texture = importer
                .import_external(&DmaBuf {
                    fourcc: image.fourcc.as_u32(),
                    modifier: image.modifier,
                    width: image.width,
                    height: image.height,
                    planes,
                })
                .unwrap();
            let bytes_per_sample = if frame.format.depth.bits() > 8 { 2 } else { 1 };
            let actual: Vec<_> = (0..2)
                .map(|plane| {
                    let size = if plane == 0 {
                        frame.resolution
                    } else {
                        Resolution::new(
                            frame.resolution.width.div_ceil(2),
                            frame.resolution.height.div_ceil(2),
                        )
                    };
                    download(
                        &gpu,
                        &texture,
                        plane,
                        bytes_per_sample * if plane == 0 { 1 } else { 2 },
                        size,
                    )
                })
                .collect();
            let expected = reference(&display, &frame);

            for (plane, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
                let differences = actual.iter().zip(expected).filter(|(a, b)| a != b).count();
                assert_eq!(
                    differences, 0,
                    "{name} frame {index} plane {plane}, modifier {:#x}",
                    image.modifier
                );
            }
        }
    }
}

#[test]
fn copied_hevc_output_matches_native_luma_and_chroma() {
    let config = VaapiDecoderConfig::default();
    if !hardware::device_present(&config.device) {
        return;
    }
    let display = Display::open(&config.device).unwrap();
    let copied_config = VaapiDecoderConfig {
        copy_output: true,
        ..config.clone()
    };
    let formats = rusplay_decode::capabilities(&VideoDecoderConfig::Vaapi(copied_config.clone()))
        .unwrap()
        .formats();

    assert!(
        formats
            .iter()
            .all(|format| format.format.chroma == ChromaSubsampling::Yuv420)
    );

    for name in ["hevc-420-8bit.hevc", "hevc-420-10bit.hevc"] {
        let packets = common::annexb_access_units(Codec::Hevc, &common::fixture(name));
        let mut native = match VaapiDecoder::new(Codec::Hevc, &config) {
            Ok(decoder) => decoder,
            Err(error @ DecodeError::CodecUnsupported { .. }) => {
                hardware::unsupported(error);
                continue;
            }
            Err(error) => panic!("opening HEVC: {error}"),
        };
        let mut copied = VaapiDecoder::new(Codec::Hevc, &copied_config).unwrap();

        for packet in &packets {
            let frame = match native.decode(packet) {
                Ok(Some(frame)) => frame,
                Err(
                    error @ (DecodeError::BitDepthUnsupported { .. }
                    | DecodeError::ChromaUnsupported { .. }),
                ) => {
                    hardware::unsupported(error);
                    break;
                }
                other => panic!("{name}: {other:?}"),
            };
            let output = copied.decode(packet).unwrap().unwrap();
            let DecodedBuffer::Cpu(cpu) = &output.buffer else {
                panic!("copy mode produced a DMA-BUF")
            };
            let expected = reference(&display, &frame);
            let shift = if frame.format.depth.bits() > 8 {
                16 - frame.format.depth.bits()
            } else {
                0
            };

            assert_eq!(output.resolution, frame.resolution);
            assert_eq!(output.format, frame.format);
            assert_eq!(output.color, frame.color);
            assert_eq!(output.timestamp, frame.timestamp);
            assert_eq!(cpu.plane_count(), 3);
            for (index, plane) in cpu.planes().enumerate() {
                let sample = usize::from(plane.bytes_per_sample);
                let step = sample * if index == 0 { 1 } else { 2 };
                let offset = usize::from(index == 2) * sample;
                let source = &expected[usize::from(index > 0)];
                let expected: Vec<_> = source
                    .chunks_exact(step)
                    .flat_map(|pixel| {
                        if sample == 1 {
                            vec![pixel[offset]]
                        } else {
                            (u16::from_le_bytes([pixel[offset], pixel[offset + 1]]) >> shift)
                                .to_le_bytes()
                                .to_vec()
                        }
                    })
                    .collect();

                assert_eq!(plane.stride, plane.width as usize * sample);
                assert_eq!(plane.data.len(), plane.stride * plane.height as usize);
                assert_eq!(plane.data, expected, "{name} plane {index}");
            }
        }
    }
}
