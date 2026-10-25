use crate::{PacketFlags, PacketHeader, ProtocolError, ProtocolVersion, StreamKind};

const SECTION_LEN_SIZE: usize = 2;
const MAX_RECORD_LEN: usize = u8::MAX as usize;

pub fn push_input_record(out: &mut Vec<u8>, record: &[u8]) -> Result<(), ProtocolError> {
    let Ok(len) = u8::try_from(record.len()) else {
        return Err(ProtocolError::InputRecordTooLong {
            len: record.len(),
            max: MAX_RECORD_LEN,
        });
    };

    out.push(len);
    out.extend_from_slice(record);

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputRecords<'a> {
    rest: &'a [u8],
}

impl<'a> InputRecords<'a> {
    pub const fn new(section: &'a [u8]) -> Self {
        Self { rest: section }
    }
}

impl<'a> Iterator for InputRecords<'a> {
    type Item = Result<&'a [u8], ProtocolError>;

    fn next(&mut self) -> Option<Self::Item> {
        let (&len, body) = self.rest.split_first()?;
        let len = usize::from(len);

        let Some((record, rest)) = body.split_at_checked(len) else {
            self.rest = &[];
            return Some(Err(ProtocolError::BufferTooShort {
                needed: len,
                available: body.len(),
            }));
        };

        self.rest = rest;

        Some(Ok(record))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputPacket<'a> {
    pub sequence: u32,
    pub current: &'a [u8],
    pub previous: &'a [u8],
}

impl<'a> InputPacket<'a> {
    pub const OVERHEAD: usize = PacketHeader::SIZE + SECTION_LEN_SIZE;

    pub const fn encoded_len(&self) -> usize {
        Self::OVERHEAD + self.current.len() + self.previous.len()
    }

    pub const fn current_records(&self) -> InputRecords<'a> {
        InputRecords::new(self.current)
    }

