use crate::ProtocolError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProtocolVersion(pub u16);

impl ProtocolVersion {
    pub const CURRENT: Self = Self(2);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum StreamKind {
    Video = 0,
    Audio = 1,
    Input = 2,
    Control = 3,
    Fec = 4,
}

impl TryFrom<u8> for StreamKind {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Video),
            1 => Ok(Self::Audio),
            2 => Ok(Self::Input),
            3 => Ok(Self::Control),
            4 => Ok(Self::Fec),
            unknown => Err(ProtocolError::UnknownStreamKind(unknown)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PacketFlags(u8);

impl PacketFlags {
    pub const KEYFRAME: Self = Self(1);
    pub const LAST_FRAGMENT: Self = Self(1 << 1);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PacketHeader {
    pub version: ProtocolVersion,
    pub stream: StreamKind,
    pub flags: PacketFlags,
    pub sequence: u32,
    pub frame: u32,
    pub fragment_index: u16,
    pub fragment_count: u16,
}

impl PacketHeader {
    pub const SIZE: usize = 16;

    pub fn write(&self, buf: &mut [u8]) -> Result<(), ProtocolError> {
        let available = buf.len();
        let Some(out) = buf.get_mut(..Self::SIZE) else {
            return Err(ProtocolError::BufferTooShort {
                needed: Self::SIZE,
                available,
            });
        };

        out[0..2].copy_from_slice(&self.version.0.to_be_bytes());
        out[2] = self.stream as u8;
        out[3] = self.flags.bits();
        out[4..8].copy_from_slice(&self.sequence.to_be_bytes());
        out[8..12].copy_from_slice(&self.frame.to_be_bytes());
        out[12..14].copy_from_slice(&self.fragment_index.to_be_bytes());
        out[14..16].copy_from_slice(&self.fragment_count.to_be_bytes());
        Ok(())
    }

    pub fn read(buf: &[u8]) -> Result<Self, ProtocolError> {
        let Some(bytes) = buf.first_chunk::<{ Self::SIZE }>() else {
            return Err(ProtocolError::BufferTooShort {
                needed: Self::SIZE,
                available: buf.len(),
            });
        };

        let version = ProtocolVersion(u16::from_be_bytes([bytes[0], bytes[1]]));
        if version != ProtocolVersion::CURRENT {
            return Err(ProtocolError::VersionMismatch {
                expected: ProtocolVersion::CURRENT,
                found: version,
            });
        }

        let header = Self {
            version,
            stream: StreamKind::try_from(bytes[2])?,
            flags: PacketFlags::from_bits(bytes[3]),
            sequence: u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            frame: u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            fragment_index: u16::from_be_bytes([bytes[12], bytes[13]]),
            fragment_count: u16::from_be_bytes([bytes[14], bytes[15]]),
        };

        if header.fragment_index >= header.fragment_count {
            return Err(ProtocolError::FragmentOutOfRange {
                index: header.fragment_index,
                count: header.fragment_count,
            });
        }

        Ok(header)
    }

    pub fn parse(datagram: &[u8]) -> Result<(Self, &[u8]), ProtocolError> {
        let header = Self::read(datagram)?;

        Ok((header, &datagram[Self::SIZE..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PacketHeader {
        PacketHeader {
            version: ProtocolVersion::CURRENT,
            stream: StreamKind::Video,
            flags: PacketFlags::KEYFRAME.with(PacketFlags::LAST_FRAGMENT),
            sequence: 0xDEAD_BEEF,
            frame: 42,
            fragment_index: 7,
            fragment_count: 8,
        }
    }

    #[test]
    fn header_round_trips() {
        let header = sample();
        let mut buf = [0; PacketHeader::SIZE];

        header.write(&mut buf).unwrap();

        assert_eq!(PacketHeader::read(&buf), Ok(header));
    }

    #[test]
    fn parse_splits_header_from_payload() {
        let mut datagram = vec![0; PacketHeader::SIZE];
        sample().write(&mut datagram).unwrap();
        datagram.extend_from_slice(b"payload");

        let (header, payload) = PacketHeader::parse(&datagram).unwrap();

        assert_eq!(header, sample());
        assert_eq!(payload, b"payload");
    }

    #[test]
    fn fragment_index_beyond_count_is_rejected() {
        let mut header = sample();
        header.fragment_index = header.fragment_count;
        let mut buf = [0; PacketHeader::SIZE];
        header.write(&mut buf).unwrap();

        assert_eq!(
            PacketHeader::read(&buf),
            Err(ProtocolError::FragmentOutOfRange { index: 8, count: 8 })
        );
    }

    #[test]
    fn zero_fragment_count_is_rejected() {
        let mut header = sample();
        header.fragment_index = 0;
        header.fragment_count = 0;
        let mut buf = [0; PacketHeader::SIZE];
        header.write(&mut buf).unwrap();

        assert!(matches!(
            PacketHeader::read(&buf),
            Err(ProtocolError::FragmentOutOfRange { .. })
        ));
    }

    #[test]
    fn short_buffer_is_rejected() {
        let mut buf = [0; PacketHeader::SIZE - 1];

        assert_eq!(
            sample().write(&mut buf),
            Err(ProtocolError::BufferTooShort {
                needed: PacketHeader::SIZE,
                available: PacketHeader::SIZE - 1,
            })
        );
    }

    #[test]
    fn foreign_version_is_rejected() {
        let mut buf = [0; PacketHeader::SIZE];
        sample().write(&mut buf).unwrap();

        buf[1] = 0xFF;

        assert!(matches!(
            PacketHeader::read(&buf),
            Err(ProtocolError::VersionMismatch { .. })
        ));
    }
}
