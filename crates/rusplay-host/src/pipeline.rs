#[cfg(test)]
mod tests;

use std::{
    os::fd::{AsFd, BorrowedFd},
    sync::mpsc,
    time::Duration,
};

use rusplay_capture::{Frame, FrameBuffer, Plane, ShmFallback};
use rusplay_config::Config;
use rusplay_core::{Codec, ColorSpace, EncodedFrame, PixelFormat, Resolution, Timestamp};
use rusplay_encode::{VideoEncoder, input::fourcc_for};
use rusplay_protocol::StreamDescription;
use rusplay_scale::{
    ChromaSiting, ColorConverter, ColorMapSettings, ColorMapper, ConvertSettings, DmaBuf,
    DmaBufImporter, DmaBufPlane, InputLight, LineariseSettings, Lineariser, PlaneLayout,
    ScalePipelines, ScaleSettings, Scaler, YuvFrame, wait_for_writes,
};

use crate::{
    Gpu, HostError,
    route::{Route, RouteKey, SourceRead},
    upload::{EncoderPlanes, UploadFormat, check_shm_frame, upload_format_for},
};

const CONVERT_ROW_ALIGNMENT: u32 = 4;
const LINEAR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StreamFormat {
    pub codec: Codec,
    pub resolution: Resolution,
    pub format: PixelFormat,
    pub color: ColorSpace,
}

impl StreamFormat {
    pub(crate) const fn describe(self, generation: u32) -> StreamDescription {
        StreamDescription {
            generation,
            codec: self.codec,
            resolution: self.resolution,
            format: self.format,
            color: self.color,
        }
    }
}

pub(crate) trait VideoPipeline {
    fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Option<EncodedFrame>, HostError>;

    fn repeat(
        &mut self,
        timestamp: Timestamp,
        keyframe: bool,
    ) -> Result<Option<EncodedFrame>, HostError>;

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), HostError>;

    fn stream_format(&self) -> Option<StreamFormat>;
}

struct Upload {
    resolution: Resolution,
    format: UploadFormat,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    srgb_view: Option<wgpu::TextureView>,
}

struct Source {
    key: RouteKey,
    view: wgpu::TextureView,
    srgb_view: Option<wgpu::TextureView>,
}

struct Linear {
    resolution: Resolution,
    view: wgpu::TextureView,
}

struct Conversion {
    converter: ColorConverter,
    output: YuvFrame,
    readback: wgpu::Buffer,
    planes: EncoderPlanes,
}

struct OpenEncoder {
    resolution: Resolution,
    encoder: Box<dyn VideoEncoder>,
}

pub(crate) struct GpuPipeline {
    gpu: Gpu,
    config: Config,
    pipelines: ScalePipelines,
    fallback: Option<ShmFallback>,
    importer: Option<DmaBufImporter>,
    upload: Option<Upload>,
    route: Option<(RouteKey, Route)>,
    prepared: Option<Linear>,
    scaled: Option<Linear>,
    relinearised: Option<Linear>,
    mapped: Option<Linear>,
    preparer: Option<Lineariser>,
    scaler: Option<Scaler>,
    relineariser: Option<Lineariser>,
    mapper: Option<ColorMapper>,
    conversion: Option<Conversion>,
    encoder: Option<OpenEncoder>,
    input: Option<Frame>,
    bitrate_kbps: Option<u32>,
}

impl GpuPipeline {
    pub(crate) fn new(gpu: Gpu, config: Config, fallback: Option<ShmFallback>) -> Self {
        let pipelines = ScalePipelines::new(&gpu.device);

        Self {
            gpu,
            config,
            pipelines,
            fallback,
            importer: None,
            upload: None,
            route: None,
            prepared: None,
            scaled: None,
            relinearised: None,
            mapped: None,
            preparer: None,
            scaler: None,
            relineariser: None,
            mapper: None,
            conversion: None,
            encoder: None,
            input: None,
            bitrate_kbps: None,
        }
    }

    fn upload(&mut self, frame: &Frame) -> Result<Option<Source>, HostError> {
        match &frame.buffer {
            FrameBuffer::Shm { data, stride } => self.upload_shm(frame, data, *stride).map(Some),
            FrameBuffer::DmaBuf {
                fd,
                modifier,
                planes,
                ..
            } => match self.import(frame, fd.as_fd(), *modifier, planes) {
                Ok(source) => Ok(Some(source)),
                Err(error) => {
                    let Some(fallback) = &self.fallback else {
                        return Err(error);
                    };
                    fallback.request();
                    Ok(None)
                }
            },
        }
    }

