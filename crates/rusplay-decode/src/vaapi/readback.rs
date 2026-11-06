use std::{os::fd::AsFd, rc::Rc};

use rusplay_core::{BitDepth, ChromaSubsampling, Resolution};
use rusplay_va::{DmaBufImport, DmaBufObjectRef, DmaBufPlaneLayout, Image, ImageLayout, Surface};

use crate::{
    CpuFrame, CpuPlane, DecodeError, DecodedBuffer, DecodedFrame,
    frame::PlaneStorage,
    vaapi::{rt_format, va::Display},
};

#[derive(Debug)]
struct Plane {
    data: Vec<u8>,
    resolution: Resolution,
    bytes_per_sample: u8,
}

#[derive(Debug)]
struct Planes([Plane; 3]);

impl PlaneStorage for Planes {
    fn plane_count(&self) -> usize {
        self.0.len()
    }

    fn plane(&self, index: usize) -> Option<CpuPlane<'_>> {
        let plane = self.0.get(index)?;

        Some(CpuPlane {
            data: &plane.data,
            stride: plane.resolution.width as usize * usize::from(plane.bytes_per_sample),
            width: plane.resolution.width,
            height: plane.resolution.height,
            bytes_per_sample: plane.bytes_per_sample,
        })
    }
}

pub(super) fn copy(
    display: &Rc<Display>,
    mut frame: DecodedFrame,
) -> Result<DecodedFrame, DecodeError> {
    let DecodedBuffer::DmaBuf(buffer) = &frame.buffer else {
        return Ok(frame);
    };
    let exported = buffer.image();
    let fourcc = match frame.format.depth {
        BitDepth::Eight => u32::from_le_bytes(*b"NV12"),
        BitDepth::Ten => u32::from_le_bytes(*b"P010"),
        BitDepth::Twelve => u32::from_le_bytes(*b"P012"),
    };
    if frame.format.chroma != ChromaSubsampling::Yuv420 || exported.fourcc.as_u32() != fourcc {
        return Err(DecodeError::ReadbackFormat {
            fourcc: exported.fourcc.as_u32(),
        });
    }
    let objects = exported
        .objects
        .iter()
        .enumerate()
        .map(|(index, object)| {
            Ok(DmaBufObjectRef {
                fd: object.fd.as_fd(),
                size: u32::try_from(object.size).map_err(|_| DecodeError::ReadbackObjectSize {
                    object: index,
                    size: object.size,
                })?,
            })
        })
        .collect::<Result<Vec<_>, DecodeError>>()?;
    let planes = exported
        .planes
        .iter()
        .enumerate()
        .map(|(index, plane)| {
            Ok(DmaBufPlaneLayout {
                object: u32::try_from(plane.object).map_err(|_| {
                    DecodeError::ReadbackObjectIndex {
                        plane: index,
                        object: plane.object,
                    }
                })?,
                offset: u32::try_from(plane.offset).map_err(|_| {
                    DecodeError::ReadbackPlaneOffset {
                        plane: index,
                        offset: plane.offset,
                    }
                })?,
                pitch: u32::try_from(plane.pitch).map_err(|_| DecodeError::ReadbackPlanePitch {
                    plane: index,
                    pitch: plane.pitch,
                })?,
            })
        })
        .collect::<Result<Vec<_>, DecodeError>>()?;
    let surface = Surface::import(
        display,
        rt_format(frame.format),
        &DmaBufImport {
            va_fourcc: fourcc,
            drm_fourcc: fourcc,
            resolution: Resolution::new(exported.width, exported.height),
            modifier: exported.modifier,
            objects: &objects,
            planes: &planes,
        },
        None,
    )?;
    let mut image = Image::new(display, fourcc, frame.resolution)?;
    let bytes = image.get(&surface)?;
    let layout = image.layout();
    let planes = [0, 1, 2]
        .map(|plane| split_plane(&bytes, layout, frame.resolution, frame.format.depth, plane));
    let [y, u, v] = planes;

    frame.buffer = DecodedBuffer::Cpu(CpuFrame::new(Planes([y?, u?, v?])));
    Ok(frame)
}

fn split_plane(
    bytes: &[u8],
    layout: ImageLayout,
    resolution: Resolution,
    depth: BitDepth,
    plane: usize,
) -> Result<Plane, DecodeError> {
    let resolution = if plane == 0 {
        resolution
    } else {
        Resolution::new(resolution.width.div_ceil(2), resolution.height.div_ceil(2))
    };
    let bytes_per_sample = if depth == BitDepth::Eight { 1 } else { 2 };
    let sample = usize::from(bytes_per_sample);
    let step = if plane == 0 { sample } else { sample * 2 };
    let source = usize::from(plane > 0);
    let component = usize::from(plane == 2) * sample;
    let row_bytes = resolution.width as usize * step;
    let mut data =
        Vec::with_capacity(resolution.width as usize * resolution.height as usize * sample);

    for row in 0..resolution.height as usize {
        let offset =
            layout.offsets[source].saturating_add(row.saturating_mul(layout.pitches[source]));
        let invalid = || DecodeError::ReadbackPlane {
            plane,
            offset,
            bytes: row_bytes,
            size: bytes.len(),
        };
        if layout.pitches[source] < row_bytes {
            return Err(invalid());
        }
        let row = bytes
            .get(offset..offset.saturating_add(row_bytes))
            .ok_or_else(invalid)?;
        for pixel in row.chunks_exact(step) {
            if sample == 1 {
                data.push(pixel[component]);
            } else {
                let value = u16::from_le_bytes([pixel[component], pixel[component + 1]])
                    >> (16 - depth.bits());
                data.extend_from_slice(&value.to_le_bytes());
            }
        }
    }

    Ok(Plane {
        data,
        resolution,
        bytes_per_sample,
    })
}
