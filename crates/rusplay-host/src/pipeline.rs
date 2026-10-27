#[cfg(test)]
mod tests;

use std::{
    os::fd::{AsFd, BorrowedFd},
    sync::mpsc,
    time::Duration,
};

use rusplay_capture::{Frame, FrameBuffer, Plane, ShmFallback};
use rusplay_config::Config;
use rusplay_core::{
    Codec, CodecFormat, ColorSpace, EncodedFrame, FormatSet, PixelFormat, Resolution, Timestamp,
};
use rusplay_encode::{VideoEncoder, input::fourcc_for};
use rusplay_protocol::StreamDescription;
use rusplay_scale::{
    ChromaSiting, ColorConverter, ColorMapSettings, ColorMapper, ConvertSettings, DmaBuf,
    DmaBufImporter, DmaBufPlane, ExportedFrame, GpuFenceWait, GpuFenceWaiter, InputLight,
    LineariseSettings, Lineariser, PlaneLayout, ScalePipelines, ScaleSettings, Scaler, WriteFence,
    YuvFrame, can_export, wait_for_writes,
};

use crate::{
    Gpu, HostError,
    encode::PipelineFactory,
    handoff::{HANDOFF_ROW_ALIGNMENT, Handoff},
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
    pub(crate) const fn describe(
        self,
        generation: u32,
        configured: CodecFormat,
    ) -> StreamDescription {
        StreamDescription {
            generation,
            codec: self.codec,
            resolution: self.resolution,
            format: self.format,
            color: self.color,
            configured,
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
    fence: Option<GpuFenceWait>,
}

struct Linear {
    resolution: Resolution,
    view: wgpu::TextureView,
}

struct Readback {
    output: YuvFrame,
    buffer: wgpu::Buffer,
    planes: EncoderPlanes,
}

enum Target {
    Readback(Readback),
    Handoff(ExportedFrame),
}

impl Target {
    const fn hands_off(&self) -> bool {
        matches!(self, Self::Handoff(_))
    }

    fn frame(&self) -> &YuvFrame {
        match self {
            Self::Readback(readback) => &readback.output,
            Self::Handoff(exported) => exported.frame(),
        }
    }
}

struct Conversion {
    converter: ColorConverter,
    target: Target,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HandoffState {
    Off,
    Trying(Handoff),
    Proven(Handoff),
}

impl HandoffState {
    const fn active(self) -> Option<Handoff> {
        match self {
            Self::Off => None,
            Self::Trying(handoff) | Self::Proven(handoff) => Some(handoff),
        }
    }
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
    fence_waiter: Option<GpuFenceWaiter>,
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
    handoff: HandoffState,
    encoder: Option<OpenEncoder>,
    input: Option<Frame>,
    bitrate_kbps: Option<u32>,
}

pub(crate) struct GpuPipelineFactory {
    gpu: Gpu,
    config: Config,
    fallback: Option<ShmFallback>,
    pipelines: Option<ScalePipelines>,
}

impl GpuPipelineFactory {
    pub(crate) fn new(gpu: Gpu, config: Config, fallback: Option<ShmFallback>) -> Self {
        Self {
            gpu,
            config,
            fallback,
            pipelines: None,
        }
    }
}

impl PipelineFactory for GpuPipelineFactory {
    fn encodable(&mut self) -> Result<FormatSet, HostError> {
        let capabilities = rusplay_encode::capabilities(&self.config.video.encoder)?;
        self.pipelines = Some(ScalePipelines::new(&self.gpu.device));

        Ok(capabilities.format_set())
    }

    fn build(self: Box<Self>, format: CodecFormat) -> Result<Box<dyn VideoPipeline>, HostError> {
        let Self {
            gpu,
            mut config,
            fallback,
            pipelines,
        } = *self;
        config.video.codec = format.codec;
        config.video.pixel_format = format.format;
        let pipelines = pipelines.unwrap_or_else(|| ScalePipelines::new(&gpu.device));

        Ok(Box::new(GpuPipeline::with_pipelines(
            gpu, config, fallback, pipelines,
        )))
    }
}

impl GpuPipeline {
    #[cfg(test)]
    pub(crate) fn new(gpu: Gpu, config: Config, fallback: Option<ShmFallback>) -> Self {
        let pipelines = ScalePipelines::new(&gpu.device);

        Self::with_pipelines(gpu, config, fallback, pipelines)
    }

    fn with_pipelines(
        gpu: Gpu,
        config: Config,
        fallback: Option<ShmFallback>,
        pipelines: ScalePipelines,
    ) -> Self {
        let fence_waiter = config
            .capture
            .dmabuf_gpu_fence_wait
            .then(|| GpuFenceWaiter::new(&gpu.device).ok())
            .flatten();
        let handoff = Handoff::plan(&config)
            .filter(|handoff| can_export(&gpu.device, handoff.handle()))
            .map_or(HandoffState::Off, HandoffState::Trying);

        Self {
            gpu,
            config,
            pipelines,
            fallback,
            importer: None,
            fence_waiter,
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
            handoff,
            encoder: None,
            input: None,
            bitrate_kbps: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn hands_off(&self) -> bool {
        self.handoff.active().is_some()
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
            FrameBuffer::OpaqueFd { .. } => Err(HostError::CaptureOpaqueFd),
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
            fence: None,
        })
    }

    fn import(
        &mut self,
        frame: &Frame,
        fd: BorrowedFd<'_>,
        modifier: u64,
        planes: &[Plane],
    ) -> Result<Source, HostError> {
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
            fence: self.gate_on_writes(fd)?,
        })
    }

    fn gate_on_writes(&self, fd: BorrowedFd<'_>) -> Result<Option<GpuFenceWait>, HostError> {
        let timeout = Duration::from_millis(u64::from(self.config.capture.dmabuf_fence_timeout_ms));
        let Some(waiter) = &self.fence_waiter else {
            wait_for_writes(fd, timeout)?;
            return Ok(None);
        };

        if wait_for_writes(fd, Duration::ZERO)? == WriteFence::Signalled {
            return Ok(None);
        }
        if let Ok(wait) = waiter.stage(&self.gpu.queue, fd) {
            return Ok(Some(wait));
        }

        wait_for_writes(fd, timeout)?;
        Ok(None)
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

    fn convert(&mut self, source: &mut Source) -> Result<wgpu::SubmissionIndex, HostError> {
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

        let settings = convert_settings(&self.config, stream, route.input, self.handoff.active());
        let conversion = match ensure_conversion(
            &self.pipelines,
            device,
            &mut self.conversion,
            settings,
            &self.config,
            self.handoff,
        ) {
            Err(_) if matches!(self.handoff, HandoffState::Trying(_)) => {
                self.handoff = HandoffState::Off;
                let settings = convert_settings(&self.config, stream, route.input, None);
                ensure_conversion(
                    &self.pipelines,
                    device,
                    &mut self.conversion,
                    settings,
                    &self.config,
                    HandoffState::Off,
                )?
            }
            result => result?,
        };
        let frame = conversion.target.frame();
        conversion
            .converter
            .encode(device, &mut encoder, current, frame)?;
        if let Target::Readback(readback) = &conversion.target {
            encoder.copy_buffer_to_buffer(
                readback.output.buffer(),
                0,
                &readback.buffer,
                0,
                readback.output.layout().size,
            );
        }
        let submission = self.gpu.queue.submit([encoder.finish()]);
        drop(source.fence.take());

        Ok(submission)
    }

    fn hand_off(
        &mut self,
        handoff: Handoff,
        submission: wgpu::SubmissionIndex,
        timestamp: Timestamp,
    ) -> Result<(), HostError> {
        let Some(Conversion {
            converter,
            target: Target::Handoff(exported),
        }) = &self.conversion
        else {
            return Ok(());
        };

        let resolution = converter.settings().resolution;
        let frame = handoff.frame(exported, resolution, self.config.video.color, timestamp)?;
        self.gpu
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            })
            .map_err(HostError::GpuPoll)?;
        self.input = Some(frame);

        Ok(())
    }

    fn encode_handed_off(
        &mut self,
        handoff: Handoff,
        source: &mut Source,
        submission: wgpu::SubmissionIndex,
        timestamp: Timestamp,
        keyframe: bool,
    ) -> Result<Option<EncodedFrame>, HostError> {
        let handed_off = self
            .hand_off(handoff, submission, timestamp)
            .and_then(|()| self.encode_input(keyframe));

        match (handed_off, self.handoff) {
            (Ok(encoded), HandoffState::Trying(handoff)) => {
                self.handoff = HandoffState::Proven(handoff);
                Ok(encoded)
            }
            (Err(_), HandoffState::Trying(_)) => {
                self.handoff = HandoffState::Off;
                self.input = None;
                self.convert(source)?;
                self.read_back(timestamp)?;
                self.encode_input(keyframe)
            }
            (result, _) => result,
        }
    }

    fn read_back(&mut self, timestamp: Timestamp) -> Result<(), HostError> {
        let Some(Conversion {
            converter,
            target: Target::Readback(readback),
        }) = &self.conversion
        else {
            return Ok(());
        };
        let (done, mapped) = mpsc::channel();
        readback
            .buffer
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

        let layout = converter.layout();
        let settings = converter.settings();
        let stride = u32::try_from(readback.planes.luma_stride).unwrap_or(u32::MAX);
        let color = self.config.video.color;
        if self
            .input
            .as_ref()
            .is_some_and(|input| !matches!(input.buffer, FrameBuffer::Shm { .. }))
        {
            self.input = None;
        }
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
            let view = readback
                .buffer
                .get_mapped_range(..)
                .map_err(HostError::GpuMappedRange)?;
            if let FrameBuffer::Shm { data, stride: held } = &mut input.buffer {
                readback.planes.repack(layout, &view, data);
                *held = stride;
            }
            Ok(())
        };
        readback.buffer.unmap();

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
        let Some(mut source) = self.upload(frame)? else {
            return Ok(None);
        };
        let submission = self.convert(&mut source)?;
        if let Some(handoff) = self.handoff.active() {
            return self.encode_handed_off(
                handoff,
                &mut source,
                submission,
                frame.timestamp,
                keyframe,
            );
        }
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
    config: &Config,
    handoff: HandoffState,
) -> Result<&'a mut Conversion, HostError> {
    let handoff = handoff.active();
    let conversion = if let Some(Conversion {
        mut converter,
        target,
    }) = slot.take()
    {
        converter.configure(device, settings)?;
        if target.frame().layout() == converter.layout() && target.hands_off() == handoff.is_some()
        {
            Conversion { converter, target }
        } else {
            drop(target);
            conversion_target(device, converter, settings, config, handoff)?
        }
    } else {
        let converter = ColorConverter::new(pipelines, device, settings)?;
        conversion_target(device, converter, settings, config, handoff)?
    };

    Ok(slot.insert(conversion))
}