    fn upload_shm(&mut self, frame: &Frame, data: &[u8], stride: u32) -> Result<Source, HostError> {
        check_shm_frame(
            data,
            stride,
            frame.resolution.width,
            frame.resolution.height,
        )?;
        let format = upload_format_for(frame.format)?;
        let upload = ensure_upload(&self.gpu.device, &mut self.upload, frame.resolution, format);

        self.gpu.queue.write_texture(
            upload.texture.as_image_copy(),
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(frame.resolution.height),
            },
            extent(frame.resolution),
        );

        Ok(Source {
            key: RouteKey {
                upload: format,
                color: frame.color,
                capture: frame.resolution,
            },
            view: upload.view.clone(),
            srgb_view: upload.srgb_view.clone(),
        })
    }

    fn import(
        &mut self,
        frame: &Frame,
        fd: BorrowedFd<'_>,
        modifier: u64,
        planes: &[Plane],
    ) -> Result<Source, HostError> {
        let fence_timeout =
            Duration::from_millis(u64::from(self.config.capture.dmabuf_fence_timeout_ms));
        wait_for_writes(fd, fence_timeout)?;

        let importer = match &mut self.importer {
            Some(importer) => importer,
            None => self.importer.insert(DmaBufImporter::new(&self.gpu.device)?),
        };
        let dmabuf = importer.import(&DmaBuf {
            fourcc: frame.format.0,
            modifier,
            width: frame.resolution.width,
            height: frame.resolution.height,
            planes: planes
                .iter()
                .map(|plane| DmaBufPlane {
                    fd,
                    offset: u64::from(plane.offset),
                    stride: u64::from(plane.stride),
                })
                .collect(),
        })?;

        let unsupported = HostError::CaptureFormatUnsupported {
            fourcc: frame.format,
        };
        let (Some(channels), Some(texture), Some(view)) =
            (dmabuf.channels(), dmabuf.texture(0), dmabuf.view(0))
        else {
            return Err(unsupported);
        };
        let srgb_view = dmabuf.linear_view().cloned();
        let upload = UploadFormat {
            texture: texture.format(),
            srgb_view: srgb_view
                .as_ref()
                .map(|_| texture.format().add_srgb_suffix()),
            channels,
        };

        Ok(Source {
            key: RouteKey {
                upload,
                color: frame.color,
                capture: frame.resolution,
            },
            view: view.clone(),
            srgb_view,
        })
    }

    fn route(&mut self, key: RouteKey) -> Result<Route, HostError> {
        if let Some((cached, route)) = &self.route
            && *cached == key
        {
            return Ok(*route);
        }

        let route = Route::plan(&self.config, &key)?;
        self.route = Some((key, route));
        Ok(route)
    }

    fn convert(&mut self, source: &Source) -> Result<(), HostError> {
        let route = self.route(source.key)?;
        let capture = source.key.capture;
        let stream = self.config.video.resolution.unwrap_or(capture);
        let device = &self.gpu.device;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rusplay-host-convert"),
        });

        let mut current = match route.read {
            SourceRead::Raw => &source.view,
            SourceRead::Srgb => source.srgb_view.as_ref().unwrap_or(&source.view),
        };

        if let Some(settings) = route.prepare {
            let preparer =
                ensure_lineariser(&self.pipelines, device, &mut self.preparer, settings)?;
            let target =
                ensure_linear(device, &mut self.prepared, capture, "rusplay-host-prepared");
            preparer.encode(device, &mut encoder, current, &target.view)?;
            current = &target.view;
        }

        if let Some(settings) = route.scale {
            let scaler = ensure_scaler(&self.pipelines, device, &mut self.scaler, settings)?;
            let target = ensure_linear(device, &mut self.scaled, stream, "rusplay-host-scaled");
            scaler.encode(device, &mut encoder, current, &target.view);
            current = &target.view;
        }

        if let Some(settings) = route.relinearise {
            let relineariser =
                ensure_lineariser(&self.pipelines, device, &mut self.relineariser, settings)?;
            let target = ensure_linear(
                device,
                &mut self.relinearised,
                stream,
                "rusplay-host-relinearised",
            );
            relineariser.encode(device, &mut encoder, current, &target.view)?;
            current = &target.view;
        }

        if let Some(settings) = route.map {
            let mapper = ensure_mapper(&self.pipelines, device, &mut self.mapper, settings)?;
            let target = ensure_linear(device, &mut self.mapped, stream, "rusplay-host-mapped");
            mapper.encode(device, &mut encoder, current, &target.view);
            current = &target.view;
        }

        let settings = convert_settings(&self.config, stream, route.input);
        let conversion =
            ensure_conversion(&self.pipelines, device, &mut self.conversion, settings)?;
        conversion
            .converter
            .encode(device, &mut encoder, current, &conversion.output)?;
        encoder.copy_buffer_to_buffer(
            conversion.output.buffer(),
            0,
            &conversion.readback,
            0,
            conversion.output.layout().size,
        );
        self.gpu.queue.submit([encoder.finish()]);

        Ok(())
    }

    fn read_back(&mut self, timestamp: Timestamp) -> Result<(), HostError> {
        let Some(conversion) = &self.conversion else {
            return Ok(());
        };
        let (done, mapped) = mpsc::channel();
        conversion
            .readback
            .map_async(wgpu::MapMode::Read, .., move |result| {
                done.send(result).ok();
            });
        self.gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(HostError::GpuPoll)?;
        if let Ok(result) = mapped.recv() {
            result.map_err(HostError::GpuReadback)?;
        }

        let layout = conversion.converter.layout();
        let settings = conversion.converter.settings();
        let stride = u32::try_from(conversion.planes.luma_stride).unwrap_or(u32::MAX);
        let color = self.config.video.color;
        let input = self.input.get_or_insert_with(|| Frame {
            resolution: settings.resolution,
            format: fourcc_for(settings.format),
            color,
            timestamp,
            buffer: FrameBuffer::Shm {
                data: Vec::new(),
                stride,
            },
        });
        input.resolution = settings.resolution;
        input.format = fourcc_for(settings.format);
        input.timestamp = timestamp;

        let repacked = {
            let view = conversion
                .readback
                .get_mapped_range(..)
                .map_err(HostError::GpuMappedRange)?;
            if let FrameBuffer::Shm { data, stride: held } = &mut input.buffer {
                conversion.planes.repack(layout, &view, data);
                *held = stride;
            }
            Ok(())
        };
        conversion.readback.unmap();

        repacked
    }

    fn encode_input(&mut self, keyframe: bool) -> Result<Option<EncodedFrame>, HostError> {
        let Some(input) = &self.input else {
            return Ok(None);
        };

        let encoder = match self.encoder.take() {
            Some(open) if open.resolution == input.resolution => open,
            _ => {
                let mut encoder = rusplay_encode::open(&self.config.video)?;
                if let Some(kbps) = self.bitrate_kbps {
                    encoder.set_bitrate(kbps)?;
                }
                OpenEncoder {
                    resolution: input.resolution,
                    encoder,
                }
            }
        };
        let encoder = self.encoder.insert(encoder);

        if keyframe {
            encoder.encoder.request_keyframe();
        }
        Ok(encoder.encoder.encode(input)?)
    }
}

