use crate::{ProtocolError, StreamKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FecHeader {
    pub protects: StreamKind,
    pub first_fragment: u16,
    pub data_shards: u8,
    pub parity_shards: u8,
    pub parity_index: u8,
    pub frame_bytes: u32,
}

impl FecHeader {
    pub const SIZE: usize = 10;

    pub fn write(&self, buf: &mut [u8]) -> Result<(), ProtocolError> {
        let available = buf.len();
        let Some(out) = buf.get_mut(..Self::SIZE) else {
            return Err(ProtocolError::BufferTooShort {
                needed: Self::SIZE,
                available,
            });
        };

        out[0] = self.protects as u8;
        out[1..3].copy_from_slice(&self.first_fragment.to_be_bytes());
        out[3] = self.data_shards;
        out[4] = self.parity_shards;
        out[5] = self.parity_index;
        out[6..10].copy_from_slice(&self.frame_bytes.to_be_bytes());
        Ok(())
    }

    pub fn parse(payload: &[u8]) -> Result<(Self, &[u8]), ProtocolError> {
        let Some(bytes) = payload.first_chunk::<{ Self::SIZE }>() else {
            return Err(ProtocolError::BufferTooShort {
                needed: Self::SIZE,
                available: payload.len(),
            });
        };

        let header = Self {
            protects: StreamKind::try_from(bytes[0])?,
            first_fragment: u16::from_be_bytes([bytes[1], bytes[2]]),
            data_shards: bytes[3],
            parity_shards: bytes[4],
            parity_index: bytes[5],
            frame_bytes: u32::from_be_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]),
        };

        if header.data_shards == 0
            || header.parity_shards == 0
            || header.parity_index >= header.parity_shards
        {
            return Err(ProtocolError::InvalidFecGeometry {
                data_shards: header.data_shards,
                parity_shards: header.parity_shards,
                parity_index: header.parity_index,
            });
        }

        Ok((header, &payload[Self::SIZE..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> FecHeader {
        FecHeader {
            protects: StreamKind::Video,
            first_fragment: 300,
            data_shards: 16,
            parity_shards: 3,
            parity_index: 2,
            frame_bytes: 123_456,
        }
    }

    #[test]
    fn fec_header_round_trips_with_payload() {
        let mut buf = vec![0; FecHeader::SIZE];
        sample().write(&mut buf).unwrap();
        buf.extend_from_slice(&[9, 8, 7]);

        let (header, parity) = FecHeader::parse(&buf).unwrap();

        assert_eq!(header, sample());
        assert_eq!(parity, [9, 8, 7]);
    }

    #[test]
    fn short_payload_is_rejected() {
        assert_eq!(
            FecHeader::parse(&[0; FecHeader::SIZE - 1]),
            Err(ProtocolError::BufferTooShort {
                needed: FecHeader::SIZE,
                available: FecHeader::SIZE - 1,
            })
        );
    }

    #[test]
    fn short_write_buffer_is_rejected() {
        assert!(matches!(
            sample().write(&mut [0; 3]),
            Err(ProtocolError::BufferTooShort { .. })
        ));
    }

    #[test]
    fn parity_index_beyond_parity_count_is_rejected() {
        let mut header = sample();
        header.parity_index = header.parity_shards;
        let mut buf = [0; FecHeader::SIZE];
        header.write(&mut buf).unwrap();

        assert!(matches!(
            FecHeader::parse(&buf),
            Err(ProtocolError::InvalidFecGeometry { .. })
        ));
    }

    #[test]
    fn zero_data_shards_are_rejected() {
        let mut header = sample();
        header.data_shards = 0;
        let mut buf = [0; FecHeader::SIZE];
        header.write(&mut buf).unwrap();

        assert!(matches!(
            FecHeader::parse(&buf),
            Err(ProtocolError::InvalidFecGeometry { .. })
        ));
    }

    #[test]
    fn unknown_protected_stream_is_rejected() {
        let mut buf = [0; FecHeader::SIZE];
        sample().write(&mut buf).unwrap();
        buf[0] = 0xEE;

        assert_eq!(
            FecHeader::parse(&buf),
            Err(ProtocolError::UnknownStreamKind(0xEE))
        );
    }
}
