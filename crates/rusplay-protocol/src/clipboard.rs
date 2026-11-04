use std::fmt;

use crate::ProtocolError;

const HEADER_LEN: usize = 4 + 2 + 2 + 2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ClipboardChunk {
    transfer: u32,
    index: u16,
    count: u16,
    len: u16,
    data: [u8; ClipboardChunk::MAX_DATA],
}

impl fmt::Debug for ClipboardChunk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClipboardChunk")
            .field("transfer", &self.transfer)
            .field("index", &self.index)
            .field("count", &self.count)
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl ClipboardChunk {
    pub const MAX_DATA: usize = 192;
    pub const MAX_COUNT: u16 = u16::MAX;
    pub const MAX_ENCODED_LEN: usize = HEADER_LEN + Self::MAX_DATA;
    pub const MAX_TEXT_BYTES: usize = Self::MAX_DATA * Self::MAX_COUNT as usize;

    pub fn new(transfer: u32, index: u16, count: u16, data: &[u8]) -> Result<Self, ProtocolError> {
        if count == 0 {
            return Err(ProtocolError::ClipboardChunkCount(count));
        }
        if index >= count {
            return Err(ProtocolError::ClipboardChunkIndex { index, count });
        }

        let is_last = index + 1 == count;
        let well_sized = if is_last {
            (1..=Self::MAX_DATA).contains(&data.len())
        } else {
            data.len() == Self::MAX_DATA
        };
        if !well_sized {
            return Err(ProtocolError::ClipboardChunkLength {
                len: data.len(),
                index,
                count,
            });
        }

        let mut bytes = [0; Self::MAX_DATA];
        bytes[..data.len()].copy_from_slice(data);

        Ok(Self {
            transfer,
            index,
            count,
            len: u16::try_from(data.len()).unwrap_or(u16::MAX),
            data: bytes,
        })
    }

    pub fn split(transfer: u32, text: &str) -> Result<Vec<Self>, ProtocolError> {
        let bytes = text.as_bytes();
        if bytes.is_empty() {
            return Err(ProtocolError::ClipboardTextEmpty);
        }
        if bytes.len() > Self::MAX_TEXT_BYTES {
            return Err(ProtocolError::ClipboardTextTooLong {
                len: bytes.len(),
                max: Self::MAX_TEXT_BYTES,
            });
        }

        let count = u16::try_from(bytes.len().div_ceil(Self::MAX_DATA))
            .map_err(|_| ProtocolError::ClipboardChunkCount(u16::MAX))?;

        bytes
            .chunks(Self::MAX_DATA)
            .zip(0..count)
            .map(|(data, index)| Self::new(transfer, index, count, data))
            .collect()
    }

    pub const fn transfer(&self) -> u32 {
        self.transfer
    }

    pub const fn index(&self) -> u16 {
        self.index
    }

    pub const fn count(&self) -> u16 {
        self.count
    }

    pub fn data(&self) -> &[u8] {
        &self.data[..usize::from(self.len)]
    }

    pub const fn encoded_len(&self) -> usize {
        HEADER_LEN + self.len as usize
    }

    pub fn write(&self, out: &mut [u8]) {
        out[0..4].copy_from_slice(&self.transfer.to_be_bytes());
        out[4..6].copy_from_slice(&self.index.to_be_bytes());
        out[6..8].copy_from_slice(&self.count.to_be_bytes());
        out[8..10].copy_from_slice(&self.len.to_be_bytes());
        out[HEADER_LEN..self.encoded_len()].copy_from_slice(self.data());
    }

    pub fn read(body: &[u8]) -> Result<Self, ProtocolError> {
        let Some((header, data)) = body.split_first_chunk::<HEADER_LEN>() else {
            return Err(ProtocolError::BufferTooShort {
                needed: HEADER_LEN,
                available: body.len(),
            });
        };

        let transfer = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        let index = u16::from_be_bytes([header[4], header[5]]);
        let count = u16::from_be_bytes([header[6], header[7]]);
        let len = usize::from(u16::from_be_bytes([header[8], header[9]]));

        match data.len().cmp(&len) {
            std::cmp::Ordering::Less => {
                return Err(ProtocolError::BufferTooShort {
                    needed: HEADER_LEN + len,
                    available: body.len(),
                });
            }
            std::cmp::Ordering::Greater => {
                return Err(ProtocolError::TrailingBytes {
                    extra: data.len() - len,
                });
            }
            std::cmp::Ordering::Equal => {}
        }

        Self::new(transfer, index, count, data)
    }
}