impl VideoPipeline for GpuPipeline {
    fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Option<EncodedFrame>, HostError> {
        let Some(source) = self.upload(frame)? else {
            return Ok(None);
        };
        self.convert(&source)?;
        self.read_back(frame.timestamp)?;
        self.encode_input(keyframe)
    }

    fn repeat(
        &mut self,
        timestamp: Timestamp,
        keyframe: bool,
    ) -> Result<Option<EncodedFrame>, HostError> {
        if let Some(input) = &mut self.input {
            input.timestamp = timestamp;
        }
        self.encode_input(keyframe)
    }

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), HostError> {
        self.bitrate_kbps = Some(kbps);
        if let Some(open) = &mut self.encoder {
            open.encoder.set_bitrate(kbps)?;
        }
        Ok(())
    }

    fn stream_format(&self) -> Option<StreamFormat> {
        let video = &self.config.video;

        self.encoder.as_ref().map(|open| StreamFormat {
            codec: video.codec,
            resolution: open.resolution,
            format: video.pixel_format,
            color: video.color,
        })
    }
}

fn extent(resolution: Resolution) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: resolution.width,
        height: resolution.height,
        depth_or_array_layers: 1,
    }
}

fn ensure_upload<'a>(
    device: &wgpu::Device,
    slot: &'a mut Option<Upload>,
    resolution: Resolution,
    format: UploadFormat,
) -> &'a Upload {
    let upload = slot
        .take()
        .filter(|upload| upload.resolution == resolution && upload.format == format)
        .unwrap_or_else(|| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("rusplay-host-capture"),
                size: extent(resolution),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: format.texture,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: format.srgb_view.as_slice(),
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let srgb_view = format.srgb_view.map(|srgb| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    format: Some(srgb),
                    ..wgpu::TextureViewDescriptor::default()
                })
            });
            Upload {
                resolution,
                format,
                texture,
                view,
                srgb_view,
            }
        });

    slot.insert(upload)
}