fn conversion_target(
    device: &wgpu::Device,
    converter: ColorConverter,
    settings: ConvertSettings,
    config: &Config,
    handoff: Option<Handoff>,
) -> Result<Conversion, HostError> {
    let target = if let Some(handoff) = handoff {
        handoff.accepts(config, settings.resolution, converter.layout())?;
        Target::Handoff(converter.create_exported_frame(device, handoff.handle())?)
    } else {
        let output = converter.create_frame(device);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rusplay-host-readback"),
            size: converter.layout().size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let planes = EncoderPlanes::of(converter.layout(), settings.format.chroma);
        Target::Readback(Readback {
            output,
            buffer,
            planes,
        })
    };

    Ok(Conversion { converter, target })
}

fn convert_settings(
    config: &Config,
    resolution: Resolution,
    input: InputLight,
    handoff: Option<Handoff>,
) -> ConvertSettings {
    let video = &config.video;
    let (layout, row_alignment) = handoff
        .map_or((PlaneLayout::Planar, CONVERT_ROW_ALIGNMENT), |handoff| {
            (handoff.layout(), HANDOFF_ROW_ALIGNMENT)
        });

    ConvertSettings {
        resolution,
        format: video.pixel_format,
        layout,
        matrix: video.color.matrix_coefficients(),
        range: video.color.range,
        transfer: video.color.transfer,
        hdr: video.color.hdr,
        input,
        chroma_filter: config.scaling.downscale.chroma_filter,
        chroma_siting: ChromaSiting::Left,
        row_alignment,
    }
}