#[derive(Debug)]
struct Pending {
    transfer: u32,
    count: u16,
    received: Vec<bool>,
    remaining: u16,
    bytes: Vec<u8>,
    last_len: usize,
}

#[derive(Debug)]
pub struct ClipboardReassembler {
    max_bytes: usize,
    pending: Option<Pending>,
    finished: Option<u32>,
}

impl ClipboardReassembler {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            pending: None,
            finished: None,
        }
    }

    pub fn push(&mut self, chunk: &ClipboardChunk) -> Result<Option<String>, ProtocolError> {
        let minimum = (usize::from(chunk.count) - 1) * ClipboardChunk::MAX_DATA + 1;
        if minimum > self.max_bytes {
            return Err(ProtocolError::ClipboardTooLarge {
                len: minimum,
                max: self.max_bytes,
            });
        }

        if self
            .finished
            .is_some_and(|done| !is_newer(chunk.transfer, done))
        {
            return Ok(None);
        }

        let supersedes = match &self.pending {
            None => true,
            Some(pending) if pending.transfer == chunk.transfer => false,
            Some(pending) => {
                if !is_newer(chunk.transfer, pending.transfer) {
                    return Ok(None);
                }
                true
            }
        };
        if supersedes {
            self.pending = Some(Pending::start(chunk));
        }

        let Some(pending) = self.pending.as_mut() else {
            return Ok(None);
        };
        if pending.count != chunk.count {
            return Err(ProtocolError::ClipboardChunkCount(chunk.count));
        }

        let slot = usize::from(chunk.index);
        if pending.received[slot] {
            return Ok(None);
        }

        let start = slot * ClipboardChunk::MAX_DATA;
        pending.bytes[start..start + chunk.data().len()].copy_from_slice(chunk.data());
        pending.received[slot] = true;
        pending.remaining -= 1;
        if chunk.index + 1 == chunk.count {
            pending.last_len = chunk.data().len();
        }
        if pending.remaining > 0 {
            return Ok(None);
        }

        let Some(mut done) = self.pending.take() else {
            return Ok(None);
        };
        self.finished = Some(done.transfer);

        let total = usize::from(done.count - 1) * ClipboardChunk::MAX_DATA + done.last_len;
        if total > self.max_bytes {
            return Err(ProtocolError::ClipboardTooLarge {
                len: total,
                max: self.max_bytes,
            });
        }
        done.bytes.truncate(total);

        String::from_utf8(done.bytes)
            .map(Some)
            .map_err(|_| ProtocolError::ClipboardNotUtf8)
    }

    pub fn reset(&mut self) {
        self.pending = None;
        self.finished = None;
    }
}

impl Pending {
    fn start(chunk: &ClipboardChunk) -> Self {
        let count = usize::from(chunk.count);

        Self {
            transfer: chunk.transfer,
            count: chunk.count,
            received: vec![false; count],
            remaining: chunk.count,
            bytes: vec![0; count * ClipboardChunk::MAX_DATA],
            last_len: 0,
        }
    }
}

