use crate::ProtocolError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFrameHeader {
    pub captured_micros: u64,
}

impl VideoFrameHeader {
    pub const SIZE: usize = 8;

    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.captured_micros.to_be_bytes());
    }

    pub fn parse(frame: &[u8]) -> Result<(Self, &[u8]), ProtocolError> {
        let Some((head, body)) = frame.split_first_chunk::<{ Self::SIZE }>() else {
            return Err(ProtocolError::BufferTooShort {
                needed: Self::SIZE,
                available: frame.len(),
            });
        };

        Ok((
            Self {
                captured_micros: u64::from_be_bytes(*head),
            },
            body,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_leads_the_frame_in_big_endian() {
        let mut frame = Vec::new();
        VideoFrameHeader {
            captured_micros: 0x0102_0304_0506_0708,
        }
        .write(&mut frame);
        frame.extend_from_slice(&[9, 9]);

        let (header, body) = VideoFrameHeader::parse(&frame).unwrap();

        assert_eq!(&frame[..8], [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(header.captured_micros, 0x0102_0304_0506_0708);
        assert_eq!(body, [9, 9]);
    }

    #[test]
    fn a_frame_shorter_than_the_header_is_rejected() {
        assert_eq!(
            VideoFrameHeader::parse(&[1, 2, 3]),
            Err(ProtocolError::BufferTooShort {
                needed: 8,
                available: 3,
            })
        );
    }

    #[test]
    fn a_header_alone_leaves_an_empty_body() {
        let (_, body) = VideoFrameHeader::parse(&[0; 8]).unwrap();

        assert_eq!(body, [0u8; 0]);
    }
}
