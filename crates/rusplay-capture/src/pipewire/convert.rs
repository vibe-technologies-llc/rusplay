use std::{
    fs::File,
    os::{
        fd::{AsRawFd, BorrowedFd, OwnedFd},
        unix::fs::MetadataExt,
    },
};

use rusplay_core::Timestamp;

use crate::{
    BufferLease, CaptureError, Frame, FrameBuffer, Plane, modifier::DRM_FORMAT_MOD_INVALID,
    pipewire::format::NegotiatedFormat,
};

const BYTES_PER_PIXEL: u32 = 4;

#[derive(Debug, Clone, Copy)]
pub(crate) struct DmaBufPlane<'a> {
    pub fd: BorrowedFd<'a>,
    pub offset: u32,
    pub stride: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

pub(crate) fn dmabuf_frame(
    format: NegotiatedFormat,
    planes: &[DmaBufPlane<'_>],
    timestamp: Timestamp,
) -> Result<Frame, CaptureError> {
    let Some((first, rest)) = planes.split_first() else {
        return Err(CaptureError::NoPlanes);
    };

    let file = File::from(
        first
            .fd
            .try_clone_to_owned()
            .map_err(CaptureError::DmaBufDup)?,
    );
    let identity = identity_of(&file)?;
    for (index, plane) in rest.iter().enumerate() {
        if plane.fd.as_raw_fd() != first.fd.as_raw_fd() && borrowed_identity(plane.fd)? != identity
        {
            return Err(CaptureError::DmaBufPlanesSplit { plane: index + 1 });
        }
    }

    let planes = planes
        .iter()
        .map(|plane| {
            let stride = u32::try_from(plane.stride).map_err(|_| CaptureError::InvalidStride {
                stride: plane.stride,
                width: format.size.width,
                format: format.fourcc,
            })?;
            Ok(Plane {
                offset: plane.offset,
                stride,
            })
        })
        .collect::<Result<Vec<_>, CaptureError>>()?;

    Ok(Frame {
        resolution: format.size,
        format: format.fourcc,
        timestamp,
        color: format.color,
        buffer: FrameBuffer::DmaBuf {
            fd: OwnedFd::from(file),
            modifier: format.modifier.unwrap_or(DRM_FORMAT_MOD_INVALID),
            planes,
            lease: BufferLease::detached(),
        },
    })
}

fn identity_of(file: &File) -> Result<FileIdentity, CaptureError> {
    let metadata = file.metadata().map_err(CaptureError::DmaBufStat)?;
    Ok(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn borrowed_identity(fd: BorrowedFd<'_>) -> Result<FileIdentity, CaptureError> {
    let file = File::from(fd.try_clone_to_owned().map_err(CaptureError::DmaBufDup)?);
    identity_of(&file)
}

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
        color: format.color,
        buffer: FrameBuffer::Shm {
            data: pixels.to_vec(),
            stride,
        },
    })
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd;

    use rusplay_core::{ColorSpace, Resolution};

    use super::*;
    use crate::DrmFourcc;

    const AMD_TILED: u64 = 0x0200_0000_0000_0001;

    fn format(width: u32, height: u32) -> NegotiatedFormat {
        NegotiatedFormat {
            fourcc: DrmFourcc::from_code(*b"XR24"),
            size: Resolution::new(width, height),
            modifier: None,
            color: ColorSpace::default(),
        }
    }

    fn dmabuf_format() -> NegotiatedFormat {
        NegotiatedFormat {
            modifier: Some(AMD_TILED),
            ..format(1920, 1080)
        }
    }

    #[test]
    fn dmabuf_planes_sharing_one_buffer_become_one_frame() {
        let buffer = tempfile::tempfile().unwrap();
        let alias = buffer.try_clone().unwrap();
        let planes = [
            DmaBufPlane {
                fd: buffer.as_fd(),
                offset: 0,
                stride: 7680,
            },
            DmaBufPlane {
                fd: alias.as_fd(),
                offset: 8_294_400,
                stride: 256,
            },
        ];

        let frame = dmabuf_frame(dmabuf_format(), &planes, Timestamp::from_micros(9)).unwrap();

        let FrameBuffer::DmaBuf {
            fd,
            modifier,
            planes,
            ..
        } = frame.buffer
        else {
            panic!("expected a dma-buf frame");
        };
        assert_eq!(modifier, AMD_TILED);
        assert_eq!(
            planes,
            [
                Plane {
                    offset: 0,
                    stride: 7680
                },
                Plane {
                    offset: 8_294_400,
                    stride: 256
                },
            ]
        );
        assert_ne!(fd.as_raw_fd(), buffer.as_raw_fd());
        assert_eq!(
            borrowed_identity(fd.as_fd()).unwrap(),
            borrowed_identity(buffer.as_fd()).unwrap()
        );
    }

    #[test]
    fn dmabuf_without_a_modifier_reports_the_implicit_one() {
        let buffer = tempfile::tempfile().unwrap();
        let planes = [DmaBufPlane {
            fd: buffer.as_fd(),
            offset: 0,
            stride: 7680,
        }];

        let frame = dmabuf_frame(format(1920, 1080), &planes, Timestamp::ZERO).unwrap();

        assert!(matches!(
            frame.buffer,
            FrameBuffer::DmaBuf {
                modifier: DRM_FORMAT_MOD_INVALID,
                ..
            }
        ));
    }

    #[test]
    fn dmabuf_planes_in_separate_buffers_are_rejected() {
        let first = tempfile::tempfile().unwrap();
        let second = tempfile::tempfile().unwrap();
        let planes = [
            DmaBufPlane {
                fd: first.as_fd(),
                offset: 0,
                stride: 7680,
            },
            DmaBufPlane {
                fd: second.as_fd(),
                offset: 0,
                stride: 256,
            },
        ];

        let result = dmabuf_frame(dmabuf_format(), &planes, Timestamp::ZERO);

        assert!(matches!(
            result,
            Err(CaptureError::DmaBufPlanesSplit { plane: 1 })
        ));
    }

    #[test]
    fn dmabuf_without_planes_is_rejected() {
        let result = dmabuf_frame(dmabuf_format(), &[], Timestamp::ZERO);

        assert!(matches!(result, Err(CaptureError::NoPlanes)));
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
