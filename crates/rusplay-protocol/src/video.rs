use crate::ProtocolError;

const FLAG_RECOVERY: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFrameHeader {
    pub captured_micros: u64,
    pub recovers_from: Option<u32>,
}

impl VideoFrameHeader {
    pub const SIZE: usize = 13;

    pub fn write(&self, out: &mut Vec<u8>) {
        let (flags, reference) = match self.recovers_from {
            Some(reference) => (FLAG_RECOVERY, reference),
            None => (0, 0),
        };

        out.extend_from_slice(&self.captured_micros.to_be_bytes());
        out.push(flags);
        out.extend_from_slice(&reference.to_be_bytes());
    }

    pub fn parse(frame: &[u8]) -> Result<(Self, &[u8]), ProtocolError> {
        let Some((head, body)) = frame.split_first_chunk::<{ Self::SIZE }>() else {
            return Err(ProtocolError::BufferTooShort {
                needed: Self::SIZE,
                available: frame.len(),
            });
        };
        let [m0, m1, m2, m3, m4, m5, m6, m7, flags, r0, r1, r2, r3] = *head;
        let captured = [m0, m1, m2, m3, m4, m5, m6, m7];
        let reference = u32::from_be_bytes([r0, r1, r2, r3]);

        let recovers_from = match flags {
            FLAG_RECOVERY => Some(reference),
            0 if reference == 0 => None,
            0 => return Err(ProtocolError::ReferenceWithoutRecovery(reference)),
            unknown => return Err(ProtocolError::UnknownVideoFrameFlags(unknown)),
        };

        Ok((
            Self {
                captured_micros: u64::from_be_bytes(captured),
                recovers_from,
            },
            body,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: VideoFrameHeader = VideoFrameHeader {
        captured_micros: 0x0102_0304_0506_0708,
        recovers_from: None,
    };

    const RECOVERY: VideoFrameHeader = VideoFrameHeader {
        captured_micros: 7,
        recovers_from: Some(0x0A0B_0C0D),
    };

    #[test]
    fn the_header_leads_the_frame_in_big_endian() {
        let mut frame = Vec::new();
        PLAIN.write(&mut frame);
        frame.extend_from_slice(&[9, 9]);

        let (header, body) = VideoFrameHeader::parse(&frame).unwrap();

        assert_eq!(&frame[..13], [1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0, 0]);
        assert_eq!(header, PLAIN);
        assert_eq!(body, [9, 9]);
    }

    #[test]
    fn a_recovery_frame_names_its_reference_after_the_flag() {
        let mut frame = Vec::new();
        RECOVERY.write(&mut frame);

        let (header, body) = VideoFrameHeader::parse(&frame).unwrap();

        assert_eq!(&frame[8..], [1, 0x0A, 0x0B, 0x0C, 0x0D]);
        assert_eq!(header, RECOVERY);
        assert_eq!(body, [0u8; 0]);
    }

    #[test]
    fn a_recovery_from_frame_zero_round_trips() {
        let header = VideoFrameHeader {
            captured_micros: 0,
            recovers_from: Some(0),
        };
        let mut frame = Vec::new();
        header.write(&mut frame);

        assert_eq!(VideoFrameHeader::parse(&frame).unwrap().0, header);
    }

    #[test]
    fn unknown_flags_are_rejected() {
        let mut frame = [0; VideoFrameHeader::SIZE];
        frame[8] = 0x82;

        assert_eq!(
            VideoFrameHeader::parse(&frame),
            Err(ProtocolError::UnknownVideoFrameFlags(0x82))
        );
    }

    #[test]
    fn a_reference_without_the_recovery_flag_is_rejected() {
        let mut frame = [0; VideoFrameHeader::SIZE];
        frame[12] = 5;

        assert_eq!(
            VideoFrameHeader::parse(&frame),
            Err(ProtocolError::ReferenceWithoutRecovery(5))
        );
    }

    #[test]
    fn a_frame_shorter_than_the_header_is_rejected() {
        assert_eq!(
            VideoFrameHeader::parse(&[1, 2, 3]),
            Err(ProtocolError::BufferTooShort {
                needed: 13,
                available: 3,
            })
        );
    }

    #[test]
    fn a_header_alone_leaves_an_empty_body() {
        let (_, body) = VideoFrameHeader::parse(&[0; 13]).unwrap();

        assert_eq!(body, [0u8; 0]);
    }

    #[test]
    fn every_strict_prefix_of_the_header_is_rejected() {
        for header in [PLAIN, RECOVERY] {
            let mut frame = Vec::new();
            header.write(&mut frame);

            for cut in 0..VideoFrameHeader::SIZE {
                assert_eq!(
                    VideoFrameHeader::parse(&frame[..cut]),
                    Err(ProtocolError::BufferTooShort {
                        needed: VideoFrameHeader::SIZE,
                        available: cut,
                    })
                );
            }
        }
    }
}