fn ensure_linear<'a>(
    device: &wgpu::Device,
    slot: &'a mut Option<Linear>,
    resolution: Resolution,
    label: &str,
) -> &'a Linear {
    let linear = slot
        .take()
        .filter(|linear| linear.resolution == resolution)
        .unwrap_or_else(|| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent(resolution),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: LINEAR_FORMAT,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            Linear {
                resolution,
                view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            }
        });

    slot.insert(linear)
}

fn ensure_scaler<'a>(
    pipelines: &ScalePipelines,
    device: &wgpu::Device,
    slot: &'a mut Option<Scaler>,
    settings: ScaleSettings,
) -> Result<&'a mut Scaler, HostError> {
    let scaler = match slot.take() {
        Some(mut scaler) => {
            scaler.configure(device, settings)?;
            scaler
        }
        None => Scaler::new(pipelines, device, settings)?,
    };

    Ok(slot.insert(scaler))
}

fn ensure_lineariser<'a>(
    pipelines: &ScalePipelines,
    device: &wgpu::Device,
    slot: &'a mut Option<Lineariser>,
    settings: LineariseSettings,
) -> Result<&'a mut Lineariser, HostError> {
    let lineariser = match slot.take() {
        Some(mut lineariser) => {
            lineariser.configure(device, settings)?;
            lineariser
        }
        None => Lineariser::new(pipelines, device, settings)?,
    };

    Ok(slot.insert(lineariser))
}

fn ensure_mapper<'a>(
    pipelines: &ScalePipelines,
    device: &wgpu::Device,
    slot: &'a mut Option<ColorMapper>,
    settings: ColorMapSettings,
) -> Result<&'a mut ColorMapper, HostError> {
    let mapper = match slot.take() {
        Some(mut mapper) => {
            mapper.configure(device, settings)?;
            mapper
        }
        None => ColorMapper::new(pipelines, device, settings)?,
    };

    Ok(slot.insert(mapper))
}

fn ensure_conversion<'a>(
    pipelines: &ScalePipelines,
    device: &wgpu::Device,
    slot: &'a mut Option<Conversion>,
    settings: ConvertSettings,
) -> Result<&'a mut Conversion, HostError> {
    let conversion = match slot.take() {
        Some(conversion) if conversion.converter.settings() == settings => conversion,
        Some(Conversion { mut converter, .. }) => {
            converter.configure(device, settings)?;
            conversion_buffers(device, converter, settings)
        }
        None => {
            let converter = ColorConverter::new(pipelines, device, settings)?;
            conversion_buffers(device, converter, settings)
        }
    };

    Ok(slot.insert(conversion))
}

fn conversion_buffers(
    device: &wgpu::Device,
    converter: ColorConverter,
    settings: ConvertSettings,
) -> Conversion {
    let output = converter.create_frame(device);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rusplay-host-readback"),
        size: converter.layout().size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let planes = EncoderPlanes::of(converter.layout(), settings.format.chroma);

    Conversion {
        converter,
        output,
        readback,
        planes,
    }
}

fn convert_settings(config: &Config, resolution: Resolution, input: InputLight) -> ConvertSettings {
    let video = &config.video;

    ConvertSettings {
        resolution,
        format: video.pixel_format,
        layout: PlaneLayout::Planar,
        matrix: video.color.matrix_coefficients(),
        range: video.color.range,
        transfer: video.color.transfer,
        hdr: video.color.hdr,
        input,
        chroma_filter: config.scaling.downscale.chroma_filter,
        chroma_siting: ChromaSiting::Left,
        row_alignment: CONVERT_ROW_ALIGNMENT,
    }
}
