use rusplay_core::Timestamp;

use crate::{CaptureError, Frame, FrameBuffer, pipewire::format::NegotiatedFormat};

const BYTES_PER_PIXEL: u32 = 4;

#[derive(Debug, Clone, Copy)]
pub(crate) struct ShmPlane<'a> {
    pub mapped: &'a [u8],
    pub offset: u32,
    pub stride: i32,
}

pub(crate) fn shm_frame(
    format: NegotiatedFormat,
    plane: ShmPlane<'_>,
    timestamp: Timestamp,
) -> Result<Frame, CaptureError> {
    let invalid_stride = CaptureError::InvalidStride {
        stride: plane.stride,
        width: format.size.width,
        format: format.fourcc,
    };
    let stride = u32::try_from(plane.stride).map_err(|_| invalid_stride)?;
    let row_bytes = format.size.width.saturating_mul(BYTES_PER_PIXEL);
    if stride < row_bytes {
        return Err(CaptureError::InvalidStride {
            stride: plane.stride,
            width: format.size.width,
            format: format.fourcc,
        });
    }

    let size = stride.saturating_mul(format.size.height);
    let out_of_bounds = CaptureError::ChunkOutOfBounds {
        offset: plane.offset,
        size,
        mapped: plane.mapped.len(),
    };
    let start = usize::try_from(plane.offset).map_err(|_| out_of_bounds)?;
    let Some(pixels) = usize::try_from(size)
        .ok()
        .and_then(|size| plane.mapped.get(start..start.checked_add(size)?))
    else {
        return Err(CaptureError::ChunkOutOfBounds {
            offset: plane.offset,
            size,
            mapped: plane.mapped.len(),
        });
    };

    Ok(Frame {
        resolution: format.size,
        format: format.fourcc,
        timestamp,
        buffer: FrameBuffer::Shm {
            data: pixels.to_vec(),
            stride,
        },
    })
}

#[cfg(test)]
mod tests {
    use rusplay_core::Resolution;

    use super::*;
    use crate::DrmFourcc;

    fn format(width: u32, height: u32) -> NegotiatedFormat {
        NegotiatedFormat {
            fourcc: DrmFourcc::from_code(*b"XR24"),
            size: Resolution::new(width, height),
        }
    }

    #[test]
    fn shm_plane_is_copied_from_its_offset() {
        let mapped: Vec<u8> = (0..=255).collect();

        let frame = shm_frame(
            format(2, 2),
            ShmPlane {
                mapped: &mapped,
                offset: 16,
                stride: 12,
            },
            Timestamp::from_micros(5),
        )
        .unwrap();

        let FrameBuffer::Shm { data, stride } = frame.buffer else {
            panic!("expected a shared-memory frame");
        };
        assert_eq!(stride, 12);
        assert_eq!(data, mapped[16..40]);
        assert_eq!(frame.timestamp, Timestamp::from_micros(5));
        assert_eq!(frame.resolution, Resolution::new(2, 2));
    }

    #[test]
    fn stride_shorter_than_a_row_is_rejected() {
        let mapped = [0; 64];

        let result = shm_frame(
            format(4, 1),
            ShmPlane {
                mapped: &mapped,
                offset: 0,
                stride: 8,
            },
            Timestamp::ZERO,
        );

        assert!(matches!(result, Err(CaptureError::InvalidStride { .. })));
    }

    #[test]
    fn negative_stride_is_rejected() {
        let mapped = [0; 64];

        let result = shm_frame(
            format(1, 1),
            ShmPlane {
                mapped: &mapped,
                offset: 0,
                stride: -4,
            },
            Timestamp::ZERO,
        );

        assert!(matches!(result, Err(CaptureError::InvalidStride { .. })));
    }

    #[test]
    fn chunk_past_the_mapping_is_rejected() {
        let mapped = [0; 16];

        let result = shm_frame(
            format(2, 2),
            ShmPlane {
                mapped: &mapped,
                offset: 4,
                stride: 8,
            },
            Timestamp::ZERO,
        );

        assert!(matches!(
            result,
            Err(CaptureError::ChunkOutOfBounds {
                offset: 4,
                size: 16,
                mapped: 16
            })
        ));
    }
}
