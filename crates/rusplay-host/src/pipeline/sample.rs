use std::sync::mpsc;

use rusplay_core::{PixelFormat, PlaneBytes, SamplePicture, SamplePlane};
use rusplay_encode::VideoEncoder;
use rusplay_scale::{ExportedFrame, FrameLayout, Plane};

use crate::{
    Gpu, HostError,
    quality::{PictureSample, Sample, SampleKind},
};

#[derive(Debug)]
pub(super) enum SourceSample {
    Luma(SamplePlane),
    Picture(SamplePicture),
}

pub(super) type CapturedSource = Result<SourceSample, HostError>;

pub(super) fn read_source(
    layout: &FrameLayout,
    bytes: &[u8],
    format: PixelFormat,
    kind: SampleKind,
) -> CapturedSource {
    let shift = layout.sample_shift();
    let bytes_per_sample = layout.bytes_per_sample as usize;
    let plane_bytes = |plane: &Plane| PlaneBytes {
        data: usize::try_from(plane.offset)
            .ok()
            .and_then(|start| bytes.get(start..))
            .unwrap_or_default(),
        stride: plane.stride as usize,
        bytes_per_sample,
        shift,
    };
    let read = |plane: &Plane| {
        SamplePlane::read(
            plane_bytes(plane),
            plane.width as usize,
            plane.height as usize,
        )
        .map_err(HostError::QualitySourceUnreadable)
    };

    let [luma, chroma @ ..] = layout.planes.as_slice() else {
        return Err(HostError::QualitySourceLayout { planes: 0 });
    };
    let luma = read(luma)?;
    if kind == SampleKind::Luma {
        return Ok(SourceSample::Luma(luma));
    }

    let [cb, cr] = match chroma {
        [interleaved] => {
            let (cb, cr) = SamplePlane::read_interleaved(
                plane_bytes(interleaved),
                interleaved.width as usize,
                interleaved.height as usize,
            )
            .map_err(HostError::QualitySourceUnreadable)?;
            [cb, cr]
        }
        [cb, cr] => [read(cb)?, read(cr)?],
        _ => {
            return Err(HostError::QualitySourceLayout {
                planes: layout.planes.len(),
            });
        }
    };

    SamplePicture::new(format, [luma, cb, cr])
        .map(SourceSample::Picture)
        .map_err(HostError::QualitySourceUnreadable)
}

pub(super) fn read_exported(
    gpu: &Gpu,
    exported: &ExportedFrame,
    format: PixelFormat,
    kind: SampleKind,
) -> CapturedSource {
    let frame = exported.frame();
    let size = frame.layout().size;
    let device = &gpu.device;
    let copy = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rusplay-host-quality-source"),
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("rusplay-host-quality-copy"),
    });
    encoder.copy_buffer_to_buffer(frame.buffer(), 0, &copy, 0, size);
    gpu.queue.submit([encoder.finish()]);

    let (done, mapped) = mpsc::channel();
    copy.map_async(wgpu::MapMode::Read, .., move |result| {
        done.send(result).ok();
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .map_err(HostError::GpuPoll)?;
    if let Ok(result) = mapped.recv() {
        result.map_err(HostError::GpuReadback)?;
    }

    let read = copy
        .get_mapped_range(..)
        .map_err(HostError::GpuMappedRange)
        .and_then(|view| read_source(frame.layout(), &view, format, kind));
    copy.unmap();

    read
}

pub(super) fn arm(encoder: &mut dyn VideoEncoder, source: Option<&CapturedSource>) -> bool {
    matches!(source, Some(Ok(SourceSample::Picture(_)))) && encoder.capture_reconstruction()
}

pub(super) fn finish(
    encoder: &mut dyn VideoEncoder,
    source: Option<CapturedSource>,
    armed: bool,
) -> Option<Sample> {
    let sample = match source? {
        Err(_) => Sample::Failed,
        Ok(SourceSample::Luma(luma)) => Sample::Luma(luma),
        Ok(SourceSample::Picture(_)) if !armed => Sample::Unsupported,
        Ok(SourceSample::Picture(source)) => match encoder.take_reconstruction() {
            Ok(reconstruction) => Sample::Picture(Box::new(PictureSample {
                source,
                reconstruction,
            })),
            Err(_) => Sample::Failed,
        },
    };

    Some(sample)
}