    pub const fn previous_records(&self) -> InputRecords<'a> {
        InputRecords::new(self.previous)
    }

    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), ProtocolError> {
        let Ok(current_len) = u16::try_from(self.current.len()) else {
            return Err(ProtocolError::InputSectionTooLong {
                len: self.current.len(),
            });
        };

        let header = PacketHeader {
            version: ProtocolVersion::CURRENT,
            stream: StreamKind::Input,
            flags: PacketFlags::empty(),
            sequence: self.sequence,
            frame: 0,
            fragment_index: 0,
            fragment_count: 1,
        };
        let start = out.len();
        out.resize(start + PacketHeader::SIZE, 0);
        header.write(&mut out[start..])?;

        out.extend_from_slice(&current_len.to_be_bytes());
        out.extend_from_slice(self.current);
        out.extend_from_slice(self.previous);

        Ok(())
    }

    pub fn read(datagram: &'a [u8]) -> Result<Self, ProtocolError> {
        let (header, payload) = PacketHeader::parse(datagram)?;

        if header.stream != StreamKind::Input {
            return Err(ProtocolError::UnexpectedStream {
                expected: StreamKind::Input,
                found: header.stream,
            });
        }

        let Some((length, sections)) = payload.split_first_chunk::<SECTION_LEN_SIZE>() else {
            return Err(ProtocolError::BufferTooShort {
                needed: Self::OVERHEAD,
                available: datagram.len(),
            });
        };

        let current_len = usize::from(u16::from_be_bytes(*length));
        let Some((current, previous)) = sections.split_at_checked(current_len) else {
            return Err(ProtocolError::BufferTooShort {
                needed: Self::OVERHEAD + current_len,
                available: datagram.len(),
            });
        };

        Ok(Self {
            sequence: header.sequence,
            current,
            previous,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAdmission {
    Stale,
    Current,
    PreviousAndCurrent,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct InputSequence {
    last: Option<u32>,
}

impl InputSequence {
    pub const fn new() -> Self {
        Self { last: None }
    }

    pub fn reset(&mut self) {
        self.last = None;
    }

    pub fn admit(&mut self, sequence: u32) -> InputAdmission {
        let admission = match self.last {
            None if sequence == 1 => InputAdmission::PreviousAndCurrent,
            None => InputAdmission::Current,
            Some(last) => match sequence.wrapping_sub(last).cast_signed() {
                ..=0 => return InputAdmission::Stale,
                1 => InputAdmission::Current,
                _ => InputAdmission::PreviousAndCurrent,
            },
        };

        self.last = Some(sequence);

        admission
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(records: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for record in records {
            push_input_record(&mut out, record).unwrap();
        }
        out
    }

    #[test]
    fn packet_round_trips_with_both_sections() {
        let current = section(&[&[1, 2, 3], &[4]]);
        let previous = section(&[&[9, 9]]);
        let packet = InputPacket {
            sequence: u32::MAX,
            current: &current,
            previous: &previous,
        };

        let mut datagram = Vec::new();
        packet.write(&mut datagram).unwrap();

        assert_eq!(datagram.len(), packet.encoded_len());
        assert_eq!(InputPacket::read(&datagram), Ok(packet));
    }

    #[test]
    fn records_are_yielded_in_order() {
        let bytes = section(&[&[1, 2, 3], &[], &[4]]);

        let records: Vec<_> = InputRecords::new(&bytes).map(Result::unwrap).collect();

        assert_eq!(records, vec![&[1, 2, 3][..], &[][..], &[4][..]]);
    }

    #[test]
    fn truncated_record_is_reported_once() {
        let mut records = InputRecords::new(&[5, 1, 2]);

        assert_eq!(
            records.next(),
            Some(Err(ProtocolError::BufferTooShort {
                needed: 5,
                available: 2
            }))
        );
        assert_eq!(records.next(), None);
    }

    #[test]
    fn oversized_record_is_refused() {
        let mut out = Vec::new();

        assert_eq!(
            push_input_record(&mut out, &[0; 256]),
            Err(ProtocolError::InputRecordTooLong { len: 256, max: 255 })
        );
        assert_eq!(out, Vec::<u8>::new());
    }

    #[test]
    fn oversized_current_section_is_refused() {
        let current = vec![0; usize::from(u16::MAX) + 1];
        let packet = InputPacket {
            sequence: 0,
            current: &current,
            previous: &[],
        };

        assert_eq!(
            packet.write(&mut Vec::new()),
            Err(ProtocolError::InputSectionTooLong {
                len: usize::from(u16::MAX) + 1
            })
        );
    }

    #[test]
    fn short_or_foreign_datagrams_are_rejected() {
        let mut datagram = Vec::new();
        InputPacket {
            sequence: 1,
            current: &[1, 7],
            previous: &[],
        }
        .write(&mut datagram)
        .unwrap();

        assert!(matches!(
            InputPacket::read(&datagram[..datagram.len() - 1]),
            Err(ProtocolError::BufferTooShort { .. })
        ));
        assert!(matches!(
            InputPacket::read(&datagram[..PacketHeader::SIZE]),
            Err(ProtocolError::BufferTooShort { .. })
        ));

        datagram[2] = StreamKind::Video as u8;
        assert_eq!(
            InputPacket::read(&datagram),
            Err(ProtocolError::UnexpectedStream {
                expected: StreamKind::Input,
                found: StreamKind::Video,
            })
        );
    }

    #[test]
    fn consecutive_sequences_apply_only_the_current_section() {
        let mut sequence = InputSequence::new();

        assert_eq!(sequence.admit(0), InputAdmission::Current);
        assert_eq!(sequence.admit(1), InputAdmission::Current);
        assert_eq!(sequence.admit(2), InputAdmission::Current);
    }

    #[test]
    fn a_gap_recovers_the_previous_section() {
        let mut sequence = InputSequence::new();
        sequence.admit(10);

        assert_eq!(sequence.admit(12), InputAdmission::PreviousAndCurrent);
        assert_eq!(sequence.admit(20), InputAdmission::PreviousAndCurrent);
    }

    #[test]
    fn duplicates_and_reordered_packets_are_stale() {
        let mut sequence = InputSequence::new();
        sequence.admit(10);
        sequence.admit(12);

        assert_eq!(sequence.admit(12), InputAdmission::Stale);
        assert_eq!(sequence.admit(11), InputAdmission::Stale);
        assert_eq!(sequence.admit(13), InputAdmission::Current);
    }

    #[test]
    fn the_first_packet_recovers_the_previous_only_when_sequence_zero_was_lost() {
        let mut after_loss = InputSequence::new();
        let mut mid_stream = InputSequence::new();

        assert_eq!(after_loss.admit(1), InputAdmission::PreviousAndCurrent);
        assert_eq!(mid_stream.admit(500), InputAdmission::Current);
    }

    #[test]
    fn wraparound_counts_as_consecutive() {
        let mut sequence = InputSequence::new();
        sequence.admit(u32::MAX);

        assert_eq!(sequence.admit(0), InputAdmission::Current);
        assert_eq!(sequence.admit(u32::MAX), InputAdmission::Stale);
    }

    #[test]
    fn reset_forgets_the_history() {
        let mut sequence = InputSequence::new();
        sequence.admit(40);

        sequence.reset();

        assert_eq!(sequence.admit(3), InputAdmission::Current);
    }
}