const fn is_newer(candidate: u32, reference: u32) -> bool {
    candidate.wrapping_sub(reference).cast_signed() > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(len: usize) -> String {
        "abcdefghij".chars().cycle().take(len).collect()
    }

    fn reassemble(
        reassembler: &mut ClipboardReassembler,
        chunks: &[ClipboardChunk],
    ) -> Result<Option<String>, ProtocolError> {
        let mut result = Ok(None);
        for chunk in chunks {
            result = reassembler.push(chunk);
            if !matches!(result, Ok(None)) {
                break;
            }
        }
        result
    }

    #[test]
    fn every_size_round_trips_through_the_wire_form() {
        for len in [
            1,
            2,
            ClipboardChunk::MAX_DATA - 1,
            ClipboardChunk::MAX_DATA,
            ClipboardChunk::MAX_DATA + 1,
            ClipboardChunk::MAX_DATA * 5,
            ClipboardChunk::MAX_DATA * 5 + 17,
        ] {
            let original = text(len);
            let mut reassembler = ClipboardReassembler::new(1 << 20);

            let chunks = ClipboardChunk::split(7, &original).unwrap();
            let mut wire = Vec::new();
            for chunk in &chunks {
                let mut buf = vec![0; chunk.encoded_len()];
                chunk.write(&mut buf);
                wire.push(ClipboardChunk::read(&buf).unwrap());
            }

            assert_eq!(wire, chunks);
            assert_eq!(
                reassemble(&mut reassembler, &wire).unwrap().as_deref(),
                Some(original.as_str())
            );
        }
    }

    #[test]
    fn chunks_arriving_out_of_order_still_complete() {
        let original = text(ClipboardChunk::MAX_DATA * 3 + 5);
        let mut chunks = ClipboardChunk::split(1, &original).unwrap();
        chunks.reverse();
        let mut reassembler = ClipboardReassembler::new(1 << 20);

        let result = reassemble(&mut reassembler, &chunks).unwrap();

        assert_eq!(result.as_deref(), Some(original.as_str()));
    }

    #[test]
    fn a_multibyte_character_split_across_chunks_survives() {
        let original = format!("a{}", "é".repeat(ClipboardChunk::MAX_DATA));
        let chunks = ClipboardChunk::split(1, &original).unwrap();
        let mut reassembler = ClipboardReassembler::new(1 << 20);

        let result = reassemble(&mut reassembler, &chunks).unwrap();

        assert_eq!(chunks.len(), 3);
        assert_eq!(result.as_deref(), Some(original.as_str()));
    }

    #[test]
    fn invalid_utf8_is_refused_when_the_transfer_completes() {
        let chunk = ClipboardChunk::new(1, 0, 1, &[0xFF, 0xFE, b'a']).unwrap();
        let mut reassembler = ClipboardReassembler::new(1024);

        assert_eq!(
            reassembler.push(&chunk),
            Err(ProtocolError::ClipboardNotUtf8)
        );
    }

    #[test]
    fn a_transfer_declaring_more_than_the_limit_allocates_nothing() {
        let chunk = ClipboardChunk::new(1, 0, 1000, &[b'a'; ClipboardChunk::MAX_DATA]).unwrap();
        let mut reassembler = ClipboardReassembler::new(4096);

        assert!(matches!(
            reassembler.push(&chunk),
            Err(ProtocolError::ClipboardTooLarge { max: 4096, .. })
        ));
        assert!(reassembler.pending.is_none());
    }

    #[test]
    fn a_transfer_whose_final_size_exceeds_the_limit_is_refused() {
        let original = text(ClipboardChunk::MAX_DATA * 2 + 100);
        let chunks = ClipboardChunk::split(1, &original).unwrap();
        let mut reassembler = ClipboardReassembler::new(ClipboardChunk::MAX_DATA * 2 + 50);

        assert!(matches!(
            reassemble(&mut reassembler, &chunks),
            Err(ProtocolError::ClipboardTooLarge { .. })
        ));
    }

    #[test]
    fn a_newer_transfer_replaces_an_unfinished_one_and_an_older_one_is_ignored() {
        let first = ClipboardChunk::split(1, &text(ClipboardChunk::MAX_DATA * 2)).unwrap();
        let second = ClipboardChunk::split(2, "second").unwrap();
        let mut reassembler = ClipboardReassembler::new(1 << 20);

        assert_eq!(reassembler.push(&first[0]), Ok(None));
        assert_eq!(
            reassembler.push(&second[0]).unwrap().as_deref(),
            Some("second")
        );
        assert_eq!(reassembler.push(&first[1]), Ok(None));
    }

    #[test]
    fn a_finished_transfer_is_not_delivered_twice() {
        let chunks = ClipboardChunk::split(5, "once").unwrap();
        let mut reassembler = ClipboardReassembler::new(1024);

        assert!(reassembler.push(&chunks[0]).unwrap().is_some());
        assert_eq!(reassembler.push(&chunks[0]), Ok(None));
    }

    #[test]
    fn a_chunk_that_changes_the_count_mid_transfer_is_refused() {
        let first = ClipboardChunk::new(1, 0, 3, &[b'a'; ClipboardChunk::MAX_DATA]).unwrap();
        let second = ClipboardChunk::new(1, 1, 4, &[b'a'; ClipboardChunk::MAX_DATA]).unwrap();
        let mut reassembler = ClipboardReassembler::new(1 << 20);

        reassembler.push(&first).unwrap();

        assert_eq!(
            reassembler.push(&second),
            Err(ProtocolError::ClipboardChunkCount(4))
        );
    }

    #[test]
    fn transfer_numbers_wrap() {
        let mut reassembler = ClipboardReassembler::new(1024);

        let last = ClipboardChunk::split(u32::MAX, "before").unwrap();
        let wrapped = ClipboardChunk::split(0, "after").unwrap();

        assert!(reassembler.push(&last[0]).unwrap().is_some());
        assert_eq!(
            reassembler.push(&wrapped[0]).unwrap().as_deref(),
            Some("after")
        );
    }

    #[test]
    fn malformed_chunks_are_refused() {
        let full = [b'a'; ClipboardChunk::MAX_DATA];

        assert_eq!(
            ClipboardChunk::new(1, 0, 0, &full),
            Err(ProtocolError::ClipboardChunkCount(0))
        );
        assert_eq!(
            ClipboardChunk::new(1, 2, 2, &full),
            Err(ProtocolError::ClipboardChunkIndex { index: 2, count: 2 })
        );
        assert!(matches!(
            ClipboardChunk::new(1, 0, 2, &full[..10]),
            Err(ProtocolError::ClipboardChunkLength { .. })
        ));
        assert!(matches!(
            ClipboardChunk::new(1, 1, 2, &[]),
            Err(ProtocolError::ClipboardChunkLength { .. })
        ));
        assert!(matches!(
            ClipboardChunk::new(1, 0, 1, &[0; ClipboardChunk::MAX_DATA + 1]),
            Err(ProtocolError::ClipboardChunkLength { .. })
        ));
    }

    #[test]
    fn a_body_whose_length_field_disagrees_with_its_bytes_is_refused() {
        let chunk = ClipboardChunk::split(1, "hello").unwrap()[0];
        let mut buf = vec![0; chunk.encoded_len() + 1];
        chunk.write(&mut buf);

        assert_eq!(
            ClipboardChunk::read(&buf),
            Err(ProtocolError::TrailingBytes { extra: 1 })
        );
        assert!(matches!(
            ClipboardChunk::read(&buf[..chunk.encoded_len() - 1]),
            Err(ProtocolError::BufferTooShort { .. })
        ));
        assert!(matches!(
            ClipboardChunk::read(&buf[..3]),
            Err(ProtocolError::BufferTooShort { .. })
        ));
    }

    #[test]
    fn empty_text_is_not_split() {
        assert_eq!(
            ClipboardChunk::split(1, ""),
            Err(ProtocolError::ClipboardTextEmpty)
        );
    }

    #[test]
    fn debug_output_never_shows_the_text() {
        let chunk = ClipboardChunk::split(1, "hunter2-secret").unwrap()[0];

        let shown = format!("{chunk:?}");

        assert!(!shown.contains("hunter2"));
        assert!(!shown.contains("104"));
    }

    #[test]
    fn wire_layout_is_big_endian() {
        let chunk = ClipboardChunk::new(0x0102_0304, 0, 1, b"hi").unwrap();
        let mut buf = vec![0; chunk.encoded_len()];

        chunk.write(&mut buf);

        assert_eq!(buf, [1, 2, 3, 4, 0, 0, 0, 1, 0, 2, b'h', b'i']);
    }
}
